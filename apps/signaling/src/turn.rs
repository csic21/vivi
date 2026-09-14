//! TURN REST API 临时凭证（对接 coturn `use-auth-secret`）。
//!
//! 方案（标准 TURN REST API）：
//! - `username = "<expiry_unix>:<user>"`，`credential = base64(HMAC-SHA1(secret, username))`；
//! - coturn 侧配同样的 `static-auth-secret` 即可验证，无需同步用户表；
//! - 客户端拿凭证配进 PeerConnection 的 TURN ice server，过期重拿。
//!
//! 生产必须经环境变量 `TURN_SECRET` 注入强密钥；默认 dev 密钥仅限本地联调。

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use base64::prelude::*;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use std::sync::Arc;

use crate::rooms::AppState;

pub const DEV_SECRET: &str = "dev-secret-change-me";
pub const DEFAULT_TTL_SECS: u64 = 86400;
pub const MAX_TTL_SECS: u64 = 7 * 86400;

#[derive(Debug, Serialize)]
pub struct TurnCredentials {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub ttl_secs: u64,
}

#[derive(Debug, Deserialize)]
pub struct TurnQuery {
    pub user: String,
    pub ttl_secs: Option<u64>,
}

/// 签发 TURN 临时凭证。`user` 非空且 ≤64 字符。
pub async fn turn_credentials(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TurnQuery>,
) -> Result<Json<TurnCredentials>, StatusCode> {
    if q.user.is_empty() || q.user.len() > 64 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let ttl = q
        .ttl_secs
        .unwrap_or(DEFAULT_TTL_SECS)
        .clamp(60, MAX_TTL_SECS);
    Ok(Json(issue(
        &state.turn_secret,
        &state.turn_urls,
        &q.user,
        ttl,
    )))
}

pub fn issue(secret: &str, urls: &[String], user: &str, ttl_secs: u64) -> TurnCredentials {
    let expiry = unix_secs() + ttl_secs;
    let username = format!("{expiry}:{user}");
    let mut mac =
        Hmac::<Sha1>::new_from_slice(secret.as_bytes()).expect("hmac accepts any key length");
    mac.update(username.as_bytes());
    let credential = BASE64_STANDARD.encode(mac.finalize().into_bytes());
    TurnCredentials {
        urls: urls.to_vec(),
        username,
        credential,
        ttl_secs,
    }
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_username_should_carry_expiry_and_user() {
        let c = issue("s3cret", &["turn:127.0.0.1:3478".into()], "alice", 3600);
        let (expiry, user) = c.username.split_once(':').unwrap();
        assert_eq!(user, "alice");
        let exp: u64 = expiry.parse().unwrap();
        let now = unix_secs();
        assert!(exp > now + 3500 && exp <= now + 3600);
        assert_eq!(c.ttl_secs, 3600);
        assert!(!c.credential.is_empty());
    }

    #[test]
    fn different_secrets_should_give_different_credentials() {
        let a = issue("s1", &[], "bob", 60);
        let b = issue("s2", &[], "bob", 60);
        assert_ne!(a.credential, b.credential);
    }
}
