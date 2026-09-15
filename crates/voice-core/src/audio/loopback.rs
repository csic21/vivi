//! 实时试听：麦克风 → DSP → 扬声器，10ms 级延迟。
//!
//! 与 [`super::mictest`] 的录播不同，这里是实时环回（Discord 式 mic test）。
//! 使用要求：**戴耳机**，否则扬声器声音回灌麦克风形成啸叫。
//! Drop 即停止（pump 线程 ~10ms 内自行退出）。

use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};
use std::time::Duration;

use ringbuf::traits::{Consumer, Producer};
use voice_common::VoiceError;

use super::capture::{capture_ring, start_capture, CaptureConfig};
use super::playback::start_playback;
use super::resample::{frame_len_10ms, MonoResampler};
use crate::codec::FRAME_SAMPLES_10MS_48K;
use crate::dsp::DspChain;

pub struct LoopbackHandle {
    _cap: super::capture::CaptureHandle,
    _play: super::playback::PlaybackHandle,
    stop: Arc<AtomicBool>,
    level_bits: Arc<AtomicU32>,
}

impl LoopbackHandle {
    /// 原始 mic RMS（0.0~1.0），无锁直读，供试麦话筒图标/电平条。
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level_bits.load(Ordering::Relaxed))
    }
}

impl Drop for LoopbackHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// 启动实时试听。ring 只留 10 帧上限（平时几乎是空的），延迟最低。
pub fn start_loopback(
    input: Option<&str>,
    output: Option<&str>,
) -> Result<LoopbackHandle, VoiceError> {
    let (cap, mic_cons) = start_capture(input, CaptureConfig::default())?;
    let capture_rate = cap.sample_rate;
    // 播放统一按 48k/mono 要，设备不支持则回退并在 pump 里重采样
    let (play_prod, play_cons) = capture_ring(CaptureConfig {
        ring_frames: 10,
        ..CaptureConfig::default()
    });
    let play = start_playback(output, play_cons, 48_000, 1)?;
    let play_rate = play.sample_rate;
    let stop = Arc::new(AtomicBool::new(false));
    let level_bits = Arc::new(AtomicU32::new(0));
    std::thread::spawn({
        let stop = Arc::clone(&stop);
        let level_bits = Arc::clone(&level_bits);
        let mut mic = mic_cons;
        let mut prod = play_prod;
        let mut dsp = DspChain::new(true);
        let mut cap_resampler =
            (capture_rate != 48_000).then(|| MonoResampler::new(capture_rate, 48_000));
        let mut play_resampler =
            (play_rate != 48_000).then(|| MonoResampler::new(48_000, play_rate));
        let mut play_frame = vec![0.0f32; frame_len_10ms(play_rate).max(1)];
        let mut frame = [0f32; FRAME_SAMPLES_10MS_48K];
        move || {
            while !stop.load(Ordering::Relaxed) {
                if let Some(r) = cap_resampler.as_mut() {
                    loop {
                        while let Some(s) = mic.try_pop() {
                            r.push(&[s]);
                        }
                        if r.pop_frame(&mut frame) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(1));
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                    }
                } else {
                    let mut n = 0;
                    while n < frame.len() {
                        match mic.try_pop() {
                            Some(s) => {
                                frame[n] = s;
                                n += 1;
                            }
                            None => std::thread::sleep(Duration::from_millis(1)),
                        }
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                    }
                }
                // 米表用 DSP 前的原始 mic，和通话会话同一口径。
                let level = crate::dsp::rms(&frame);
                level_bits.store(level.to_bits(), Ordering::Relaxed);
                // 与通话上行同构（NS→VAD→AGC），听到即队友将听到的
                dsp.process(&mut frame);
                if let Some(r) = play_resampler.as_mut() {
                    r.push(&frame);
                    if r.pop_frame(&mut play_frame) {
                        for s in play_frame.iter() {
                            if prod.try_push(*s).is_err() {
                                break;
                            }
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                        }
                    }
                } else {
                    for s in frame.iter() {
                        if prod.try_push(*s).is_err() {
                            break;
                        }
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                    }
                }
            }
        }
    });
    Ok(LoopbackHandle {
        _cap: cap,
        _play: play,
        stop,
        level_bits,
    })
}
