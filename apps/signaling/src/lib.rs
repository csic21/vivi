//! 信令库入口：二进制 thin wrapper 与集成测试共用 `app()` / `AppState`。

pub mod rooms;
pub mod signal;
pub mod turn;

use axum::{
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

use rooms::{create_room, get_room, AppState};

/// 身份探测响应。客户端在 8080 被占用时用它区分"是我们自己的信令"还是
/// "某个无关服务碰巧占了 8080"——前者应该让路复用，后者应该换端口。
#[derive(serde::Serialize)]
pub struct Info {
    pub name: &'static str,
    pub version: &'static str,
    /// 响应结构版本，将来形状变了靠它兼容。
    pub v: u32,
}

/// 故意**不返回房间列表**：局域网里任何人都能打到这个端点，
/// 不该顺带暴露"这台机器上有哪些房间"。
async fn info() -> axum::Json<Info> {
    axum::Json(Info {
        name: "vivi-signaling",
        version: env!("CARGO_PKG_VERSION"),
        v: 1,
    })
}

pub fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/rooms", post(create_room))
        .route("/rooms/{id}", get(get_room))
        .route("/turn/credentials", get(turn::turn_credentials))
        .route("/signal", get(signal::ws_handler))
        .route("/info", get(info))
        // /health 保持纯文本 "ok" 不动：已有调用方和文档都按这个预期。
        .route("/health", get(|| async { "ok" }))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
