//! 中继 DataChannel 验证：open → 对端自动收到 → 发包络往返。

use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;
use voice_webrtc::{PeerConfig, PeerState, VoicePeer};

#[tokio::test]
async fn relay_channel_should_carry_envelopes() -> anyhow::Result<()> {
    let run = async {
        let config = PeerConfig {
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

        // 中继通道建 peer 时 upfront 建好：直接发探测包，B 侧经 on_data_channel 收包
        let probe = vec![0xAAu8; 32];
        timeout(Duration::from_secs(10), async {
            loop {
                let _ = a.send_relay(&probe).await;
                match timeout(Duration::from_millis(200), eb.relay_frames.recv()).await {
                    Ok(Some(got)) if got == probe => break,
                    _ => continue,
                }
            }
        })
        .await?;

        a.close().await?;
        b.close().await?;
        anyhow::Ok(())
    };
    timeout(Duration::from_secs(60), run).await??;
    Ok(())
}
