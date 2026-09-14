//! 试麦：录一段 → 过完整上行链（DSP + Opus 编解码）→ 回放，附电平报告。
//!
//! 录播分离（非实时环回），无啸叫风险，不用戴耳机。
//! 用户听到的就是队友会听到的声音（含降噪/增益/Opus）。
//! 同步阻塞约 2× 录音时长，调用方放 `spawn_blocking` 里跑。

use std::time::{Duration, Instant};

use ringbuf::traits::{Consumer, Producer};
use voice_common::VoiceError;

use super::capture::{capture_ring, start_capture, CaptureConfig};
use super::playback::start_playback;
use crate::codec::{
    LiveDecoder, LiveEncoder, OpusConfig, OpusDecoder, OpusEncoder, MAX_PACKET_BYTES,
};
use crate::dsp::{DspChain, FRAME_LEN};

/// 试麦报告（电平基于**原始 mic** 信号，调增益用它为准）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct MicTestReport {
    pub record_secs: u64,
    pub peak: f32,
    pub rms_dbfs: f32,
    /// |s| >= 0.99 的采样占比（>1% 说明要关小麦克风音量）。
    pub clipped_ratio: f32,
    pub played_secs: f32,
}

pub fn mic_test(
    input: Option<&str>,
    output: Option<&str>,
    record_secs: u64,
) -> Result<MicTestReport, VoiceError> {
    let secs = record_secs.clamp(2, 10);

    // ---- 1. 录
    let (cap, mut mic) = start_capture(input, CaptureConfig::default())?;
    if cap.sample_rate != 48_000 {
        return Err(VoiceError::Device(format!(
            "capture rate {}Hz != 48kHz",
            cap.sample_rate
        )));
    }
    let want = 48_000usize * secs as usize;
    let mut raw: Vec<f32> = Vec::with_capacity(want);
    let deadline = Instant::now() + Duration::from_secs(secs + 5);
    while raw.len() < want {
        while let Some(s) = mic.try_pop() {
            raw.push(s);
            if raw.len() >= want {
                break;
            }
        }
        if Instant::now() > deadline {
            return Err(VoiceError::Stream("mic test capture timed out".into()));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(cap);

    let peak = raw.iter().fold(0.0f32, |a, s| a.max(s.abs()));
    let rms = (raw.iter().map(|s| s * s).sum::<f32>() / raw.len().max(1) as f32).sqrt();
    let rms_dbfs = 20.0 * rms.max(1e-6).log10();
    let clipped = raw.iter().filter(|s| s.abs() >= 0.99).count();
    let report = MicTestReport {
        record_secs: secs,
        peak,
        rms_dbfs,
        clipped_ratio: clipped as f32 / raw.len().max(1) as f32,
        played_secs: 0.0,
    };

    // ---- 2. 过完整上行链（与通话 pump 同构：DSP → Opus 编码 → 解码）
    let mut chain = DspChain::new(true);
    let mut enc =
        LiveEncoder::new(OpusConfig::default()).map_err(|e| VoiceError::Codec(e.to_string()))?;
    let mut dec = LiveDecoder::new(48_000).map_err(|e| VoiceError::Codec(e.to_string()))?;
    let mut pkt = vec![0u8; MAX_PACKET_BYTES];
    let mut heard: Vec<f32> = Vec::with_capacity(want);
    for frame in raw.as_chunks::<FRAME_LEN>().0 {
        let mut f = [0.0f32; FRAME_LEN];
        f.copy_from_slice(frame);
        chain.process(&mut f);
        let n = enc
            .encode(&f, &mut pkt)
            .map_err(|e| VoiceError::Codec(e.to_string()))?;
        let mut back = [0.0f32; FRAME_LEN];
        let m = dec
            .decode(&pkt[..n], &mut back)
            .map_err(|e| VoiceError::Codec(e.to_string()))?;
        heard.extend_from_slice(&back[..m]);
    }

    // ---- 3. 放
    let (mut prod, cons) = capture_ring(CaptureConfig::default());
    let play = start_playback(output, cons, 48_000, 1)?;
    let total = heard.len() as u64;
    let mut pushed = 0usize;
    let play_deadline = Instant::now() + Duration::from_secs(heard.len() as u64 / 48_000 + 8);
    while play.stats.played() < total {
        while pushed < heard.len() {
            match prod.try_push(heard[pushed]) {
                Ok(()) => pushed += 1,
                Err(_) => break,
            }
        }
        if Instant::now() > play_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let played_secs = play.stats.played() as f32 / 48_000.0;
    drop(play);

    Ok(MicTestReport {
        played_secs,
        ..report
    })
}
