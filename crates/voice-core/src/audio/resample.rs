//! 单声道线性重采样（设备采样率 ↔ 48kHz 语音链路）。
//!
//! 背景：macOS CoreAudio 多数给 48kHz，Windows WASAPI 常见 44100Hz；
//! 之前采集非 48k 直接报错进不了房，Windows↔Mac 也就无从互通。
//! 这里用线性插值把任意设备率归一到 48k（DSP/Opus 链路），
//! 再把 48k 混音转回播放设备率。质量够语音用，零依赖、无分配热路径之外。

/// 流式单声道重采样器：`push` 设备侧采样，`pop_frame` 取目标率整帧。
#[derive(Debug)]
pub struct MonoResampler {
    from_rate: u32,
    to_rate: u32,
    /// 每个输出采样跨过的输入采样数。
    step: f64,
    /// 下一个输出采样在 `buf` 坐标系里的读位置。
    pos: f64,
    buf: Vec<f32>,
}

impl MonoResampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        assert!(from_rate > 0 && to_rate > 0, "sample rate must be > 0");
        Self {
            from_rate,
            to_rate,
            step: from_rate as f64 / to_rate as f64,
            pos: 0.0,
            buf: Vec::new(),
        }
    }

    pub fn from_rate(&self) -> u32 {
        self.from_rate
    }

    pub fn to_rate(&self) -> u32 {
        self.to_rate
    }

    pub fn is_passthrough(&self) -> bool {
        self.from_rate == self.to_rate
    }

    /// 已缓存但尚未消费的输入采样数。
    pub fn buffered(&self) -> usize {
        self.buf.len().saturating_sub(self.pos.floor() as usize)
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.buf.extend_from_slice(samples);
    }

    /// 丢弃全部缓存（静音排空/切设备时防陈旧突发）。
    pub fn clear(&mut self) {
        self.buf.clear();
        self.pos = 0.0;
    }

    /// 尝试产出恰好 `out.len()` 个采样；输入不足返回 `false`（`out` 不动）。
    pub fn pop_frame(&mut self, out: &mut [f32]) -> bool {
        if out.is_empty() {
            return true;
        }
        if self.is_passthrough() {
            if self.buf.len() < out.len() {
                return false;
            }
            out.copy_from_slice(&self.buf[..out.len()]);
            self.buf.drain(..out.len());
            return true;
        }
        // 最后一个输出采样还需要后一个输入点做插值，多留 1 个。
        let need = self.pos + (out.len() - 1) as f64 * self.step + 1.0;
        if need > self.buf.len() as f64 {
            return false;
        }
        for (i, o) in out.iter_mut().enumerate() {
            let p = self.pos + i as f64 * self.step;
            let idx = p.floor() as usize;
            let frac = (p - idx as f64) as f32;
            let a = self.buf[idx];
            let b = self.buf[idx + 1];
            *o = a + (b - a) * frac;
        }
        self.pos += out.len() as f64 * self.step;
        let drain_n = self.pos.floor() as usize;
        self.buf.drain(..drain_n);
        self.pos -= drain_n as f64;
        true
    }

    /// 尽力产出最多 `max` 个采样，返回实际产出数（流式排空用）。
    pub fn pop_available(&mut self, out: &mut Vec<f32>, max: usize) -> usize {
        if self.is_passthrough() {
            let n = max.min(self.buf.len());
            out.extend_from_slice(&self.buf[..n]);
            self.buf.drain(..n);
            return n;
        }
        let mut n = 0;
        while n < max {
            let p = self.pos + n as f64 * self.step;
            let idx = p.floor() as usize;
            if idx + 1 >= self.buf.len() {
                break;
            }
            let frac = (p - idx as f64) as f32;
            let a = self.buf[idx];
            let b = self.buf[idx + 1];
            out.push(a + (b - a) * frac);
            n += 1;
        }
        self.pos += n as f64 * self.step;
        let drain_n = self.pos.floor() as usize;
        self.buf.drain(..drain_n);
        self.pos -= drain_n as f64;
        n
    }
}

/// 一次性整块变速（无状态，首尾各借 1 采样做插值；流式场景请用 [`MonoResampler`]）。
pub fn resample_once(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if input.is_empty() || from_rate == to_rate {
        return input.to_vec();
    }
    let mut r = MonoResampler::new(from_rate, to_rate);
    r.push(input);
    // 输出长度按比例估算，末尾插值多要 1 个输入点，少算 1 个输出保底。
    let expect = ((input.len() as f64 * to_rate as f64 / from_rate as f64).floor() as usize)
        .saturating_sub(1);
    let mut out = Vec::with_capacity(expect);
    r.pop_available(&mut out, expect);
    out
}

/// 10ms 帧在某采样率下的采样数（44100→441，48000→480）。
pub fn frame_len_10ms(sample_rate: u32) -> usize {
    ((sample_rate as u64 * 10) / 1000) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(len: usize, freq_hz: f32, rate: u32) -> Vec<f32> {
        (0..len)
            .map(|i| (2.0 * std::f32::consts::PI * freq_hz * i as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|s| s * s).sum::<f32>() / v.len().max(1) as f32).sqrt()
    }

    #[test]
    fn passthrough_should_preserve_samples() {
        let mut r = MonoResampler::new(48_000, 48_000);
        r.push(&[0.1, 0.2, 0.3, 0.4]);
        let mut out = [0.0f32; 4];
        assert!(r.pop_frame(&mut out));
        assert_eq!(out, [0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn upsample_441_to_480_should_fill_frame() {
        let src = sine(441, 440.0, 44_100);
        let mut r = MonoResampler::new(44_100, 48_000);
        r.push(&src);
        let mut out = [0.0f32; 480];
        // 末尾插值需要多 1 个输入点，再补一点
        r.push(&sine(44, 440.0, 44_100));
        assert!(r.pop_frame(&mut out));
        let ratio = rms(&out) / rms(&src).max(1e-6);
        assert!((ratio - 1.0).abs() < 0.25, "rms ratio = {ratio}");
    }

    #[test]
    fn downsample_480_to_441_should_fill_frame() {
        let src = sine(480, 440.0, 48_000);
        let mut r = MonoResampler::new(48_000, 44_100);
        r.push(&src);
        r.push(&sine(48, 440.0, 48_000));
        let mut out = [0.0f32; 441];
        assert!(r.pop_frame(&mut out));
        let ratio = rms(&out) / rms(&src).max(1e-6);
        assert!((ratio - 1.0).abs() < 0.25, "rms ratio = {ratio}");
    }

    #[test]
    fn streaming_should_track_ratio_over_time() {
        // 模拟 44100 设备按 128 块到来，取 48k 的 480 帧
        let mut r = MonoResampler::new(44_100, 48_000);
        let mut produced = 0usize;
        let mut fed = 0usize;
        let mut out = [0.0f32; 480];
        for _ in 0..50 {
            let chunk = sine(128, 440.0, 44_100);
            r.push(&chunk);
            fed += chunk.len();
            while r.pop_frame(&mut out) {
                produced += out.len();
            }
        }
        let expect = fed as f64 * 48_000.0 / 44_100.0;
        let diff = (produced as f64 - expect).abs();
        assert!(diff < 960.0, "produced={produced} expect={expect:.0}");
    }

    #[test]
    fn resample_once_should_scale_length() {
        let src = sine(4410, 440.0, 44_100);
        let out = resample_once(&src, 44_100, 48_000);
        assert!((out.len() as i32 - 4800).abs() < 4, "len={}", out.len());
    }

    #[test]
    fn frame_len_10ms_should_match_common_rates() {
        assert_eq!(frame_len_10ms(48_000), 480);
        assert_eq!(frame_len_10ms(44_100), 441);
        assert_eq!(frame_len_10ms(16_000), 160);
    }
}
