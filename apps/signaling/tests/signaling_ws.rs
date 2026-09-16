//! Week 6 信令联调：建房/查询 + WS 入会/定向转发/离会/错误全流程。

use futures_util::{SinkExt, StreamExt};
use signaling::{app, rooms::AppState};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use voice_common::UserId;
use voice_protocol::SignalMessage;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn spawn_server() -> std::net::SocketAddr {
    let state = Arc::new(AppState::with_turn(
        "test-secret".into(),
        vec!["turn:127.0.0.1:9".into()],
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app(state)).await.unwrap();
    });
    addr
}

async fn http_roundtrip(addr: std::net::SocketAddr, req: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    String::from_utf8(buf).unwrap()
}

fn body_of(resp: &str) -> &str {
    resp.split("\r\n\r\n").nth(1).unwrap_or("")
}

async fn post_room(addr: std::net::SocketAddr) -> String {
    let body = r#"{"owner":null}"#;
    let req = format!(
        "POST /rooms HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let resp = http_roundtrip(addr, &req).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    let v: serde_json::Value = serde_json::from_str(body_of(&resp)).unwrap();
    let id = v["room_id"].as_str().unwrap().to_owned();
    assert_eq!(id.len(), 8);
    id
}

async fn ws_connect(addr: std::net::SocketAddr) -> Ws {
    let (ws, _) = connect_async(format!("ws://{addr}/signal")).await.unwrap();
    ws
}

async fn ws_send(ws: &mut Ws, msg: &SignalMessage) {
    let text = serde_json::to_string(msg).unwrap();
    ws.send(Message::Text(text.into())).await.unwrap();
}

async fn ws_recv(ws: &mut Ws) -> SignalMessage {
    let msg = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("recv timeout")
        .expect("stream ended")
        .expect("ws error");
    let Message::Text(t) = msg else {
        panic!("want text, got {msg:?}");
    };
    serde_json::from_str(t.as_ref()).unwrap()
}

fn join(room: &str, uid: u64) -> SignalMessage {
    SignalMessage::JoinRoom {
        room_id: room.to_owned(),
        user_id: UserId(uid),
    }
}

#[tokio::test]
async fn signaling_should_route_room_messages() {
    let addr = spawn_server().await;

    // HTTP 面：health + 建房 + 查房
    let health = http_roundtrip(
        addr,
        "GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(health.starts_with("HTTP/1.1 200") && body_of(&health) == "ok");
    let room = post_room(addr).await;
    let info = http_roundtrip(
        addr,
        &format!("GET /rooms/{room} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert!(info.starts_with("HTTP/1.1 200"));
    assert!(body_of(&info).contains("\"members\":[]"));

    // WS 面：A(1) 先入，B(2) 后入
    let mut a = ws_connect(addr).await;
    let mut b = ws_connect(addr).await;
    ws_send(&mut a, &join(&room, 1)).await;
    ws_send(&mut b, &join(&room, 2)).await;

    // 新人 B 先收到老人列表（PeerJoined{1}），老人 A 收到新人到来
    assert_eq!(
        ws_recv(&mut b).await,
        SignalMessage::PeerJoined { user_id: UserId(1) }
    );
    assert_eq!(
        ws_recv(&mut a).await,
        SignalMessage::PeerJoined { user_id: UserId(2) }
    );

    // Offer 定向 + from 被服务端纠正（客户端谎报 999）
    ws_send(
        &mut a,
        &SignalMessage::Offer {
            from: UserId(999),
            to: UserId(2),
            sdp: "SDP-O".into(),
        },
    )
    .await;
    assert_eq!(
        ws_recv(&mut b).await,
        SignalMessage::Offer {
            from: UserId(1),
            to: UserId(2),
            sdp: "SDP-O".into()
        }
    );

    // Answer 回来
    ws_send(
        &mut b,
        &SignalMessage::Answer {
            from: UserId(2),
            to: UserId(1),
            sdp: "SDP-A".into(),
        },
    )
    .await;
    assert_eq!(
        ws_recv(&mut a).await,
        SignalMessage::Answer {
            from: UserId(2),
            to: UserId(1),
            sdp: "SDP-A".into()
        }
    );

    // ICE 转发
    ws_send(
        &mut a,
        &SignalMessage::IceCandidate {
            from: UserId(1),
            to: UserId(2),
            candidate: "cand-1".into(),
        },
    )
    .await;
    assert_eq!(
        ws_recv(&mut b).await,
        SignalMessage::IceCandidate {
            from: UserId(1),
            to: UserId(2),
            candidate: "cand-1".into()
        }
    );

    // 发给不在线用户 → Error（不静默吞）
    ws_send(
        &mut a,
        &SignalMessage::Offer {
            from: UserId(1),
            to: UserId(99),
            sdp: "x".into(),
        },
    )
    .await;
    assert!(matches!(ws_recv(&mut a).await, SignalMessage::Error { .. }));

    // 进不存在的房间 → Error
    let mut c = ws_connect(addr).await;
    ws_send(&mut c, &join("nope-room", 3)).await;
    assert!(matches!(ws_recv(&mut c).await, SignalMessage::Error { .. }));

    // A 断开 → B 收到 PeerLeft{1}
    drop(a);
    assert_eq!(
        ws_recv(&mut b).await,
        SignalMessage::PeerLeft { user_id: UserId(1) }
    );

    // TURN 凭证接口
    let turn = http_roundtrip(
        addr,
        "GET /turn/credentials?user=alice HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(turn.starts_with("HTTP/1.1 200"), "{turn}");
    let v: serde_json::Value = serde_json::from_str(body_of(&turn)).unwrap();
    assert!(v["username"].as_str().unwrap().ends_with(":alice"));
    assert!(!v["credential"].as_str().unwrap().is_empty());
}

/// 房主在房间里换音频设备时，客户端是 `leave → join`（`rejoin`），中间有一段
/// 谁都不在的空窗。以前这段空窗会把房间直接删掉，而房主的 mDNS 广播还挂着，
/// 队友因此"发现得到这台机器、却找不到房间"，报错还会说成"房号打错了"。
#[tokio::test]
async fn room_should_survive_host_rejoin() {
    let addr = spawn_server().await;
    let room = post_room(addr).await;

    let mut host = ws_connect(addr).await;
    ws_send(&mut host, &join(&room, 1)).await;
    // 换设备：旧会话整个拆掉（Rust 侧 `join_room` 也是先 leave 再 join）
    drop(host);

    let info = http_roundtrip(
        addr,
        &format!("GET /rooms/{room} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert!(
        info.starts_with("HTTP/1.1 200"),
        "重进空窗里房间不该消失：{info}"
    );

    // 房主重进：客户端每次 join_room 都会重新生成 user_id，所以这里换个号
    let mut host2 = ws_connect(addr).await;
    ws_send(&mut host2, &join(&room, 11)).await;

    let info = http_roundtrip(
        addr,
        &format!("GET /rooms/{room} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert!(
        body_of(&info).contains("11"),
        "房主重进后该在成员表里：{info}"
    );
}

#[tokio::test]
async fn signaling_should_reject_bad_input() {
    let addr = spawn_server().await;
    let mut ws = ws_connect(addr).await;
    // 非 JSON → Error，不踢人
    ws.send(Message::Text("not json".into())).await.unwrap();
    assert!(matches!(
        ws_recv(&mut ws).await,
        SignalMessage::Error { .. }
    ));
    // 服务端消息发回来 → Error
    ws_send(&mut ws, &SignalMessage::PeerJoined { user_id: UserId(1) }).await;
    assert!(matches!(
        ws_recv(&mut ws).await,
        SignalMessage::Error { .. }
    ));
}

#[tokio::test]
async fn signaling_should_forward_relay_messages() {
    let addr = spawn_server().await;
    let room = post_room(addr).await;
    let mut a = ws_connect(addr).await;
    let mut b = ws_connect(addr).await;
    ws_send(&mut a, &join(&room, 1)).await;
    ws_send(&mut b, &join(&room, 2)).await;
    // 消费入会广播（B 先收到 PeerJoined{1}，A 收到 PeerJoined{2}）
    assert!(matches!(
        ws_recv(&mut b).await,
        SignalMessage::PeerJoined { .. }
    ));
    assert!(matches!(
        ws_recv(&mut a).await,
        SignalMessage::PeerJoined { .. }
    ));

    // B 请 A 中转：A 应收到 from=B 的请求（from 被服务端盖章）
    ws_send(
        &mut b,
        &SignalMessage::RelayRequest {
            from: UserId(999),
            to: UserId(1),
            target: UserId(3),
        },
    )
    .await;
    assert_eq!(
        ws_recv(&mut a).await,
        SignalMessage::RelayRequest {
            from: UserId(2),
            to: UserId(1),
            target: UserId(3),
        }
    );

    // A 接受 → B 收到接受
    ws_send(
        &mut a,
        &SignalMessage::RelayAccepted {
            from: UserId(1),
            to: UserId(2),
            target: UserId(3),
        },
    )
    .await;
    assert_eq!(
        ws_recv(&mut b).await,
        SignalMessage::RelayAccepted {
            from: UserId(1),
            to: UserId(2),
            target: UserId(3),
        }
    );

    // B 叫停 → A 收到
    ws_send(
        &mut b,
        &SignalMessage::RelayStop {
            from: UserId(2),
            to: UserId(1),
            target: UserId(3),
        },
    )
    .await;
    assert_eq!(
        ws_recv(&mut a).await,
        SignalMessage::RelayStop {
            from: UserId(2),
            to: UserId(1),
            target: UserId(3),
        }
    );
}

/// `/info` 是客户端用来判断"这个端口上跑的是不是我们自己的信令"的探针
/// （`apps/desktop/src-tauri/src/net.rs` 的 `probe_vivi` 认 `name` 里的
/// `vivi-signaling`），形状被依赖，别随手改。
#[tokio::test]
async fn info_should_identify_the_service() {
    let addr = spawn_server().await;
    let resp = http_roundtrip(
        addr,
        "GET /info HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    let v: serde_json::Value = serde_json::from_str(body_of(&resp)).unwrap();
    assert_eq!(v["name"], "vivi-signaling");
    assert!(v["version"].as_str().is_some_and(|s| !s.is_empty()));
    // 故意不返回房间列表：局域网里任何人都能打到这个端点
    assert!(v.get("rooms").is_none(), "不该暴露房间列表：{v}");
}

/// `/health` 保持纯文本 `ok`：已有调用方和 docs/NETWORK.md 都按这个预期写的。
#[tokio::test]
async fn health_should_stay_plain_text() {
    let addr = spawn_server().await;
    let resp = http_roundtrip(
        addr,
        "GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert_eq!(body_of(&resp), "ok");
}
