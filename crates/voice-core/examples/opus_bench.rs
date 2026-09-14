//! Section 6 benchmark：Opus encode/decode 延迟分布 + 等效 CPU 占用。
//!
//! 信号为类语音调制音（载波 + 慢速 AM/FM，相位连续），避免纯净正弦触发 DTX
//! 导致测到的是舒适噪声帧；务必 `--release` 跑，否则数字无意义：
//!
//! ```sh
//! cargo run -p voice-core --example opus_bench --release -- 20000
//! ```

use std::time::Instant;
use voice_core::codec::{
    LiveDecoder, LiveEncoder, OpusConfig, OpusDecoder, OpusEncoder, FRAME_SAMPLES_10MS_48K,
    MAX_PACKET_BYTES,
};

/// 2 秒相位连续的类语音信号：440Hz 载波 + 二次谐波 + 慢速 AM/FM，幅度恒 >0。
fn speech_like_frames() -> Vec<Vec<f32>> {
    const FRAMES: usize = 200;
    let mut frames = Vec::with_capacity(FRAMES);
    for f in 0..FRAMES {
        let mut frame = Vec::with_capacity(FRAME_SAMPLES_10MS_48K);
        for i in 0..FRAME_SAMPLES_10MS_48K {
            let t = (f * FRAME_SAMPLES_10MS_48K + i) as f32 / 48_000.0;
            let am = 0.55 + 0.45 * (2.0 * std::f32::consts::PI * 2.5 * t).sin();
            let fm = 30.0 * (2.0 * std::f32::consts::PI * 0.7 * t).sin();
            let s = ((2.0 * std::f32::consts::PI * (440.0 + fm) * t).sin()
                + 0.3 * (2.0 * std::f32::consts::PI * 880.0 * t).sin())
                * 0.25
                * am;
            frame.push(s);
        }
        frames.push(frame);
    }
    frames
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    sorted[((p * sorted.len() as f64) as usize).min(sorted.len().saturating_sub(1))]
}

fn report(name: &str, mut nanos: Vec<u64>, frames: usize, target_ms: f64) {
    nanos.sort_unstable();
    let sum: u128 = nanos.iter().map(|&n| n as u128).sum();
    let avg_us = sum as f64 / nanos.len() as f64 / 1000.0;
    let p50_us = percentile(&nanos, 0.50) as f64 / 1000.0;
    let p99_us = percentile(&nanos, 0.99) as f64 / 1000.0;
    let max_us = nanos[nanos.len() - 1] as f64 / 1000.0;
    // 等效 CPU：编解码 frames×10ms 音频所用的墙钟比（越大越省）
    let audio_secs = frames as f64 * 0.01;
    let wall_secs = sum as f64 / 1e9;
    println!(
        "{name}: avg={avg_us:.1}us p50={p50_us:.1}us p99={p99_us:.1}us max={max_us:.1}us  x{:.0}-realtime",
        audio_secs / wall_secs
    );
    if p99_us < target_ms * 1000.0 {
        println!("{name}: HIT target (<{target_ms}ms p99)");
    } else {
        println!("{name}: MISS target (<{target_ms}ms p99)");
    }
}

fn main() -> anyhow::Result<()> {
    println!("libopus: {}", opus::version());
    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20000);

    let mut enc = LiveEncoder::new(OpusConfig::default())?;
    let mut dec = LiveDecoder::new(48_000)?;

    let corpus = speech_like_frames();
    let mut pkt = vec![0u8; MAX_PACKET_BYTES];
    let mut back = vec![0.0f32; FRAME_SAMPLES_10MS_48K];

    // 热身：让分支预测/页缓存/编解码状态稳定
    for frame in corpus.iter().cycle().take(500) {
        let n = enc.encode(frame, &mut pkt)?;
        dec.decode(&pkt[..n], &mut back)?;
    }

    let mut enc_ns = Vec::with_capacity(iters);
    let mut dec_ns = Vec::with_capacity(iters);
    let mut pkt_bytes: u64 = 0;
    for frame in corpus.iter().cycle().take(iters) {
        let t = Instant::now();
        let n = enc.encode(frame, &mut pkt)?;
        enc_ns.push(t.elapsed().as_nanos() as u64);
        pkt_bytes += n as u64;

        let t = Instant::now();
        let m = dec.decode(&pkt[..n], &mut back)?;
        dec_ns.push(t.elapsed().as_nanos() as u64);
        debug_assert_eq!(m, FRAME_SAMPLES_10MS_48K);
    }

    println!(
        "frames={iters} avg_packet={:.1}B",
        pkt_bytes as f64 / iters as f64
    );
    report("encode", enc_ns, iters, 2.0);
    report("decode", dec_ns, iters, 2.0);
    Ok(())
}
