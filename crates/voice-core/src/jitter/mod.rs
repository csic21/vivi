//! Phase 4：自适应 Jitter Buffer（乱序重排 / 迟到丢弃 / 丢包检测 / 自适应目标延迟）。
//!
//! 拉模型：解码 pump 每 `frame_ms` tick 一次 [`AdaptiveJitterBuffer::pop`]。
//! - 首包到达后按 `initial_delay_ms`（默认 20ms）保持再吐帧，形成初始缓冲；
//! - `pop()` 返回 `Frame`（按序）/ `Lost`（空洞，调用方 PLC）/ `Empty`（未就绪，不计丢包）；
//! - 目标延迟按 RFC3550 jitter 自适应：`target = clamp(min + gain*jitter, min, max)`，
//!   好网 10~20ms，差网 30~60ms+（Section 8）；
//! - 缓冲超 `max_buffered_ms` 丢最老（抗收发时钟漂移 / 突发堆积）；
//! - seq 大跳变（> `resync_threshold_frames`，如静音后恢复 / 发送端重启）重同步。
//!
//! Opus in-band FEC 已在编码侧打开（Week 2）；解码侧 LBRR 恢复需要前瞻包，
//! 留到后续阶段（当前丢包一律 PLC，见 `LiveDecoder`）。

use std::collections::{btree_map::Entry, BTreeMap};
use std::time::Instant;

use super::stats::JitterEstimator;

pub const DEFAULT_TARGET_DELAY_MS: u64 = 20;

#[derive(Debug, Clone, Copy)]
pub struct JitterConfig {
    /// 每帧毫秒数（Opus 语音帧固定 10ms）。
    pub frame_ms: u64,
    /// 首包到达后的保持时间（形成初始缓冲）。
    pub initial_delay_ms: u64,
    /// 自适应目标下限（好网 → 10ms 级）。
    pub min_delay_ms: f64,
    /// 自适应目标上限。
    pub max_delay_ms: f64,
    /// `target = min + gain * jitter_ms` 的增益。
    pub jitter_gain: f64,
    /// 缓冲上限（超了丢最老，抗漂移）。
    pub max_buffered_ms: u64,
    /// 超过多少帧没见过的 seq 算新话轮，直接重同步。
    pub resync_threshold_frames: u32,
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            frame_ms: 10,
            initial_delay_ms: DEFAULT_TARGET_DELAY_MS,
            min_delay_ms: 10.0,
            max_delay_ms: 120.0,
            jitter_gain: 3.0,
            max_buffered_ms: 200,
            resync_threshold_frames: 100,
        }
    }
}

/// `pop()` 的一次吐帧结果。
#[derive(Debug)]
pub enum Plucked<T> {
    /// 按序帧。
    Frame(T),
    /// 空洞：该 seq 的包没赶上，调用方应对此 seq 做 PLC。
    Lost { sequence: u32 },
    /// 尚未就绪（首包保持期）：播静音，不计丢包。
    Empty,
}

/// 快照统计（联调打印 / 后续 UI 用）。
#[derive(Debug, Clone, Copy, Default)]
pub struct JitterStats {
    pub late_dropped: u64,
    pub lost: u64,
    pub duplicates: u64,
    pub resyncs: u64,
    pub overflow_dropped: u64,
    pub target_delay_ms: f64,
    pub buffered_ms: u64,
    pub jitter_ms: f32,
}

pub struct AdaptiveJitterBuffer<T> {
    config: JitterConfig,
    jitter: JitterEstimator,
    expected: Option<u32>,
    ready_at: Option<Instant>,
    buf: BTreeMap<u32, T>,
    last_sender_ts: Option<u64>,
    last_arrival: Option<Instant>,
    target_delay_ms: f64,
    late_dropped: u64,
    lost: u64,
    duplicates: u64,
    resyncs: u64,
    overflow_dropped: u64,
}

impl<T> AdaptiveJitterBuffer<T> {
    pub fn new(config: JitterConfig) -> Self {
        Self {
            target_delay_ms: config.min_delay_ms,
            config,
            jitter: JitterEstimator::default(),
            expected: None,
            ready_at: None,
            buf: BTreeMap::new(),
            last_sender_ts: None,
            last_arrival: None,
            late_dropped: 0,
            lost: 0,
            duplicates: 0,
            resyncs: 0,
            overflow_dropped: 0,
        }
    }

    /// 到包（收包线程调用）。`sender_ts_ms` 取包内发送时间戳。
    pub fn push(&mut self, seq: u32, sender_ts_ms: u64, pkt: T) {
        let now = Instant::now();
        // 新话轮（首包/seq 大跳变）的包不参与 jitter 观测：
        // 静音间隙的发送时间戳差毫无意义，会污染估计。
        let is_new_spurt = match self.expected {
            None => true,
            Some(e) => seq.wrapping_sub(e) as i32 > self.config.resync_threshold_frames as i32,
        };
        if !is_new_spurt {
            if let (Some(prev_ts), Some(prev_now)) = (self.last_sender_ts, self.last_arrival) {
                let arrival_ms = now.duration_since(prev_now).as_secs_f64() * 1000.0;
                let sender_ms = sender_ts_ms.wrapping_sub(prev_ts) as i64 as f64;
                self.jitter.observe(arrival_ms - sender_ms);
                self.adapt_target();
            }
        }
        self.last_sender_ts = Some(sender_ts_ms);
        self.last_arrival = Some(now);

        match self.expected {
            None => {
                // 首包：定锚 + 初始保持，形成第一段缓冲。
                self.expected = Some(seq);
                self.ready_at = Some(now + self.initial_hold());
                self.buf.insert(seq, pkt);
            }
            Some(e) => {
                let diff = seq.wrapping_sub(e) as i32;
                if diff < 0 {
                    self.late_dropped += 1;
                } else if diff > self.config.resync_threshold_frames as i32 {
                    // 新话轮（静音恢复/发送端重启）：清旧缓冲重同步。
                    self.overflow_dropped += self.buf.len() as u64;
                    self.buf.clear();
                    self.resyncs += 1;
                    self.expected = Some(seq);
                    self.ready_at = Some(now + self.resync_hold());
                    self.buf.insert(seq, pkt);
                } else {
                    let inserted = match self.buf.entry(seq) {
                        Entry::Occupied(_) => {
                            self.duplicates += 1;
                            false
                        }
                        Entry::Vacant(slot) => {
                            slot.insert(pkt);
                            true
                        }
                    };
                    if inserted {
                        self.enforce_cap();
                    }
                }
            }
        }
    }

    /// 每 `frame_ms` tick 一次（解码 pump 调用）。
    pub fn pop(&mut self) -> Plucked<T> {
        let e = match self.expected {
            None => return Plucked::Empty,
            Some(e) => e,
        };
        if let Some(deadline) = self.ready_at {
            if Instant::now() < deadline {
                return Plucked::Empty;
            }
            self.ready_at = None;
        }
        match self.buf.remove(&e) {
            Some(pkt) => {
                self.expected = Some(e.wrapping_add(1));
                Plucked::Frame(pkt)
            }
            None => {
                self.expected = Some(e.wrapping_add(1));
                self.lost += 1;
                Plucked::Lost { sequence: e }
            }
        }
    }

    /// 当前自适应目标延迟。
    pub fn target_delay_ms(&self) -> f64 {
        self.target_delay_ms
    }

    /// 当前缓冲时长。
    pub fn buffered_ms(&self) -> u64 {
        self.buf.len() as u64 * self.config.frame_ms
    }

    pub fn stats(&self) -> JitterStats {
        JitterStats {
            late_dropped: self.late_dropped,
            lost: self.lost,
            duplicates: self.duplicates,
            resyncs: self.resyncs,
            overflow_dropped: self.overflow_dropped,
            target_delay_ms: self.target_delay_ms,
            buffered_ms: self.buffered_ms(),
            jitter_ms: self.jitter.jitter_ms(),
        }
    }

    fn adapt_target(&mut self) {
        let t = self.config.min_delay_ms + self.config.jitter_gain * self.jitter.jitter_ms() as f64;
        self.target_delay_ms = t.clamp(self.config.min_delay_ms, self.config.max_delay_ms);
    }

    fn initial_hold(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.config.initial_delay_ms)
    }

    fn resync_hold(&self) -> std::time::Duration {
        // 恢复后按当前自适应目标重建缓冲（静音恢复的短暂代价）。
        std::time::Duration::from_millis(self.target_delay_ms as u64)
    }

    fn enforce_cap(&mut self) {
        while self.buffered_ms() > self.config.max_buffered_ms {
            if self.buf.pop_first().is_none() {
                break;
            }
            self.overflow_dropped += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> JitterConfig {
        JitterConfig {
            initial_delay_ms: 0,
            ..JitterConfig::default()
        }
    }

    #[test]
    fn reorder_should_play_in_sequence() {
        let mut jb = AdaptiveJitterBuffer::new(test_config());
        jb.push(1, 0, "a");
        jb.push(3, 20, "c");
        jb.push(2, 10, "b");
        assert!(matches!(jb.pop(), Plucked::Frame("a")));
        assert!(matches!(jb.pop(), Plucked::Frame("b")));
        assert!(matches!(jb.pop(), Plucked::Frame("c")));
    }

    #[test]
    fn late_packet_should_drop_and_advance() {
        let mut jb = AdaptiveJitterBuffer::new(test_config());
        jb.push(1, 0, "a");
        assert!(matches!(jb.pop(), Plucked::Frame("a")));
        jb.push(1, 30, "late");
        assert_eq!(jb.stats().late_dropped, 1);
        assert!(matches!(jb.pop(), Plucked::Lost { sequence: 2 }));
        assert_eq!(jb.stats().lost, 1);
    }

    #[test]
    fn initial_hold_should_gate_first_frames() {
        let mut jb = AdaptiveJitterBuffer::new(JitterConfig::default());
        jb.push(1, 0, "a");
        assert!(matches!(jb.pop(), Plucked::Empty));
        assert_eq!(jb.stats().lost, 0);
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert!(matches!(jb.pop(), Plucked::Frame("a")));
    }

    #[test]
    fn resync_should_jump_on_large_gap() {
        let mut jb = AdaptiveJitterBuffer::new(test_config());
        jb.push(1, 0, "a");
        jb.push(2, 10, "b");
        assert!(matches!(jb.pop(), Plucked::Frame("a")));
        assert!(matches!(jb.pop(), Plucked::Frame("b")));
        jb.push(500, 5000, "new");
        // 重同步带保持（目标 10ms）：立即 pop 为 Empty，随后吐新帧
        assert!(matches!(jb.pop(), Plucked::Empty));
        assert_eq!(jb.stats().resyncs, 1);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(matches!(jb.pop(), Plucked::Frame("new")));
    }

    #[test]
    fn duplicate_should_count_and_keep_first() {
        let mut jb = AdaptiveJitterBuffer::new(test_config());
        jb.push(7, 0, "x");
        jb.push(7, 10, "y");
        assert_eq!(jb.stats().duplicates, 1);
        assert!(matches!(jb.pop(), Plucked::Frame("x")));
    }

    #[test]
    fn overflow_should_bound_buffer() {
        let mut jb = AdaptiveJitterBuffer::new(JitterConfig {
            initial_delay_ms: 0,
            max_buffered_ms: 30,
            ..JitterConfig::default()
        });
        for s in 10u32..21 {
            jb.push(s, (s as u64) * 10, s);
        }
        let st = jb.stats();
        assert_eq!(st.overflow_dropped, 8);
        assert_eq!(st.buffered_ms, 30);
        // expected 仍是 10：先吐 8 个 Lost 推进到 18，再拿到缓冲帧
        for e in 10u32..18 {
            assert!(matches!(jb.pop(), Plucked::Lost { sequence } if sequence == e));
        }
        assert!(matches!(jb.pop(), Plucked::Frame(18)));
    }

    #[test]
    fn target_should_stay_low_on_steady_stream() {
        let mut jb = AdaptiveJitterBuffer::new(test_config());
        for i in 0u32..40 {
            jb.push(i, 1000, i);
        }
        assert!(jb.target_delay_ms() < 15.0);
    }

    #[test]
    fn target_should_grow_with_jitter() {
        let mut jb = AdaptiveJitterBuffer::new(test_config());
        for i in 0u32..40 {
            // 发送时间戳来回跳 100ms，模拟剧烈抖动
            jb.push(i, if i % 2 == 0 { 0 } else { 100 }, i);
        }
        assert!(jb.target_delay_ms() > 60.0);
    }
}
