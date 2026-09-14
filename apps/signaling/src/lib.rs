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

pub fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/rooms", post(create_room))
        .route("/rooms/{id}", get(get_room))
        .route("/turn/credentials", get(turn::turn_credentials))
        .route("/signal", get(signal::ws_handler))
        .route("/health", get(|| async { "ok" }))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
