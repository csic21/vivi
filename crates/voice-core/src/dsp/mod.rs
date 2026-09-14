//! Week 8 语音处理链：Mic → NS → VAD → AGC → Opus。
//!
//! - NS：RNNoise（纯 Rust `nnnoiseless`，10ms/48k 帧天然匹配），同时给出语音概率；
//! - VAD：能量 + hangover + RNNoise 概率融合，驱动 UI 🟢 与 speaking 标志；
//! - AGC：慢自适应增益，稳定不同麦克风的音量；
//! - AEC：有意缺席——真回声消除需要 far-end 参考 + 严格延迟对齐（APM 级集成是
//!   独立工作流）；Week 8 以头戴式耳机 + PTT 为主场景，扬声器外放场景后续接 APM。
//!
//! VAD 刻意跑在 AGC **之前**（NS 之后）：避免 AGC 把本底噪声抬进门限造成常亮。

/// 10ms @ 48kHz mono 语音帧。
pub const FRAME_LEN: usize = 480;

/// VAD 判决：是否有人声（驱动 UI 🟢）。
pub trait Vad {
    fn is_speech(&mut self, pcm_48k_mono_10ms: &[f32]) -> bool;
}

/// 噪声抑制：原地处理。
pub trait NoiseSuppressor {
    fn process(&mut self, pcm: &mut [f32]);
}

/// 回声消除：需要 far-end（扬声器）参考信号。
/// 当前无实现（见模块文档）；接口为 APM 接入预留。
pub trait EchoCanceller {
    fn process(&mut self, near_end: &mut [f32], far_end: &[f32]);
}

/// 自动增益：稳定麦克风音量。
pub trait AutoGain {
    fn process(&mut self, pcm: &mut [f32]);
}

/// 能量 VAD：RMS 双门限 + 开口确认 + hangover。
/// - 开口需连续 `attack_need` 帧超 attack 门限（默认 3 帧 = 30ms），
///   单个敲击/风扇爆音触不亮灯，真人开口延迟无感；
/// - 低于 release 门限后靠 hangover 续 80ms，短停顿不闪断。
pub struct EnergyVad {
    attack_rms: f32,
    release_rms: f32,
    hangover_frames: u32,
    attack_need: u32,
    attack_left: u32,
    hangover_left: u32,
    speaking: bool,
}

impl EnergyVad {
    pub fn new(attack_rms: f32, release_rms: f32, hangover_frames: u32) -> Self {
        Self {
            attack_rms,
            release_rms,
            hangover_frames,
            attack_need: 3,
            attack_left: 3,
            hangover_left: 0,
            speaking: false,
        }
    }
}

impl Default for EnergyVad {
    /// 经验值：0.5 幅正弦 RMS≈0.35 说话，本底噪声 RMS 通常 <0.01。
    fn default() -> Self {
        Self::new(0.02, 0.012, 8)
    }
}

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

impl Vad for EnergyVad {
    fn is_speech(&mut self, pcm: &[f32]) -> bool {
        let e = rms(pcm);
        if e >= self.attack_rms {
            if self.attack_left > 0 {
                self.attack_left -= 1;
            }
            if self.attack_left == 0 {
                self.speaking = true;
                self.hangover_left = self.hangover_frames;
            }
        } else {
            self.attack_left = self.attack_need;
            if e < self.release_rms {
                if self.hangover_left > 0 {
                    self.hangover_left -= 1;
                } else {
                    self.speaking = false;
                }
            }
        }
        // 灰区（release..attack）：保持上次状态，天然迟滞
        self.speaking
    }
}

/// RNNoise 降噪（`nnnoiseless`，纯 Rust）。顺带输出语音概率供 VAD 融合。
///
/// 注意：nnnoiseless 要求输入是 **16-bit 量程**（±32768，不是 ±1.0），
/// 包装层负责放大/归一，调用方始终用 ±1.0。
pub struct RnNoise {
    inner: Box<nnnoiseless::DenoiseState<'static>>,
    scratch: [f32; FRAME_LEN],
    last_voice_prob: f32,
}

impl Default for RnNoise {
    fn default() -> Self {
        Self::new()
    }
}

impl RnNoise {
    pub fn new() -> Self {
        Self {
            inner: nnnoiseless::DenoiseState::new(),
            scratch: [0.0; FRAME_LEN],
            last_voice_prob: 0.0,
        }
    }

    /// 上一帧语音概率（0.0~1.0）。
    pub fn voice_prob(&self) -> f32 {
        self.last_voice_prob
    }
}

impl NoiseSuppressor for RnNoise {
    fn process(&mut self, pcm: &mut [f32]) {
        // chunks_exact_mut 保证每块恰好 FRAME_LEN，无需长度断言
        for chunk in pcm.chunks_exact_mut(FRAME_LEN) {
            for (s, d) in chunk.iter().zip(self.scratch.iter_mut()) {
                *d = s * 32768.0;
            }
            self.last_voice_prob = self.inner.process_frame(chunk, &self.scratch);
            for s in chunk.iter_mut() {
                *s = (*s / 32768.0).clamp(-1.0, 1.0);
            }
        }
    }
}

/// 慢自适应 AGC：向目标 RMS 靠拢；突发大声快压（attack），小声慢提（release），
/// 避免抽吸效应（pumping）。增益限幅防爆。
pub struct SimpleAgc {
    target_rms: f32,
    min_gain: f32,
    max_gain: f32,
    attack: f32,
    release: f32,
    gain: f32,
}

impl SimpleAgc {
    pub fn new(target_rms: f32) -> Self {
        Self {
            target_rms,
            min_gain: 0.25,
            max_gain: 6.0,
            attack: 0.3,
            release: 0.02,
            gain: 1.0,
        }
    }

    pub fn gain(&self) -> f32 {
        self.gain
    }
}

impl Default for SimpleAgc {
    fn default() -> Self {
        Self::new(0.12)
    }
}

impl AutoGain for SimpleAgc {
    fn process(&mut self, pcm: &mut [f32]) {
        let e = rms(pcm).max(1e-4);
        let desired = (self.target_rms / e).clamp(self.min_gain, self.max_gain);
        // 大声快压、小声慢提
        let rate = if desired < self.gain {
            self.attack
        } else {
            self.release
        };
        self.gain += (desired - self.gain) * rate;
        for s in pcm.iter_mut() {
            *s = (*s * self.gain).clamp(-1.0, 1.0);
        }
    }
}

/// 单帧处理结论。
#[derive(Debug, Clone, Copy)]
pub struct FrameDecision {
    /// 是否有人声（UI 🟢 / speaking 标志）。
    pub speech: bool,
    /// RNNoise 语音概率（无 NS 时为 0.0）。
    pub voice_prob: f32,
}

/// 上行处理链：手动增益 → NS → VAD → AGC，开关均可运行时切换。
pub struct DspChain {
    ns: Option<RnNoise>,
    ns_enabled: bool,
    vad: EnergyVad,
    agc: SimpleAgc,
    agc_enabled: bool,
    /// 手动麦克风增益（NS 之前的前置放大；AGC 量程有限，推子依然有效）。
    input_gain: f32,
    /// 融合门限：NS 开启时要求 voice_prob 超过它才算说话。
    pub vad_prob_threshold: f32,
}

impl DspChain {
    pub fn new(ns_enabled: bool) -> Self {
        Self {
            ns: Some(RnNoise::new()),
            ns_enabled,
            vad: EnergyVad::default(),
            agc: SimpleAgc::default(),
            agc_enabled: true,
            input_gain: 1.0,
            vad_prob_threshold: 0.25,
        }
    }

    /// 手动麦克风增益（0.0~4.0，1.0 = 100%）。
    pub fn set_input_gain(&mut self, gain: f32) {
        self.input_gain = gain.clamp(0.0, 4.0);
    }

    pub fn set_ns_enabled(&mut self, enabled: bool) {
        self.ns_enabled = enabled;
    }

    pub fn set_agc_enabled(&mut self, enabled: bool) {
        self.agc_enabled = enabled;
    }

    pub fn process(&mut self, frame: &mut [f32; FRAME_LEN]) -> FrameDecision {
        if self.input_gain != 1.0 {
            for s in frame.iter_mut() {
                *s *= self.input_gain;
            }
        }
        if self.ns_enabled {
            if let Some(ns) = self.ns.as_mut() {
                ns.process(frame);
            }
        }
        let energy_speech = self.vad.is_speech(frame);
        let prob = self
            .ns
            .as_ref()
            .filter(|_| self.ns_enabled)
            .map(|n| n.voice_prob())
            .unwrap_or(1.0);
        let speech = energy_speech && prob >= self.vad_prob_threshold.clamp(0.0, 1.0);
        if self.agc_enabled {
            self.agc.process(frame);
        }
        FrameDecision {
            speech,
            voice_prob: prob,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_frame(amp: f32) -> [f32; FRAME_LEN] {
        let mut f = [0.0; FRAME_LEN];
        for (i, s) in f.iter_mut().enumerate() {
            *s = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin() * amp;
        }
        f
    }

    /// 确定性 PRNG（xorshift64*），测 NS 用。
    fn noise_frame(amp: f32, seed: u64) -> [f32; FRAME_LEN] {
        let mut st = seed.max(1);
        let mut f = [0.0; FRAME_LEN];
        for s in f.iter_mut() {
            st ^= st >> 12;
            st ^= st << 25;
            st ^= st >> 27;
            let u = (st.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f32 / (1u64 << 53) as f32;
            *s = (u * 2.0 - 1.0) * amp;
        }
        f
    }

    #[test]
    fn vad_should_detect_tone_and_hangover() {
        let mut vad = EnergyVad::default();
        // 开口确认：连续 3 帧超门限才亮
        assert!(!vad.is_speech(&tone_frame(0.5)));
        assert!(!vad.is_speech(&tone_frame(0.5)));
        assert!(vad.is_speech(&tone_frame(0.5)));
        // 静音后 hangover 期内仍判说话，之后恢复
        for _ in 0..8 {
            assert!(vad.is_speech(&[0.0; FRAME_LEN]));
        }
        assert!(!vad.is_speech(&[0.0; FRAME_LEN]));
    }

    #[test]
    fn vad_should_ignore_single_loud_spike() {
        let mut vad = EnergyVad::default();
        // 单帧爆音（敲击/气流声）不应点亮
        assert!(!vad.is_speech(&tone_frame(0.5)));
        assert!(!vad.is_speech(&[0.0; FRAME_LEN]));
    }

    #[test]
    fn vad_should_stay_silent_on_low_noise() {
        let mut vad = EnergyVad::default();
        assert!(!vad.is_speech(&noise_frame(0.005, 7)));
    }

    #[test]
    fn agc_should_boost_quiet_and_tame_loud() {
        // 注意：每次用新帧（真实麦克风行为）；同 buffer 循环是闭环，测的是别的特性
        let mut agc = SimpleAgc::default();
        let mut out_rms = 0.0;
        for _ in 0..200 {
            let mut f = tone_frame(0.02);
            agc.process(&mut f);
            out_rms = rms(&f);
        }
        assert!(agc.gain() > 1.5, "gain = {}", agc.gain());
        assert!(
            (out_rms - 0.12).abs() < 0.04,
            "output should converge to target, got {out_rms}"
        );
        for _ in 0..200 {
            let mut f = tone_frame(0.5);
            agc.process(&mut f);
            out_rms = rms(&f);
        }
        assert!(agc.gain() < 1.0, "gain = {}", agc.gain());
        assert!(
            (out_rms - 0.12).abs() < 0.05,
            "output should converge to target, got {out_rms}"
        );
    }

    #[test]
    fn rnnoise_should_suppress_stationary_noise() {
        let mut ns = RnNoise::new();
        // 先喂几帧让状态稳定
        for _ in 0..5 {
            let mut w = noise_frame(0.1, 42);
            ns.process(&mut w);
        }
        let input = noise_frame(0.1, 99);
        let in_rms = rms(&input);
        let mut out = input;
        ns.process(&mut out);
        let out_rms = rms(&out);
        assert!(out_rms < in_rms * 0.9, "in={in_rms} out={out_rms}");
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn chain_should_flag_speech_on_tone() {
        let mut chain = DspChain::new(true);
        let mut decision = FrameDecision {
            speech: false,
            voice_prob: 0.0,
        };
        // 前几帧是 RNNoise/AGC 启动瞬态，多跑几帧再断言
        for _ in 0..10 {
            let mut f = tone_frame(0.5);
            decision = chain.process(&mut f);
            assert!(f.iter().all(|s| s.is_finite()));
        }
        assert!(decision.speech, "prob={}", decision.voice_prob);
    }

    #[test]
    fn chain_switches_should_take_effect() {
        let mut chain = DspChain::new(true);
        chain.set_ns_enabled(false);
        chain.set_agc_enabled(false);
        chain.set_input_gain(2.0);
        // 手动增益 2x：输出能量应约为输入 4 倍（先喂几帧过 VAD 状态）
        let mut e = 0.0;
        for _ in 0..5 {
            let mut f = tone_frame(0.1);
            chain.process(&mut f);
            e = rms(&f);
        }
        let plain = rms(&tone_frame(0.1));
        assert!((e / plain - 2.0).abs() < 0.3, "e={e} plain={plain}");
        // AGC 关着，200% 推子不应被压回去
        chain.set_input_gain(0.5);
        for _ in 0..30 {
            let mut f = tone_frame(0.1);
            chain.process(&mut f);
            e = rms(&f);
        }
        assert!((e / plain - 0.5).abs() < 0.15, "e={e} plain={plain}");
    }

    #[test]
    fn chain_should_stay_silent_on_quiet_noise() {
        let mut chain = DspChain::new(true);
        let mut decision = FrameDecision {
            speech: true,
            voice_prob: 1.0,
        };
        for i in 0..10 {
            let mut f = noise_frame(0.005, 100 + i);
            decision = chain.process(&mut f);
        }
        assert!(!decision.speech);
    }
}
