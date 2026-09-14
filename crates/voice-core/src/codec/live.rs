//! 基于 `opus` crate 的具体编解码实现（VoIP 应用，语音优化）。

use super::{valid_frame_len_48k_mono, OpusConfig, OpusDecoder, OpusEncoder};
use voice_common::VoiceError;

/// Opus 编码器：48kHz/mono/VoIP。非 `Sync`（libopus 状态机），按线程独占使用。
pub struct LiveEncoder {
    inner: opus::Encoder,
    config: OpusConfig,
}

impl LiveEncoder {
    /// 按 [`OpusConfig`] 创建并应用全部旋钮（bitrate/DTX/FEC/预期丢包/complexity）。
    /// 当前仅支持 mono（多人链路每路独立编码器，见 Phase 8）。
    pub fn new(config: OpusConfig) -> anyhow::Result<Self> {
        if config.channels != 1 {
            return Err(VoiceError::Codec(format!(
                "only mono supported, got {} channels",
                config.channels
            ))
            .into());
        }
        let mut inner = opus::Encoder::new(
            config.sample_rate,
            opus::Channels::Mono,
            opus::Application::Voip,
        )?;
        inner.set_signal(opus::Signal::Voice)?;
        Self::apply(&mut inner, &config)?;
        Ok(Self { inner, config })
    }

    /// 运行时整体换配置（不断流重建之外的轻量路径）。
    pub fn reconfig(&mut self, config: OpusConfig) -> anyhow::Result<()> {
        if config.channels != 1 || config.sample_rate != self.config.sample_rate {
            // 采样率/声道变化需要重建状态机，显式报错避免静默错配。
            return Err(VoiceError::Codec(
                "sample rate/channel change requires a new encoder".into(),
            )
            .into());
        }
        Self::apply(&mut self.inner, &config)?;
        self.config = config;
        Ok(())
    }

    /// 当前生效配置。
    pub fn config(&self) -> OpusConfig {
        self.config
    }

    fn apply(inner: &mut opus::Encoder, config: &OpusConfig) -> anyhow::Result<()> {
        inner.set_bitrate(opus::Bitrate::Bits(config.bitrate_bps as i32))?;
        inner.set_complexity(config.complexity.min(10) as i32)?;
        inner.set_dtx(config.dtx)?;
        inner.set_inband_fec(config.fec)?;
        inner.set_packet_loss_perc(config.packet_loss_percent.min(100) as i32)?;
        Ok(())
    }
}

impl OpusEncoder for LiveEncoder {
    fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> anyhow::Result<usize> {
        if self.config.sample_rate != 48_000 || !valid_frame_len_48k_mono(pcm.len()) {
            return Err(VoiceError::Codec(format!(
                "unsupported frame: {} samples @ {}Hz (want 120/240/480/960/1920/2880 @ 48kHz)",
                pcm.len(),
                self.config.sample_rate
            ))
            .into());
        }
        Ok(self.inner.encode_float(pcm, out)?)
    }

    fn set_bitrate(&mut self, bps: u32) -> anyhow::Result<()> {
        self.inner.set_bitrate(opus::Bitrate::Bits(bps as i32))?;
        self.config.bitrate_bps = bps;
        Ok(())
    }

    fn set_packet_loss_perc(&mut self, percent: u8) -> anyhow::Result<()> {
        self.inner.set_packet_loss_perc(percent.min(100) as i32)?;
        self.config.packet_loss_percent = percent;
        Ok(())
    }

    fn set_dtx(&mut self, enabled: bool) -> anyhow::Result<()> {
        self.inner.set_dtx(enabled)?;
        self.config.dtx = enabled;
        Ok(())
    }

    fn set_complexity(&mut self, complexity: u8) -> anyhow::Result<()> {
        self.inner.set_complexity(complexity.min(10) as i32)?;
        self.config.complexity = complexity;
        Ok(())
    }

    fn set_fec(&mut self, enabled: bool) -> anyhow::Result<()> {
        self.inner.set_inband_fec(enabled)?;
        self.config.fec = enabled;
        Ok(())
    }
}

/// Opus 解码器：48kHz/mono。FEC 恢复（`fec=true` 解码）留到 jitter 阶段按丢包状态驱动，
/// 这里正常包一律 `fec=false`，丢包走空包 PLC。
pub struct LiveDecoder {
    inner: opus::Decoder,
}

impl LiveDecoder {
    pub fn new(sample_rate: u32) -> anyhow::Result<Self> {
        Ok(Self {
            inner: opus::Decoder::new(sample_rate, opus::Channels::Mono)?,
        })
    }
}

impl OpusDecoder for LiveDecoder {
    fn decode(&mut self, packet: &[u8], pcm_out: &mut [f32]) -> anyhow::Result<usize> {
        Ok(self.inner.decode_float(packet, pcm_out, false)?)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{FRAME_SAMPLES_10MS_48K, MAX_PACKET_BYTES};
    use super::*;

    fn tone_frame(freq_hz: f32) -> Vec<f32> {
        (0..FRAME_SAMPLES_10MS_48K)
            .map(|i| (2.0 * std::f32::consts::PI * freq_hz * i as f32 / 48_000.0).sin() * 0.5)
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn encoder_should_accept_default_10ms_frame() {
        let mut enc = LiveEncoder::new(OpusConfig::default()).unwrap();
        let pcm = tone_frame(440.0);
        let mut out = vec![0u8; MAX_PACKET_BYTES];
        let n = enc.encode(&pcm, &mut out).unwrap();
        assert!(n > 0 && n <= MAX_PACKET_BYTES);
    }

    #[test]
    fn encoder_should_reject_bad_frame_length() {
        let mut enc = LiveEncoder::new(OpusConfig::default()).unwrap();
        let pcm = vec![0.0f32; 100];
        let mut out = vec![0u8; MAX_PACKET_BYTES];
        assert!(enc.encode(&pcm, &mut out).is_err());
    }

    #[test]
    fn roundtrip_should_preserve_tone_energy() {
        let mut enc = LiveEncoder::new(OpusConfig::default()).unwrap();
        let mut dec = LiveDecoder::new(48_000).unwrap();
        let pcm = tone_frame(440.0);
        let mut pkt = vec![0u8; MAX_PACKET_BYTES];
        let mut back = vec![0.0f32; FRAME_SAMPLES_10MS_48K];
        // 编解码器启动存在前瞻/淡入瞬态，先跑 5 帧进入稳态再断言
        let mut ratio = 0.0;
        for _ in 0..5 {
            let n = enc.encode(&pcm, &mut pkt).unwrap();
            let m = dec.decode(&pkt[..n], &mut back).unwrap();
            assert_eq!(m, FRAME_SAMPLES_10MS_48K);
            ratio = rms(&back) / rms(&pcm);
        }
        assert!((ratio - 1.0).abs() < 0.35, "rms ratio = {ratio}");
    }

    #[test]
    fn decode_should_conceal_when_packet_empty() {
        let mut dec = LiveDecoder::new(48_000).unwrap();
        let mut back = vec![0.0f32; FRAME_SAMPLES_10MS_48K];
        let m = dec.decode(&[], &mut back).unwrap();
        assert_eq!(m, FRAME_SAMPLES_10MS_48K);
    }

    #[test]
    fn set_bitrate_should_update_effective_config() {
        let mut enc = LiveEncoder::new(OpusConfig::default()).unwrap();
        enc.set_bitrate(48_000).unwrap();
        assert_eq!(enc.config().bitrate_bps, 48_000);
    }
}
