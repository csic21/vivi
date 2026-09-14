//! 房间内存状态：创建/加入/离开/查询/定向转发。
//! MVP 单机即可，多机再上 Redis。
//!
//! 锁纪律：`RwLock` 临界区只做 HashMap 操作，发送用 `UnboundedSender`
//! （永不阻塞），且一律在锁外发送。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
};
use tokio::sync::mpsc::{self, UnboundedSender};
use voice_common::UserId;
use voice_protocol::SignalMessage;

pub type RoomSender = UnboundedSender<SignalMessage>;

#[derive(Debug, Default)]
pub struct AppState {
    pub rooms: RwLock<HashMap<String, Room>>,
    pub turn_secret: String,
    pub turn_urls: Vec<String>,
}

impl AppState {
    /// 生产构造：TURN 签发密钥与地址走环境变量，缺省为本地开发值。
    pub fn from_env() -> Self {
        let turn_secret =
            std::env::var("TURN_SECRET").unwrap_or_else(|_| crate::turn::DEV_SECRET.to_owned());
        if turn_secret == crate::turn::DEV_SECRET {
            tracing::warn!("TURN_SECRET not set; using dev secret (local only!)");
        }
        let turn_urls = std::env::var("TURN_URLS")
            .map(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_else(|_| vec!["turn:127.0.0.1:3478".to_owned()]);
        Self {
            rooms: RwLock::new(HashMap::new()),
            turn_secret,
            turn_urls,
        }
    }

    /// 测试构造：调用方指定 TURN 参数。
    pub fn with_turn(turn_secret: String, turn_urls: Vec<String>) -> Self {
        Self {
            rooms: RwLock::new(HashMap::new()),
            turn_secret,
            turn_urls,
        }
    }

    /// 新人入会：返回（已在房间的老人 id， 老人发送通道）。
    /// 不存在房间返回 Err（typo 进错房比幽灵房好）。
    pub fn join(
        &self,
        room_id: &str,
        user_id: u64,
        tx: RoomSender,
    ) -> Result<(Vec<u64>, Vec<RoomSender>), String> {
        let mut rooms = Self::lock_write(&self.rooms);
        let room = rooms
            .get_mut(room_id)
            .ok_or_else(|| format!("room not found: {room_id}"))?;
        room.members.insert(user_id, tx);
        let others: Vec<(u64, RoomSender)> = room
            .members
            .iter()
            .filter(|(id, _)| **id != user_id)
            .map(|(id, tx)| (*id, tx.clone()))
            .collect();
        Ok((
            others.iter().map(|(id, _)| *id).collect(),
            others.into_iter().map(|(_, tx)| tx).collect(),
        ))
    }

    /// 离会：返回需通知 PeerLeft 的老人通道（锁外发送）。
    pub fn leave(&self, room_id: &str, user_id: u64) -> Vec<RoomSender> {
        let mut rooms = Self::lock_write(&self.rooms);
        let Some(room) = rooms.get_mut(room_id) else {
            return Vec::new();
        };
        room.members.remove(&user_id);
        let others: Vec<RoomSender> = room.members.values().cloned().collect();
        if room.members.is_empty() {
            rooms.remove(room_id);
        }
        others
    }

    /// 取某房间某成员的发送通道（Offer/Answer/ICE 定向转发用）。
    pub fn target(&self, room_id: &str, user_id: u64) -> Option<RoomSender> {
        Self::lock_read(&self.rooms)
            .get(room_id)?
            .members
            .get(&user_id)
            .cloned()
    }

    pub fn new_outbox() -> (RoomSender, mpsc::UnboundedReceiver<SignalMessage>) {
        mpsc::unbounded_channel()
    }

    fn lock_read(
        lock: &RwLock<HashMap<String, Room>>,
    ) -> RwLockReadGuard<'_, HashMap<String, Room>> {
        lock.read().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_write(
        lock: &RwLock<HashMap<String, Room>>,
    ) -> RwLockWriteGuard<'_, HashMap<String, Room>> {
        lock.write().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Debug, Clone)]
pub struct Room {
    pub id: String,
    pub members: HashMap<u64, RoomSender>,
}

/// GET /rooms/:id 用 DTO（Sender 不可序列化）。
#[derive(Debug, Clone, Serialize)]
pub struct RoomInfo {
    pub id: String,
    pub members: Vec<u64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRoomReq {
    #[allow(dead_code)]
    pub owner: Option<UserId>,
}

#[derive(Debug, Serialize)]
pub struct CreateRoomRes {
    pub room_id: String,
}

fn new_room_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// 同步建房（测试/example 预置房间用；HTTP 等价于 POST /rooms）。
pub fn create_room_now(state: &AppState) -> String {
    let room_id = new_room_id();
    AppState::lock_write(&state.rooms).insert(
        room_id.clone(),
        Room {
            id: room_id.clone(),
            members: HashMap::new(),
        },
    );
    room_id
}

pub async fn create_room(
    State(state): State<Arc<AppState>>,
    Json(_req): Json<CreateRoomReq>,
) -> Json<CreateRoomRes> {
    let room_id = new_room_id();
    AppState::lock_write(&state.rooms).insert(
        room_id.clone(),
        Room {
            id: room_id.clone(),
            members: HashMap::new(),
        },
    );
    tracing::info!(room_id = %room_id, "room created");
    Json(CreateRoomRes { room_id })
}

pub async fn get_room(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<RoomInfo>, StatusCode> {
    AppState::lock_read(&state.rooms)
        .get(&id)
        .map(|r| {
            Json(RoomInfo {
                id: r.id.clone(),
                members: r.members.keys().copied().collect(),
            })
        })
        .ok_or(StatusCode::NOT_FOUND)
}
