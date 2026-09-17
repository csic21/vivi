//! Mesh 通话主体：单 tick 混音回路 + 信令驱动的 peer 增删。
//!
//! Offer 方向规则（防 glare，无需 rollback）：收到 `PeerJoined(user)` 时，
//! **id 小的一方发起 offer**，另一方只等 offer。两边算出同一结论，与“谁新谁旧”无关，
//! 同时入会也成立。

use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc, Mutex, MutexGuard,
};
use std::time::Duration;
use std::time::Instant;

use ringbuf::traits::{Consumer, Producer};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use voice_common::{RouteType, UserId};
use voice_core::audio::{
    capture_ring, frame_len_10ms, start_capture, start_playback, CaptureConfig, CaptureProducer,
    MonoResampler, PlaybackHandle,
};
use voice_core::codec::{
    LiveDecoder, LiveEncoder, OpusConfig, OpusDecoder, OpusEncoder, FRAME_SAMPLES_10MS_48K,
    MAX_PACKET_BYTES,
};
use voice_core::dsp::{DspChain, EnergyVad, Vad};
use voice_core::jitter::{AdaptiveJitterBuffer, JitterConfig, Plucked};
use voice_core::mixer::Mixer;
use voice_protocol::{decode_view, SignalMessage, VoicePacket, MAX_UDP_DATAGRAM};
use voice_webrtc::{PeerConfig, PeerState, RTCIceGatheringState, RemoteFrame, VoicePeer};

use super::manual::{ManualEvent, ManualLink};
use super::signaling_client::{connect as sig_connect, SigSender};
use super::stats::{PeerStats, SessionStats};

/// 入会音频模式。
pub enum AudioMode {
    /// 真实设备（`None` = 默认设备）。
    Live {
        input: Option<String>,
        output: Option<String>,
    },
    /// 合成正弦（测试/无头用），不碰硬件；频率按 user 区分。
    Synthetic,
}

/// 信令通道。
///
/// 房间模式（`Server`）要求两端连同一个信令进程；异地时"那个进程在哪"没法自动
/// 解决，`Manual` 就是为此存在的旁路：一个字节都不经过服务器。
#[derive(Debug, Clone)]
pub enum Signaling {
    /// 连 WS 信令服务器，如 `ws://127.0.0.1:8080/signal`。
    Server { url: String },
    /// 手动（剪贴板）打洞，见 [`crate::manual`]。
    ///
    /// `peer_id` 是对端预置 id；与 `user_id` 比大小决定谁发 offer
    /// （沿用房间模式的确定性规则，小的一方发）。
    Manual { peer_id: u64 },
}

pub struct SessionConfig {
    pub user_id: u64,
    pub room_id: String,
    pub signaling: Signaling,
    /// STUN/TURN（TURN 凭证调用方提前从信令拿好填入）。
    pub peer_config: PeerConfig,
    pub audio: AudioMode,
    /// 直连多久不通则自动请求队友中继（秒，默认 15；测试可调小）。
    pub relay_timeout_secs: u64,
    /// 永不直连的用户（互 blok 则只走中继；测试/排障用，也可运行时改）。
    pub no_direct: Vec<u64>,
}

impl SessionConfig {
    pub fn relay_timeout(&self) -> Duration {
        Duration::from_secs(self.relay_timeout_secs.max(1))
    }
}

enum Control {
    Mute(bool),
    Deafen(bool),
    Gain(u64, f32),
    MicGain(f32),
    SpeakerGain(f32),
    NsEnabled(bool),
    AgcEnabled(bool),
    Unblock(u64),
    Leave(oneshot::Sender<()>),
}

/// 通话控制句柄（可跨任务克隆使用；`leave()` 消费并等待清理完成）。
pub struct SessionHandle {
    control: mpsc::Sender<Control>,
    snapshot: Arc<Mutex<SessionStats>>,
    blocklist: Arc<Mutex<HashSet<u64>>>,
    audio_io: Arc<AudioSettings>,
}

impl Clone for SessionHandle {
    fn clone(&self) -> Self {
        Self {
            control: self.control.clone(),
            snapshot: Arc::clone(&self.snapshot),
            blocklist: Arc::clone(&self.blocklist),
            audio_io: Arc::clone(&self.audio_io),
        }
    }
}

impl SessionHandle {
    pub async fn set_muted(&self, muted: bool) {
        let _ = self.control.send(Control::Mute(muted)).await;
    }

    /// 同步版（全局热键回调等非 async 上下文用；通道有 32 缓冲，不会久堵）。
    pub fn set_muted_blocking(&self, muted: bool) {
        let _ = self.control.blocking_send(Control::Mute(muted));
    }

    pub async fn set_deafened(&self, deafened: bool) {
        let _ = self.control.send(Control::Deafen(deafened)).await;
    }

    /// 单用户音量（0.0~2.0，Mixer 内部 clamp）。
    pub async fn set_user_gain(&self, user: u64, gain: f32) {
        let _ = self.control.send(Control::Gain(user, gain)).await;
    }

    /// 麦克风音量（0.0~4.0，1.0 = 100%，NS 之前的前置增益）。
    pub async fn set_mic_gain(&self, gain: f32) {
        let _ = self.control.send(Control::MicGain(gain)).await;
    }

    /// 扬声器音量（0.0~2.0，1.0 = 100%，混音后总控）。
    pub async fn set_speaker_gain(&self, gain: f32) {
        let _ = self.control.send(Control::SpeakerGain(gain)).await;
    }

    /// 麦克风降噪开关（RNNoise）。
    pub async fn set_ns_enabled(&self, enabled: bool) {
        let _ = self.control.send(Control::NsEnabled(enabled)).await;
    }

    /// 麦克风增强开关（自动增益 AGC）。
    pub async fn set_agc_enabled(&self, enabled: bool) {
        let _ = self.control.send(Control::AgcEnabled(enabled)).await;
    }

    /// 永不与该用户直连（只走中继；测试/排障用，可随时解除）。
    /// 注意：只阻止**新建**直连，已连上的不受影响。
    pub fn block_peer(&self, user: u64) {
        self.blocklist
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(user);
    }

    /// 解除直连屏蔽；主任务随后按 id 规则补 offer（升级路径）。
    pub fn unblock_peer(&self, user: u64) {
        self.blocklist
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&user);
        let _ = self.control.try_send(Control::Unblock(user));
    }

    pub fn stats(&self) -> SessionStats {
        Self::lock(&self.snapshot).clone()
    }

    /// 本端 mic 实时电平 RMS（0.0~1.0，无锁原子直读，供 50ms 级律动条用）。
    pub fn mic_level(&self) -> f32 {
        f32::from_bits(self.audio_io.mic_level_bits.load(Ordering::Relaxed))
    }

    pub async fn leave(self) {
        let (tx, rx) = oneshot::channel();
        let _ = self.control.send(Control::Leave(tx)).await;
        let _ = rx.await;
    }

    fn lock(snapshot: &Mutex<SessionStats>) -> MutexGuard<'_, SessionStats> {
        snapshot.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 对端槽位：在收包任务与混音 tick 间共享（JB 短临界区 Mutex，无 await）。
struct Slot {
    user: u64,
    peer: Arc<VoicePeer>,
    jb: Mutex<AdaptiveJitterBuffer<Vec<u8>>>,
    decoded: AtomicU64,
    plc: AtomicU64,
    speaking: AtomicBool,
}

/// 中继入槽（B 侧）：经某 peer 转发的源语音，与直连槽位同等参与混音。
/// 以源用户 id 为键存放在 `relay_in` 中。
struct RelaySlot {
    via: u64,
    jb: Mutex<AdaptiveJitterBuffer<Vec<u8>>>,
    decoded: AtomicU64,
    plc: AtomicU64,
}

/// C 侧转发表：(source, dest) -> 转发通道。
type RelayTable = Arc<Mutex<HashMap<(u64, u64), mpsc::Sender<RemoteFrame>>>>;

/// 单 tick 内某用户的出声路径（PLC 连续性 + UI 标识用）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlayPath {
    Direct,
    Relay,
}

/// stats 任务写、混音 tick 读的媒体缓存。
#[derive(Debug, Clone, Copy, Default)]
struct MediaCache {
    route: Option<RouteType>,
    rtt_ms: Option<f32>,
    jitter_ms: f32,
    loss_percent: f32,
}

/// pump 线程读、主任务写的音频 IO 设置 + pump 写的麦克风电平（全原子，无锁）。
pub struct AudioSettings {
    mic_gain_bits: AtomicU32,
    speaker_gain_bits: AtomicU32,
    ns_on: AtomicBool,
    agc_on: AtomicBool,
    mic_level_bits: AtomicU32,
    /// 期望丢包率（0~100，stats 任务按实测写入；pump 据此调编码器 FEC）。
    loss_hint: AtomicU32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            mic_gain_bits: AtomicU32::new(1.0f32.to_bits()),
            speaker_gain_bits: AtomicU32::new(1.0f32.to_bits()),
            ns_on: AtomicBool::new(true),
            agc_on: AtomicBool::new(true),
            mic_level_bits: AtomicU32::new(0),
            loss_hint: AtomicU32::new(0),
        }
    }
}

impl AudioSettings {
    pub fn mic_gain(&self) -> f32 {
        f32::from_bits(self.mic_gain_bits.load(Ordering::Relaxed))
    }

    pub fn set_mic_gain(&self, gain: f32) {
        self.mic_gain_bits
            .store(gain.clamp(0.0, 4.0).to_bits(), Ordering::Relaxed);
    }

    pub fn speaker_gain(&self) -> f32 {
        f32::from_bits(self.speaker_gain_bits.load(Ordering::Relaxed))
    }

    pub fn set_speaker_gain(&self, gain: f32) {
        self.speaker_gain_bits
            .store(gain.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }

    pub fn loss_hint(&self) -> u8 {
        self.loss_hint.load(Ordering::Relaxed).min(100) as u8
    }

    pub fn set_loss_hint(&self, percent: u8) {
        self.loss_hint
            .store((percent as u32).min(100), Ordering::Relaxed);
    }
}

/// 实测丢包率 → 编码器策略：冗余量跟丢包走，FEC 在 ≥3% 才开（省码率）。
/// 变化 <3 个点不折腾编码器（防抖）。
pub fn fec_for_loss(loss_percent: f32) -> (u8, bool) {
    let pct = loss_percent.clamp(0.0, 100.0).round() as u8;
    (pct, pct >= 3)
}

pub fn loss_hint_changed(old_hint: u8, loss_percent: f32) -> bool {
    let (pct, _) = fec_for_loss(loss_percent);
    pct.abs_diff(old_hint) >= 3
}

pub struct Session;

impl Session {
    /// 入会：连信令 → 起音频 → 发 JoinRoom，立即返回句柄，对端陆续接入。
    pub async fn join(config: SessionConfig) -> anyhow::Result<SessionHandle> {
        Ok(Self::join_inner(config).await?.0)
    }

    /// 手动打洞入会：不连信令服务器，额外返回手工信令通道
    /// （出站攒连接码 / 入站喂对端连接码，见 [`crate::manual`]）。
    ///
    /// `config.signaling` 必须是 [`Signaling::Manual`]。
    pub async fn join_manual(config: SessionConfig) -> anyhow::Result<(SessionHandle, ManualLink)> {
        let (handle, manual) = Self::join_inner(config).await?;
        let link = manual.ok_or_else(|| {
            anyhow::anyhow!("join_manual 需要 Signaling::Manual；房间模式请用 Session::join")
        })?;
        Ok((handle, link))
    }

    async fn join_inner(
        config: SessionConfig,
    ) -> anyhow::Result<(SessionHandle, Option<ManualLink>)> {
        let me = config.user_id;
        let room = config.room_id.clone();
        let relay_timeout = config.relay_timeout();
        let blocklist: Arc<Mutex<HashSet<u64>>> =
            Arc::new(Mutex::new(config.no_direct.iter().copied().collect()));
        let peer_config = Arc::new(config.peer_config);

        let (sig_tx, sig_rx, sig_tasks, manual, manual_out) = match config.signaling {
            Signaling::Server { ref url } => {
                let (tx, rx, tasks) = sig_connect(url).await?;
                (tx, rx, tasks, None, None)
            }
            Signaling::Manual { peer_id } => {
                // 手动模式：不连任何东西。主任务照旧往 `SigSender` 里吐信令，
                // 这里用一条桥把它转成 `ManualEvent` 交给调用方；
                // 反向的入站信令由 `ManualLink::feed` 注入 `sig_rx`。
                let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<SignalMessage>();
                let (in_tx, in_rx) = mpsc::channel::<SignalMessage>(64);
                let (ev_tx, ev_rx) = mpsc::unbounded_channel::<ManualEvent>();
                // 桥持有一份发普通信令；主任务另持一份发 gathering 完成事件。
                let ev_from_main = ev_tx.clone();
                let bridge = tokio::spawn(async move {
                    while let Some(m) = raw_rx.recv().await {
                        if ev_tx.send(ManualEvent::Signal(m)).is_err() {
                            break;
                        }
                    }
                });
                (
                    SigSender::from_channel(raw_tx),
                    in_rx,
                    vec![bridge],
                    Some(ManualLink::new(ev_rx, in_tx)),
                    Some(ManualOut {
                        events: ev_from_main,
                        peer_id,
                    }),
                )
            }
        };
        let peers: Arc<Mutex<HashMap<u64, Arc<Slot>>>> = Arc::new(Mutex::new(HashMap::new()));
        let gains: Arc<Mutex<HashMap<u64, f32>>> = Arc::new(Mutex::new(HashMap::new()));
        let media: Arc<Mutex<HashMap<u64, MediaCache>>> = Arc::new(Mutex::new(HashMap::new()));
        let snapshot: Arc<Mutex<SessionStats>> = Arc::new(Mutex::new(SessionStats::default()));
        let mixed_frames = Arc::new(AtomicU64::new(0));
        let muted = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let speaking_self = Arc::new(AtomicBool::new(false));
        let audio_io = Arc::new(AudioSettings::default());
        let (control_tx, control_rx) = mpsc::channel::<Control>(32);
        let (peer_state_tx, peer_state_rx) = mpsc::channel::<(u64, PeerState)>(64);
        let (relay_inbox_tx, relay_inbox_rx) = mpsc::channel::<(u64, Vec<u8>)>(256);

        // ---- 音频：采集 ring + 播放 ring（Synthetic 模式跳过硬件）
        // 跨平台注意：Windows 常见 44100Hz，macOS 常见 48000Hz。
        // 采集统一重采样到 48k 进 DSP/Opus，混音 48k 再转回播放设备率，
        // 两端采样率不同也能互通（之前非 48k 直接报错进不了房）。
        let (mic_consumer, play_prod, _play_handle, capture_rate, play_rate) = match config.audio {
            AudioMode::Live { input, output } => {
                let (cap, mic) = start_capture(input.as_deref(), CaptureConfig::default())?;
                let capture_rate = cap.sample_rate;
                if capture_rate != 48_000 {
                    tracing::info!(
                        capture_rate,
                        "capture not 48kHz; resampling to 48kHz for voice pipeline"
                    );
                }
                let (prod, cons) = capture_ring(CaptureConfig::default());
                // 播放统一按 48k/mono 要：设备支持则零转换，不支持则回退默认
                // 并在混音侧重采样（handle 带回实际值）。
                let handle = start_playback(output.as_deref(), cons, 48_000, 1)?;
                let play_rate = handle.sample_rate;
                if play_rate != 48_000 {
                    tracing::info!(
                        play_rate,
                        "playback not 48kHz; resampling mix to device rate"
                    );
                }
                (Some(mic), Some(prod), Some(handle), capture_rate, play_rate)
            }
            AudioMode::Synthetic => (None, None, None, 48_000, 48_000),
        };
        // _play_handle 必须活着：move 进主任务保持播放流运行
        let _play_handle: Option<PlaybackHandle> = _play_handle;

        // ---- mic pump 线程：实时帧 → DSP(NS/VAD/AGC) → encode 一次 → 通道（发送任务扇出）
        let (encoded_tx, encoded_rx) = mpsc::channel::<Vec<u8>>(64);
        let pump = std::thread::spawn({
            let stop = Arc::clone(&stop);
            let muted = Arc::clone(&muted);
            let speaking_self = Arc::clone(&speaking_self);
            let audio_io = Arc::clone(&audio_io);
            // 类语音合成信号：基频谐波栈 + 颤音 + 慢速 AM（110~170Hz 言语基频段）。
            // 注意：纯净正弦是 RNNoise 的病态输入（特定频率会被当作平稳噪声吃掉），
            // 不能拿它当测试/合成信号，见 Week 8 记录。
            let base_f0 = 110.0 + (me % 5) as f64 * 15.0;
            move || {
                let mut enc = match LiveEncoder::new(OpusConfig::default()) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("session encoder init failed: {e:#}");
                        return;
                    }
                };
                let mut dsp = DspChain::new(true);
                let mut pkt = vec![0u8; MAX_PACKET_BYTES];
                let mut last_hint: u8 = 0;
                let mut mic = mic_consumer;
                // 采集设备率 → 48k：Windows 44100 等非 48k 设备在这里归一化，
                // 与对端平台/采样率无关，保证 Windows↔Mac 互通。
                let mut cap_resampler =
                    (capture_rate != 48_000).then(|| MonoResampler::new(capture_rate, 48_000));
                let mut phase_cyc = 0.0f64;
                let mut abs_sample = 0u64;
                let mut next = std::time::Instant::now();
                let mut frame = [0f32; FRAME_SAMPLES_10MS_48K];
                while !stop.load(Ordering::Relaxed) {
                    if muted.load(Ordering::Relaxed) {
                        // 静音：排空 mic + 重采样器（防 unmute 时 500ms 陈旧突发），合成模式直接等
                        if let Some(mic) = mic.as_mut() {
                            while mic.try_pop().is_some() {}
                        }
                        if let Some(r) = cap_resampler.as_mut() {
                            r.clear();
                        }
                        speaking_self.store(false, Ordering::Relaxed);
                        audio_io.mic_level_bits.store(0, Ordering::Relaxed);
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    match mic.as_mut() {
                        Some(mic) => {
                            if let Some(resampler) = cap_resampler.as_mut() {
                                // 流式重采样：攒设备率采样，凑够一帧 48k 再走 DSP/Opus
                                loop {
                                    while let Some(s) = mic.try_pop() {
                                        resampler.push(&[s]);
                                    }
                                    if resampler.pop_frame(&mut frame) {
                                        break;
                                    }
                                    std::thread::sleep(Duration::from_millis(1));
                                    if stop.load(Ordering::Relaxed) {
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
                                    if stop.load(Ordering::Relaxed) {
                                        return;
                                    }
                                }
                            }
                        }
                        None => {
                            for s in frame.iter_mut() {
                                let t = abs_sample as f64 / 48_000.0;
                                let f0 = base_f0
                                    * (1.0 + 0.02 * (2.0 * std::f64::consts::PI * 5.0 * t).sin());
                                phase_cyc += f0 / 48_000.0;
                                let am = 0.6 + 0.4 * (2.0 * std::f64::consts::PI * 2.2 * t).sin();
                                let v = (2.0 * std::f64::consts::PI * phase_cyc).sin()
                                    + 0.4 * (4.0 * std::f64::consts::PI * phase_cyc).sin()
                                    + 0.2 * (6.0 * std::f64::consts::PI * phase_cyc).sin();
                                *s = (v * 0.25 * am) as f32;
                                abs_sample += 1;
                            }
                            next += Duration::from_millis(10);
                            let now = std::time::Instant::now();
                            if next > now {
                                std::thread::sleep(next - now);
                            } else {
                                next = now;
                            }
                        }
                    }
                    // 上行链：手动增益 → NS → VAD → AGC → Opus。
                    // 米表用电平是 DSP 之前的原始 mic（调增益的依据）。
                    let level = voice_core::dsp::rms(&frame);
                    audio_io
                        .mic_level_bits
                        .store(level.to_bits(), Ordering::Relaxed);
                    dsp.set_input_gain(audio_io.mic_gain());
                    dsp.set_ns_enabled(audio_io.ns_on.load(Ordering::Relaxed));
                    dsp.set_agc_enabled(audio_io.agc_on.load(Ordering::Relaxed));
                    // 自适应 FEC：丢包上来就加冗余，好了就撤（变化才调编码器）
                    let hint = audio_io.loss_hint();
                    if hint != last_hint {
                        let (pct, fec) = fec_for_loss(hint as f32);
                        let _ = enc.set_packet_loss_perc(pct);
                        let _ = enc.set_fec(fec);
                        last_hint = hint;
                    }
                    let speech = dsp.process(&mut frame).speech;
                    speaking_self.store(speech, Ordering::Relaxed);
                    if let Ok(n) = enc.encode(&frame, &mut pkt) {
                        let _ = encoded_tx.try_send(pkt[..n].to_vec());
                    }
                }
            }
        });

        // ---- 发送任务：编码包扇出给全部对端
        let send_peers = Arc::clone(&peers);
        let send_stop = Arc::clone(&stop);
        let sender = tokio::spawn(async move {
            let mut rx = encoded_rx;
            while let Some(pkt) = rx.recv().await {
                if send_stop.load(Ordering::Relaxed) {
                    break;
                }
                let targets: Vec<Arc<VoicePeer>> = {
                    let map = send_peers.lock().unwrap_or_else(|e| e.into_inner());
                    map.values().map(|s| Arc::clone(&s.peer)).collect()
                };
                for peer in targets {
                    if let Err(e) = peer.send_opus(&pkt).await {
                        tracing::warn!(error = %e, "send_opus failed");
                    }
                }
            }
        });

        // ---- stats 任务：2s 刷一次每对端选路 + 媒体指标
        let stat_peers = Arc::clone(&peers);
        let stat_media = Arc::clone(&media);
        let stat_stop = Arc::clone(&stop);
        let stat_audio = Arc::clone(&audio_io);
        let stats = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            loop {
                tick.tick().await;
                if stat_stop.load(Ordering::Relaxed) {
                    break;
                }
                let targets: Vec<(u64, Arc<VoicePeer>)> = {
                    let map = stat_peers.lock().unwrap_or_else(|e| e.into_inner());
                    map.iter().map(|(u, s)| (*u, Arc::clone(&s.peer))).collect()
                };
                let mut fresh = HashMap::new();
                for (user, peer) in targets {
                    let (route, m) = peer.link_stats().await;
                    fresh.insert(
                        user,
                        MediaCache {
                            route,
                            rtt_ms: m.rtt_ms,
                            jitter_ms: m.jitter_ms,
                            loss_percent: m.loss_percent,
                        },
                    );
                }
                // 全场最差丢包 → 编码器期望（自适应 FEC；变化够大才写，防抖）
                let worst = fresh
                    .values()
                    .map(|m| m.loss_percent)
                    .fold(0.0f32, f32::max);
                if loss_hint_changed(stat_audio.loss_hint(), worst) {
                    let (pct, _) = fec_for_loss(worst);
                    stat_audio.set_loss_hint(pct);
                }
                *stat_media.lock().unwrap_or_else(|e| e.into_inner()) = fresh;
            }
        });

        // ---- 主任务：信令 mesh 协议 + 控制 + 10ms 混音 tick
        let main = CallMain {
            me,
            room: room.clone(),
            sig: sig_tx,
            peers,
            gains,
            media,
            snapshot: Arc::clone(&snapshot),
            mixed_frames,
            muted,
            stop,
            audio_io: Arc::clone(&audio_io),
            decoders: HashMap::new(),
            scratch: HashMap::new(),
            vad: HashMap::new(),
            speaking_self,
            mixer: Mixer::default(),
            deafened: false,
            play_prod,
            play_handle: _play_handle,
            play_drops: 0,
            mix_buf: [0.0; FRAME_SAMPLES_10MS_48K],
            play_resampler: (play_rate != 48_000).then(|| MonoResampler::new(48_000, play_rate)),
            play_frame: vec![0.0; frame_len_10ms(play_rate).max(1)],
            connected: HashMap::new(),
            control_rx,
            sig_rx,
            peer_state_rx,
            peer_state_tx,
            relay_inbox_rx,
            relay_inbox_tx,
            mix_tick: {
                let mut t = tokio::time::interval(Duration::from_millis(10));
                t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                t
            },
            peer_config,
            tasks: sig_tasks,
            blocklist: Arc::clone(&blocklist),
            relay_timeout,
            relay_wanted: HashMap::new(),
            relay_requested_at: HashMap::new(),
            relay_in: HashMap::new(),
            relay_dec: HashMap::new(),
            direct_streak: HashMap::new(),
            last_path: HashMap::new(),
            last_via: HashMap::new(),
            relays_out: Arc::new(Mutex::new(HashMap::new())),
            offered_at: HashMap::new(),
            signal_error: None,
            manual: manual_out,
        };
        main.spawn(sender, stats, pump);

        // 入会宣告（新人等 PeerJoined；老人按 id 规则决定是否 offer，见模块文档）
        // 注意：handle 先返回，JoinRoom 在主任务启动后立刻发出
        Ok((
            SessionHandle {
                control: control_tx,
                snapshot,
                blocklist: Arc::clone(&blocklist),
                audio_io: Arc::clone(&audio_io),
            },
            manual,
        ))
    }
}

/// 手动打洞模式下主任务要往哪儿吐事件（房间模式恒为 `None`）。
struct ManualOut {
    events: mpsc::UnboundedSender<ManualEvent>,
    /// 对端预置 id；没有服务器广播"人到了"，只能开工时直接合成一条 PeerJoined。
    peer_id: u64,
}

/// 主任务体（单任务串行：无数据竞争；JB 例外，用短 Mutex 跨收包/tick 共享）。
struct CallMain {
    me: u64,
    room: String,
    sig: SigSender,
    peers: Arc<Mutex<HashMap<u64, Arc<Slot>>>>,
    gains: Arc<Mutex<HashMap<u64, f32>>>,
    media: Arc<Mutex<HashMap<u64, MediaCache>>>,
    snapshot: Arc<Mutex<SessionStats>>,
    mixed_frames: Arc<AtomicU64>,
    muted: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    audio_io: Arc<AudioSettings>,
    decoders: HashMap<u64, LiveDecoder>,
    scratch: HashMap<u64, [f32; FRAME_SAMPLES_10MS_48K]>,
    vad: HashMap<u64, EnergyVad>,
    speaking_self: Arc<AtomicBool>,
    mixer: Mixer,
    deafened: bool,
    play_prod: Option<CaptureProducer>,
    /// 持有即保持播放流运行（不读字段，靠 Drop 语义工作）。
    #[allow(dead_code)]
    play_handle: Option<PlaybackHandle>,
    play_drops: u64,
    mix_buf: [f32; FRAME_SAMPLES_10MS_48K],
    /// 48kHz 混音 → 播放设备率（`None` = 直通）。
    play_resampler: Option<MonoResampler>,
    /// 重采样输出暂存（按 10ms 播放帧长）。
    play_frame: Vec<f32>,
    connected: HashMap<u64, bool>,
    control_rx: mpsc::Receiver<Control>,
    sig_rx: mpsc::Receiver<SignalMessage>,
    peer_state_rx: mpsc::Receiver<(u64, PeerState)>,
    peer_state_tx: mpsc::Sender<(u64, PeerState)>,
    /// 中继包络 fan-in：(来源 leg 用户, 包络字节)，主任务按源分发进中继 JB。
    relay_inbox_rx: mpsc::Receiver<(u64, Vec<u8>)>,
    relay_inbox_tx: mpsc::Sender<(u64, Vec<u8>)>,
    mix_tick: tokio::time::Interval,
    peer_config: Arc<PeerConfig>,
    tasks: Vec<JoinHandle<()>>,
    // ---- 队友中继状态 ----
    /// 永不直连的用户（运行时可改；测试/排障用）。
    blocklist: Arc<Mutex<HashSet<u64>>>,
    relay_timeout: Duration,
    /// B 侧生效中的中继：source -> via。
    relay_wanted: HashMap<u64, u64>,
    /// B 侧上次请求时刻（去重/冷却）。
    relay_requested_at: HashMap<u64, Instant>,
    /// B 侧中继 JB：source -> 槽。
    relay_in: HashMap<u64, Arc<RelaySlot>>,
    /// B 侧中继解码器：source -> decoder（与直连流独立状态）。
    relay_dec: HashMap<u64, LiveDecoder>,
    /// B 侧直连连续交付计数（升级判定：连续 10 帧直连即切回）。
    direct_streak: HashMap<u64, u32>,
    /// B 侧各源上次出声路径（PLC 解码器选择 + 快照 relayed_via 用）。
    last_path: HashMap<u64, PlayPath>,
    /// B 侧上次使用的中继方（快照 relayed_via 用；直连时无条目）。
    last_via: HashMap<u64, u64>,
    /// C 侧转发表：(source, dest) -> 转发通道（frames-pump 读，try_send 永不阻塞）。
    relays_out: RelayTable,
    /// source -> 首次建连时刻（自动触发计时）。
    offered_at: HashMap<u64, Instant>,
    /// 服务端回的最后一条 `SignalMessage::Error`（附收到时刻），经快照透给 UI。
    signal_error: Option<(String, Instant)>,
    /// 手动打洞的出站通道（房间模式为 `None`）。
    manual: Option<ManualOut>,
}

/// 信令错误在快照里保留多久。
///
/// 够用户看见，又不至于把"刚才闪了一下"一直说成"现在还是坏的"——`peer offline`
/// 这类消息在一次 ICE 协商里出现几次是正常的，粘在界面上就成了误导。
const SIGNAL_ERROR_TTL: Duration = Duration::from_secs(30);

impl CallMain {
    fn spawn(
        mut self,
        sender: JoinHandle<()>,
        stats: JoinHandle<()>,
        pump: std::thread::JoinHandle<()>,
    ) {
        self.tasks.push(sender);
        self.tasks.push(stats);
        tokio::spawn(async move {
            // 先宣告入会，再进循环
            match self.manual.as_ref().map(|m| m.peer_id) {
                None => self.sig.send(SignalMessage::JoinRoom {
                    room_id: self.room.clone(),
                    user_id: UserId(self.me),
                }),
                // 手动模式没有服务器替我们广播"人到了"，直接合成一条。
                // 这样 id 规则（小者发 offer）在两种模式下是同一条代码路径。
                Some(peer_id) => {
                    self.on_signal(SignalMessage::PeerJoined {
                        user_id: UserId(peer_id),
                    })
                    .await;
                }
            }
            loop {
                tokio::select! {
                    Some(msg) = self.sig_rx.recv() => self.on_signal(msg).await,
                    Some((leg, envelope)) = self.relay_inbox_rx.recv() => {
                        self.on_relay_envelope(leg, &envelope);
                    }
                    Some(state) = self.peer_state_rx.recv() => {
                        self.connected.insert(state.0, matches!(state.1, PeerState::Connected));
                    }
                    Some(ctrl) = self.control_rx.recv() => {
                        if self.on_control(ctrl).await {
                            break;
                        }
                    }
                    _ = self.mix_tick.tick() => self.mix_once(),
                }
            }
            // 清理：停 pump/发送/统计，关全部 peer连接
            self.stop.store(true, Ordering::Relaxed);
            for h in self.tasks.drain(..) {
                h.abort();
            }
            let peers: Vec<Arc<VoicePeer>> = {
                self.peers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .values()
                    .map(|s| Arc::clone(&s.peer))
                    .collect()
            };
            for peer in peers {
                let _ = peer.close().await;
            }
            let _ = pump.join();
        });
    }

    fn is_blocked(&self, user: u64) -> bool {
        self.blocklist
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&user)
    }

    async fn on_signal(&mut self, msg: SignalMessage) {
        match msg {
            SignalMessage::PeerJoined { user_id } => {
                let user = user_id.0;
                if user == self.me
                    || self
                        .peers
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .contains_key(&user)
                {
                    return;
                }
                // 槽位照建（中继/统计/升级都需要它）；屏蔽只拦 SDP，不拦槽位
                let slot = self.ensure_peer(user).await;
                // id 小者发起 offer（确定性，天然防 glare）；被屏蔽的不发
                if self.me < user && !self.is_blocked(user) {
                    if let Some(slot) = slot {
                        match slot.peer.create_offer().await {
                            Ok(sdp) => self.sig.send(SignalMessage::Offer {
                                from: UserId(self.me),
                                to: UserId(user),
                                sdp,
                            }),
                            Err(e) => tracing::warn!(user, error = %e, "create_offer failed"),
                        }
                    }
                }
            }
            SignalMessage::Offer { from, sdp, .. } => {
                let user = from.0;
                if user == self.me || self.is_blocked(user) {
                    return;
                }
                if let Some(slot) = self.ensure_peer(user).await {
                    match slot.peer.apply_offer(&sdp).await {
                        Ok(answer) => self.sig.send(SignalMessage::Answer {
                            from: UserId(self.me),
                            to: UserId(user),
                            sdp: answer,
                        }),
                        Err(e) => tracing::warn!(user, error = %e, "apply_offer failed"),
                    }
                }
            }
            SignalMessage::Answer { from, sdp, .. } => {
                if self.is_blocked(from.0) {
                    return;
                }
                let slot = {
                    let peers = self.peers.lock().unwrap_or_else(|e| e.into_inner());
                    peers.get(&from.0).cloned()
                };
                match slot {
                    Some(slot) => {
                        if let Err(e) = slot.peer.apply_answer(&sdp).await {
                            tracing::warn!(user = from.0, error = %e, "apply_answer failed");
                        }
                    }
                    None => tracing::warn!(user = from.0, "answer for unknown peer"),
                }
            }
            SignalMessage::IceCandidate {
                from, candidate, ..
            } => {
                if self.is_blocked(from.0) {
                    return;
                }
                let slot = {
                    let peers = self.peers.lock().unwrap_or_else(|e| e.into_inner());
                    peers.get(&from.0).cloned()
                };
                match slot {
                    Some(slot) => {
                        if let Err(e) = slot.peer.add_remote_candidate(&candidate).await {
                            tracing::warn!(user = from.0, error = %e, "add_remote_candidate failed");
                        }
                    }
                    None => tracing::warn!(user = from.0, "candidate for unknown peer"),
                }
            }
            SignalMessage::RelayRequest { from, target, .. } => {
                self.on_relay_request(from.0, target.0).await;
            }
            SignalMessage::RelayAccepted { from, target, .. } => {
                self.on_relay_accepted(from.0, target.0).await;
            }
            SignalMessage::RelayRejected { from, target, .. } => {
                tracing::info!(relayer = from.0, target = target.0, "relay rejected");
                // 冷却：本次不再打扰该中继，触发循环会另选他人
                self.relay_requested_at.insert(target.0, Instant::now());
            }
            SignalMessage::RelayStop { from, target, .. } => {
                // 我是中继方：停掉 target → from 的转发
                self.drop_relay_out(target.0, from.0);
            }
            SignalMessage::PeerLeft { user_id } => self.remove_peer(user_id.0).await,
            SignalMessage::Error { message } => {
                tracing::warn!(error = %message, "signaling error");
                self.signal_error = Some((message, Instant::now()));
            }
            SignalMessage::JoinRoom { .. } | SignalMessage::LeaveRoom { .. } => {}
        }
    }

    /// C 侧：B 请我把 target(A) 的语音转给它。有活路才接（防中继环）。
    async fn on_relay_request(&mut self, from: u64, target: u64) {
        if from == self.me || target == self.me {
            return;
        }
        let have_live_source = self
            .peers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&target)
            && self.connected.get(&target).copied().unwrap_or(false);
        let dest = self
            .peers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&from)
            .cloned();
        let (accept, dest) = match (have_live_source, dest) {
            (true, Some(dest)) => (true, Some(dest)),
            _ => (false, None),
        };
        if accept {
            // 建转发任务：target 的帧 → dest leg 的 DataChannel
            let (tx, mut rx) = mpsc::channel::<RemoteFrame>(64);
            self.relays_out
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((target, from), tx);
            let dest = dest.expect("checked above");
            let me = self.me;
            self.tasks.push(tokio::spawn(async move {
                let mut buf = [0u8; MAX_UDP_DATAGRAM];
                while let Some(f) = rx.recv().await {
                    let n = match VoicePacket::encode_parts(
                        target,
                        f.sequence,
                        f.timestamp_ms,
                        &f.payload,
                        &mut buf,
                    ) {
                        Some(n) => n,
                        None => continue,
                    };
                    if dest.peer.send_relay(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                tracing::info!(target, dest = from, me, "relay forward task ended");
            }));
            self.sig.send(SignalMessage::RelayAccepted {
                from: UserId(self.me),
                to: UserId(from),
                target: UserId(target),
            });
            tracing::info!(target, dest = from, "relay accepted");
        } else {
            self.sig.send(SignalMessage::RelayRejected {
                from: UserId(self.me),
                to: UserId(from),
                target: UserId(target),
            });
        }
    }

    /// B 侧：中继方接受，建该源的中继 JB（直连通后会自动升级切回）。
    async fn on_relay_accepted(&mut self, from: u64, target: u64) {
        if self.relay_in.contains_key(&target) {
            return;
        }
        // 直连若已通，无需中继（顺手让对方停发，避免空转）
        if self.connected.get(&target).copied().unwrap_or(false) {
            self.sig.send(SignalMessage::RelayStop {
                from: UserId(self.me),
                to: UserId(from),
                target: UserId(target),
            });
            return;
        }
        self.relay_in.insert(
            target,
            Arc::new(RelaySlot {
                via: from,
                jb: Mutex::new(AdaptiveJitterBuffer::new(JitterConfig::default())),
                decoded: AtomicU64::new(0),
                plc: AtomicU64::new(0),
            }),
        );
        // 中继解码器与直连流独立状态（ histories 不同，不能共用）
        if let Ok(dec) = LiveDecoder::new(48_000) {
            self.relay_dec.insert(target, dec);
        }
        self.relay_wanted.insert(target, from);
        tracing::info!(target, via = from, "relay established");
    }

    /// 停掉 (source → dest) 的转发（任务随通道关闭自动退出）。
    fn drop_relay_out(&mut self, source: u64, dest: u64) {
        self.relays_out
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(source, dest));
    }

    /// 丢掉到 source 的中继（升级/离会/换路用），并通知中继方停发。
    fn drop_relay_in(&mut self, source: u64) {
        if let Some(via) = self.relay_wanted.remove(&source) {
            self.sig.send(SignalMessage::RelayStop {
                from: UserId(self.me),
                to: UserId(via),
                target: UserId(source),
            });
        }
        self.relay_in.remove(&source);
        self.relay_dec.remove(&source);
        self.direct_streak.remove(&source);
        self.last_path.remove(&source);
        self.last_via.remove(&source);
    }

    /// 自动触发：直连超期不通 + 有别人可当中继 → 发 RelayRequest。
    /// 约 1s 跑一次（由 mix tick 限流调用）；30s 冷却防骚扰。
    fn maybe_request_relays(&mut self) {
        let now = Instant::now();
        let users: Vec<u64> = {
            self.peers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .keys()
                .copied()
                .collect()
        };
        for user in users {
            // 注意：被屏蔽的不建直连，但正需要中继——这里不能跳过
            if user == self.me
                || self.connected.get(&user).copied().unwrap_or(false)
                || self.relay_wanted.contains_key(&user)
            {
                continue;
            }
            let eligible = self
                .offered_at
                .get(&user)
                .map(|t| now.duration_since(*t) >= self.relay_timeout)
                .unwrap_or(false);
            if !eligible {
                continue;
            }
            if let Some(last) = self.relay_requested_at.get(&user) {
                if now.duration_since(*last) < Duration::from_secs(30) {
                    continue;
                }
            }
            if let Some(relay) = self.pick_relay_for(user) {
                tracing::info!(target = user, via = relay, "requesting peer relay");
                self.sig.send(SignalMessage::RelayRequest {
                    from: UserId(self.me),
                    to: UserId(relay),
                    target: UserId(user),
                });
                self.relay_requested_at.insert(user, now);
            }
        }
    }

    /// 在已直连的对端里选 RTT 最小的当/default 中继（无缓存则任选首个）。
    fn pick_relay_for(&self, target: u64) -> Option<u64> {
        let media = self.media.lock().unwrap_or_else(|e| e.into_inner());
        let mut best: Option<(u64, f32)> = None;
        for (user, conn) in &self.connected {
            if *user == target || *user == self.me || !conn {
                continue;
            }
            let rtt = media.get(user).and_then(|m| m.rtt_ms).unwrap_or(f32::MAX);
            if best.map(|(_, b)| rtt < b).unwrap_or(true) {
                best = Some((*user, rtt));
            }
        }
        best.map(|(u, _)| u)
    }

    /// 取现有或新建对端槽位（含 JB/解码器/混音增益/三路转发任务）。
    async fn ensure_peer(&mut self, user: u64) -> Option<Arc<Slot>> {
        if let Some(slot) = self
            .peers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&user)
        {
            return Some(Arc::clone(slot));
        }
        let (peer, ev) = match VoicePeer::new(&self.peer_config).await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(user, error = %e, "peer create failed");
                return None;
            }
        };
        let slot = Arc::new(Slot {
            user,
            peer: Arc::clone(&peer),
            jb: Mutex::new(AdaptiveJitterBuffer::new(JitterConfig::default())),
            decoded: AtomicU64::new(0),
            plc: AtomicU64::new(0),
            speaking: AtomicBool::new(false),
        });

        // 本地 candidate → 信令（手动模式改成往 ManualEvent 里放，由调用方攒进连接码）
        let sig = self.sig.clone();
        let manual_ev = self.manual.as_ref().map(|m| m.events.clone());
        let me = self.me;
        self.tasks.push(tokio::spawn({
            let manual_ev = manual_ev.clone();
            async move {
                let mut rx = ev.local_candidates;
                while let Some(c) = rx.recv().await {
                    let msg = SignalMessage::IceCandidate {
                        from: UserId(me),
                        to: UserId(user),
                        candidate: c,
                    };
                    match &manual_ev {
                        Some(ev) => {
                            if ev.send(ManualEvent::Signal(msg)).is_err() {
                                break;
                            }
                        }
                        None => sig.send(msg),
                    }
                }
            }
        }));
        // gathering 完成 → 手动模式下通知调用方"候选收齐，可以出码了"。
        // 房间模式走 trickle，不需要这个信号，但任务照样收干净免得通道积压。
        self.tasks.push(tokio::spawn(async move {
            let mut rx = ev.gathering;
            while let Some(state) = rx.recv().await {
                if state == RTCIceGatheringState::Complete {
                    match &manual_ev {
                        Some(ev) => {
                            if ev.send(ManualEvent::GatheringComplete).is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }));
        // 远端帧 → 该对端 JB（+ 中继分发：有人订阅此源就同步一份）
        let slot2 = Arc::clone(&slot);
        let relays_out = Arc::clone(&self.relays_out);
        self.tasks.push(tokio::spawn(async move {
            let mut rx = ev.remote_frames;
            while let Some(f) = rx.recv().await {
                slot2.jb.lock().unwrap_or_else(|e| e.into_inner()).push(
                    f.sequence,
                    f.timestamp_ms,
                    f.payload.clone(),
                );
                // try_send 永不阻塞：中继腿跟不上就丢该帧，对端 JB/PLC 兜底
                let table = relays_out.lock().unwrap_or_else(|e| e.into_inner());
                for ((src, _), tx) in table.iter() {
                    if *src == slot2.user {
                        let _ = tx.try_send(RemoteFrame {
                            sequence: f.sequence,
                            timestamp_ms: f.timestamp_ms,
                            payload: f.payload.clone(),
                        });
                    }
                }
            }
        }));
        // 中继包络 → 主任务 fan-in（按包内源用户分发进中继 JB）
        let relay_tx = self.relay_inbox_tx.clone();
        self.tasks.push(tokio::spawn(async move {
            let mut rx = ev.relay_frames;
            while let Some(envelope) = rx.recv().await {
                if relay_tx.send((user, envelope)).await.is_err() {
                    break;
                }
            }
        }));
        // 状态 → 主任务 fan-in（connected 唯一真相源）
        let state_tx = self.peer_state_tx.clone();
        self.tasks.push(tokio::spawn(async move {
            let mut rx = ev.states;
            while let Some(s) = rx.recv().await {
                if state_tx.send((user, s)).await.is_err() {
                    break;
                }
            }
        }));

        self.decoders.insert(
            user,
            match LiveDecoder::new(48_000) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(user, error = %e, "decoder create failed");
                    return None;
                }
            },
        );
        self.offered_at.insert(user, Instant::now());
        self.scratch.insert(user, [0.0; FRAME_SAMPLES_10MS_48K]);
        self.vad.insert(user, EnergyVad::default());
        self.mixer.set_gain(UserId(user), 1.0);
        self.gains
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(user, 1.0);
        self.connected.insert(user, false);
        self.peers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(user, Arc::clone(&slot));
        Some(slot)
    }

    async fn remove_peer(&mut self, user: u64) {
        let slot = self
            .peers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&user);
        if let Some(slot) = slot {
            let _ = slot.peer.close().await;
        }
        self.decoders.remove(&user);
        self.scratch.remove(&user);
        self.vad.remove(&user);
        self.mixer.remove(UserId(user));
        self.gains
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&user);
        self.connected.remove(&user);
        self.offered_at.remove(&user);
        // 中继全量清理（涉及该用户的源/目标/中继方三类）：
        // - 我经它中转的源：丢中继 JB，下次触发循环会另找他人
        let dropped_sources: Vec<u64> = self
            .relay_in
            .iter()
            .filter(|(_, r)| r.via == user)
            .map(|(s, _)| *s)
            .collect();
        for source in dropped_sources {
            self.relay_in.remove(&source);
            self.relay_dec.remove(&source);
            self.relay_requested_at.remove(&source);
            self.direct_streak.remove(&source);
            self.last_path.remove(&source);
            self.last_via.remove(&source);
        }
        // - 以它为源的中继：停收（中继方收到 PeerLeft 也会停发，双保险）
        self.relay_in.remove(&user);
        self.relay_dec.remove(&user);
        self.relay_wanted.remove(&user);
        self.relay_requested_at.remove(&user);
        self.direct_streak.remove(&user);
        self.last_path.remove(&user);
        self.last_via.remove(&user);
        // - 经我转发的腿：撤表（转发任务随通道关闭退出）
        self.relays_out
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(src, dst), _| *src != user && *dst != user);
        tracing::info!(user, "peer removed");
    }

    /// 中继包络入站：按包内源用户分发进对应中继 JB（来源腿不对/无订阅则丢弃）。
    fn on_relay_envelope(&mut self, leg: u64, envelope: &[u8]) {
        let Some(view) = decode_view(envelope) else {
            return;
        };
        let source = view.user_id;
        let relay = match self.relay_in.get(&source) {
            Some(r) if r.via == leg => r,
            _ => return,
        };
        relay.jb.lock().unwrap_or_else(|e| e.into_inner()).push(
            view.sequence,
            view.timestamp_ms,
            view.payload.to_vec(),
        );
    }

    /// 控制命令；返回 true 表示 Leave（主循环退出）。
    async fn on_control(&mut self, ctrl: Control) -> bool {
        match ctrl {
            Control::Mute(m) => {
                self.muted.store(m, Ordering::Relaxed);
            }
            Control::Deafen(d) => {
                self.deafened = d;
            }
            Control::Gain(user, gain) => {
                self.mixer.set_gain(UserId(user), gain);
                self.gains
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(user, gain.clamp(0.0, 2.0));
            }
            Control::MicGain(gain) => self.audio_io.set_mic_gain(gain),
            Control::SpeakerGain(gain) => self.audio_io.set_speaker_gain(gain),
            Control::NsEnabled(on) => {
                self.audio_io.ns_on.store(on, Ordering::Relaxed);
            }
            Control::AgcEnabled(on) => {
                self.audio_io.agc_on.store(on, Ordering::Relaxed);
            }
            Control::Leave(ack) => {
                self.sig.send(SignalMessage::LeaveRoom {
                    room_id: self.room.clone(),
                    user_id: UserId(self.me),
                });
                let _ = ack.send(());
                return true;
            }
            Control::Unblock(user) => {
                // 升级路径：已有槽位、未直连、id 规则归我发起 → 补 offer
                if user == self.me || self.is_blocked(user) {
                    return false;
                }
                let slot = {
                    self.peers
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&user)
                        .cloned()
                };
                let connected = self.connected.get(&user).copied().unwrap_or(false);
                if let Some(slot) = slot {
                    if !connected && self.me < user {
                        match slot.peer.create_offer().await {
                            Ok(sdp) => self.sig.send(SignalMessage::Offer {
                                from: UserId(self.me),
                                to: UserId(user),
                                sdp,
                            }),
                            Err(e) => tracing::warn!(user, error = %e, "upgrade offer failed"),
                        }
                    }
                }
            }
        }
        false
    }

    /// 10ms 总 tick：各对端吐帧→解码/PLC→混音→扬声器 ring→快照。
    ///
    /// 两阶段避免同时持有多个暂存可变借用：先逐路解码进 scratch，再只读混音。
    fn mix_once(&mut self) {
        let slots: Vec<Arc<Slot>> = {
            self.peers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .cloned()
                .collect()
        };
        // 阶段一：逐路吐帧解码（可变借用不出本循环体）。
        // 路径偏好：直连 Frame 优先；直连没货才看中继（升级/降级天然发生）。
        for slot in &slots {
            let user = slot.user;
            let direct_pop = slot.jb.lock().unwrap_or_else(|e| e.into_inner()).pop();
            enum PlayItem {
                Voice(Vec<u8>, PlayPath),
                Conceal(PlayPath),
                Silence,
            }
            let item = match direct_pop {
                Plucked::Frame(p) => {
                    // 直连交付：有中继时连够 10 帧即升级切回（无缝）
                    if self.relay_wanted.contains_key(&user) {
                        let streak = {
                            let e = self.direct_streak.entry(user).or_insert(0);
                            *e += 1;
                            *e
                        };
                        if streak >= 10 {
                            tracing::info!(user, "direct recovered; dropping relay");
                            self.drop_relay_in(user);
                        }
                    }
                    PlayItem::Voice(p, PlayPath::Direct)
                }
                _ => {
                    self.direct_streak.remove(&user);
                    match self.relay_in.get(&user) {
                        Some(relay) => {
                            match relay.jb.lock().unwrap_or_else(|e| e.into_inner()).pop() {
                                Plucked::Frame(p) => PlayItem::Voice(p, PlayPath::Relay),
                                Plucked::Lost { .. } => {
                                    relay.plc.fetch_add(1, Ordering::Relaxed);
                                    PlayItem::Conceal(PlayPath::Relay)
                                }
                                Plucked::Empty => PlayItem::Silence,
                            }
                        }
                        None => match direct_pop {
                            Plucked::Lost { .. } => {
                                slot.plc.fetch_add(1, Ordering::Relaxed);
                                PlayItem::Conceal(PlayPath::Direct)
                            }
                            _ => PlayItem::Silence,
                        },
                    }
                }
            };
            // 解码（PLC 用上次出声路径的解码器，保持连续性）
            let path = match &item {
                PlayItem::Voice(_, p) => {
                    self.last_path.insert(user, *p);
                    *p
                }
                PlayItem::Conceal(p) => *p,
                PlayItem::Silence => {
                    if let Some(buf) = self.scratch.get_mut(&user) {
                        buf.fill(0.0);
                    }
                    // 静音帧不更新 VAD（靠 hangover 自然衰减）
                    continue;
                }
            };
            // borrow check：PlayPath 是 Copy，先把 payload 拿出来
            let payload: Option<Vec<u8>> = match item {
                PlayItem::Voice(p, _) => Some(p),
                _ => None,
            };
            let (Some(buf), dec) = (
                self.scratch.get_mut(&user),
                match path {
                    PlayPath::Direct => self.decoders.get_mut(&user),
                    PlayPath::Relay => self.relay_dec.get_mut(&user),
                },
            ) else {
                continue;
            };
            match (payload, dec) {
                (Some(p), Some(dec)) => {
                    if dec.decode(&p, buf).is_ok() {
                        match path {
                            PlayPath::Direct => {
                                slot.decoded.fetch_add(1, Ordering::Relaxed);
                            }
                            PlayPath::Relay => {
                                if let Some(r) = self.relay_in.get(&user) {
                                    r.decoded.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    } else {
                        buf.fill(0.0);
                    }
                }
                (None, Some(dec)) => {
                    let _ = dec.decode(&[], buf);
                    match path {
                        PlayPath::Direct => {
                            slot.plc.fetch_add(1, Ordering::Relaxed);
                        }
                        PlayPath::Relay => {
                            if let Some(r) = self.relay_in.get(&user) {
                                r.plc.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
                _ => buf.fill(0.0),
            }
            // 路径记录（UI 中继标识用）+ 解码侧 VAD
            match path {
                PlayPath::Direct => {
                    self.last_via.remove(&user);
                }
                PlayPath::Relay => {
                    if let Some(r) = self.relay_in.get(&user) {
                        self.last_via.insert(user, r.via);
                    }
                }
            }
            let speaking = match self.vad.get_mut(&user) {
                Some(v) => v.is_speech(buf),
                None => false,
            };
            slot.speaking.store(speaking, Ordering::Relaxed);
        }
        // 阶段二：只读混音
        let inputs: Vec<(UserId, &[f32])> = slots
            .iter()
            .filter_map(|slot| {
                self.scratch
                    .get(&slot.user)
                    .map(|buf| (UserId(slot.user), &buf[..]))
            })
            .collect();
        self.mixer.mix(&inputs, &mut self.mix_buf);
        if self.deafened {
            self.mix_buf.fill(0.0);
        }
        // 扬声器总控（混音后，deafen 之后）
        let speaker_gain = self.audio_io.speaker_gain();
        if speaker_gain != 1.0 {
            for s in self.mix_buf.iter_mut() {
                *s = (*s * speaker_gain).clamp(-1.0, 1.0);
            }
        }
        if let Some(prod) = self.play_prod.as_mut() {
            if let Some(resampler) = self.play_resampler.as_mut() {
                // 48k 混音 → 播放设备率（如 Windows 44100）；ring 攒的是设备率采样
                resampler.push(&self.mix_buf);
                if resampler.pop_frame(&mut self.play_frame) {
                    for s in self.play_frame.iter() {
                        if prod.try_push(*s).is_err() {
                            self.play_drops += 1;
                        }
                    }
                } else {
                    // 重采样器预热不足（首帧插值多要 1 采样）：丢一帧静音，避免爆音
                    self.play_drops += self.play_frame.len() as u64;
                }
            } else {
                for s in self.mix_buf.iter() {
                    if prod.try_push(*s).is_err() {
                        self.play_drops += 1;
                    }
                }
            }
        }
        self.mixed_frames.fetch_add(1, Ordering::Relaxed);

        // 自动触发检查（约 1s 一次，用 mixed 计数限流）
        if self
            .mixed_frames
            .load(Ordering::Relaxed)
            .is_multiple_of(100)
        {
            self.maybe_request_relays();
        }

        // 快照：连接态（主任务）+ 媒体指标（stats 任务）+ 计数器
        let media = self.media.lock().unwrap_or_else(|e| e.into_inner());
        let gains = self.gains.lock().unwrap_or_else(|e| e.into_inner());
        let peers = slots
            .iter()
            .map(|slot| {
                let m = media.get(&slot.user).copied().unwrap_or_default();
                let (mut decoded, mut plc) = (
                    slot.decoded.load(Ordering::Relaxed),
                    slot.plc.load(Ordering::Relaxed),
                );
                if let Some(r) = self.relay_in.get(&slot.user) {
                    decoded += r.decoded.load(Ordering::Relaxed);
                    plc += r.plc.load(Ordering::Relaxed);
                }
                PeerStats {
                    user_id: slot.user,
                    connected: self.connected.get(&slot.user).copied().unwrap_or(false),
                    speaking: slot.speaking.load(Ordering::Relaxed),
                    relayed_via: self.last_via.get(&slot.user).copied(),
                    route: m.route,
                    rtt_ms: m.rtt_ms,
                    jitter_ms: m.jitter_ms,
                    loss_percent: m.loss_percent,
                    gain: gains.get(&slot.user).copied().unwrap_or(1.0),
                    decoded_frames: decoded,
                    plc_frames: plc,
                }
            })
            .collect();
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = SessionStats {
            peers,
            mixed_frames: self.mixed_frames.load(Ordering::Relaxed),
            speaking_self: self.speaking_self.load(Ordering::Relaxed),
            mic_level: f32::from_bits(self.audio_io.mic_level_bits.load(Ordering::Relaxed)),
            mic_gain: self.audio_io.mic_gain(),
            speaker_gain: self.audio_io.speaker_gain(),
            ns_enabled: self.audio_io.ns_on.load(Ordering::Relaxed),
            agc_enabled: self.audio_io.agc_on.load(Ordering::Relaxed),
            muted: self.muted.load(Ordering::Relaxed),
            signal_error: self
                .signal_error
                .as_ref()
                .filter(|(_, at)| at.elapsed() < SIGNAL_ERROR_TTL)
                .map(|(msg, _)| msg.clone()),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{fec_for_loss, loss_hint_changed};

    #[test]
    fn fec_should_stay_off_on_clean_network() {
        assert_eq!(fec_for_loss(0.0), (0, false));
        assert_eq!(fec_for_loss(2.4), (2, false));
    }

    #[test]
    fn fec_should_kick_in_with_loss() {
        assert_eq!(fec_for_loss(3.0), (3, true));
        assert_eq!(fec_for_loss(15.6), (16, true));
        assert_eq!(fec_for_loss(250.0), (100, true));
    }

    #[test]
    fn hint_should_ignore_small_jitter() {
        assert!(!loss_hint_changed(5, 6.9));
        assert!(loss_hint_changed(5, 8.0));
        assert!(loss_hint_changed(0, 3.0));
    }
}
