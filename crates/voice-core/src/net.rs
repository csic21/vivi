//! Phase 3：UDP 点对点语音链路（Mic→Opus→UDP→Opus→Spk 的中间一段）。
//!
//! - 语音包：seq / timestamp / loss / jitter 统计；控制包 Ping/Pong 测 RTT；
//! - 只认预设 peer（Phase 5 ICE 之前的简化版；`set_peer` 可随时更新，已为 ICE 预留）；
//! - 统计经短临界区 Mutex 共享，await 期间绝不持锁。

use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicU32, AtomicU64, Ordering},
    Arc, Mutex, MutexGuard,
};
use std::time::Instant;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use voice_common::{unix_ms, NetworkStats, RouteType, VoiceError};
use voice_protocol::{
    decode_view, ControlPacket, VoicePacket, CONTROL_PACKET_VERSION, MAX_UDP_DATAGRAM,
    VOICE_PACKET_VERSION,
};

use super::stats::{JitterEstimator, RttEstimator, SeqTracker};

/// 从收包循环递交给解码侧的语音帧。
#[derive(Debug)]
pub struct ReceivedVoice {
    pub user_id: u64,
    pub sequence: u32,
    pub timestamp_ms: u64,
    pub payload: Vec<u8>,
}

/// 对外快照（UI / 联调打印用）。
#[derive(Debug, Clone, Copy, Default)]
pub struct LinkSnapshot {
    pub rtt_ms: Option<f32>,
    pub jitter_ms: f32,
    pub loss_percent: f32,
    pub voice_tx: u64,
    pub voice_rx: u64,
}

impl LinkSnapshot {
    /// 转 UI 用的 [`NetworkStats`]；RTT 未测出时填 `u32::MAX`，UI 渲染为 "—"。
    pub fn to_network_stats(&self, route: RouteType) -> NetworkStats {
        NetworkStats {
            rtt_ms: self.rtt_ms.map(|r| r as u32).unwrap_or(u32::MAX),
            jitter_ms: self.jitter_ms as u32,
            loss_percent: self.loss_percent,
            route: Some(route),
        }
    }
}

#[derive(Debug, Default)]
struct LinkState {
    rtt: RttEstimator,
    loss: SeqTracker,
    jitter: JitterEstimator,
    last_sender_ts: Option<u64>,
    last_arrival: Option<Instant>,
    ping_seq: u32,
    ping_outstanding: Option<(u32, u64)>,
    foreign: u64,
    malformed: u64,
}

pub struct UdpVoiceLink {
    socket: UdpSocket,
    peer: Mutex<Option<SocketAddr>>,
    user_id: u64,
    seq: AtomicU32,
    voice_tx: AtomicU64,
    voice_rx: AtomicU64,
    state: Mutex<LinkState>,
}

impl UdpVoiceLink {
    /// 绑定本地地址；对端稍后用 [`UdpVoiceLink::set_peer`] 指定（ICE 就绪即更新）。
    pub async fn bind(local: SocketAddr, user_id: u64) -> anyhow::Result<Arc<Self>> {
        let socket = UdpSocket::bind(local).await?;
        Ok(Arc::new(Self {
            socket,
            peer: Mutex::new(None),
            user_id,
            seq: AtomicU32::new(0),
            voice_tx: AtomicU64::new(0),
            voice_rx: AtomicU64::new(0),
            state: Mutex::new(LinkState::default()),
        }))
    }

    /// 本地实际地址（bind 端口 0 时用它告诉对端）。
    pub fn local_addr(&self) -> anyhow::Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    /// 指定 / 更新对端；收包只认该地址（陌生源计数后丢弃）。
    pub fn set_peer(&self, peer: SocketAddr) {
        *self.lock_peer() = Some(peer);
    }

    /// 发送一帧 Opus（调用方已编码好）。热路径：栈缓冲零分配。
    pub async fn send_voice(&self, opus: &[u8]) -> anyhow::Result<()> {
        let peer = self.peer_addr()?;
        let mut buf = [0u8; MAX_UDP_DATAGRAM];
        let n = VoicePacket::encode_parts(
            self.user_id,
            self.seq.fetch_add(1, Ordering::Relaxed),
            unix_ms(),
            opus,
            &mut buf,
        )
        .ok_or_else(|| VoiceError::Network("opus frame too large for datagram".into()))?;
        self.socket.send_to(&buf[..n], peer).await?;
        self.voice_tx.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// 发 Ping 测 RTT（建议 2s 一次，由调用方定时）。
    pub async fn send_ping(&self) -> anyhow::Result<()> {
        let peer = self.peer_addr()?;
        let now = unix_ms();
        let seq = {
            let mut st = self.lock_state();
            st.ping_seq = st.ping_seq.wrapping_add(1);
            st.ping_outstanding = Some((st.ping_seq, now));
            st.ping_seq
        };
        let mut buf = [0u8; ControlPacket::WIRE_LEN];
        let n = ControlPacket::Ping { seq, send_ms: now }
            .encode_into(&mut buf)
            .ok_or_else(|| VoiceError::Network("control buffer too small".into()))?;
        self.socket.send_to(&buf[..n], peer).await?;
        Ok(())
    }

    /// 收包循环（spawn 到后台）：语音帧进 `tx`，Ping 回 Pong，Pong 更新 RTT。
    /// `tx` 关闭或 socket 致命错误时退出。
    pub async fn recv_loop(self: Arc<Self>, tx: mpsc::Sender<ReceivedVoice>) {
        let mut buf = [0u8; MAX_UDP_DATAGRAM];
        loop {
            let (n, src) = match self.socket.recv_from(&mut buf).await {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!(error = %e, "udp recv error; loop exits");
                    break;
                }
            };
            if self.peer_addr_opt() != Some(src) {
                self.lock_state().foreign += 1;
                continue;
            }
            let Some(&kind) = buf[..n].first() else {
                self.lock_state().malformed += 1;
                continue;
            };
            match kind {
                VOICE_PACKET_VERSION => {
                    let msg = {
                        let mut st = self.lock_state();
                        match decode_view(&buf[..n]) {
                            None => {
                                st.malformed += 1;
                                None
                            }
                            Some(v) => {
                                st.loss.on_packet(v.sequence);
                                let now = Instant::now();
                                if let (Some(prev_ts), Some(prev_now)) =
                                    (st.last_sender_ts, st.last_arrival)
                                {
                                    let arrival_ms =
                                        now.duration_since(prev_now).as_secs_f64() * 1000.0;
                                    let sender_ms =
                                        v.timestamp_ms.wrapping_sub(prev_ts) as i64 as f64;
                                    st.jitter.observe(arrival_ms - sender_ms);
                                }
                                st.last_sender_ts = Some(v.timestamp_ms);
                                st.last_arrival = Some(now);
                                Some(ReceivedVoice {
                                    user_id: v.user_id,
                                    sequence: v.sequence,
                                    timestamp_ms: v.timestamp_ms,
                                    payload: v.payload.to_vec(),
                                })
                            }
                        }
                    };
                    if let Some(msg) = msg {
                        self.voice_rx.fetch_add(1, Ordering::Relaxed);
                        if tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                }
                CONTROL_PACKET_VERSION => {
                    let reply = {
                        let mut st = self.lock_state();
                        match ControlPacket::decode(&buf[..n]) {
                            None => {
                                st.malformed += 1;
                                None
                            }
                            Some(ControlPacket::Ping { seq, send_ms }) => {
                                Some(ControlPacket::Pong { seq, send_ms })
                            }
                            Some(ControlPacket::Pong { seq, send_ms }) => {
                                if st.ping_outstanding == Some((seq, send_ms)) {
                                    st.rtt.observe(unix_ms().wrapping_sub(send_ms) as f32);
                                    st.ping_outstanding = None;
                                }
                                None
                            }
                        }
                    };
                    if let Some(rep) = reply {
                        let mut cbuf = [0u8; ControlPacket::WIRE_LEN];
                        if let Some(n) = rep.encode_into(&mut cbuf) {
                            if self.socket.send_to(&cbuf[..n], src).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                _ => {
                    self.lock_state().malformed += 1;
                }
            }
        }
    }

    /// 当前统计快照。
    pub fn snapshot(&self) -> LinkSnapshot {
        let st = self.lock_state();
        LinkSnapshot {
            rtt_ms: st.rtt.srtt_ms(),
            jitter_ms: st.jitter.jitter_ms(),
            loss_percent: st.loss.loss_percent(),
            voice_tx: self.voice_tx.load(Ordering::Relaxed),
            voice_rx: self.voice_rx.load(Ordering::Relaxed),
        }
    }

    fn peer_addr(&self) -> anyhow::Result<SocketAddr> {
        self.peer_addr_opt()
            .ok_or_else(|| VoiceError::Network("peer not set; call set_peer first".into()).into())
    }

    fn peer_addr_opt(&self) -> Option<SocketAddr> {
        *self.lock_peer()
    }

    fn lock_peer(&self) -> MutexGuard<'_, Option<SocketAddr>> {
        self.peer.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_state(&self) -> MutexGuard<'_, LinkState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn link_should_exchange_voice_and_measure_rtt() {
        let a = UdpVoiceLink::bind("127.0.0.1:0".parse().unwrap(), 1)
            .await
            .unwrap();
        let b = UdpVoiceLink::bind("127.0.0.1:0".parse().unwrap(), 2)
            .await
            .unwrap();
        a.set_peer(b.local_addr().unwrap());
        b.set_peer(a.local_addr().unwrap());

        let (tx_a, rx_a) = mpsc::channel(64);
        let (tx_b, mut rx_b) = mpsc::channel(64);
        let ha = tokio::spawn(Arc::clone(&a).recv_loop(tx_a));
        let hb = tokio::spawn(Arc::clone(&b).recv_loop(tx_b));

        a.send_voice(b"hello").await.unwrap();
        a.send_voice(b"world").await.unwrap();
        let m1 = tokio::time::timeout(Duration::from_secs(2), rx_b.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(m1.payload, b"hello");
        assert_eq!(m1.user_id, 1);
        let m2 = tokio::time::timeout(Duration::from_secs(2), rx_b.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(m2.payload, b"world");

        a.send_ping().await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let snap = a.snapshot();
        assert!(snap.rtt_ms.is_some());
        assert_eq!(snap.loss_percent, 0.0);
        assert_eq!(snap.voice_tx, 2);
        assert_eq!(b.snapshot().voice_rx, 2);
        let _ = rx_a;

        ha.abort();
        hb.abort();
    }
}
