//! Task 3：音频播放（欠载补零，mono 自动扩展到多声道）。
//!
//! consumer 所有权直接移入 callback 线程：无锁、无共享状态，
//! 空包填 0 并计数欠载（上层据此判断 jitter / 消费端掉队）。

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample};
use ringbuf::{traits::Consumer, HeapCons};
use voice_common::{DeviceKind, VoiceError};

use super::capture::CaptureConsumer;
use super::device::find_device;

/// 热路径之外的播放统计（原子操作，callback 与控制线程共享）。
#[derive(Debug, Default)]
pub struct PlaybackStats {
    played: AtomicU64,
    underruns: AtomicU64,
    stream_errors: AtomicU64,
}

impl PlaybackStats {
    /// 实际播出的采样数。
    pub fn played(&self) -> u64 {
        self.played.load(Ordering::Relaxed)
    }

    /// ring 为空而补零的采样数（持续增长说明生产端跟不上）。
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// 流错误回调次数（设备拔出等异常，非每帧热路径）。
    pub fn stream_errors(&self) -> u64 {
        self.stream_errors.load(Ordering::Relaxed)
    }
}

/// 运行中的播放流；drop 即停止。
pub struct PlaybackHandle {
    _stream: cpal::Stream,
    pub stats: Arc<PlaybackStats>,
    /// 硬件实际生效的采样率/声道（重采样目标，`start_playback` 回填）。
    pub sample_rate: u32,
    pub channels: u16,
}

/// 本机可驱动的采样格式（Windows 上一些 USB/虚拟设备只给 U8，
/// 之前遇到就直接 `unsupported output sample format` 报错试麦失败）。
fn is_supported_format(f: SampleFormat) -> bool {
    matches!(
        f,
        SampleFormat::F32
            | SampleFormat::F64
            | SampleFormat::I8
            | SampleFormat::I16
            | SampleFormat::I32
            | SampleFormat::U8
            | SampleFormat::U16
            | SampleFormat::U32
    )
}

/// 优先同采样率 + 同声道 + F32；否则同采样率任意配置；再退化为默认输出配置。
/// 采样率对不上时不做重采样（后续阶段处理），调用方应据日志检查。
/// 只挑可驱动的格式（不支持的直接跳过，免得选中了后面建流才报错）。
fn pick_output_config(
    device: &cpal::Device,
    sample_rate: u32,
    channels: u16,
) -> Result<cpal::SupportedStreamConfig, VoiceError> {
    if let Ok(ranges) = device.supported_output_configs() {
        let mut same_rate = None;
        for range in ranges {
            if range.contains_rate(sample_rate) {
                if range.channels() == channels && range.sample_format() == SampleFormat::F32 {
                    if let Some(cfg) = range.try_with_sample_rate(sample_rate) {
                        return Ok(cfg);
                    }
                } else if same_rate.is_none() && is_supported_format(range.sample_format()) {
                    same_rate = range.try_with_sample_rate(sample_rate);
                }
            }
        }
        if let Some(cfg) = same_rate {
            return Ok(cfg);
        }
    }
    let fallback = device
        .default_output_config()
        .map_err(|e| VoiceError::Device(e.to_string()))?;
    if !is_supported_format(fallback.sample_format()) {
        return Err(VoiceError::Stream(format!(
            "unsupported output sample format: {:?} (device default, try another device)",
            fallback.sample_format()
        )));
    }
    Ok(fallback)
}

/// 每帧热路径：取 mono 采样并复制到全声道；取空则补零并计数。
fn fill_frames<T>(
    data: &mut [T],
    channels: usize,
    consumer: &mut HeapCons<f32>,
    stats: &PlaybackStats,
) where
    T: SizedSample + Copy,
    T: cpal::FromSample<f32>,
{
    let channels = channels.max(1);
    for frame in data.chunks_mut(channels) {
        let sample = match consumer.try_pop() {
            Some(s) => {
                stats.played.fetch_add(1, Ordering::Relaxed);
                s
            }
            None => {
                stats.underruns.fetch_add(1, Ordering::Relaxed);
                0.0
            }
        };
        frame.fill(T::from_sample(sample));
    }
}

fn build_output_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut consumer: CaptureConsumer,
    stats: Arc<PlaybackStats>,
) -> Result<cpal::Stream, VoiceError>
where
    T: SizedSample + Copy + Send + 'static,
    T: cpal::FromSample<f32>,
{
    let channels = config.channels.max(1) as usize;
    let err_stats = Arc::clone(&stats);
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                fill_frames(data, channels, &mut consumer, &stats);
            },
            move |err| {
                // 异常路径（设备拔出等）：只计数 + warn，不碰热路径。
                err_stats.stream_errors.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(error = %err, "output stream error");
            },
            // 流初始化超时：10s 后直接报错而不是无限挂起。
            Some(std::time::Duration::from_secs(10)),
        )
        .map_err(|e| VoiceError::Stream(e.to_string()))
}

/// 启动音频播放，消费采集侧的 ring。
///
/// `sample_rate` / `channels` 通常取 [`super::capture::CaptureHandle`] 的实际值；
/// 输出设备不支持该采样率时退化为默认配置（此时音调会偏，需重采样，后续阶段处理）。
pub fn start_playback(
    device_query: Option<&str>,
    consumer: CaptureConsumer,
    sample_rate: u32,
    channels: u16,
) -> Result<PlaybackHandle, VoiceError> {
    let device = find_device(DeviceKind::Output, device_query)?;
    let supported = pick_output_config(&device, sample_rate, channels)?;
    let actual = supported.config();
    if actual.sample_rate != sample_rate {
        tracing::warn!(
            want = sample_rate,
            got = actual.sample_rate,
            "output rate differs from capture rate; resampling not implemented yet"
        );
    }

    let stats = Arc::new(PlaybackStats::default());
    let stream = match supported.sample_format() {
        SampleFormat::F32 => {
            build_output_stream::<f32>(&device, &actual, consumer, Arc::clone(&stats))
        }
        SampleFormat::F64 => {
            build_output_stream::<f64>(&device, &actual, consumer, Arc::clone(&stats))
        }
        SampleFormat::I8 => {
            build_output_stream::<i8>(&device, &actual, consumer, Arc::clone(&stats))
        }
        SampleFormat::I16 => {
            build_output_stream::<i16>(&device, &actual, consumer, Arc::clone(&stats))
        }
        SampleFormat::I32 => {
            build_output_stream::<i32>(&device, &actual, consumer, Arc::clone(&stats))
        }
        SampleFormat::U8 => {
            build_output_stream::<u8>(&device, &actual, consumer, Arc::clone(&stats))
        }
        SampleFormat::U16 => {
            build_output_stream::<u16>(&device, &actual, consumer, Arc::clone(&stats))
        }
        SampleFormat::U32 => {
            build_output_stream::<u32>(&device, &actual, consumer, Arc::clone(&stats))
        }
        other => {
            return Err(VoiceError::Stream(format!(
                "unsupported output sample format: {other:?}"
            )));
        }
    }?;

    stream
        .play()
        .map_err(|e| VoiceError::Stream(e.to_string()))?;
    Ok(PlaybackHandle {
        _stream: stream,
        stats,
        sample_rate: actual.sample_rate,
        channels: actual.channels,
    })
}

pub fn probe_default_output() -> Result<cpal::SupportedStreamConfig, VoiceError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| VoiceError::Device("no default output device".into()))?;
    device
        .default_output_config()
        .map_err(|e: cpal::Error| VoiceError::Device(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::super::capture::{capture_ring, CaptureConfig};
    use super::*;
    use ringbuf::traits::Producer;

    #[test]
    fn fill_frames_should_emit_silence_when_ring_empty() {
        let (_prod, mut cons) = capture_ring(CaptureConfig::default());
        let stats = PlaybackStats::default();
        let mut buf = [1.0f32; 8];
        fill_frames::<f32>(&mut buf, 1, &mut cons, &stats);
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn fill_frames_should_count_underruns_when_ring_empty() {
        let (_prod, mut cons) = capture_ring(CaptureConfig::default());
        let stats = PlaybackStats::default();
        let mut buf = [0.0f32; 4];
        fill_frames::<f32>(&mut buf, 1, &mut cons, &stats);
        assert_eq!(stats.underruns(), 4);
    }

    #[test]
    fn fill_frames_should_duplicate_mono_across_channels() {
        let (mut prod, mut cons) = capture_ring(CaptureConfig::default());
        prod.try_push(0.5).unwrap();
        let stats = PlaybackStats::default();
        let mut stereo = [0.0f32; 2];
        fill_frames::<f32>(&mut stereo, 2, &mut cons, &stats);
        assert_eq!(stereo, [0.5, 0.5]);
    }
}
