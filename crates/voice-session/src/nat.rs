//! 本机 NAT 类型判定：回答「打洞这条路值不值得走」。
//!
//! 手法是经典的 STUN 多目标比对：**从同一个本地 UDP socket** 向多个 STUN
//! 服务器发 Binding Request，比对各自看到的出口映射。
//!
//! - 出口 `IP:port` 全都一样 → 锥型 NAT。对方只要知道这个映射就能把包打进来。
//! - **出口端口随目标变 → 对称型 NAT**。对方无从预测该往哪个端口打，
//!   打洞必失败，只能上 TURN。这是个"一票否决"的结论，值得在让用户折腾之前
//!   先告诉他。
//!
//! ⚠️ 关键细节：**必须复用同一个本地 socket**。换 socket 等于换了 src port，
//! 锥型 NAT 也会给出不同的映射，从而被误判成对称型。同理，目标 IP 也得够分散 ——
//! 两台 STUN 落在同一个 IP 上时，对称 NAT 也会给出相同的映射。
//!
//! 判的是**本机这一侧**。锥型只说明"你这边有戏"，对方那侧还得看他自己；
//! 但对称型是决定性的：不用等队友了。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

const STUN_MAGIC: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];
const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_SUCCESS: u16 = 0x0101;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const FAMILY_IPV4: u8 = 0x01;

/// 单台 STUN 的等待上限。判定要在用户开房/入会前跑，不能磨蹭。
pub const PER_SERVER: Duration = Duration::from_millis(800);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatKind {
    /// 各目标看到同一个出口映射 —— 打洞有戏。
    Cone { external: SocketAddr, probed: usize },
    /// 出口端口随目标变化 —— 对称 NAT，打洞必失败，只能上 TURN。
    Symmetric {
        probed: usize,
        seen: Vec<SocketAddr>,
    },
    /// 样本不够（STUN 不通 / 只回了一台 / 目标 IP 撞一起），判不了。
    /// **不是"没问题"**，只是测不出来。
    Unknown { responded: usize },
}

impl NatKind {
    /// 能不能给出结论（`Unknown` 除外）。
    pub fn is_conclusive(&self) -> bool {
        !matches!(self, Self::Unknown { .. })
    }

    /// 打洞还有没有戏。`Unknown` 时保守地返回 `true` —— 测不出来不等于打不通，
    /// 不该因为探测失败就劝退用户。
    pub fn punching_may_work(&self) -> bool {
        !matches!(self, Self::Symmetric { .. })
    }
}

impl std::fmt::Display for NatKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cone { external, probed } => {
                write!(f, "锥型 NAT（{probed} 台 STUN 都看到 {external}）→ 打洞有戏")
            }
            Self::Symmetric { probed, seen } => write!(
                f,
                "对称 NAT（{probed} 台 STUN 看到 {} 个不同出口）→ 打洞打不通，只能靠 TURN",
                seen.len()
            ),
            Self::Unknown { responded } => write!(
                f,
                "判不出（{responded} 台 STUN 回话；至少要 2 台、且出口 IP 不同）→ 测不出来不等于打不通"
            ),
        }
    }
}

/// 一次探测的原始结果：问的是哪个出口 IP，看到的是哪个映射。
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub target: IpAddr,
    pub mapped: SocketAddr,
}

/// 纯判定逻辑（单测直接喂样本，不碰网络）。
///
/// 不足 2 台回话、或回话的目标 IP 全撞在一起时判 `Unknown` —— 后者是因为
/// 目标 IP 相同时对称 NAT 也会给出同一个映射，拿这种样本下结论会把对称型
/// 误判成锥型，那是最坏的一种错（用户会一直试一个永远打不通的东西）。
pub fn classify_samples(samples: &[Sample]) -> NatKind {
    let distinct_targets: Vec<IpAddr> =
        samples
            .iter()
            .map(|s| s.target)
            .fold(Vec::new(), |mut acc, ip| {
                if !acc.contains(&ip) {
                    acc.push(ip);
                }
                acc
            });
    if samples.len() < 2 || distinct_targets.len() < 2 {
        return NatKind::Unknown {
            responded: samples.len(),
        };
    }

    let mut seen: Vec<SocketAddr> = Vec::new();
    for s in samples {
        if !seen.contains(&s.mapped) {
            seen.push(s.mapped);
        }
    }
    match seen.as_slice() {
        [only] => NatKind::Cone {
            external: *only,
            probed: samples.len(),
        },
        _ => NatKind::Symmetric {
            probed: samples.len(),
            seen,
        },
    }
}

/// 跑一轮探测并给出判定。
///
/// **同一个 socket 走完全程**：src port 不变，差异才只来自 NAT 对目标的处理，
/// 这正是要测的东西。
pub async fn classify(stun_urls: &[String], per_server: Duration) -> NatKind {
    let Ok(sock) = tokio::net::UdpSocket::bind("0.0.0.0:0").await else {
        return NatKind::Unknown { responded: 0 };
    };
    let mut samples = Vec::new();
    for url in stun_urls {
        let Some(target) = resolve(url).await else {
            continue;
        };
        if let Some(mapped) = probe_once(&sock, target, per_server).await {
            samples.push(Sample {
                target: target.ip(),
                mapped,
            });
        }
    }
    classify_samples(&samples)
}

/// 复用给定 socket 问一台 STUN。
///
/// `connect()` 会重设默认对端，但**不会改动已经 bind 好的本地端口**，
/// 所以每轮换目标问是安全的：src port 自始至终是同一个。
async fn probe_once(
    sock: &tokio::net::UdpSocket,
    target: SocketAddr,
    timeout: Duration,
) -> Option<SocketAddr> {
    sock.connect(target).await.ok()?;
    let txn = txn_id();
    sock.send(&binding_request(&txn)).await.ok()?;
    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(timeout, sock.recv(&mut buf))
        .await
        .ok()?
        .ok()?;
    parse_mapped(&buf[..n], &txn)
}

async fn resolve(url: &str) -> Option<SocketAddr> {
    let hostport = url.trim().strip_prefix("stun:").unwrap_or(url.trim());
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (hostport, 3478),
    };
    tokio::net::lookup_host((host, port))
        .await
        .ok()?
        .find(|a| a.is_ipv4())
}

fn binding_request(txn: &[u8; 12]) -> [u8; 20] {
    let mut msg = [0u8; 20];
    msg[0..2].copy_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
    // length = 0：不带属性
    msg[4..8].copy_from_slice(&STUN_MAGIC);
    msg[8..20].copy_from_slice(txn);
    msg
}

/// 12 字节事务 ID。只要唯一即可，不要求密码学随机。
fn txn_id() -> [u8; 12] {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let stack = &nanos as *const u64 as u64;
    let mut state = nanos ^ stack.rotate_left(17) ^ 0x2545_F491_4F6C_DD1D;
    let mut out = [0u8; 12];
    for b in out.iter_mut() {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        *b = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as u8;
    }
    out
}

/// 从 Binding Success 里取出口映射（**IP 和端口都要** —— 端口才是判对称型的依据）。
fn parse_mapped(msg: &[u8], txn: &[u8; 12]) -> Option<SocketAddr> {
    if msg.len() < 20 || u16::from_be_bytes([msg[0], msg[1]]) != STUN_BINDING_SUCCESS {
        return None;
    }
    if msg[8..20] != txn[..] {
        return None;
    }
    let attr_len = u16::from_be_bytes([msg[2], msg[3]]) as usize;
    let mut pos = 20usize;
    let end = (20 + attr_len).min(msg.len());
    while pos + 4 <= end {
        let ty = u16::from_be_bytes([msg[pos], msg[pos + 1]]);
        let len = u16::from_be_bytes([msg[pos + 2], msg[pos + 3]]) as usize;
        let value_start = pos + 4;
        let value_end = value_start + len;
        if value_end > msg.len() {
            return None;
        }
        if (ty == ATTR_XOR_MAPPED_ADDRESS || ty == ATTR_MAPPED_ADDRESS)
            && len >= 8
            && msg[value_start + 1] == FAMILY_IPV4
        {
            let v = &msg[value_start..value_end];
            let raw_port = u16::from_be_bytes([v[2], v[3]]);
            let raw = [v[4], v[5], v[6], v[7]];
            return Some(if ty == ATTR_XOR_MAPPED_ADDRESS {
                SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(
                        raw[0] ^ STUN_MAGIC[0],
                        raw[1] ^ STUN_MAGIC[1],
                        raw[2] ^ STUN_MAGIC[2],
                        raw[3] ^ STUN_MAGIC[3],
                    )),
                    raw_port ^ 0x2112,
                )
            } else {
                SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3])),
                    raw_port,
                )
            });
        }
        // 属性值按 4 字节对齐
        pos = value_start + len.div_ceil(4) * 4;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(target: &str, mapped: &str) -> Sample {
        Sample {
            target: target.parse().unwrap(),
            mapped: mapped.parse().unwrap(),
        }
    }

    #[test]
    fn same_mapping_across_targets_is_cone() {
        // 三个不同出口 IP 看到同一个 203.0.113.7:54321 → 锥型
        let kind = classify_samples(&[
            s("1.1.1.1", "203.0.113.7:54321"),
            s("2.2.2.2", "203.0.113.7:54321"),
            s("3.3.3.3", "203.0.113.7:54321"),
        ]);
        assert!(matches!(kind, NatKind::Cone { .. }), "{kind:?}");
        assert!(kind.punching_may_work());
    }

    #[test]
    fn port_changes_are_symmetric() {
        // 同样三个目标，端口各不同 → 对称型，一票否决
        let kind = classify_samples(&[
            s("1.1.1.1", "203.0.113.7:54321"),
            s("2.2.2.2", "203.0.113.7:54322"),
            s("3.3.3.3", "203.0.113.7:54323"),
        ]);
        assert!(matches!(kind, NatKind::Symmetric { .. }), "{kind:?}");
        assert!(!kind.punching_may_work());
    }

    #[test]
    fn fewer_than_two_replies_is_unknown() {
        let kind = classify_samples(&[s("1.1.1.1", "203.0.113.7:54321")]);
        assert_eq!(kind, NatKind::Unknown { responded: 1 });
        assert!(!kind.is_conclusive());
        // 测不出来不等于打不通，不该劝退
        assert!(kind.punching_may_work());
    }

    #[test]
    fn same_target_ip_cannot_conclude() {
        // 两台 STUN 落在同一个 IP 上：对称 NAT 也会给出同一个映射，
        // 拿它下结论会把对称型误判成锥型 —— 最坏的一种错，宁可说"判不出"
        let kind = classify_samples(&[
            s("1.1.1.1", "203.0.113.7:54321"),
            s("1.1.1.1", "203.0.113.7:54321"),
        ]);
        assert_eq!(kind, NatKind::Unknown { responded: 2 }, "{kind:?}");
    }

    #[test]
    fn xor_mapped_address_decodes_ip_and_port() {
        // 手搓 Binding Success：出口 203.0.113.7:54321
        let ip = Ipv4Addr::new(203, 0, 113, 7);
        let port: u16 = 54321;
        let txn = [7u8; 12];
        let mut msg = Vec::new();
        msg.extend_from_slice(&STUN_BINDING_SUCCESS.to_be_bytes());
        msg.extend_from_slice(&12u16.to_be_bytes());
        msg.extend_from_slice(&STUN_MAGIC);
        msg.extend_from_slice(&txn);
        msg.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        msg.extend_from_slice(&8u16.to_be_bytes());
        msg.push(0);
        msg.push(FAMILY_IPV4);
        msg.extend_from_slice(&(port ^ 0x2112).to_be_bytes());
        for (i, b) in ip.octets().iter().enumerate() {
            msg.push(b ^ STUN_MAGIC[i]);
        }
        assert_eq!(
            parse_mapped(&msg, &txn),
            Some(SocketAddr::new(IpAddr::V4(ip), port))
        );
    }

    #[test]
    fn wrong_transaction_id_is_rejected() {
        // 防串包：事务 ID 对不上就当没收到（同一 socket 会收到多个目标的回包）
        let mut msg = Vec::new();
        msg.extend_from_slice(&STUN_BINDING_SUCCESS.to_be_bytes());
        msg.extend_from_slice(&0u16.to_be_bytes());
        msg.extend_from_slice(&STUN_MAGIC);
        msg.extend_from_slice(&[1u8; 12]);
        assert_eq!(parse_mapped(&msg, &[2u8; 12]), None);
    }
}
