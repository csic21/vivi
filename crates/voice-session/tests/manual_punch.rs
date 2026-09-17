//! 手动（剪贴板）打洞：两端只靠一段连接码互通，全程不碰任何信令服务器。
//!
//! 这条路径存在的意义是把「牵线」和「打洞」拆开：房间模式异地连不上时，你分不清
//! 是信令没通还是 NAT 打不穿；这里信令压根不存在（码是两段字符串），连不上就只剩
//! 打洞一个原因。
//!
//! 同机跑，ICE 走 host candidate，验证的是机制本身 —— 跨 NAT 能不能成，得拿
//! `manual_punch` 那个 example 在两台真机上试（见 `docs/NETWORK.md`）。

use std::time::Duration;

use voice_session::manual::{self, DEFAULT_GATHER_TIMEOUT};
use voice_session::{AudioMode, Session, SessionConfig, SessionHandle, Signaling};
use voice_webrtc::PeerConfig;

fn config(user_id: u64, peer_id: u64) -> SessionConfig {
    SessionConfig {
        user_id,
        room_id: String::new(),
        signaling: Signaling::Manual { peer_id },
        // 无 STUN：测试不依赖外网，ICE 只用 host candidate，同机足够连通
        peer_config: PeerConfig {
            stun_urls: vec![],
            ..PeerConfig::default()
        },
        audio: AudioMode::Synthetic,
        relay_timeout_secs: 15,
        no_direct: vec![],
    }
}

/// 等两端都真收到对方的音频帧 —— 只 "connected" 不算数，那是信令面，
/// 有声音才是媒体面（`docs/NETWORK.md` 第 5 节点名过的坑）。
async fn wait_frames(handles: &[&SessionHandle], min_frames: u64, secs: u64) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let ok = handles.iter().all(|h| {
            h.stats()
                .peers
                .iter()
                .any(|p| p.connected && p.decoded_frames >= min_frames)
        });
        if ok {
            return Ok(());
        }
        if tokio::time::Instant::now() > deadline {
            for (i, h) in handles.iter().enumerate() {
                eprintln!("session{i}: {:?}", h.stats());
            }
            anyhow::bail!("{secs}s 内没收到 {min_frames} 帧");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// 等选路快照落到非空。
///
/// 快照 2s 才刷一轮（`session.rs` 的 stats 任务），而音频帧几百毫秒就到齐了 ——
/// 连上就立刻读会读到连上之前那一轮的 `None`。这个等待不掩盖问题：
/// `None` 和 `Some(Relay)` 最终都会被断言挡下来。
async fn wait_route(h: &SessionHandle, timeout: Duration) -> Option<voice_common::RouteType> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(r) = h.stats().peers.first().and_then(|p| p.route) {
            return Some(r);
        }
        if tokio::time::Instant::now() > deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn pair_connects_with_nothing_but_a_pasted_code() -> anyhow::Result<()> {
    // id 小的发 offer，两边各自算出同一结论（沿用房间模式的规则）
    let (host, mut host_link) = Session::join_manual(config(1, 2)).await?;
    let (guest, mut guest_link) = Session::join_manual(config(2, 1)).await?;

    // ① 房主出码
    let (code_host, outcome) = host_link.next_code(DEFAULT_GATHER_TIMEOUT).await?;
    assert!(outcome.is_usable(), "房主应当能出一份可用的码：{outcome:?}");
    assert!(code_host.starts_with(manual::CODE_PREFIX));

    // ② 队友收码 → 出码
    let fed = guest_link.feed(&code_host).await?;
    assert!(fed > 0, "码里应该有信令");
    let (code_guest, outcome) = guest_link.next_code(DEFAULT_GATHER_TIMEOUT).await?;
    assert!(outcome.is_usable(), "队友应当能出一份可用的码：{outcome:?}");

    // ③ 房主收码 → ICE 开跑
    host_link.feed(&code_guest).await?;

    wait_frames(&[&host, &guest], 20, 30).await?;

    // 走过的路要看得见：这条 leg 应该是直连，不是中继
    let route = wait_route(&host, Duration::from_secs(6)).await;
    eprintln!("host<-guest route={route:?}");
    assert_eq!(
        route,
        Some(voice_common::RouteType::Direct),
        "手动打洞应当直连；relay 或 None 都说明这条腿没走通"
    );

    host.leave().await;
    guest.leave().await;
    Ok(())
}
