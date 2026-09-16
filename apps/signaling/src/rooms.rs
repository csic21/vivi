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
    time::{Duration, Instant},
};
use tokio::sync::mpsc::{self, UnboundedSender};
use voice_common::UserId;
use voice_protocol::SignalMessage;

pub type RoomSender = UnboundedSender<SignalMessage>;

/// 空房间的宽限期。
///
/// 房间是"最后一人离开就该没了"没错，但**离开并不总是真离开**：房主在房间里换
/// 音频设备会走 `leave → join`（`apps/desktop/src/stores/useVoiceStore.ts` 的
/// `rejoin`），中间有几秒空窗。以前这个空窗会把房间直接删掉，而房主的 mDNS 广播
/// 还挂着，队友于是"发现得到这台机器、却找不到房间"——
/// 报错文案还会把它说成"房号打错了，或者房主刚关了 App"，把人往错方向带。
///
/// 代价：房主真的关掉 App 后，房间会多活这么久，这期间手上有房号的人可能进到
/// 一个只有自己的空房。相比"正常重进被误杀"，这个方向划算得多；而且房主回来时
/// `join` 会把两边通过 PeerJoined 接上，空房自己就消失了。
///
/// TODO: 建房时带上 owner（`CreateRoomReq.owner` 目前恒为 null）就能区分
///       "房主重进"和"陌生人进空房"，这个窗口可以再收紧。
pub const EMPTY_ROOM_GRACE: Duration = Duration::from_secs(20);

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
        // 没显式配 TURN_URLS 就**不签发**凭证，而不是兜一个 loopback 地址。
        // 以前默认 `turn:127.0.0.1:3478`，等于告诉客户端"你的 TURN 在自己本机"——
        // 客户端真的会去连自己的 127.0.0.1，必然分配失败，还要白等一轮 relay 超时。
        // 没有 relay 候选是"打洞失败就没声音"，指向自己的假 TURN 则是"一定没声音还慢"。
        // 客户端拿到空 urls 会直接跳过 TURN（api/signaling.ts 里 `!data.urls?.length` → null）。
        let turn_urls: Vec<String> = std::env::var("TURN_URLS")
            .map(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if turn_urls.is_empty() {
            tracing::info!("TURN_URLS not set; no relay credentials will be issued");
        }
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

    /// 清掉空太久的房间。
    ///
    /// 只挂在写路径上。读路径（`get_room` / `target`）不为了清理去抢写锁，
    /// 而是用 [`Room::alive`] 把过期房间当不存在——效果一样，还省一次锁升级。
    fn sweep_expired(rooms: &mut HashMap<String, Room>) {
        rooms.retain(|_, r| r.alive());
    }

    /// 新人入会：返回（已在房间的老人 id， 老人发送通道）。
    /// 不存在房间返回 Err（typo 进错房比幽灵房好）。
    ///
    /// 空着但还在宽限期内的房间**可以进**——那正是"房主重进"要走的路径。
    pub fn join(
        &self,
        room_id: &str,
        user_id: u64,
        tx: RoomSender,
    ) -> Result<(Vec<u64>, Vec<RoomSender>), String> {
        let mut rooms = Self::lock_write(&self.rooms);
        Self::sweep_expired(&mut rooms);
        let room = rooms
            .get_mut(room_id)
            .ok_or_else(|| format!("room not found: {room_id}"))?;
        room.members.insert(user_id, tx);
        // 又有人了：清掉宽限期计时，别让房间在通话中途"过期"。
        room.empty_since = None;
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
    ///
    /// 人走空了**不立刻删房间**，只记下空窗起点，交给 [`EMPTY_ROOM_GRACE`]
    /// 和下一次写操作回收。理由见 [`EMPTY_ROOM_GRACE`] 的注释。
    pub fn leave(&self, room_id: &str, user_id: u64) -> Vec<RoomSender> {
        let mut rooms = Self::lock_write(&self.rooms);
        Self::sweep_expired(&mut rooms);
        let Some(room) = rooms.get_mut(room_id) else {
            return Vec::new();
        };
        room.members.remove(&user_id);
        let others: Vec<RoomSender> = room.members.values().cloned().collect();
        // 只在"刚变空"时打点：重复 leave（断连清理 + 显式离房都走这里）不该
        // 把宽限期一次次往后延。
        if room.members.is_empty() && room.empty_since.is_none() {
            room.empty_since = Some(Instant::now());
        }
        others
    }

    /// 取某房间某成员的发送通道（Offer/Answer/ICE 定向转发用）。
    /// 过期但还没被清扫掉的房间，在这里等价于不存在。
    pub fn target(&self, room_id: &str, user_id: u64) -> Option<RoomSender> {
        let rooms = Self::lock_read(&self.rooms);
        let room = rooms.get(room_id)?;
        if !room.alive() {
            return None;
        }
        room.members.get(&user_id).cloned()
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
    /// 房间变空的时刻；`None` = 还有人在（或刚建好还没进过人）。
    empty_since: Option<Instant>,
}

impl Room {
    fn new(id: String) -> Self {
        Self {
            id,
            members: HashMap::new(),
            empty_since: None,
        }
    }

    /// 还有人，或者刚空没多久（重进还能把这间房捡回来）。
    ///
    /// 刚建好人还没进的房间：`members` 空、`empty_since` 是 `None` → 活着。
    /// 人走空过的房间：`empty_since` 打点，超过 [`EMPTY_ROOM_GRACE`] 才算没了。
    fn alive(&self) -> bool {
        !self.members.is_empty()
            || self
                .empty_since
                .is_none_or(|t| t.elapsed() < EMPTY_ROOM_GRACE)
    }
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
    let mut rooms = AppState::lock_write(&state.rooms);
    AppState::sweep_expired(&mut rooms);
    rooms.insert(room_id.clone(), Room::new(room_id.clone()));
    room_id
}

pub async fn create_room(
    State(state): State<Arc<AppState>>,
    Json(_req): Json<CreateRoomReq>,
) -> Json<CreateRoomRes> {
    let room_id = new_room_id();
    let mut rooms = AppState::lock_write(&state.rooms);
    AppState::sweep_expired(&mut rooms);
    rooms.insert(room_id.clone(), Room::new(room_id.clone()));
    drop(rooms);
    tracing::info!(room_id = %room_id, "room created");
    Json(CreateRoomRes { room_id })
}

/// 查房。过期但还没被清扫掉的房间按"不存在"处理 —— 客户端拿这个 404 判断
/// "房主还在不在"，所以这里不能把宽限期用完的房间报成 200。
pub async fn get_room(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<RoomInfo>, StatusCode> {
    AppState::lock_read(&state.rooms)
        .get(&id)
        .filter(|r| r.alive())
        .map(|r| {
            Json(RoomInfo {
                id: r.id.clone(),
                members: r.members.keys().copied().collect(),
            })
        })
        .ok_or(StatusCode::NOT_FOUND)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        AppState::with_turn("test-secret".into(), Vec::new())
    }

    fn outbox() -> RoomSender {
        AppState::new_outbox().0
    }

    /// 把房间的空窗起点往前拨，模拟"已经空了这么久"，省得测试真的 sleep。
    fn backdate_empty(state: &AppState, room_id: &str, age: Duration) {
        AppState::lock_write(&state.rooms)
            .get_mut(room_id)
            .expect("房间没了")
            .empty_since = Some(Instant::now() - age);
    }

    /// 刚建好、还没人进的房间必须能进（`empty_since` 是 `None`，别被当成过期）。
    #[test]
    fn fresh_room_should_be_joinable() {
        let st = state();
        let id = create_room_now(&st);
        assert!(st.join(&id, 1, outbox()).is_ok());
    }

    /// 房主重进（换音频设备走的就是 leave → join）中间的空窗不该把房间带走。
    #[test]
    fn room_should_survive_rejoin_within_grace() {
        let st = state();
        let id = create_room_now(&st);
        st.join(&id, 1, outbox()).unwrap();
        st.leave(&id, 1);

        assert!(
            st.join(&id, 1, outbox()).is_ok(),
            "宽限期内重进不该报 room not found"
        );
    }

    /// 但空太久还是要回收 —— 房主真关了 App，房间不能永远留着。
    #[test]
    fn room_should_expire_after_grace() {
        let st = state();
        let id = create_room_now(&st);
        st.join(&id, 1, outbox()).unwrap();
        st.leave(&id, 1);
        backdate_empty(&st, &id, EMPTY_ROOM_GRACE);

        assert!(
            st.join(&id, 1, outbox()).is_err(),
            "过了宽限期该报房间不存在"
        );
        assert!(
            AppState::lock_read(&st.rooms).get(&id).is_none(),
            "过期房间该被写路径清扫掉"
        );
    }

    /// 有人回来就把计时清零，否则一次重进会吃掉下一次的宽限期。
    #[test]
    fn rejoin_should_reset_the_grace_timer() {
        let st = state();
        let id = create_room_now(&st);
        st.join(&id, 1, outbox()).unwrap();
        st.leave(&id, 1);
        backdate_empty(&st, &id, EMPTY_ROOM_GRACE / 2);
        st.join(&id, 1, outbox()).unwrap();

        assert!(
            AppState::lock_read(&st.rooms)[&id].empty_since.is_none(),
            "有人进来后宽限期计时该清零"
        );

        // 再走一轮完整的宽限期，仍然进得去
        st.leave(&id, 1);
        backdate_empty(&st, &id, EMPTY_ROOM_GRACE / 2);
        assert!(
            st.join(&id, 1, outbox()).is_ok(),
            "计时没重置，宽限期被吃掉了"
        );
    }

    /// 重复 leave（断连清理 + 显式离房都会走）不该把宽限期一次次往后延。
    #[test]
    fn repeated_leave_should_not_extend_the_grace() {
        let st = state();
        let id = create_room_now(&st);
        st.join(&id, 1, outbox()).unwrap();
        st.leave(&id, 1);
        backdate_empty(&st, &id, EMPTY_ROOM_GRACE / 2);

        st.leave(&id, 1); // 同一个用户再离一次

        let rooms = AppState::lock_read(&st.rooms);
        let since = rooms[&id].empty_since.expect("该有空窗打点");
        assert!(
            since.elapsed() >= EMPTY_ROOM_GRACE / 2,
            "重复 leave 把空窗起点重置了，宽限期被无限延长"
        );
    }
}
