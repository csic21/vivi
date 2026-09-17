//! Phase 5：P2P 语音（ICE/STUN/TURN + DTLS-SRTP + Opus），基于 webrtc-rs 0.20。
//!
//! 设计：
//! - 1 个 [`VoicePeer`] = 1 条 PeerConnection（Mesh 组网时每对端一个，见 Phase 8）；
//! - 信令面（SDP Offer/Answer + trickle candidate JSON）走字符串，与 signaling 的
//!   `SignalMessage` 一一对应；
//! - 媒体面：1 个 Opus 包 = 1 个 Sample（10ms），[`VoicePeer::send_opus`] 发送，
//!   远端帧经 channel 出队（调用方进 JB）；
//! - 暂无 interceptor（无 NACK / RTCP 反馈；丢包由 JB + PLC 覆盖，Week 7/8 再评估）；
//! - 选路经 `get_stats` 的 nominated pair 判定：host/srflx → Direct，relay → Relay。

use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc, Mutex, MutexGuard,
};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::media_engine::{MediaEngine, MIME_TYPE_OPUS};
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use voice_common::{RouteType, VoiceError};

use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
/// gathering 状态由上层（`voice-session`）判读，所以从这里透出去。
pub use webrtc::peer_connection::RTCIceGatheringState;
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceCandidateInit, RTCIceCandidateType, RTCIceServer, RTCIceTransportPolicy,
    RTCPeerConnectionIceEvent, RTCPeerConnectionState, RTCSessionDescription, RTCStatsReport,
    RTCStatsReportEntry, StatsSelector,
};

/// Opus 在 SDP 中的注册参数（RFC 7587：始终 48000Hz，channels=2 声明，mono 照发）。
const OPUS_PAYLOAD_TYPE: u8 = 111;
const OPUS_CLOCK_RATE: u32 = 48_000;

/// 队友中继 DataChannel 的 label（一条 leg 上多源复用，包络区分源）。
pub const RELAY_DC_LABEL: &str = "relay";

/// 连接配置：STUN 走 srflx，TURN 走 relay（Week 7 用本地 coturn 联调 relay）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConfig {
    pub stun_urls: Vec<String>,
    pub turn_urls: Vec<String>,
    pub turn_username: Option<String>,
    pub turn_credential: Option<String>,
    /// 只用 relay candidate（强制走 TURN，联调/排障用；正常开黑保持 false 走 P2P 优先）。
    #[serde(default)]
    pub force_relay: bool,
}

impl Default for PeerConfig {
    fn default() -> Self {
        // 多 STUN：对称 NAT 下不同目标看到不同映射，多源 = 多组 srflx 候选。
        //
        // 列表按 2026-09-15 从国内出口实测（UDP Binding Request）挑的：
        //   ✓ Google / Cloudflare / miwifi / bilibili
        //   ✗ stun.qq.com、stun.syncthing.net —— 超时，别加回来：
        //     不通的 STUN 不会报错，只会把 ICE gathering 拖到超时。
        // 可达性随网络/运营商变化，可用 VIVI_STUN_URLS（逗号分隔）整组覆盖。
        Self {
            stun_urls: vec![
                // 官方公开的 WebRTC STUN
                "stun:stun.l.google.com:19302".into(),
                "stun:stun1.l.google.com:19302".into(),
                "stun:stun.cloudflare.com:3478".into(),
                // 国内公共 STUN：第三方服务，作备用源，随时可能失效
                "stun:stun.miwifi.com:3478".into(),
                "stun:stun.chat.bilibili.com:3478".into(),
            ],
            turn_urls: vec![],
            turn_username: None,
            turn_credential: None,
            force_relay: false,
        }
    }
}

/// 连接状态（选路 Direct/Relay 另由 [`VoicePeer::refresh_route`] 给出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerState {
    New,
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
}

impl From<RTCPeerConnectionState> for PeerState {
    fn from(s: RTCPeerConnectionState) -> Self {
        match s {
            RTCPeerConnectionState::New => Self::New,
            RTCPeerConnectionState::Connecting => Self::Connecting,
            RTCPeerConnectionState::Connected => Self::Connected,
            RTCPeerConnectionState::Disconnected => Self::Disconnected,
            RTCPeerConnectionState::Failed => Self::Failed,
            RTCPeerConnectionState::Closed => Self::Closed,
            _ => Self::New,
        }
    }
}

/// 远端 SDP 种类（调 `set_remote_description` 用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdpKind {
    Offer,
    Answer,
}

/// 远端语音帧：RTP 载荷 + JB 排序/抖动计算所需的包头信息。
#[derive(Debug)]
pub struct RemoteFrame {
    /// RTP 序列号（JB 按 u32 回绕语义处理）。
    pub sequence: u32,
    /// RTP 时间戳折算毫秒（48kHz → /48；只用差值，无需时钟同步）。
    pub timestamp_ms: u64,
    /// Opus 包字节（单包单帧）。
    pub payload: Vec<u8>,
}

/// 单路媒体统计（UI 每用户 PING/JITTER/LOSS 数据源）。
#[derive(Debug, Clone, Copy, Default)]
pub struct PeerMediaStats {
    pub rtt_ms: Option<f32>,
    pub jitter_ms: f32,
    pub loss_percent: f32,
}

/// Peer 事件（均为 mpsc，调用方逐个消费）。
pub struct PeerEvents {
    /// 本地 trickle candidate（JSON 序列化的 `RTCIceCandidateInit`，直接发信令）。
    pub local_candidates: mpsc::Receiver<String>,
    /// 远端 Opus 帧（调用方按 sequence 进 JB）。
    pub remote_frames: mpsc::Receiver<RemoteFrame>,
    /// 中继包络字节（VoicePacket 线格式，源见包内 user_id；调用方按源进中继 JB）。
    pub relay_frames: mpsc::Receiver<Vec<u8>>,
    /// 连接状态变化。
    pub states: mpsc::Receiver<PeerState>,
    /// ICE gathering 状态变化。`Complete` = 本端候选已收齐，此刻的
    /// `local_description()` 才含全部 `a=candidate:` 行——非 trickle 交换的前提。
    ///
    /// 注意 webrtc-rs 的「候选结束」**不走 `on_ice_candidate`**（空候选在
    /// `rtc::peer_connection::add_local_candidate` 里被拦下，只改 gathering 状态、
    /// 只发 `OnIceGatheringStateChangeEvent`），所以想看"收齐没有"只能盯这个通道。
    pub gathering: mpsc::Receiver<RTCIceGatheringState>,
}

#[derive(Clone)]
struct Handler {
    candidates: mpsc::Sender<String>,
    frames: mpsc::Sender<RemoteFrame>,
    relays: mpsc::Sender<Vec<u8>>,
    states: mpsc::Sender<PeerState>,
    gathering: mpsc::Sender<RTCIceGatheringState>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        match event
            .candidate
            .to_json()
            .map_err(|e| anyhow::anyhow!("{e}"))
        {
            Ok(init) => match serde_json::to_string(&init) {
                Ok(s) => {
                    let _ = self.candidates.try_send(s);
                }
                Err(e) => tracing::warn!(error = %e, "candidate json failed"),
            },
            Err(e) => tracing::warn!(error = %e, "candidate to_json failed"),
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        tracing::info!(state = %state, "peer connection state");
        let _ = self.states.try_send(PeerState::from(state));
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        tracing::debug!(state = %state, "ice gathering state");
        let _ = self.gathering.try_send(state);
    }

    async fn on_data_channel(&self, dc: Arc<dyn DataChannel>) {
        let label = dc.label().await.unwrap_or_default();
        if label != RELAY_DC_LABEL {
            tracing::warn!(label = %label, "unknown data channel ignored");
            return;
        }
        let tx = self.relays.clone();
        tokio::spawn(async move {
            loop {
                match dc.poll().await {
                    Some(DataChannelEvent::OnMessage(msg)) => {
                        if !msg.is_string
                            && !msg.data.is_empty()
                            && tx.send(msg.data.to_vec()).await.is_err()
                        {
                            break;
                        }
                    }
                    Some(
                        DataChannelEvent::OnClose
                        | DataChannelEvent::OnClosing
                        | DataChannelEvent::OnError,
                    )
                    | None => break,
                    _ => {}
                }
            }
        });
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let tx = self.frames.clone();
        tokio::spawn(async move {
            loop {
                match track.poll().await {
                    Some(TrackRemoteEvent::OnRtpPacket(pkt)) => {
                        let frame = RemoteFrame {
                            sequence: pkt.header.sequence_number as u32,
                            timestamp_ms: (pkt.header.timestamp / 48) as u64,
                            payload: pkt.payload.to_vec(),
                        };
                        if tx.send(frame).await.is_err() {
                            break;
                        }
                    }
                    // 远端结束/出错/驱动关闭：退出读循环
                    Some(
                        TrackRemoteEvent::OnEnded
                        | TrackRemoteEvent::OnEnding
                        | TrackRemoteEvent::OnError,
                    )
                    | None => break,
                    _ => {}
                }
            }
        });
    }
}

static NEXT_SSRC: AtomicU32 = AtomicU32::new(0x1000_0000);

fn fresh_ssrc() -> u32 {
    let base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    base.wrapping_add(NEXT_SSRC.fetch_add(1, Ordering::Relaxed))
}

/// 一条 P2P 语音连接。`close()` 显式关闭（Drop 不保证后台驱动退出）。
pub struct VoicePeer {
    pc: Arc<dyn PeerConnection>,
    audio: Arc<TrackLocalStaticSample>,
    sender: Arc<dyn webrtc::rtp_transceiver::RtpSender>,
    ssrc: u32,
    payload_type: Mutex<Option<u8>>,
    route_cache: Mutex<Option<RouteType>>,
    relay_dc: Mutex<Option<Arc<dyn DataChannel>>>,
}

impl VoicePeer {
    /// 建 peer + 注册 Opus 发送轨。返回 peer 与事件通道。
    pub async fn new(config: &PeerConfig) -> anyhow::Result<(Arc<Self>, PeerEvents)> {
        let mut media_engine = MediaEngine::default();
        let audio_codec = RTCRtpCodecParameters {
            rtp_codec: RTCRtpCodec {
                mime_type: MIME_TYPE_OPUS.to_owned(),
                clock_rate: OPUS_CLOCK_RATE,
                channels: 2,
                sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                rtcp_feedback: vec![],
            },
            payload_type: OPUS_PAYLOAD_TYPE,
        };
        media_engine.register_codec(audio_codec.clone(), RtpCodecKind::Audio)?;

        let mut ice_servers = Vec::new();
        if !config.stun_urls.is_empty() {
            ice_servers.push(RTCIceServer {
                urls: config.stun_urls.clone(),
                username: String::new(),
                credential: String::new(),
            });
        }
        if !config.turn_urls.is_empty() {
            ice_servers.push(RTCIceServer {
                urls: config.turn_urls.clone(),
                username: config.turn_username.clone().unwrap_or_default(),
                credential: config.turn_credential.clone().unwrap_or_default(),
            });
        }

        let (cand_tx, cand_rx) = mpsc::channel(64);
        let (frame_tx, frame_rx) = mpsc::channel(256);
        let (relay_tx, relay_rx) = mpsc::channel::<Vec<u8>>(256);
        let (state_tx, state_rx) = mpsc::channel(16);
        let (gather_tx, gather_rx) = mpsc::channel(16);
        let handler = Handler {
            candidates: cand_tx,
            frames: frame_tx,
            relays: relay_tx,
            states: state_tx,
            gathering: gather_tx,
        };

        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::<&str>::new()
                .with_configuration({
                    let mut builder =
                        RTCConfigurationBuilder::default().with_ice_servers(ice_servers);
                    if config.force_relay {
                        builder = builder.with_ice_transport_policy(RTCIceTransportPolicy::Relay);
                    }
                    builder.build()
                })
                .with_media_engine(media_engine)
                .with_handler(Arc::new(handler))
                .with_udp_addrs(vec!["0.0.0.0:0"])
                .build()
                .await?,
        );

        let ssrc = fresh_ssrc();
        let audio = Arc::new(TrackLocalStaticSample::new(MediaStreamTrack::new(
            "vivi-stream".into(),
            "vivi-audio".into(),
            "audio".into(),
            RtpCodecKind::Audio,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(ssrc),
                    ..Default::default()
                },
                codec: audio_codec.rtp_codec.clone(),
                ..Default::default()
            }],
        ))?);
        let sender = pc
            .add_track(Arc::clone(&audio) as Arc<dyn TrackLocal>)
            .await?;

        // 中继通道 upfront 建好（进首个 SDP），双向各一条：
        // 发用自己建的，收走对方建的（on_data_channel），天然无冲突。
        // SCTP 握手多一两 RTT，为中继能力付的固定成本。
        let relay_dc = pc
            .create_data_channel(
                RELAY_DC_LABEL,
                Some(RTCDataChannelInit {
                    ordered: false,
                    max_retransmits: Some(0),
                    ..Default::default()
                }),
            )
            .await?;

        Ok((
            Arc::new(Self {
                pc,
                audio,
                sender,
                ssrc,
                payload_type: Mutex::new(None),
                route_cache: Mutex::new(None),
                relay_dc: Mutex::new(Some(relay_dc)),
            }),
            PeerEvents {
                local_candidates: cand_rx,
                remote_frames: frame_rx,
                relay_frames: relay_rx,
                states: state_rx,
                gathering: gather_rx,
            },
        ))
    }

    /// 建 offer 并设为 local，返回 SDP 字符串（发信令）。
    pub async fn create_offer(&self) -> anyhow::Result<String> {
        let offer = self.pc.create_offer(None).await?;
        self.pc.set_local_description(offer).await?;
        self.local_sdp().await
    }

    /// 收 offer：设 remote → 建 answer 设 local，返回 answer SDP。
    pub async fn apply_offer(&self, offer_sdp: &str) -> anyhow::Result<String> {
        self.set_remote(SdpKind::Offer, offer_sdp).await?;
        let answer = self.pc.create_answer(None).await?;
        self.pc.set_local_description(answer).await?;
        self.local_sdp().await
    }

    /// 收 answer：设 remote。
    pub async fn apply_answer(&self, answer_sdp: &str) -> anyhow::Result<()> {
        self.set_remote(SdpKind::Answer, answer_sdp).await
    }

    /// 收 trickle candidate（`RTCIceCandidateInit` JSON）。
    pub async fn add_remote_candidate(&self, json: &str) -> anyhow::Result<()> {
        let init: RTCIceCandidateInit = serde_json::from_str(json)?;
        self.pc.add_ice_candidate(init).await?;
        Ok(())
    }

    /// 发一包 Opus（10ms 一帧）。RTP 时间戳/序列号由 packetizer 按 duration 推导。
    pub async fn send_opus(&self, opus: &[u8]) -> anyhow::Result<()> {
        let pt = self.negotiated_payload_type().await?;
        self.audio
            .sample_writer(self.ssrc, pt)
            .write_sample(&Sample {
                data: Bytes::copy_from_slice(opus),
                duration: Duration::from_millis(10),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    /// 经中继通道发一个包络（调用方已按 VoicePacket 线格式组好）。
    /// 通道在建 peer 时 upfront 建好；SCTP 未就绪前发送失败，调用方重试即可。
    pub async fn send_relay(&self, envelope: &[u8]) -> anyhow::Result<()> {
        let dc = self
            .lock_relay()
            .clone()
            .ok_or_else(|| VoiceError::Network("relay channel not open".into()))?;
        dc.send(BytesMut::from(envelope))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(())
    }

    /// 用 `get_stats` 的 nominated pair 刷新选路并缓存；`None` = 尚无可用 pair。
    ///
    /// 缓存是 sticky 的：瞬时 None（远端 candidate 条目还没进 stats）不会覆盖
    /// 上次已知选路，UI 轮询显示才稳定。
    pub async fn refresh_route(&self) -> Option<RouteType> {
        let report = self.pc.get_stats(Instant::now(), StatsSelector::None).await;
        let route = selected_route(&report);
        if route.is_some() {
            *self.lock_route() = route;
        }
        route.or_else(|| *self.lock_route())
    }

    /// 上次 `refresh_route` 的结果（UI 轮询用）。
    pub fn route(&self) -> Option<RouteType> {
        *self.lock_route()
    }

    /// 单路媒体统计：nominated pair 的 RTT + 首条 audio inbound 的 jitter/loss。
    /// 本 peer 只有一条音频轨，取首条 inbound 即可。
    pub async fn media_stats(&self) -> PeerMediaStats {
        self.link_stats().await.1
    }

    /// 选路 + 媒体统计一次取全（高频轮询用，少一次 `get_stats`）。
    pub async fn link_stats(&self) -> (Option<RouteType>, PeerMediaStats) {
        let report = self.pc.get_stats(Instant::now(), StatsSelector::None).await;
        (selected_route(&report), media_from_report(&report))
    }

    /// 调试：返回 candidate pair / candidate 条目摘要（排查选路判定用）。
    pub async fn stats_debug(&self) -> Vec<String> {
        let report = self.pc.get_stats(Instant::now(), StatsSelector::None).await;
        let mut out = Vec::new();
        for entry in report.iter() {
            match entry {
                RTCStatsReportEntry::IceCandidatePair(p) => out.push(format!(
                    "pair local={} remote={} nominated={} sent={} rcvd={}",
                    p.local_candidate_id,
                    p.remote_candidate_id,
                    p.nominated,
                    p.packets_sent,
                    p.packets_received
                )),
                RTCStatsReportEntry::LocalCandidate(c) => out.push(format!(
                    "local id={} {:?} addr={:?}",
                    c.stats.id, c.candidate_type, c.address
                )),
                RTCStatsReportEntry::RemoteCandidate(c) => out.push(format!(
                    "remote id={} {:?} addr={:?}",
                    c.stats.id, c.candidate_type, c.address
                )),
                _ => {}
            }
        }
        out
    }

    pub async fn close(&self) -> anyhow::Result<()> {
        self.pc.close().await?;
        Ok(())
    }

    async fn local_sdp(&self) -> anyhow::Result<String> {
        self.pc
            .local_description()
            .await
            .map(|d| d.sdp)
            .ok_or_else(|| VoiceError::Network("no local description".into()).into())
    }

    async fn set_remote(&self, kind: SdpKind, sdp: &str) -> anyhow::Result<()> {
        let desc = match kind {
            SdpKind::Offer => RTCSessionDescription::offer(sdp.to_owned())?,
            SdpKind::Answer => RTCSessionDescription::answer(sdp.to_owned())?,
        };
        self.pc.set_remote_description(desc).await?;
        Ok(())
    }

    /// 首发前从 sender 拿协商好的 PT（双方都是我们时即 111，对异构端点也正确）。
    async fn negotiated_payload_type(&self) -> anyhow::Result<u8> {
        if let Some(pt) = *self.lock_pt() {
            return Ok(pt);
        }
        let params = self.sender.get_parameters().await?;
        let pt = params
            .rtp_parameters
            .codecs
            .first()
            .map(|c| c.payload_type)
            .ok_or_else(|| VoiceError::Network("sender has no negotiated codec".into()))?;
        *self.lock_pt() = Some(pt);
        Ok(pt)
    }

    fn lock_relay(&self) -> MutexGuard<'_, Option<Arc<dyn DataChannel>>> {
        self.relay_dc.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_pt(&self) -> MutexGuard<'_, Option<u8>> {
        self.payload_type.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_route(&self) -> MutexGuard<'_, Option<RouteType>> {
        self.route_cache.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 从单份 report 解析媒体统计（供 `link_stats` 一次取全）。
fn media_from_report(report: &RTCStatsReport) -> PeerMediaStats {
    let mut rtt_ms = None;
    for entry in report.iter() {
        if let RTCStatsReportEntry::IceCandidatePair(p) = entry {
            if p.nominated && p.responses_received > 0 {
                rtt_ms = Some((p.current_round_trip_time * 1000.0) as f32);
                break;
            }
        }
    }
    let mut jitter_ms = 0f32;
    let mut loss_percent = 0f32;
    for entry in report.iter() {
        if let RTCStatsReportEntry::InboundRtp(s) = entry {
            let base = &s.received_rtp_stream_stats;
            jitter_ms = (base.jitter * 1000.0) as f32;
            let recv = base.packets_received as f64;
            let lost = base.packets_lost.max(0) as f64;
            if recv + lost > 0.0 {
                loss_percent = (100.0 * lost / (recv + lost)) as f32;
            }
            break;
        }
    }
    PeerMediaStats {
        rtt_ms,
        jitter_ms,
        loss_percent,
    }
}

/// nominated pair 定选路：任一端 relay → Relay，否则 Direct（host/srflx/prflx）。
fn selected_route(report: &RTCStatsReport) -> Option<RouteType> {
    use std::collections::HashMap;
    fn short_id(id: &str) -> &str {
        id.strip_prefix("RTCLocalIceCandidate_")
            .or_else(|| id.strip_prefix("RTCRemoteIceCandidate_"))
            .unwrap_or(id)
    }
    let mut relay_by_id: HashMap<&str, bool> = HashMap::new();
    for entry in report.iter() {
        match entry {
            RTCStatsReportEntry::LocalCandidate(c) => {
                relay_by_id.insert(
                    short_id(&c.stats.id),
                    matches!(c.candidate_type, RTCIceCandidateType::Relay),
                );
            }
            RTCStatsReportEntry::RemoteCandidate(c) => {
                relay_by_id.insert(
                    short_id(&c.stats.id),
                    matches!(c.candidate_type, RTCIceCandidateType::Relay),
                );
            }
            _ => {}
        }
    }
    let mut idle_fallback = None;
    for entry in report.iter() {
        let RTCStatsReportEntry::IceCandidatePair(pair) = entry else {
            continue;
        };
        if !pair.nominated {
            continue;
        }
        let route = match (
            relay_by_id.get(pair.local_candidate_id.as_str()),
            relay_by_id.get(pair.remote_candidate_id.as_str()),
        ) {
            (Some(l), Some(r)) => Some(if *l || *r {
                RouteType::Relay
            } else {
                RouteType::Direct
            }),
            _ => None,
        };
        if pair.packets_received > 0 || pair.packets_sent > 0 {
            return route;
        }
        idle_fallback = idle_fallback.or(route);
    }
    idle_fallback
}
