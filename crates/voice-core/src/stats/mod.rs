//! Phase 3/8：RTT / Loss / Jitter 估计（RFC3550 风格）。
//! UI 右上角 PING/JITTER/LOSS 数据源；`udp_talk` 联调时直接打印这些值。

/// RTT 平滑（EWMA α=1/8，TCP 风格）：`srtt = 0.875*srtt + 0.125*sample`。
#[derive(Debug, Clone, Default)]
pub struct RttEstimator {
    srtt_ms: Option<f32>,
}

impl RttEstimator {
    pub fn observe(&mut self, sample_ms: f32) {
        self.srtt_ms = Some(match self.srtt_ms {
            None => sample_ms,
            Some(s) => 0.875 * s + 0.125 * sample_ms,
        });
    }

    pub fn srtt_ms(&self) -> Option<f32> {
        self.srtt_ms
    }
}

/// RFC3550 到达间隔抖动：`J += (|D| - J) / 16`，
/// `D = (Rj - Ri) - (Sj - Si)`，R 为本地到达钟，S 为对端发送时间戳。
///
/// 调用方算好 `transit_delta_ms`（D）再传进来：
/// 到达差用本地单调钟、发送差用 u64 包时间戳先相减（避免 f32 精度灾难），
/// 本结构只做纯数学，因此可单测、无需伪造时钟。
#[derive(Debug, Clone, Default)]
pub struct JitterEstimator {
    jitter_ms: f64,
}

impl JitterEstimator {
    pub fn observe(&mut self, transit_delta_ms: f64) {
        let d = transit_delta_ms.abs();
        self.jitter_ms += (d - self.jitter_ms) / 16.0;
    }

    pub fn jitter_ms(&self) -> f32 {
        self.jitter_ms as f32
    }
}

/// 按 sequence 的丢包跟踪：处理乱序 / 迟到 / 回绕。
///
/// - 前进跳变：中间空洞计入 expected（单次封顶 [`SeqTracker::MAX_GAP`]，防野 sequence 污染统计）；
/// - 迟到旧包：视为补洞，计入 received（UDP 无重传，重复包极少）；
/// - 原地重复（seq == highest）：计 duplicate，不影响收发计数；
/// - 发送端重启（seq 归零）会被当作迟到包：loss 短暂偏低，可接受（后续靠 session 重置解决）。
#[derive(Debug, Clone, Default)]
pub struct SeqTracker {
    baseline: Option<u32>,
    highest: u32,
    received: u64,
    expected: u64,
    duplicates: u64,
}

impl SeqTracker {
    /// 单次跳变最多补这么多 expected（防损坏 sequence 污染）。
    pub const MAX_GAP: u32 = 1000;

    /// 返回该包是否计为有效接收（重复包返回 false）。
    pub fn on_packet(&mut self, seq: u32) -> bool {
        match self.baseline {
            None => {
                self.baseline = Some(seq);
                self.highest = seq;
                self.received = 1;
                self.expected = 1;
                true
            }
            Some(_) => {
                let diff = seq.wrapping_sub(self.highest) as i32;
                if diff > 0 {
                    self.expected += (diff as u32).min(Self::MAX_GAP) as u64;
                    self.received += 1;
                    self.highest = seq;
                    true
                } else if diff == 0 {
                    self.duplicates += 1;
                    false
                } else {
                    self.received += 1;
                    true
                }
            }
        }
    }

    pub fn duplicates(&self) -> u64 {
        self.duplicates
    }

    pub fn loss_percent(&self) -> f32 {
        if self.expected == 0 {
            return 0.0;
        }
        let loss = 100.0 * (1.0 - self.received as f32 / self.expected as f32);
        loss.clamp(0.0, 100.0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct LossTracker {
    pub expected: u64,
    pub received: u64,
}

impl LossTracker {
    pub fn on_receive(&mut self, received: bool) {
        self.expected += 1;
        if received {
            self.received += 1;
        }
    }

    pub fn loss_percent(&self) -> f32 {
        if self.expected == 0 {
            return 0.0;
        }
        100.0 * (1.0 - self.received as f32 / self.expected as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtt_should_start_at_first_sample() {
        let mut r = RttEstimator::default();
        r.observe(100.0);
        assert_eq!(r.srtt_ms(), Some(100.0));
    }

    #[test]
    fn jitter_should_stay_near_zero_on_steady_stream() {
        let mut j = JitterEstimator::default();
        for _ in 0..100 {
            j.observe(0.5);
        }
        assert!(j.jitter_ms() < 1.0);
    }

    #[test]
    fn jitter_should_spike_on_delay_jump() {
        let mut j = JitterEstimator::default();
        for _ in 0..10 {
            j.observe(0.0);
        }
        j.observe(50.0);
        assert!(j.jitter_ms() > 2.0 && j.jitter_ms() < 5.0);
    }

    #[test]
    fn seq_tracker_should_count_gap_as_loss() {
        let mut t = SeqTracker::default();
        for s in [1u32, 2, 5] {
            t.on_packet(s);
        }
        assert!((t.loss_percent() - 40.0).abs() < 0.001);
    }

    #[test]
    fn seq_tracker_should_fill_gap_on_late_packet() {
        let mut t = SeqTracker::default();
        for s in [1u32, 2, 5, 3, 4] {
            t.on_packet(s);
        }
        assert_eq!(t.loss_percent(), 0.0);
    }

    #[test]
    fn seq_tracker_should_ignore_exact_duplicate() {
        let mut t = SeqTracker::default();
        assert!(t.on_packet(1));
        assert!(t.on_packet(2));
        assert!(!t.on_packet(2));
        assert_eq!(t.duplicates(), 1);
        assert_eq!(t.loss_percent(), 0.0);
    }
}
