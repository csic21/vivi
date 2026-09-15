//! Week 3/4 双机对讲：Mic→Opus→UDP→JB→Opus→Spk，全双工。
//!
//! 同机联调（开两个终端）：
//! ```sh
//! cargo run -p voice-core --example udp_talk -- --bind 127.0.0.1:5001 --peer 127.0.0.1:5002 --id 1 --secs 15
//! cargo run -p voice-core --example udp_talk -- --bind 127.0.0.1:5002 --peer 127.0.0.1:5001 --id 2 --secs 15
//! ```
//! 双机：`--bind 0.0.0.0:5001`，`--peer` 填对面 IP:端口（同一局域网，先关防火墙或放行 UDP）。
//!
//! `--no-audio`：合成正弦当麦克风、不播放（无头验证 / 不打扰周围人）。
//! `--drop-pct N`：收包侧模拟 N% 随机丢包，验证 JB + PLC（Week 4 联调用）。
//! ⚠ 真实音频模式请佩戴耳机。

use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use ringbuf::traits::{Consumer, Producer};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use voice_core::audio::{
    capture_ring, frame_len_10ms, start_capture, start_playback, CaptureConfig, MonoResampler,
};
use voice_core::codec::{
    LiveDecoder, LiveEncoder, OpusConfig, OpusDecoder, OpusEncoder, FRAME_SAMPLES_10MS_48K,
    MAX_PACKET_BYTES,
};
use voice_core::jitter::{AdaptiveJitterBuffer, JitterConfig, JitterStats, Plucked};
use voice_core::net::{ReceivedVoice, UdpVoiceLink};

struct Args {
    bind: SocketAddr,
    peer: SocketAddr,
    id: u64,
    secs: u64,
    no_audio: bool,
    drop_pct: u8,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut bind = None;
    let mut peer = None;
    let mut id = None;
    let mut secs = 0u64;
    let mut no_audio = false;
    let mut drop_pct = 0u8;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bind" => bind = it.next().and_then(|s| s.parse().ok()),
            "--peer" => peer = it.next().and_then(|s| s.parse().ok()),
            "--id" => id = it.next().and_then(|s| s.parse().ok()),
            "--secs" => secs = it.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            "--no-audio" => no_audio = true,
            "--drop-pct" => drop_pct = it.next().and_then(|s| s.parse().ok()).unwrap_or(0).min(100),
            _ => {}
        }
    }
    Ok(Args {
        bind: bind.ok_or_else(|| anyhow::anyhow!("--bind ADDR required"))?,
        peer: peer.ok_or_else(|| anyhow::anyhow!("--peer ADDR required"))?,
        id: id.ok_or_else(|| anyhow::anyhow!("--id N required"))?,
        secs,
        no_audio,
        drop_pct,
    })
}

#[derive(Default)]
struct Counters {
    /// mic pump 因 net 通道满而丢的已编码包
    pump_dropped: AtomicU64,
    /// 解码后因播放 ring 满而丢的采样
    play_dropped: AtomicU64,
    /// 正常解出的帧数
    decoded: AtomicU64,
    /// PLC 补出的帧数（JB 空洞）
    plc_frames: AtomicU64,
    /// --drop-pct 模拟丢弃的包数
    sim_dropped: AtomicU64,
}

/// 确定性 PRNG（xorshift64*）丢包模拟，无外部依赖。
fn should_drop(state: &mut u64, pct: u8) -> bool {
    if pct == 0 {
        return false;
    }
    if pct >= 100 {
        return true;
    }
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (*state % 100) < pct as u64
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let link = UdpVoiceLink::bind(args.bind, args.id).await?;
    link.set_peer(args.peer);
    eprintln!("id={} bind={} peer={}", args.id, args.bind, args.peer);

    let counters = Arc::new(Counters::default());
    // pump 线程 -> net 发送任务（编码包）；收包循环 -> decode 任务（语音帧）
    let (pkt_tx, mut pkt_rx) = mpsc::channel::<Vec<u8>>(64);
    let (voice_tx, mut voice_rx) = mpsc::channel::<ReceivedVoice>(64);

    let stop = Arc::new(AtomicBool::new(false));

    // ---- 上行 pump（独立线程：实时采集/合成 → 编码 → 通道）
    let pump_stop = Arc::clone(&stop);
    let pump_counters = Arc::clone(&counters);
    let pump = std::thread::spawn(move || {
        let mut enc = match LiveEncoder::new(OpusConfig::default()) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("encoder init failed: {e:#}");
                return;
            }
        };
        let mut pkt = vec![0u8; MAX_PACKET_BYTES];
        if args.no_audio {
            // 合成 440Hz 正弦，相位连续；按绝对时刻表 pacing（sleep-after-work 会越走越慢，
            // 接收侧会误判为时钟漂移而大量 late-drop，见 Week 4 联调记录）。
            let mut phase = 0u64;
            let mut next = Instant::now();
            while !pump_stop.load(Ordering::Relaxed) {
                let mut frame = [0f32; FRAME_SAMPLES_10MS_48K];
                for s in frame.iter_mut() {
                    *s = (2.0 * std::f32::consts::PI * 440.0 * phase as f32 / 48_000.0).sin() * 0.4;
                    phase += 1;
                }
                if let Ok(n) = enc.encode(&frame, &mut pkt) {
                    if pkt_tx.try_send(pkt[..n].to_vec()).is_err() {
                        pump_counters.pump_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
                next += Duration::from_millis(10);
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now; // 掉队太多就重定时刻表，不追帧
                }
            }
            return;
        }

        let (capture, mut mic) = match start_capture(None, CaptureConfig::default()) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("capture failed: {e:#}");
                return;
            }
        };
        let capture_rate = capture.sample_rate;
        if capture_rate != 48_000 {
            eprintln!("capture: {capture_rate}Hz, resampling to 48kHz");
        }
        eprintln!("capture: {}Hz ch={}", capture.sample_rate, capture.channels);
        let mut cap_resampler =
            (capture_rate != 48_000).then(|| MonoResampler::new(capture_rate, 48_000));
        let mut frame = [0f32; FRAME_SAMPLES_10MS_48K];
        while !pump_stop.load(Ordering::Relaxed) {
            if let Some(r) = cap_resampler.as_mut() {
                loop {
                    while let Some(s) = mic.try_pop() {
                        r.push(&[s]);
                    }
                    if r.pop_frame(&mut frame) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                    if pump_stop.load(Ordering::Relaxed) {
                        return;
                    }
                }
            } else {
                let mut n = 0;
                while n < frame.len() {
                    match mic.try_pop() {
                        Some(s) => {
                            frame[n] = s;
                            n += 1;
                        }
                        None => std::thread::sleep(Duration::from_millis(1)),
                    }
                    if pump_stop.load(Ordering::Relaxed) {
                        return;
                    }
                }
            }
            if let Ok(m) = enc.encode(&frame, &mut pkt) {
                if pkt_tx.try_send(pkt[..m].to_vec()).is_err() {
                    pump_counters.pump_dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    });

    // ---- net 发送任务
    let send_link = Arc::clone(&link);
    let sender = tokio::spawn(async move {
        while let Some(pkt) = pkt_rx.recv().await {
            if let Err(e) = send_link.send_voice(&pkt).await {
                eprintln!("send failed: {e:#}");
                break;
            }
        }
    });

    // ---- 收包循环
    let recv_link = Arc::clone(&link);
    let recver = tokio::spawn(recv_link.recv_loop(voice_tx));

    // ---- 解码任务（+ 播放/计数）
    let play_prod = if args.no_audio {
        None
    } else {
        let (prod, cons) = capture_ring(CaptureConfig::default());
        // 播放走默认输出；声道不匹配由 playback 内部扩展
        match start_playback(None, cons, 48_000, 1) {
            Ok(_handle) => {
                let play_rate = _handle.sample_rate;
                // handle 必须活着：move 进任务保持流运行
                Some((prod, _handle, play_rate))
            }
            Err(e) => {
                eprintln!("playback failed (continuing without audio out): {e:#}");
                None
            }
        }
    };
    // ---- 解码任务：收包进 JB，10ms tick 吐帧解码（空洞 PLC）
    let dec_counters = Arc::clone(&counters);
    let jb_stats_shared = Arc::new(Mutex::new(JitterStats::default()));
    let jb_stats_writer = Arc::clone(&jb_stats_shared);
    let drop_pct = args.drop_pct;
    let decoder = tokio::spawn(async move {
        let mut dec = match LiveDecoder::new(48_000) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("decoder init failed: {e:#}");
                return;
            }
        };
        let mut play = play_prod;
        let play_rate = play.as_ref().map(|(_, _, r)| *r).unwrap_or(48_000);
        let mut play_resampler =
            (play_rate != 48_000).then(|| MonoResampler::new(48_000, play_rate));
        let mut play_frame = vec![0.0f32; frame_len_10ms(play_rate).max(1)];
        let mut jb = AdaptiveJitterBuffer::new(JitterConfig::default());
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        // 播放时钟：错过的 tick 直接跳过（追帧只会制造突发，不补）
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut pcm = [0f32; FRAME_SAMPLES_10MS_48K];
        loop {
            tokio::select! {
                msg = voice_rx.recv() => {
                    let Some(msg) = msg else { break };
                    if should_drop(&mut rng, drop_pct) {
                        dec_counters.sim_dropped.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    jb.push(msg.sequence, msg.timestamp_ms, msg.payload);
                }
                _ = tick.tick() => {
                    let payload: Option<Vec<u8>> = match jb.pop() {
                        Plucked::Frame(p) => {
                            dec_counters.decoded.fetch_add(1, Ordering::Relaxed);
                            Some(p)
                        }
                        Plucked::Lost { .. } => {
                            dec_counters.plc_frames.fetch_add(1, Ordering::Relaxed);
                            None
                        }
                        // 初始保持期：靠播放欠载自然静音，不计丢包
                        Plucked::Empty => {
                            *jb_stats_writer.lock().unwrap_or_else(|e| e.into_inner()) =
                                jb.stats();
                            continue;
                        }
                    };
                    let bytes = payload.as_deref().unwrap_or(&[]);
                    let m = match dec.decode(bytes, &mut pcm) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    if let Some((prod, _, _)) = play.as_mut() {
                        if let Some(r) = play_resampler.as_mut() {
                            r.push(&pcm[..m]);
                            if r.pop_frame(&mut play_frame) {
                                for s in play_frame.iter() {
                                    if prod.try_push(*s).is_err() {
                                        dec_counters.play_dropped.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            }
                        } else {
                            for s in &pcm[..m] {
                                if prod.try_push(*s).is_err() {
                                    dec_counters.play_dropped.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                    *jb_stats_writer.lock().unwrap_or_else(|e| e.into_inner()) = jb.stats();
                }
            }
        }
    });

    // ---- Ping + 状态打印
    let stat_link = Arc::clone(&link);
    let stat_counters = Arc::clone(&counters);
    let jb_stats_reader = Arc::clone(&jb_stats_shared);
    let stats = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            let _ = stat_link.send_ping().await;
            let s = stat_link.snapshot();
            let js = *jb_stats_reader.lock().unwrap_or_else(|e| e.into_inner());
            eprintln!(
                "rtt={:?}ms jitter={:.1}ms loss={:.1}% tx={} rx={} dec={} plc={} jb_tgt={:.0}ms jb_buf={}ms jb_lost={} late={} simdrop={} pump_drop={} play_drop={}",
                s.rtt_ms,
                s.jitter_ms,
                s.loss_percent,
                s.voice_tx,
                s.voice_rx,
                stat_counters.decoded.load(Ordering::Relaxed),
                stat_counters.plc_frames.load(Ordering::Relaxed),
                js.target_delay_ms,
                js.buffered_ms,
                js.lost,
                js.late_dropped,
                stat_counters.sim_dropped.load(Ordering::Relaxed),
                stat_counters.pump_dropped.load(Ordering::Relaxed),
                stat_counters.play_dropped.load(Ordering::Relaxed),
            );
        }
    });

    if args.secs > 0 {
        tokio::time::sleep(Duration::from_secs(args.secs)).await;
    } else {
        tokio::signal::ctrl_c().await?;
    }
    stop.store(true, Ordering::Relaxed);

    let s = link.snapshot();
    eprintln!(
        "final: tx={} rx={} dec={} plc={} simdrop={} loss={:.1}% jitter={:.1}ms rtt={:?}ms",
        s.voice_tx,
        s.voice_rx,
        counters.decoded.load(Ordering::Relaxed),
        counters.plc_frames.load(Ordering::Relaxed),
        counters.sim_dropped.load(Ordering::Relaxed),
        s.loss_percent,
        s.jitter_ms,
        s.rtt_ms,
    );

    stats.abort();
    sender.abort();
    decoder.abort();
    recver.abort();
    let _ = pump.join();
    Ok(())
}
