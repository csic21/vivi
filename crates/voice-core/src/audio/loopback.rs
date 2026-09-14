//! 实时试听：麦克风 → DSP → 扬声器，10ms 级延迟。
//!
//! 与 [`super::mictest`] 的录播不同，这里是实时环回（Discord 式 mic test）。
//! 使用要求：**戴耳机**，否则扬声器声音回灌麦克风形成啸叫。
//! Drop 即停止（pump 线程 ~10ms 内自行退出）。

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use ringbuf::traits::{Consumer, Producer};
use voice_common::VoiceError;

use super::capture::{capture_ring, start_capture, CaptureConfig};
use super::playback::start_playback;
use crate::codec::FRAME_SAMPLES_10MS_48K;
use crate::dsp::DspChain;

pub struct LoopbackHandle {
    _cap: super::capture::CaptureHandle,
    _play: super::playback::PlaybackHandle,
    stop: Arc<AtomicBool>,
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
    if cap.sample_rate != 48_000 {
        return Err(VoiceError::Device(format!(
            "capture rate {}Hz != 48kHz",
            cap.sample_rate
        )));
    }
    let (play_prod, play_cons) = capture_ring(CaptureConfig {
        ring_frames: 10,
        ..CaptureConfig::default()
    });
    let play = start_playback(output, play_cons, cap.sample_rate, cap.channels)?;
    let stop = Arc::new(AtomicBool::new(false));
    std::thread::spawn({
        let stop = Arc::clone(&stop);
        let mut mic = mic_cons;
        let mut prod = play_prod;
        let mut dsp = DspChain::new(true);
        let mut frame = [0f32; FRAME_SAMPLES_10MS_48K];
        move || {
            while !stop.load(Ordering::Relaxed) {
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
                // 与通话上行同构（NS→VAD→AGC），听到即队友将听到的
                dsp.process(&mut frame);
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
    });
    Ok(LoopbackHandle {
        _cap: cap,
        _play: play,
        stop,
    })
}
