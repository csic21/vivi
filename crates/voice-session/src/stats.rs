//! 通话快照（UI 成员列表 / 右上角状态栏数据源）。

use serde::{Deserialize, Serialize};
use voice_common::RouteType;

/// 单个远端用户的状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerStats {
    pub user_id: u64,
    pub connected: bool,
    /// 有人声（解码侧能量 VAD + hangover，UI 🟢 用）。
    pub speaking: bool,
    /// 经队友中转收听（Some(中继方)；None = 直连）。
    pub relayed_via: Option<u64>,
    pub route: Option<RouteType>,
    pub rtt_ms: Option<f32>,
    pub jitter_ms: f32,
    pub loss_percent: f32,
    /// 本地为该用户设置的增益（0.0~2.0）。
    pub gain: f32,
    pub decoded_frames: u64,
    pub plc_frames: u64,
}

/// 整场通话快照（暂不含自己；UI 自带本端状态）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionStats {
    pub peers: Vec<PeerStats>,
    pub mixed_frames: u64,
    /// 本端麦克风有人声（上行 VAD）。
    pub speaking_self: bool,
    /// 本端 mic 电平 RMS（0.0~1.0，DSP 之前，调增益用）。
    pub mic_level: f32,
    /// 本端各项设置回显（UI 开关状态用）。
    pub mic_gain: f32,
    pub speaker_gain: f32,
    pub ns_enabled: bool,
    pub agc_enabled: bool,
    /// 本端是否静音（PTT 松开/自由说话点静音都走这里）。
    pub muted: bool,
    /// 信令层最近一条错误原文（服务端 `SignalMessage::Error`），`None` = 近段时间没出过错。
    ///
    /// 为什么要抬到快照里：这些错误以前只走 `tracing::warn!`，而桌面端根本没装
    /// tracing subscriber —— "房间不存在""目标不在线"这类话等于被扔进黑洞，
    /// 用户只能看到一个和真实原因无关的界面状态。
    ///
    /// 有时效（见 `session.rs` 的 `SIGNAL_ERROR_TTL`），过期就回到 `None`。
    pub signal_error: Option<String>,
}
