//! Task 3：本机 loopback（Mic -> Capture -> Playback -> Speaker）。
//!
//! ⚠ 请佩戴耳机后再运行，否则麦克风会拾取扬声器形成啸叫。
//!
//! ```sh
//! cargo run -p voice-core --example loopback -- 5
//! ```

use std::time::Duration;
use voice_core::audio::{start_capture, start_playback, CaptureConfig};

fn main() -> anyhow::Result<()> {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    eprintln!("loopback for {secs}s — wear headphones, speak now");

    let (capture, consumer) = start_capture(None, CaptureConfig::default())?;
    eprintln!("capture: {}Hz ch={}", capture.sample_rate, capture.channels);
    let playback = start_playback(None, consumer, capture.sample_rate, capture.channels)?;

    std::thread::sleep(Duration::from_secs(secs));

    eprintln!(
        "capture: pushed={} dropped={} stream_errors={}",
        capture.stats.pushed(),
        capture.stats.dropped(),
        capture.stats.stream_errors()
    );
    eprintln!(
        "playback: played={} underruns={} stream_errors={}",
        playback.stats.played(),
        playback.stats.underruns(),
        playback.stats.stream_errors()
    );
    Ok(())
}
