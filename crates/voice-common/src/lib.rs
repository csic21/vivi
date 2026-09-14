//! 共享基础类型：音频配置 / 设备 / 网络状态。
//! 约束：不依赖 cpal / tauri / tokio，保证到处可复用。

use serde::{Deserialize, Serialize};

/// 当前 Unix 纪元毫秒（语音包发送时间戳）。
/// 跨机器时钟不需同步：jitter/RTT 计算只用差值，offset 相互抵消。
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Phase 1 默认音频格式：48kHz / Mono / 10ms / 480 samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub frame_ms: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 1,
            frame_ms: 10,
        }
    }
}

impl AudioConfig {
    /// 每帧采样数（mono）：48000 * 10 / 1000 = 480.
    pub fn samples_per_frame(&self) -> usize {
        (self.sample_rate as usize * self.frame_ms as usize / 1000) * self.channels as usize
    }
}

/// 音频设备方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceKind {
    Input,
    Output,
}

/// 可选音频设备描述（Task 1 枚举结果）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub is_default: bool,
}

/// 传输路径：P2P 直连 / TURN 中继（Section 11 UI 状态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteType {
    Direct,
    Relay,
}

/// 实时网络指标（UI 右上角 PING/JITTER/LOSS/ROUTE）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct NetworkStats {
    pub rtt_ms: u32,
    pub jitter_ms: u32,
    pub loss_percent: f32,
    pub route: Option<RouteType>,
}

impl NetworkStats {
    /// Section 11 三色状态：🟢 DIRECT / 🟡 TURN / 🔴 BAD NETWORK.
    pub fn health(&self) -> &'static str {
        if self.loss_percent > 5.0 || self.rtt_ms > 300 {
            "bad"
        } else if matches!(self.route, Some(RouteType::Relay)) {
            "relay"
        } else {
            "direct"
        }
    }
}

/// 端到端延迟分解（Section 20 benchmark）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct LatencyBreakdown {
    pub capture_ms: f32,
    pub encode_ms: f32,
    pub network_ms: f32,
    pub jitter_ms: f32,
    pub decode_ms: f32,
    pub playback_ms: f32,
}

impl LatencyBreakdown {
    pub fn total_ms(&self) -> f32 {
        self.capture_ms
            + self.encode_ms
            + self.network_ms
            + self.jitter_ms
            + self.decode_ms
            + self.playback_ms
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserId(pub u64);

#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error("audio device error: {0}")]
    Device(String),
    #[error("audio stream error: {0}")]
    Stream(String),
    #[error("codec error: {0}")]
    Codec(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_frame_is_480_samples() {
        assert_eq!(AudioConfig::default().samples_per_frame(), 480);
    }

    #[test]
    fn health_bad_network_threshold() {
        let s = NetworkStats {
            rtt_ms: 32,
            jitter_ms: 6,
            loss_percent: 0.0,
            route: Some(RouteType::Direct),
        };
        assert_eq!(s.health(), "direct");
    }
}
