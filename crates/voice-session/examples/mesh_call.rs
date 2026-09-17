//! Week 7：3 人 Mesh 全双工验证（进程内信令 + 3 个 Session + 合成音频）。
//!
//! 每对 peer 都是独立 P2P（host）：验证入会、offer 方向、混音流动、每用户统计、
//! 音量调节、PeerLeft 清理。
//!
//! ```sh
//! cargo run -p voice-session --example mesh_call
//! ```

use std::sync::Arc;
use std::time::Duration;

use signaling::rooms::{create_room_now, AppState};
use tokio::net::TcpListener;
use voice_session::{AudioMode, Session, SessionConfig, Signaling};
use voice_webrtc::PeerConfig;

async fn wait_mesh(
    handles: &[voice_session::SessionHandle],
    want_peers: usize,
    min_frames: u64,
    secs: u64,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let ok = handles.iter().all(|h| {
            let s = h.stats();
            s.peers.len() == want_peers
                && s.peers
                    .iter()
                    .all(|p| p.connected && p.decoded_frames >= min_frames)
        });
        if ok {
            return Ok(());
        }
        if tokio::time::Instant::now() > deadline {
            for (i, h) in handles.iter().enumerate() {
                eprintln!("session {i}: {:?}", h.stats());
            }
            anyhow::bail!("mesh not ready: want {want_peers} peers x{min_frames} frames");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 进程内信令 + 预置房间
    let state = Arc::new(AppState::with_turn(
        "mesh-dev".into(),
        vec!["turn:127.0.0.1:9".into()],
    ));
    let room = create_room_now(&state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, signaling::app(state)).await;
    });
    let url = format!("ws://{addr}/signal");
    eprintln!("signaling: {url} room={room}");

    let peer_config = PeerConfig {
        stun_urls: vec![],
        ..PeerConfig::default()
    };
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3)
        .clamp(2, 6);
    let users: Vec<u64> = (0..n as u64).map(|i| 11 + i * 11).collect();
    let mut handles = Vec::new();
    for user in users.clone() {
        let h = Session::join(SessionConfig {
            user_id: user,
            room_id: room.clone(),
            signaling: Signaling::Server { url: url.clone() },
            peer_config: peer_config.clone(),
            audio: AudioMode::Synthetic,
            relay_timeout_secs: 15,
            no_direct: vec![],
        })
        .await?;
        handles.push(h);
    }

    wait_mesh(&handles, n - 1, 50, 30).await?;
    eprintln!("mesh up: {n} peers x full-mesh");

    // 每用户音量：把 users[0] 在 handles[1] 侧的增益调到 0.5，看快照生效
    handles[1].set_user_gain(users[0], 0.5).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let s2 = handles[1].stats();
    let g = s2
        .peers
        .iter()
        .find(|p| p.user_id == users[0])
        .map(|p| p.gain)
        .unwrap_or(-1.0);
    assert!((g - 0.5).abs() < 0.001, "gain not applied: {g}");
    eprintln!("per-user gain ok");

    // 矩阵打印
    for (i, h) in handles.iter().enumerate() {
        let s = h.stats();
        for p in &s.peers {
            eprintln!(
                "session{i} <- user{}: connected={} speaking={} route={:?} rtt={:?}ms dec={} plc={} gain={}",
                p.user_id,
                p.connected,
                p.speaking,
                p.route,
                p.rtt_ms,
                p.decoded_frames,
                p.plc_frames,
                p.gain
            );
        }
        eprintln!(
            "session{i}: mixed_frames={} speaking_self={}",
            s.mixed_frames, s.speaking_self
        );
    }

    // 最后一个离会：剩下各端对端数减一
    handles.pop().unwrap().leave().await;
    wait_mesh(&handles, n - 2, 50, 15).await?;
    eprintln!("peer-left cleanup ok");

    for h in handles {
        h.leave().await;
    }
    eprintln!("MESH PASS");
    Ok(())
}
