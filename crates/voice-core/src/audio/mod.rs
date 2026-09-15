//! 音频 IO（Phase 1）：设备枚举 / 采集 / 播放。
//! 数据面用 lock-free ring 递交给上层，callback 内只做 try_push。

pub mod capture;
pub mod device;
pub mod loopback;
pub mod mictest;
pub mod playback;
pub mod resample;

pub use capture::{
    capture_ring, probe_default_input, start_capture, CaptureConfig, CaptureConsumer,
    CaptureHandle, CaptureProducer, CaptureStats,
};
pub use device::{default_devices, find_device, list_devices};
pub use loopback::{start_loopback, LoopbackHandle};
pub use mictest::{mic_test, MicTestReport};
pub use playback::{probe_default_output, start_playback, PlaybackHandle, PlaybackStats};
pub use resample::{frame_len_10ms, resample_once, MonoResampler};
