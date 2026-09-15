//! Week 6 relay 验证：强制 relay，经本地 coturn 中转语音帧。
//!
//! 前置：
//! ```sh
//! docker run -d --name vivi-coturn -p 3478:3478 -p 3478:3478/udp \
//!   -p 49160-49200:49160-49200/udp \
//!   -v $PWD/infra/coturn/turnserver.conf:/etc/coturn/turnserver.conf:ro \
//!   coturn/coturn:latest
//! TURN_TEST=1 cargo test -p voice-webrtc --test p2p_relay
//! ```
//! 未设 TURN_TEST 时自动跳过（CI 友好）。

use base64::prelude::*;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;
use voice_common::RouteType;
use voice_webrtc::{PeerConfig, PeerState, VoicePeer};

/// 与 signaling TURN REST 同算法的 dev 凭证（dev-secret-change-me）。
/// 生产凭证走信令 GET /turn/credentials，这里只为验证 coturn 互通。
fn dev_turn_creds(user: &str) -> (String, String) {
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        + 86400;
    let username = format!("{expiry}:{user}");
    let mut mac = Hmac::<Sha1>::new_from_slice(b"dev-secret-change-me").unwrap();
    mac.update(username.as_bytes());
    let credential = BASE64_STANDARD.encode(mac.finalize().into_bytes());
    (username, credential)
}

fn pattern(seq: u32) -> Vec<u8> {
    let mut v = vec![0x5Au8; 40];
    v[..4].copy_from_slice(&seq.to_le_bytes());
    v
}

#[tokio::test]
async fn p2p_should_relay_opus_via_turn() -> anyhow::Result<()> {
    if std::env::var("TURN_TEST").is_err() {
        eprintln!("skip: set TURN_TEST=1 with local coturn on 127.0.0.1:3478");
        return Ok(());
    }
    let run = async {
        let mut peers = Vec::new();
        for user in ["relay-a", "relay-b"] {
            let (username, credential) = dev_turn_creds(user);
            peers.push(PeerConfig {
                stun_urls: vec![],
                turn_urls: vec!["turn:127.0.0.1:3478".into()],
                turn_username: Some(username),
                turn_credential: Some(credential),
                force_relay: true,
            });
        }
        let (a, mut ea) = VoicePeer::new(&peers[0]).await?;
        let (b, mut eb) = VoicePeer::new(&peers[1]).await?;

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
            timeout(Duration::from_secs(30), async {
                while let Some(s) = states.recv().await {
                    assert_ne!(s, PeerState::Failed, "{name} failed");
                    if s == PeerState::Connected {
                        break;
                    }
                }
            })
            .await?;
        }

        // 按实时节奏发（10ms 一帧）：burst 猛发会被中继路径整形/丢弃，不代表真实语音行为
        const N: u32 = 20;
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        for seq in 0..N {
            tick.tick().await;
            a.send_opus(&pattern(seq)).await?;
        }
        // 中继路可能乱序/偶发丢失（UDP 本来如此，排序+补洞是 JB 的活）：
        // 收 5 秒，能解出 15+/20 即算中继通（内容逐包校验）
        let mut got_seqs = Vec::new();
        let recv_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while got_seqs.len() < N as usize && tokio::time::Instant::now() < recv_deadline {
            let remain = recv_deadline.saturating_duration_since(tokio::time::Instant::now());
            match timeout(remain, eb.remote_frames.recv()).await {
                Ok(Some(frame)) => {
                    // RTP 序号/时间戳随包透出（JB 用）；内容逐包校验
                    assert_eq!(frame.payload.len(), pattern(0).len());
                    assert!(frame.payload[4..].iter().all(|&b| b == 0x5A));
                    got_seqs.push(u32::from_le_bytes(frame.payload[..4].try_into().unwrap()));
                }
                _ => break,
            }
        }
        got_seqs.sort_unstable();
        eprintln!("relay frames got {}/{}: {got_seqs:?}", got_seqs.len(), N);
        assert!(
            got_seqs.len() >= 15,
            "too few frames through relay: {}/{}",
            got_seqs.len(),
            N
        );

        // 中继选路收敛（allocation + nominated 比直连慢，多等会儿）
        let mut route = None;
        for _ in 0..200 {
            if let Some(r) = a.refresh_route().await {
                route = Some(r);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(route, Some(RouteType::Relay));

        a.close().await?;
        b.close().await?;
        anyhow::Ok(())
    };
    timeout(Duration::from_secs(120), run).await??;
    Ok(())
}
