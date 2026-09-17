//! 队友中继集成测试：A—B 永不直连（互 blok），经 C 中转语音；
//! 解除屏蔽后自动升级回直连。

use std::sync::Arc;
use std::time::Duration;

use signaling::rooms::{create_room_now, AppState};
use tokio::net::TcpListener;
use voice_session::{AudioMode, Session, SessionConfig, Signaling};
use voice_webrtc::PeerConfig;

fn test_config(user: u64, room: &str, url: &str, no_direct: Vec<u64>) -> SessionConfig {
    SessionConfig {
        user_id: user,
        room_id: room.to_owned(),
        signaling: Signaling::Server {
            url: url.to_owned(),
        },
        peer_config: PeerConfig {
            stun_urls: vec![],
            ..PeerConfig::default()
        },
        audio: AudioMode::Synthetic,
        relay_timeout_secs: 2,
        no_direct,
    }
}

async fn wait_relay(
    handles: &[voice_session::SessionHandle],
    idx: usize,
    source: u64,
    via: u64,
    secs: u64,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let ok =
            handles[idx].stats().peers.iter().any(|p| {
                p.user_id == source && p.relayed_via == Some(via) && p.decoded_frames >= 10
            });
        if ok {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no relay leg in {secs}s"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_direct(handles: &[voice_session::SessionHandle], idx: usize, source: u64, secs: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let ok = handles[idx]
            .stats()
            .peers
            .iter()
            .any(|p| p.user_id == source && p.connected && p.relayed_via.is_none());
        if ok {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no direct upgrade in {secs}s"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn mesh_should_relay_and_upgrade() {
    let state = Arc::new(AppState::with_turn(
        "relay-test".into(),
        vec!["turn:127.0.0.1:9".into()],
    ));
    let room = create_room_now(&state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, signaling::app(state)).await;
    });
    let url = format!("ws://{addr}/signal");

    // A(11) 与 B(22) 互 blok：直连永远不建，只能经 C(33) 中转
    let a = Session::join(test_config(11, &room, &url, vec![22]))
        .await
        .unwrap();
    let b = Session::join(test_config(22, &room, &url, vec![11]))
        .await
        .unwrap();
    let c = Session::join(test_config(33, &room, &url, vec![]))
        .await
        .unwrap();
    let handles = vec![a, b, c];

    // B 经 C 听到 A（反向 A 经 C 听到 B 也会成立，断言一边即可）
    wait_relay(&handles, 1, 11, 33, 30).await;
    // 直连槽仍在但没通，UI 应标中继而非直连
    let pb = handles[1]
        .stats()
        .peers
        .into_iter()
        .find(|p| p.user_id == 11)
        .unwrap();
    assert!(!pb.connected);

    // 解除屏蔽 → 自动升级直连，中继摘除
    handles[0].unblock_peer(22);
    handles[1].unblock_peer(11);
    wait_direct(&handles, 1, 11, 30).await;
    wait_direct(&handles, 0, 22, 30).await;

    for h in handles {
        h.leave().await;
    }
}
