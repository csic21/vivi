//! Task 2：麦克风采集（48kHz/mono/10ms 优先，多格式归一化为 f32 mono）。
//!
//! 约束（Section 20）：callback 内只做 `try_push`，ring 满直接计数丢弃，
//! 永不阻塞 / 分配 / 打日志；数据经 lock-free SPSC ring 交给 Opus / playback / 网络线程。

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample};
use ringbuf::{
    traits::{Producer, Split},
    HeapCons, HeapProd, HeapRb,
};
use voice_common::{AudioConfig, DeviceKind, VoiceError};

use super::device::find_device;

/// 采集配置：默认 48kHz/mono/10ms。
#[derive(Debug, Clone, Copy)]
pub struct CaptureConfig {
    pub audio: AudioConfig,
    /// callback -> consumer 的环形缓冲帧数（默认 50 帧 = 500ms 兜底）。
    pub ring_frames: usize,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            audio: AudioConfig::default(),
            ring_frames: 50,
        }
    }
}

/// 生产者持有端（给 CPAL callback），只允许 `try_push`，永不阻塞。
pub type CaptureProducer = HeapProd<f32>;
/// 消费者持有端（给 Opus / playback / 网络线程）。
pub type CaptureConsumer = HeapCons<f32>;

/// 创建 SPSC ring：容量 = samples_per_frame * ring_frames。
pub fn capture_ring(cfg: CaptureConfig) -> (CaptureProducer, CaptureConsumer) {
    let capacity = cfg.audio.samples_per_frame() * cfg.ring_frames;
    let rb = HeapRb::<f32>::new(capacity);
    rb.split()
}

/// 热路径之外的统计（原子操作，callback 与控制线程共享）。
#[derive(Debug, Default)]
pub struct CaptureStats {
    pushed: AtomicU64,
    dropped: AtomicU64,
    stream_errors: AtomicU64,
}

impl CaptureStats {
    /// 成功写入 ring 的采样数。
    pub fn pushed(&self) -> u64 {
        self.pushed.load(Ordering::Relaxed)
    }

    /// ring 满而丢弃的采样数（持续增长说明消费端跟不上）。
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// 流错误回调次数（设备拔出等异常，非每帧热路径）。
    pub fn stream_errors(&self) -> u64 {
        self.stream_errors.load(Ordering::Relaxed)
    }
}

/// 运行中的采集流；drop 即停止。
/// `sample_rate` / `channels` 为硬件实际生效值（重采样留到后续阶段）。
pub struct CaptureHandle {
    _stream: cpal::Stream,
    pub sample_rate: u32,
    pub channels: u16,
    pub stats: Arc<CaptureStats>,
}

/// 优先 F32 / mono / 48k；硬件不支持则退化为默认输入配置。
fn pick_input_config(
    device: &cpal::Device,
    want: AudioConfig,
) -> Result<cpal::SupportedStreamConfig, VoiceError> {
    if let Ok(ranges) = device.supported_input_configs() {
        let mut any_mono_48k = None;
        for range in ranges {
            if range.channels() == want.channels && range.contains_rate(want.sample_rate) {
                if range.sample_format() == SampleFormat::F32 {
                    if let Some(cfg) = range.try_with_sample_rate(want.sample_rate) {
                        return Ok(cfg);
                    }
                } else if any_mono_48k.is_none() {
                    any_mono_48k = range.try_with_sample_rate(want.sample_rate);
                }
            }
        }
        if let Some(cfg) = any_mono_48k {
            return Ok(cfg);
        }
    }
    device
        .default_input_config()
        .map_err(|e| VoiceError::Device(e.to_string()))
}

fn average_frame(sum: f32, channels: usize) -> f32 {
    sum / channels.max(1) as f32
}

/// 每帧热路径：多声道下混为 mono 并 `try_push`，满则计数丢弃。
fn push_frames<T>(data: &[T], channels: usize, producer: &mut CaptureProducer, stats: &CaptureStats)
where
    T: SizedSample + Copy,
    f32: cpal::FromSample<T>,
{
    let channels = channels.max(1);
    for frame in data.chunks_exact(channels) {
        let mut sum = 0.0f32;
        for s in frame {
            sum += (*s).to_sample::<f32>();
        }
        if producer.try_push(average_frame(sum, channels)).is_ok() {
            stats.pushed.fetch_add(1, Ordering::Relaxed);
        } else {
            stats.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn build_input_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut producer: CaptureProducer,
    stats: Arc<CaptureStats>,
) -> Result<cpal::Stream, VoiceError>
where
    T: SizedSample + Copy + Send + 'static,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let err_stats = Arc::clone(&stats);
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                push_frames(data, channels, &mut producer, &stats);
            },
            move |err| {
                // 异常路径（设备拔出等）：只计数 + warn，不碰热路径。
                err_stats.stream_errors.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(error = %err, "input stream error");
            },
            // 流初始化超时：macOS 无麦克风权限时会卡住等用户授权，10s 后直接报错而不是无限挂起。
            Some(std::time::Duration::from_secs(10)),
        )
        .map_err(|e| VoiceError::Stream(e.to_string()))
}

/// 启动麦克风采集。
///
/// `device_query` 为 id（`in:{name}`）、精确名或子串；`None` 用默认输入设备。
/// 返回 handle（持有流）与 consumer（交给 Opus / playback / 网络线程）。
pub fn start_capture(
    device_query: Option<&str>,
    cfg: CaptureConfig,
) -> Result<(CaptureHandle, CaptureConsumer), VoiceError> {
    let device = find_device(DeviceKind::Input, device_query)?;
    let supported = pick_input_config(&device, cfg.audio)?;
    let sample_rate = supported.sample_rate();
    let channels = supported.channels();
    let stream_config = supported.config();
    let sample_format = supported.sample_format();

    let (producer, consumer) = capture_ring(cfg);
    let stats = Arc::new(CaptureStats::default());
    let stream = match sample_format {
        SampleFormat::F32 => {
            build_input_stream::<f32>(&device, &stream_config, producer, Arc::clone(&stats))
        }
        SampleFormat::I16 => {
            build_input_stream::<i16>(&device, &stream_config, producer, Arc::clone(&stats))
        }
        SampleFormat::U16 => {
            build_input_stream::<u16>(&device, &stream_config, producer, Arc::clone(&stats))
        }
        other => {
            return Err(VoiceError::Stream(format!(
                "unsupported input sample format: {other:?}"
            )));
        }
    }?;

    stream
        .play()
        .map_err(|e| VoiceError::Stream(e.to_string()))?;
    Ok((
        CaptureHandle {
            _stream: stream,
            sample_rate,
            channels,
            stats,
        },
        consumer,
    ))
}

/// 查询默认输入流的实际配置（采样率/声道/buffer），用于 Task 4 延迟测量。
pub fn probe_default_input() -> Result<cpal::SupportedStreamConfig, VoiceError> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| VoiceError::Device("no default input device".into()))?;
    device
        .default_input_config()
        .map_err(|e| VoiceError::Device(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::Consumer;

    #[test]
    fn capture_ring_should_preserve_sample_order() {
        let (mut prod, mut cons) = capture_ring(CaptureConfig::default());
        prod.try_push(0.25).unwrap();
        assert_eq!(cons.try_pop(), Some(0.25));
    }

    #[test]
    fn average_frame_should_divide_sum_by_channel_count() {
        assert_eq!(average_frame(1.5, 2), 0.75);
    }
}
