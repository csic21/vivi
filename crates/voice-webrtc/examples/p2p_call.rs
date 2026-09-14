//! Week 5：双端 P2P 语音验证（真实 UDP + ICE + DTLS-SRTP + Opus 透传）。
//!
//! 单进程内起 A/B 两个 [`VoicePeer`](voice_webrtc::VoicePeer)，trickle 经内存通道
//! 交叉，SDP 直传。A 发 300 帧“Opus 形”负载 → B 逐字节校验（SRTP 完整性）。
//!
//! STUN 默认开启：无公网时只有 host candidate（照样连通）；有公网会多出 srflx，
//! 打印出来即证明 NAT 映射 + STUN 生效（跨 NAT 通话的前置条件）。
//!
//! ```sh
//! cargo run -p voice-webrtc --example p2p_call --release
//! ```

use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;
use voice_common::RouteType;
use voice_webrtc::{PeerConfig, PeerState, VoicePeer};

const FRAMES: u32 = 300;

/// 40B 假 Opus 负载：[seq:4 LE][0xAB × 36]（≈32kbps×10ms 的包大小）。
fn fake_opus(seq: u32) -> Vec<u8> {
    let mut v = vec![0xABu8; 40];
    v[..4].copy_from_slice(&seq.to_le_bytes());
    v
}

fn candidate_type(json: &str) -> &str {
    if json.contains("typ host") {
        "host"
    } else if json.contains("typ srflx") {
        "srflx"
    } else if json.contains("typ relay") {
        "relay"
    } else if json.contains("typ prflx") {
        "prflx"
    } else {
        "?"
    }
}

async fn wait_connected(
    name: &str,
    states: &mut tokio::sync::mpsc::Receiver<PeerState>,
) -> anyhow::Result<()> {
    timeout(Duration::from_secs(20), async {
        while let Some(s) = states.recv().await {
            eprintln!("{name} state: {s:?}");
            if s == PeerState::Connected {
                return anyhow::Ok(());
            }
            if s == PeerState::Failed {
                anyhow::bail!("{name} failed");
            }
        }
        anyhow::bail!("{name} states channel closed")
    })
    .await??;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let run = async {
        let config = PeerConfig::default();
        let (a, mut ea) = VoicePeer::new(&config).await?;
        let (b, mut eb) = VoicePeer::new(&config).await?;

        // trickle 交叉转发：对端 remote 描述就绪前先缓存。
        // 注意：gather 可能在 SDP 往返前就完成，所以开门时必须主动 flush，
        // 不能等下一个 candidate 到达（否则 stash 永久卡住，ICE 建连失败）。
        let (ready_tx_a, ready_rx_a) = tokio::sync::watch::channel(false);
        let (ready_tx_b, ready_rx_b) = tokio::sync::watch::channel(false);
        let fwd_a = {
            let (b2, mut ready) = (Arc::clone(&b), ready_rx_b);
            let rx = ea.local_candidates;
            tokio::spawn(async move {
                let mut pending: Vec<String> = Vec::new();
                let mut rx = rx;
                let mut count = 0u32;
                loop {
                    tokio::select! {
                        c = rx.recv() => {
                            let Some(c) = c else { break };
                            eprintln!("A→B candidate [{}]: {}", candidate_type(&c), c);
                            count += 1;
                            if *ready.borrow() {
                                for p in pending.drain(..) {
                                    let _ = b2.add_remote_candidate(&p).await;
                                }
                                let _ = b2.add_remote_candidate(&c).await;
                            } else {
                                pending.push(c);
                            }
                        }
                        _ = ready.changed() => {
                            if *ready.borrow() {
                                for p in pending.drain(..) {
                                    let _ = b2.add_remote_candidate(&p).await;
                                }
                            }
                        }
                    }
                }
                count
            })
        };
        let fwd_b = {
            let (a2, mut ready) = (Arc::clone(&a), ready_rx_a);
            let rx = eb.local_candidates;
            tokio::spawn(async move {
                let mut pending: Vec<String> = Vec::new();
                let mut rx = rx;
                let mut count = 0u32;
                loop {
                    tokio::select! {
                        c = rx.recv() => {
                            let Some(c) = c else { break };
                            eprintln!("B→A candidate [{}]: {}", candidate_type(&c), c);
                            count += 1;
                            if *ready.borrow() {
                                for p in pending.drain(..) {
                                    let _ = a2.add_remote_candidate(&p).await;
                                }
                                let _ = a2.add_remote_candidate(&c).await;
                            } else {
                                pending.push(c);
                            }
                        }
                        _ = ready.changed() => {
                            if *ready.borrow() {
                                for p in pending.drain(..) {
                                    let _ = a2.add_remote_candidate(&p).await;
                                }
                            }
                        }
                    }
                }
                count
            })
        };

        // SDP 交换（直传，生产环境走 signaling WS）
        let offer = a.create_offer().await?;
        eprintln!("offer ok ({} bytes)", offer.len());
        let answer = b.apply_offer(&offer).await?;
        eprintln!("answer ok ({} bytes)", answer.len());
        let _ = ready_tx_b.send(true);
        a.apply_answer(&answer).await?;
        let _ = ready_tx_a.send(true);

        wait_connected("A", &mut ea.states).await?;
        wait_connected("B", &mut eb.states).await?;

        // 语音：A paced 发 300 帧，B 逐字节校验
        let send = tokio::spawn({
            let a = Arc::clone(&a);
            async move {
                let mut tick = tokio::time::interval(Duration::from_millis(10));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                for seq in 0..FRAMES {
                    tick.tick().await;
                    a.send_opus(&fake_opus(seq)).await?;
                }
                anyhow::Ok(())
            }
        });
        let mut got = 0u32;
        let recv = timeout(Duration::from_secs(20), async {
            while got < FRAMES {
                match timeout(Duration::from_secs(5), eb.remote_frames.recv()).await {
                    Ok(Some(frame)) => {
                        assert_eq!(frame.payload, fake_opus(got), "frame {got} corrupted");
                        got += 1;
                    }
                    _ => anyhow::bail!("remote frames stalled at {got}/{FRAMES}"),
                }
            }
            anyhow::Ok(got)
        })
        .await??;
        send.await??;
        assert_eq!(recv, FRAMES);

        // 选路：等 nominated pair 出现
        let mut route = None;
        for _ in 0..50 {
            if let Some(r) = a.refresh_route().await {
                route = Some(r);
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        eprintln!("route: {route:?} (want Direct on localhost)");
        assert_eq!(route, Some(RouteType::Direct));

        a.close().await?;
        b.close().await?;
        fwd_a.abort();
        fwd_b.abort();
        anyhow::Ok(())
    };

    timeout(Duration::from_secs(60), run).await?
}
