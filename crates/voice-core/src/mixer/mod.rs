//! Phase 8：多人混音（Mesh，最多 6 人）。
//! 每路远端流独立增益（B 100% / C 40% / D 80%），mute/deafen 在上层控制。

use std::collections::HashMap;
use voice_common::UserId;

/// 简单线性混音 + 软削波，48k mono f32。
#[derive(Default)]
pub struct Mixer {
    gains: HashMap<u64, f32>,
}

impl Mixer {
    pub fn set_gain(&mut self, user: UserId, gain: f32) {
        self.gains.insert(user.0, gain.clamp(0.0, 2.0));
    }

    pub fn remove(&mut self, user: UserId) {
        self.gains.remove(&user.0);
    }

    /// inputs: (user, pcm)。等长；输出写入 `out`（调用方清零或累加前清零）。
    pub fn mix(&self, inputs: &[(UserId, &[f32])], out: &mut [f32]) {
        out.fill(0.0);
        for (user, pcm) in inputs {
            let g = self.gains.get(&user.0).copied().unwrap_or(1.0);
            let n = pcm.len().min(out.len());
            for i in 0..n {
                out[i] += pcm[i] * g;
            }
        }
        // 软削波防爆音
        for s in out.iter_mut() {
            *s = (*s).clamp(-1.0, 1.0);
        }
    }
}
