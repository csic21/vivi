//! Task 2 / Task 4 验证：只采集不播放（无啸叫风险），确认有采样流入 ring。
//!
//! drain 线程模拟 Opus / 网络消费端，健康状态下 dropped 应为 0。
//!
//! ```sh
//! cargo run -p voice-core --example capture_probe
//! ```

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use ringbuf::traits::Consumer;
use voice_core::audio::{start_capture, CaptureConfig};

fn main() -> anyhow::Result<()> {
    let (capture, mut consumer) = start_capture(None, CaptureConfig::default())?;
    eprintln!(
        "capturing 2s @ {}Hz ch={} ...",
        capture.sample_rate, capture.channels
    );

    // 模拟下游消费（Opus/网络线程）：不停 drain，避免 ring 被动填满。
    let stop = Arc::new(AtomicBool::new(false));
    let drained = Arc::new(AtomicU64::new(0));
    let worker = std::thread::spawn({
        let stop = Arc::clone(&stop);
        let drained = Arc::clone(&drained);
        move || {
            while !stop.load(Ordering::Relaxed) {
                let mut n = 0;
                while consumer.try_pop().is_some() {
                    n += 1;
                }
                if n > 0 {
                    drained.fetch_add(n, Ordering::Relaxed);
                } else {
                    std::thread::yield_now();
                }
            }
            // 退出前把剩余采样也取完
            while consumer.try_pop().is_some() {
                drained.fetch_add(1, Ordering::Relaxed);
            }
        }
    });

    std::thread::sleep(Duration::from_secs(2));
    stop.store(true, Ordering::Relaxed);
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("drain thread panicked"))?;

    let pushed = capture.stats.pushed();
    eprintln!(
        "pushed={} drained={} dropped={} stream_errors={}",
        pushed,
        drained.load(Ordering::Relaxed),
        capture.stats.dropped(),
        capture.stats.stream_errors()
    );
    anyhow::ensure!(pushed > 0, "no samples captured; check mic permission");
    Ok(())
}
