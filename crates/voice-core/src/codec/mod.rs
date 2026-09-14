//! Phase 2：Opus 编解码（PCM <-> Packet），默认 48kHz/mono/10ms/32kbps。
//!
//! 热路径零分配：调用方提供包缓冲与 PCM 缓冲，编解码只做借用写入。

mod live;

pub use live::{LiveDecoder, LiveEncoder};

/// 10ms @ 48kHz mono 的采样数（标准语音帧）。
pub const FRAME_SAMPLES_10MS_48K: usize = 480;
/// 调用方包缓冲建议大小（Opus 单包远小于此，留足余量避免编码失败）。
pub const MAX_PACKET_BYTES: usize = 4000;

/// 48kHz mono 下合法的 Opus 帧长（2.5/5/10/20/40/60ms）。
pub fn valid_frame_len_48k_mono(len: usize) -> bool {
    matches!(len, 120 | 240 | 480 | 960 | 1920 | 2880)
}

/// Section 6 默认配置。
#[derive(Debug, Clone, Copy)]
pub struct OpusConfig {
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_ms: u32,
    pub bitrate_bps: u32,
    pub dtx: bool,
    pub fec: bool,
    pub packet_loss_percent: u8,
    pub complexity: u8,
}

impl Default for OpusConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 1,
            frame_ms: 10,
            bitrate_bps: 32_000,
            dtx: true,
            fec: true,
            packet_loss_percent: 0,
            complexity: 5,
        }
    }
}

pub trait OpusEncoder {
    /// 编码一帧 f32 mono PCM。
    /// `out` 为调用方提供的包缓冲（建议 [`MAX_PACKET_BYTES`]），返回写入字节数。零分配。
    fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> anyhow::Result<usize>;
    fn set_bitrate(&mut self, bps: u32) -> anyhow::Result<()>;
    fn set_packet_loss_perc(&mut self, percent: u8) -> anyhow::Result<()>;
    fn set_dtx(&mut self, enabled: bool) -> anyhow::Result<()>;
    fn set_complexity(&mut self, complexity: u8) -> anyhow::Result<()>;
    fn set_fec(&mut self, enabled: bool) -> anyhow::Result<()>;
}

pub trait OpusDecoder {
    /// 解码一包。`packet` 为空时做 PLC（丢包隐藏）；
    /// `pcm_out` 须能容纳一帧，返回每声道采样数。
    fn decode(&mut self, packet: &[u8], pcm_out: &mut [f32]) -> anyhow::Result<usize>;
}
