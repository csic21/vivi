//! 手动（剪贴板）打洞：不连信令服务器，SDP/candidate 由调用方经中转信道搬。
//!
//! 为什么需要这条路径：房间模式要求两端连**同一个**信令进程，异地时那个进程
//! 在哪是没法自动解决的问题（见 `docs/NETWORK.md` 第 3 节）。但两人局真正要
//! 交换的只有两个各几 KB 的 blob —— 顺手扔进聊天框就够了，不需要任何常驻服务。
//!
//! 交换流程（两边各粘一次，`offerer` = id 小的那个）：
//!
//! ```text
//! 发起方 A (id 1)                          应答方 B (id 2)
//!   join_manual(peer = 2)                    join_manual(peer = 1)
//!   收齐候选 → 出连接码 ────────────────→ 粘进 B
//!                                           收齐候选 → 出连接码
//!   粘进 A ←────────────────────────────────┘
//!   ICE 连通，开始说话
//! ```
//!
//! **非 trickle**：ICE 候选默认是边收边发的（trickle）。这里不行——连接码得一次性
//! 成文，所以得等到候选收集告一段落再编码。判据见 [`GatherOutcome`]：配了 STUN 的
//! 真实网络里基本等不到 webrtc-rs 的正式完成信号，靠的是候选静默 [`SETTLE`]；
//! 硬超时也会把手上已有的候选发出去，标 [`GatherOutcome::TimedOut`]——能连就赚了，
//! 连不上用户至少知道该重试，而不是对着一个没反应的界面猜。
//!
//! 连接码 = `deflate + base64url`（前缀 `VIVI1.`）。压是因为原始 JSON 有 4~6KB，
//! 粘进聊天框会长到没法看；压完通常 1~2KB。用 url-safe 字母表是因为 `+` `/` `=`
//! 在部分聊天客户端里会被转义或折行。

use std::io::{Read, Write};
use std::time::Duration;

use base64::prelude::*;
use tokio::sync::mpsc;
use voice_protocol::SignalMessage;

/// 连接码前缀。带版本号，将来换格式可以并存解析。
pub const CODE_PREFIX: &str = "VIVI1.";

/// 候选收齐的兜底等待上限。等不到任何可出的东西时，也要在这么久之后出声，
/// 而不是把界面吊死。
pub const DEFAULT_GATHER_TIMEOUT: Duration = Duration::from_secs(8);

/// 候选「静默判定」窗口：距上一条候选安静了这么久，就当作收齐了。
///
/// 为什么不能只等 webrtc-rs 的 gathering 完成信号：`stun_gatherer::gather()` 只在
/// `stun_clients.is_empty()` 时立刻置 Complete，**一旦配了 STUN 服务器就得等全部
/// STUN 客户端收尾**，而公开 STUN 里总有慢的 —— 实测（2026-09-16）配了默认那组
/// STUN 之后 8s 都等不到完成信号。trickle 模式下没人察觉（候选边到边发），
/// 非 trickle 就被它拖死。
///
/// 窗口取得比典型 srflx 往返长：漏掉一个 srflx 就是完全打不通，而多等两秒在这个
/// 「切到微信去粘码」的流程里根本不算什么。
pub const SETTLE: Duration = Duration::from_secs(2);

/// `next_code` 收工的三种方式 —— 调用方据此决定要不要让用户重来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatherOutcome {
    /// 拿到了 webrtc-rs 明确的 gathering 完成信号（无 STUN 时是这条）。
    Complete,
    /// 候选静默了 [`SETTLE`]，按「稳定了」出码。配了 STUN 的真实网络里这是常态。
    Settled,
    /// 硬超时：主信令或候选可能没到齐，这份码不一定能用。
    TimedOut,
}

impl GatherOutcome {
    /// 这份码值不值得让对方试。
    pub fn is_usable(self) -> bool {
        !matches!(self, Self::TimedOut)
    }
}

/// 手动打洞的出站事件。
#[derive(Debug, Clone)]
pub enum ManualEvent {
    /// 需要对端收到的信令。**按到达顺序**消费即可：Offer/Answer 一定排在
    /// 它自己的 candidates 前面，对端按序注入就不会出现"给不存在的 peer 加候选"。
    Signal(SignalMessage),
    /// 本端候选已收齐 —— 此刻起到下一个信令之前收集到的，就是完整的一份。
    GatheringComplete,
}

/// 手动打洞的信令通道：出站事件由调用方攒成连接码，入站码由调用方注入。
///
/// 房间模式没有这个东西（`Session::join` 不会返回它）。
pub struct ManualLink {
    events: mpsc::UnboundedReceiver<ManualEvent>,
    inbound: mpsc::Sender<SignalMessage>,
}

impl ManualLink {
    pub(crate) fn new(
        events: mpsc::UnboundedReceiver<ManualEvent>,
        inbound: mpsc::Sender<SignalMessage>,
    ) -> Self {
        Self { events, inbound }
    }

    /// 把对端给的连接码注入本端。返回注入了几条信令。
    ///
    /// 顺序即码里的顺序，逐条 `await` 送到主任务，所以不会和本端的 set_remote
    /// 抢跑（主任务单线程串行处理）。
    pub async fn feed(&self, code: &str) -> anyhow::Result<usize> {
        let msgs = decode(code)?;
        let n = msgs.len();
        for m in msgs {
            self.inbound
                .send(m)
                .await
                .map_err(|_| anyhow::anyhow!("session already gone"))?;
        }
        Ok(n)
    }

    /// 攒出一个连接码：从事件流里收信令，直到候选收齐（或超时）。
    ///
    /// 返回 `(连接码, 候选是否收齐)`。超时的情况下码里是"已有的候选"，
    /// 能连通就通，不通则调用方应提示重来一次。
    pub async fn next_code(
        &mut self,
        timeout: Duration,
    ) -> anyhow::Result<(String, GatherOutcome)> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut collected: Vec<SignalMessage> = Vec::new();
        let mut have_primary = false;
        let mut gathering_done = false;
        let mut outcome = None;
        loop {
            // 主信令和完成信号都在手 —— 明确收齐了，不必再等静默窗口
            if have_primary && gathering_done {
                outcome = Some(GatherOutcome::Complete);
                break;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                break;
            }
            // 静默窗口：每收到一条信令就被重置，所以它衡量的是「距上一条候选多久」
            match tokio::time::timeout(SETTLE.min(left), self.events.recv()).await {
                Ok(Some(ManualEvent::Signal(m))) => {
                    if is_primary(&m) {
                        have_primary = true;
                    }
                    if is_exchangeable(&m) {
                        collected.push(m);
                    }
                }
                // 完成信号可能比主信令先到（三个任务各送各的），所以只记下不急着收工
                Ok(Some(ManualEvent::GatheringComplete)) => gathering_done = true,
                // 发送端全 drop 了 = 会话已结束
                Ok(None) => anyhow::bail!("session ended while collecting connection code"),
                // 静默窗口走完
                Err(_) => {
                    if have_primary && collected.iter().any(is_candidate) {
                        outcome = Some(GatherOutcome::Settled);
                        break;
                    }
                    // 主信令或首个候选还没到，继续等 —— 由 deadline 兜底，
                    // 一份没有候选的码对谁都没用，提前出没有意义
                }
            }
        }
        if collected.is_empty() {
            anyhow::bail!("no signaling produced; peer session never started");
        }
        // 主信令排最前，且保持同类之间的相对顺序（稳定排序）：对端必须先
        // set_remote_description 才能收 candidate，而两类消息在事件流里
        // 是不同任务送来的，谁先谁后说不准。
        collected.sort_by_key(|m| u8::from(!is_primary(m)));
        Ok((
            encode(&collected)?,
            outcome.unwrap_or(GatherOutcome::TimedOut),
        ))
    }
}

/// 主信令：先有它才有 peer 槽位，candidate 才有地方落。
fn is_primary(m: &SignalMessage) -> bool {
    matches!(
        m,
        SignalMessage::Offer { .. } | SignalMessage::Answer { .. }
    )
}

fn is_candidate(m: &SignalMessage) -> bool {
    matches!(m, SignalMessage::IceCandidate { .. })
}

/// 只有这三种会在两端之间搬：其余（JoinRoom/PeerJoined/中继协商）都是房间模式
/// 的服务器语义，手动模式没有对应物，混进连接码只会让对端困惑。
fn is_exchangeable(m: &SignalMessage) -> bool {
    matches!(
        m,
        SignalMessage::Offer { .. }
            | SignalMessage::Answer { .. }
            | SignalMessage::IceCandidate { .. }
    )
}

/// 信令列表 → 连接码。
pub fn encode(msgs: &[SignalMessage]) -> anyhow::Result<String> {
    let json = serde_json::to_vec(msgs)?;
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(&json)?;
    let packed = enc.finish()?;
    Ok(format!(
        "{CODE_PREFIX}{}",
        BASE64_URL_SAFE_NO_PAD.encode(packed)
    ))
}

/// 连接码 → 信令列表。
///
/// 解析写得很宽容：空白（换行/空格，聊天框和终端都爱插）先全部剥掉再解码——
/// 让用户为了粘一段码还得先手工删换行是不可接受的。
pub fn decode(code: &str) -> anyhow::Result<Vec<SignalMessage>> {
    let cleaned: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    let body = cleaned
        .strip_prefix(CODE_PREFIX)
        .ok_or_else(|| anyhow::anyhow!("不是 Vivi 连接码（应以 {CODE_PREFIX} 开头）"))?;
    let packed = BASE64_URL_SAFE_NO_PAD
        .decode(body)
        .map_err(|e| anyhow::anyhow!("连接码损坏（base64 解不开）：{e}"))?;
    let mut json = Vec::new();
    flate2::read::ZlibDecoder::new(&packed[..])
        .read_to_end(&mut json)
        .map_err(|e| anyhow::anyhow!("连接码损坏（解压失败）：{e}"))?;
    let msgs: Vec<SignalMessage> =
        serde_json::from_slice(&json).map_err(|e| anyhow::anyhow!("连接码内容不合法：{e}"))?;
    if msgs.is_empty() {
        anyhow::bail!("连接码里没有信令");
    }
    if !msgs.iter().all(is_exchangeable) {
        anyhow::bail!("连接码里有本模式不认识的条目");
    }
    Ok(msgs)
}

/// 连接码里各类候选的数量。
///
/// 这是**给用户看的诊断**，不是内部状态：跨 NAT 全靠 srflx/relay，一个都没有
/// 就是打不通。与其让人对着一个连不上的界面猜，不如把这句话直接摆在连接码旁边。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CandidateSummary {
    pub host: usize,
    pub srflx: usize,
    pub relay: usize,
    pub other: usize,
    pub sdp_bytes: usize,
}

impl CandidateSummary {
    /// 有没有能穿透 NAT 的候选。全 host 意味着只有同一个局域网能用。
    pub fn can_cross_nat(&self) -> bool {
        self.srflx > 0 || self.relay > 0
    }

    /// 一句话摘要（直接进 UI）。
    pub fn describe(&self) -> String {
        let base = format!(
            "候选 host={} srflx={} relay={}",
            self.host, self.srflx, self.relay
        );
        if self.other > 0 {
            format!("{base} 其他={}", self.other)
        } else {
            base
        }
    }
}

/// 解一段连接码，数一下里面有哪些候选。
pub fn summarize_code(code: &str) -> anyhow::Result<CandidateSummary> {
    let msgs = decode(code)?;
    let mut s = CandidateSummary::default();
    for m in &msgs {
        match m {
            SignalMessage::Offer { sdp, .. } | SignalMessage::Answer { sdp, .. } => {
                s.sdp_bytes = sdp.len();
            }
            SignalMessage::IceCandidate { candidate, .. } => {
                // 候选本身是 JSON 化的 RTCIceCandidateInit，类型写在 candidate 串里
                if candidate.contains(" typ relay") {
                    s.relay += 1;
                } else if candidate.contains(" typ srflx") {
                    s.srflx += 1;
                } else if candidate.contains(" typ host") {
                    s.host += 1;
                } else {
                    s.other += 1;
                }
            }
            _ => {}
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use voice_common::UserId;

    fn sample() -> Vec<SignalMessage> {
        vec![
            SignalMessage::Offer {
                from: UserId(1),
                to: UserId(2),
                sdp: "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\n".into(),
            },
            SignalMessage::IceCandidate {
                from: UserId(1),
                to: UserId(2),
                candidate:
                    r#"{"candidate":"candidate:1 1 udp 2130706431 192.168.1.5 54321 typ host"}"#
                        .into(),
            },
            SignalMessage::IceCandidate {
                from: UserId(1),
                to: UserId(2),
                candidate:
                    r#"{"candidate":"candidate:2 1 udp 1694498815 1.2.3.4 54321 typ srflx"}"#.into(),
            },
        ]
    }

    #[test]
    fn code_roundtrips() {
        let msgs = sample();
        let code = encode(&msgs).unwrap();
        let back = decode(&code).unwrap();
        assert_eq!(back.len(), msgs.len());
        assert!(matches!(back[0], SignalMessage::Offer { .. }));
    }

    #[test]
    fn code_survives_chat_whitespace() {
        // 聊天框/终端常插换行，粘回来必须还能认
        let code = encode(&sample()).unwrap();
        let mut mangled = String::new();
        for (i, c) in code.chars().enumerate() {
            if i > 0 && i % 40 == 0 {
                mangled.push('\n');
            }
            mangled.push(c);
        }
        mangled.push_str("\n  ");
        assert_eq!(decode(&mangled).unwrap().len(), 3);
    }

    #[test]
    fn code_is_shorter_than_raw_json() {
        // deflate 的意义就在这儿：不压的话粘进聊天框没法看
        let msgs = sample();
        let raw = serde_json::to_vec(&msgs).unwrap();
        let code = encode(&msgs).unwrap();
        assert!(
            code.len() < raw.len(),
            "code {} should beat raw json {}",
            code.len(),
            raw.len()
        );
    }

    #[test]
    fn rejects_foreign_and_corrupt_codes() {
        assert!(decode("vivi://a1b2c3d4@1.2.3.4:8080").is_err());
        assert!(decode("VIVI1.!!!!not-base64!!!!").is_err());
        assert!(decode("").is_err());
    }

    #[tokio::test]
    async fn primary_signal_lands_first_even_when_events_arrive_backwards() {
        // 最坏顺序：candidate 先到 → gathering 完成 → offer 最后才到。
        // 三个任务各送各的，真实运行时就是这个次序没有保证。
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (in_tx, _in_rx) = mpsc::channel(8);
        let mut link = ManualLink::new(ev_rx, in_tx);

        let msgs = sample();
        let (offer, cands) = (msgs[0].clone(), msgs[1..].to_vec());
        for c in &cands {
            ev_tx.send(ManualEvent::Signal(c.clone())).unwrap();
        }
        ev_tx.send(ManualEvent::GatheringComplete).unwrap();
        ev_tx.send(ManualEvent::Signal(offer)).unwrap();

        let (code, outcome) = link.next_code(Duration::from_secs(1)).await.unwrap();
        assert_eq!(outcome, GatherOutcome::Complete);
        let back = decode(&code).unwrap();
        assert_eq!(back.len(), 1 + cands.len(), "不该丢消息");
        assert!(
            matches!(back[0], SignalMessage::Offer { .. }),
            "offer 必须排到 candidate 前面（对端要先 set_remote_description）"
        );
    }

    #[tokio::test]
    async fn settles_when_webrtc_never_reports_gathering_complete() {
        // 配了 STUN 的真实网络就长这样：永远等不到明确的完成信号，
        // 只能靠「安静了这么久」收工。这是主要路径，不是边角料。
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (in_tx, _in_rx) = mpsc::channel(8);
        let mut link = ManualLink::new(ev_rx, in_tx);
        let msgs = sample();
        ev_tx.send(ManualEvent::Signal(msgs[0].clone())).unwrap();
        ev_tx.send(ManualEvent::Signal(msgs[1].clone())).unwrap();
        // 之后一直安静（没有 GatheringComplete）

        let started = std::time::Instant::now();
        let (code, outcome) = link.next_code(Duration::from_secs(5)).await.unwrap();
        assert_eq!(outcome, GatherOutcome::Settled);
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "应当静默窗口一到就出码，而不是耗到硬超时"
        );
        assert!(outcome.is_usable());
        assert_eq!(decode(&code).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn times_out_instead_of_hanging_when_no_candidate_ever_arrives() {
        // 一份没有候选的码对谁都没用，所以这时不该"假装稳定"提前出，
        // 但也不能把界面吊死 —— 到点照样出码，如实标 TimedOut
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (in_tx, _in_rx) = mpsc::channel(8);
        let mut link = ManualLink::new(ev_rx, in_tx);
        ev_tx
            .send(ManualEvent::Signal(sample()[0].clone()))
            .unwrap();

        let (code, outcome) = link.next_code(Duration::from_millis(120)).await.unwrap();
        assert_eq!(outcome, GatherOutcome::TimedOut);
        assert!(!outcome.is_usable(), "这种情况 CLI 要提示用户重试");
        assert!(decode(&code).is_ok());
    }

    #[test]
    fn summary_counts_candidates_by_type() {
        let s = summarize_code(&encode(&sample()).unwrap()).unwrap();
        assert_eq!((s.host, s.srflx, s.relay, s.other), (1, 1, 0, 0));
        assert!(s.sdp_bytes > 0, "SDP 长度也该数出来");
        assert!(s.can_cross_nat(), "有 srflx 就该判定为能穿 NAT");
    }

    #[test]
    fn summary_without_srflx_flags_unreachable() {
        // 只有 host 候选 = 出了局域网就没用，UI 要据此劝退
        let only_host = vec![sample()[0].clone(), sample()[1].clone()];
        let s = summarize_code(&encode(&only_host).unwrap()).unwrap();
        assert_eq!(s.srflx, 0);
        assert!(!s.can_cross_nat());
    }

    #[test]
    fn rejects_room_only_messages() {
        // 房间语义的消息不该出现在连接码里
        let bad = vec![SignalMessage::PeerJoined { user_id: UserId(9) }];
        assert!(decode(&encode(&bad).unwrap()).is_err());
    }
}
