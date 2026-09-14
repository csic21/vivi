//! Week 5 最小闭环：offer/answer + trickle + 30 帧透传 + Direct 选路。

use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;
use voice_common::RouteType;
use voice_webrtc::{PeerConfig, PeerState, VoicePeer};

fn pattern(seq: u32) -> Vec<u8> {
    let mut v = vec![0x5Au8; 40];
    v[..4].copy_from_slice(&seq.to_le_bytes());
    v
}

#[tokio::test]
async fn p2p_should_connect_and_relay_opus_frames() -> anyhow::Result<()> {
    let run = async {
        let config = PeerConfig {
            // 单测不依赖公网 STUN：host candidate 足够连通
            stun_urls: vec![],
            ..PeerConfig::default()
        };
        let (a, mut ea) = VoicePeer::new(&config).await?;
        let (b, mut eb) = VoicePeer::new(&config).await?;

        let (ready_tx_a, ready_rx_a) = tokio::sync::watch::channel(false);
        let (ready_tx_b, ready_rx_b) = tokio::sync::watch::channel(false);
        for (peer, mut ready, mut rx) in [
            (Arc::clone(&b), ready_rx_b, ea.local_candidates),
            (Arc::clone(&a), ready_rx_a, eb.local_candidates),
        ] {
            tokio::spawn(async move {
                let mut pending: Vec<String> = Vec::new();
                loop {
                    tokio::select! {
                        c = rx.recv() => {
                            let Some(c) = c else { break };
                            if *ready.borrow() {
                                for p in pending.drain(..) {
                                    let _ = peer.add_remote_candidate(&p).await;
                                }
                                let _ = peer.add_remote_candidate(&c).await;
                            } else {
                                pending.push(c);
                            }
                        }
                        _ = ready.changed() => {
                            if *ready.borrow() {
                                for p in pending.drain(..) {
                                    let _ = peer.add_remote_candidate(&p).await;
                                }
                            }
                        }
                    }
                }
            });
        }

        let offer = a.create_offer().await?;
        let answer = b.apply_offer(&offer).await?;
        let _ = ready_tx_b.send(true);
        a.apply_answer(&answer).await?;
        let _ = ready_tx_a.send(true);

        for (name, states) in [("A", &mut ea.states), ("B", &mut eb.states)] {
            timeout(Duration::from_secs(20), async {
                while let Some(s) = states.recv().await {
                    assert_ne!(s, PeerState::Failed, "{name} failed");
                    if s == PeerState::Connected {
                        break;
                    }
                }
            })
            .await?;
        }

        const N: u32 = 30;
        for seq in 0..N {
            a.send_opus(&pattern(seq)).await?;
        }
        for seq in 0..N {
            let got = timeout(Duration::from_secs(5), eb.remote_frames.recv())
                .await?
                .expect("remote frames closed");
            assert_eq!(got.payload, pattern(seq));
        }

        // 选路收敛需要远端 candidate 条目进 stats：轮询等，最多 15s
        let mut route = None;
        for _ in 0..150 {
            if let Some(r) = a.refresh_route().await {
                route = Some(r);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(route, Some(RouteType::Direct));

        a.close().await?;
        b.close().await?;
        anyhow::Ok(())
    };
    timeout(Duration::from_secs(60), run).await??;
    Ok(())
}
