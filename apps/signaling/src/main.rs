//! Section 10 信令服务器二进制入口（逻辑在 lib.rs，测试可复用）。

use signaling::{app, rooms::AppState};
use std::{net::SocketAddr, sync::Arc};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let state = Arc::new(AppState::from_env());
    let app = app(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    tracing::info!("signaling listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
