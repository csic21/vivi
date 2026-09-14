//! WS /signal：房间内 Offer/Answer/ICE 定向转发，PeerJoined/PeerLeft 广播。
//!
//! - 每连接一切分为读/写两半：写侧是 unbounded channel（读侧关闭即退出）；
//! - `from` 一律按连接入会身份覆盖，客户端冒充无效；
//! - 未入会就发 Offer/Answer/ICE → Error；目标不在线 → Error（不静默吞）；
//! - 连接断开视为离会，广播 PeerLeft（客户端崩溃也能清理）。

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use voice_common::UserId;
use voice_protocol::SignalMessage;

use crate::rooms::{AppState, RoomSender};

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (out_tx, mut out_rx) = AppState::new_outbox();
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            match serde_json::to_string(&msg) {
                Ok(text) => {
                    if ws_tx.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(e) => tracing::warn!(error = %e, "signal serialize failed"),
            }
        }
    });

    let mut me: Option<(String, u64)> = None;
    while let Some(frame) = ws_rx.next().await {
        let text = match frame {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };
        let msg: SignalMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(_) => {
                send_error(&out_tx, "malformed message (not SignalMessage JSON)");
                continue;
            }
        };
        route(&state, &mut me, &out_tx, msg);
    }

    if let Some((room_id, uid)) = me.take() {
        let evt = SignalMessage::PeerLeft {
            user_id: UserId(uid),
        };
        for tx in state.leave(&room_id, uid) {
            let _ = tx.send(evt.clone());
        }
        tracing::info!(room = %room_id, user = uid, "peer disconnected");
    }
    writer.abort();
}

fn route(
    state: &AppState,
    me: &mut Option<(String, u64)>,
    out: &UnboundedSender<SignalMessage>,
    msg: SignalMessage,
) {
    match msg {
        SignalMessage::JoinRoom { room_id, user_id } => {
            match state.join(&room_id, user_id.0, out.clone()) {
                Ok((existing, others)) => {
                    *me = Some((room_id.clone(), user_id.0));
                    tracing::info!(room = %room_id, user = user_id.0, peers = existing.len(), "peer joined");
                    // 先告诉新人老人都有谁（新人负责发起 offer，防 glare）
                    for uid in existing {
                        let _ = out.send(SignalMessage::PeerJoined {
                            user_id: UserId(uid),
                        });
                    }
                    // 再告诉老人新人来了
                    let evt = SignalMessage::PeerJoined { user_id };
                    for tx in others {
                        let _ = tx.send(evt.clone());
                    }
                }
                Err(e) => send_error(out, &e),
            }
        }
        SignalMessage::LeaveRoom { room_id, user_id } => {
            if *me != Some((room_id.clone(), user_id.0)) {
                send_error(out, "not in that room");
                return;
            }
            *me = None;
            let evt = SignalMessage::PeerLeft { user_id };
            for tx in state.leave(&room_id, user_id.0) {
                let _ = tx.send(evt.clone());
            }
        }
        SignalMessage::Offer { to, sdp, .. } => {
            forward(state, me, out, to, |from| SignalMessage::Offer {
                from,
                to,
                sdp,
            });
        }
        SignalMessage::Answer { to, sdp, .. } => {
            forward(state, me, out, to, |from| SignalMessage::Answer {
                from,
                to,
                sdp,
            });
        }
        SignalMessage::IceCandidate { to, candidate, .. } => {
            forward(state, me, out, to, |from| SignalMessage::IceCandidate {
                from,
                to,
                candidate,
            });
        }
        SignalMessage::RelayRequest { to, target, .. } => {
            forward(state, me, out, to, |from| SignalMessage::RelayRequest {
                from,
                to,
                target,
            });
        }
        SignalMessage::RelayAccepted { to, target, .. } => {
            forward(state, me, out, to, |from| SignalMessage::RelayAccepted {
                from,
                to,
                target,
            });
        }
        SignalMessage::RelayRejected { to, target, .. } => {
            forward(state, me, out, to, |from| SignalMessage::RelayRejected {
                from,
                to,
                target,
            });
        }
        SignalMessage::RelayStop { to, target, .. } => {
            forward(state, me, out, to, |from| SignalMessage::RelayStop {
                from,
                to,
                target,
            });
        }
        // 服务端产生的消息，客户端发来直接拒绝（协议卫生）
        SignalMessage::PeerJoined { .. }
        | SignalMessage::PeerLeft { .. }
        | SignalMessage::Error { .. } => {
            send_error(out, "server-only message");
        }
    }
}

/// Offer/Answer/ICE 定向转发：`from` 按连接入会身份覆盖（防冒充），
/// 目标不在线回 Error（不静默吞）。
fn forward(
    state: &AppState,
    me: &Option<(String, u64)>,
    out: &UnboundedSender<SignalMessage>,
    to: UserId,
    make: impl FnOnce(UserId) -> SignalMessage,
) {
    let Some((room_id, uid)) = me else {
        send_error(out, "join a room first");
        return;
    };
    match state.target(room_id, to.0) {
        Some(tx) => {
            let _ = tx.send(make(UserId(*uid)));
        }
        None => send_error(out, &format!("peer {} offline", to.0)),
    }
}

fn send_error(out: &RoomSender, message: &str) {
    let _ = out.send(SignalMessage::Error {
        message: message.to_owned(),
    });
}
