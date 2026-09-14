//! 实时语音核心：采集 / 编解码 / DSP / jitter / mixer / stats。
//! 硬约束：不依赖 React / Tauri / UI；audio callback 内不阻塞。

pub mod audio;
pub mod codec;
pub mod dsp;
pub mod jitter;
pub mod mixer;
pub mod net;
pub mod stats;

pub use voice_common::{AudioConfig, LatencyBreakdown, NetworkStats};
