//! 信令 WS 客户端：收发 `SignalMessage`。
//!
//! 断线即错直接上抛（Week 7 不做自动重连；V0.2 再加退避重连 + 状态恢复）。

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use voice_protocol::SignalMessage;

/// 可克隆的发送端（candidate 转发等任务各持一份）。
#[derive(Clone)]
pub struct SigSender {
    tx: mpsc::UnboundedSender<SignalMessage>,
}

impl SigSender {
    pub fn send(&self, msg: SignalMessage) {
        let _ = self.tx.send(msg);
    }
}

/// 连接信令 WS。返回（发送端， 接收端， 后台任务——调用方 leave 时 abort）。
pub async fn connect(
    url: &str,
) -> anyhow::Result<(
    SigSender,
    mpsc::Receiver<SignalMessage>,
    Vec<JoinHandle<()>>,
)> {
    let (ws, _) = tokio_tungstenite::connect_async(url).await?;
    let (mut ws_tx, mut ws_rx) = ws.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<SignalMessage>();
    let (in_tx, in_rx) = mpsc::channel::<SignalMessage>(64);

    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            match serde_json::to_string(&msg) {
                Ok(text) => {
                    let text_msg: tokio_tungstenite::tungstenite::Message =
                        tokio_tungstenite::tungstenite::Message::Text(text.into());
                    if ws_tx.send(text_msg).await.is_err() {
                        break;
                    }
                }
                Err(e) => tracing::warn!(error = %e, "signal serialize failed"),
            }
        }
    });
    let reader = tokio::spawn(async move {
        while let Some(frame) = ws_rx.next().await {
            let text = match frame {
                Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => t.to_string(),
                Ok(tokio_tungstenite::tungstenite::Message::Close(_)) | Err(_) => break,
                _ => continue,
            };
            match serde_json::from_str::<SignalMessage>(&text) {
                Ok(msg) => {
                    if in_tx.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(_) => tracing::warn!("malformed signal message ignored"),
            }
        }
    });

    Ok((SigSender { tx: out_tx }, in_rx, vec![writer, reader]))
}
