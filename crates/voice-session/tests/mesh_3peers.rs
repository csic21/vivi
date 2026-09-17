//! Week 7：3 人 Mesh 集成测试（合成音频，无硬件依赖）。

use std::sync::Arc;
use std::time::Duration;

use signaling::rooms::{create_room_now, AppState};
use tokio::net::TcpListener;
use voice_session::{AudioMode, Session, SessionConfig, Signaling};
use voice_webrtc::PeerConfig;

async fn wait_full_mesh(
    handles: &[voice_session::SessionHandle],
    want_peers: usize,
    min_frames: u64,
    secs: u64,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let ready = handles.iter().all(|h| {
            let s = h.stats();
            s.peers.len() == want_peers
                && s.peers
                    .iter()
                    .all(|p| p.connected && p.decoded_frames >= min_frames)
        });
        if ready {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "mesh not ready in {secs}s"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn mesh_should_connect_mix_and_cleanup() {
    let state = Arc::new(AppState::with_turn(
        "mesh-test".into(),
        vec!["turn:127.0.0.1:9".into()],
    ));
    let room = create_room_now(&state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, signaling::app(state)).await;
    });
    let url = format!("ws://{addr}/signal");
    let peer_config = PeerConfig {
        stun_urls: vec![],
        ..PeerConfig::default()
    };

    let mut handles = Vec::new();
    for user in [11u64, 22, 33] {
        handles.push(
            Session::join(SessionConfig {
                user_id: user,
                room_id: room.clone(),
                signaling: Signaling::Server { url: url.clone() },
                peer_config: peer_config.clone(),
                audio: AudioMode::Synthetic,
                relay_timeout_secs: 15,
                no_direct: vec![],
            })
            .await
            .unwrap(),
        );
    }

    // 全 mesh：每端 2 对端，全 connected，都有解码帧
    wait_full_mesh(&handles, 2, 20, 25).await;

    // VAD：合成正弦都算说话（对端 🟢 + 本端 🟢），电平表有数
    for (i, h) in handles.iter().enumerate() {
        let s = h.stats();
        eprintln!(
            "session{i}: self={} mic_level={:.3} peers={:?}",
            s.speaking_self,
            h.mic_level(),
            s.peers
                .iter()
                .map(|p| (p.user_id, p.speaking, p.decoded_frames, p.plc_frames))
                .collect::<Vec<_>>()
        );
        assert!(s.speaking_self, "self should be speaking");
        assert!(h.mic_level() > 0.02, "mic meter should move");
        assert!(s.peers.iter().all(|p| p.speaking), "all peers speaking");
    }

    // 混音在跑
    for h in &handles {
        assert!(h.stats().mixed_frames > 0);
    }

    // 单用户音量生效
    handles[0].set_user_gain(22, 0.3).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let g = handles[0]
        .stats()
        .peers
        .iter()
        .find(|p| p.user_id == 22)
        .map(|p| p.gain)
        .unwrap();
    assert!((g - 0.3).abs() < 0.001);

    // 一人离会：剩下两端回到 1 对端
    handles.pop().unwrap().leave().await;
    wait_full_mesh(&handles, 1, 20, 15).await;

    for h in handles {
        h.leave().await;
    }
}
