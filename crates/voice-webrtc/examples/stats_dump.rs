//! Week 5 调试工具：建连后打印 stats report 全貌（排查选路判定用）。
//!
//! ```sh
//! cargo run -p voice-webrtc --example stats_dump 2>&1 | tail -n 40
//! ```

use std::sync::Arc;
use std::time::Duration;

use voice_webrtc::{PeerConfig, PeerState, VoicePeer};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
        tokio::time::timeout(Duration::from_secs(20), async {
            while let Some(s) = states.recv().await {
                eprintln!("{name}: {s:?}");
                if s == PeerState::Connected || s == PeerState::Failed {
                    break;
                }
            }
        })
        .await?;
    }

    // 发几帧让 pair 产生流量
    for seq in 0..10u32 {
        let mut p = vec![0u8; 40];
        p[..4].copy_from_slice(&seq.to_le_bytes());
        a.send_opus(&p).await?;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    eprintln!("route mediated by helper: {:?}", a.refresh_route().await);
    for line in a.stats_debug().await {
        eprintln!("A stats: {line}");
    }
    // 模拟 UI 轮询：每 500ms 刷一次，看选路何时收敛
    for i in 0..30u32 {
        if let Some(r) = a.refresh_route().await {
            eprintln!("poll {i}: route={r:?}");
            break;
        }
        if i % 5 == 0 {
            eprintln!("poll {i}: still unknown");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    a.close().await?;
    b.close().await?;
    Ok(())
}
