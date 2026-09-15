//! 本机地址探测 + 两个手写的极简协议探测。
//!
//! 都不引 HTTP 客户端：`reqwest` 只是 tauri/updater 的传递依赖，直接引用要处理
//! rustls 特性对齐，而这里只需要"发一段字节、收一段字节"。
//!
//! - [`primary_lan_ip`] / [`lan_ipv4s`]：给邀请串挑一个别人连得上的地址；
//! - [`probe_vivi`]：8080 被占用时判断"是不是我们自己的信令"；
//! - [`probe_public_ip`]：手写 STUN，拿本机出口的公网地址。

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 探测类请求的统一超时。局域网内正常是个位数毫秒，800ms 已经很宽松。
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(800);

/// 明显是虚拟网卡的接口名前缀。
///
/// 这些网卡（Docker/WSL/虚拟机/隧道）经常占着 172.17.x、192.168.56.x 之类的地址，
/// 把它们的 IP 写进邀请串发给队友，对方一定连不上——这是多网卡机器上最经典的翻车。
const VIRTUAL_IF_PREFIXES: &[&str] = &[
    "bridge", "docker", "vbox", "vmnet", "vmenet", "utun", "awdl", "llw", "anpi", "tap", "tun",
    "virbr", "veth", "zt", "wg", "ppp",
];

fn is_virtual_if(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    VIRTUAL_IF_PREFIXES.iter().any(|p| lower.starts_with(p))
}

/// 本机所有可用的局域网 IPv4 地址（滤掉回环、link-local 与虚拟网卡）。
///
/// 只做过滤不做排序：需要"唯一那个最可能对的地址"时用 [`primary_lan_ip`]。
pub fn lan_ipv4s() -> Vec<Ipv4Addr> {
    let Ok(ifs) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut out: Vec<Ipv4Addr> = ifs
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter(|i| !is_virtual_if(&i.name))
        .filter_map(|i| match i.addr.ip() {
            IpAddr::V4(v4) if !v4.is_link_local() && !v4.is_unspecified() => Some(v4),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 默认路由出口的局域网 IP —— 多网卡机器上要发给别人的就是它。
///
/// 手法：UDP socket `connect` 到公网地址**不会真的发包**，只是让内核按路由表
/// 选一次出口，然后 `local_addr()` 就是那张网卡的地址。
/// 没有默认路由（纯离线局域网）时退回 [`lan_ipv4s`] 的第一个。
pub fn primary_lan_ip() -> Option<Ipv4Addr> {
    if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
        if sock.connect("8.8.8.8:80").is_ok() {
            if let Ok(SocketAddr::V4(addr)) = sock.local_addr() {
                if !addr.ip().is_unspecified() && !addr.ip().is_loopback() {
                    return Some(*addr.ip());
                }
            }
        }
    }
    lan_ipv4s().into_iter().next()
}

/// 运营商大内网（RFC 6598，100.64.0.0/10）。命中就说明端口映射也救不了。
pub fn is_cgnat(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..=127).contains(&o[1])
}

/// 这个地址是不是私有/不可路由的（用来判断公网探测结果有没有意义）。
pub fn is_private(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback() || ip.is_link_local() || is_cgnat(ip)
}

/// 探 127.0.0.1:port 上跑的是不是我们自己的信令服务。
///
/// 判据是响应体里出现 `vivi-signaling`（见 `signaling::Info`）。
/// 任何错误/超时都当"不是"，因为这本来就是用来跟无关服务做区分的。
pub async fn probe_vivi(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(Ok(mut stream)) = tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(addr)).await
    else {
        return false;
    };
    let req = format!(
        "GET /info HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(req.as_bytes()).await.is_err() {
        return false;
    }
    let mut buf = vec![0u8; 2048];
    let mut read = 0usize;
    // 分几次读：header 和 body 可能不在同一个包里。
    // 读到 EOF / 缓冲满 / 超时都停，然后再找关键字。
    loop {
        if read == buf.len() {
            break;
        }
        match tokio::time::timeout(PROBE_TIMEOUT, stream.read(&mut buf[read..])).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => {
                read += n;
                if String::from_utf8_lossy(&buf[..read]).contains("vivi-signaling") {
                    return true;
                }
            }
            Ok(Err(_)) => break,
        }
    }
    String::from_utf8_lossy(&buf[..read]).contains("vivi-signaling")
}

// ---------- STUN ----------

const STUN_MAGIC: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];
const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_SUCCESS: u16 = 0x0101;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const FAMILY_IPV4: u8 = 0x01;

fn stun_binding_request(txn: &[u8; 12]) -> [u8; 20] {
    let mut msg = [0u8; 20];
    msg[0..2].copy_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
    // length = 0：没有属性
    msg[4..8].copy_from_slice(&STUN_MAGIC);
    msg[8..20].copy_from_slice(txn);
    msg
}

/// 从 STUN 响应里取映射地址。只认 IPv4 —— 邀请串要的是 IPv4 公网地址。
fn parse_mapped_v4(msg: &[u8]) -> Option<Ipv4Addr> {
    if msg.len() < 20 || u16::from_be_bytes([msg[0], msg[1]]) != STUN_BINDING_SUCCESS {
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
        if ty == ATTR_XOR_MAPPED_ADDRESS || ty == ATTR_MAPPED_ADDRESS {
            let v = &msg[value_start..value_end];
            if v.len() >= 8 && v[1] == FAMILY_IPV4 {
                let raw = [v[4], v[5], v[6], v[7]];
                let ip = if ty == ATTR_XOR_MAPPED_ADDRESS {
                    // 地址与 magic cookie 逐字节异或
                    Ipv4Addr::new(
                        raw[0] ^ STUN_MAGIC[0],
                        raw[1] ^ STUN_MAGIC[1],
                        raw[2] ^ STUN_MAGIC[2],
                        raw[3] ^ STUN_MAGIC[3],
                    )
                } else {
                    Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3])
                };
                return Some(ip);
            }
        }
        // 属性值按 4 字节对齐
        pos = value_start + len.div_ceil(4) * 4;
    }
    None
}

/// 挨个问 `stun_urls`（形如 `stun:host:port`），返回第一个可用的公网 IPv4。
///
/// 只在用户主动看邀请信息时调用——**绝不在启动时联网**。
pub async fn probe_public_ip(stun_urls: &[String], per_server: Duration) -> Option<Ipv4Addr> {
    for url in stun_urls {
        if let Some(ip) = probe_one_stun(url, per_server).await {
            return Some(ip);
        }
    }
    None
}

async fn probe_one_stun(url: &str, timeout: Duration) -> Option<Ipv4Addr> {
    let hostport = url.trim().strip_prefix("stun:").unwrap_or(url.trim());
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (hostport, 3478),
    };
    // 域名要解析；直接是 IP 时 lookup_host 也能吃下去
    let target = tokio::net::lookup_host((host, port))
        .await
        .ok()?
        .find(|a| a.is_ipv4())?;

    let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.ok()?;
    sock.connect(target).await.ok()?;

    let mut txn = [0u8; 12];
    // 事务 ID 只要唯一即可，不要求密码学随机性
    txn.copy_from_slice(&fastrand_bytes(12));
    sock.send(&stun_binding_request(&txn)).await.ok()?;

    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(timeout, sock.recv(&mut buf)).await.ok()?.ok()?;
    let ip = parse_mapped_v4(&buf[..n])?;
    (!is_private(ip)).then_some(ip)
}

/// 12 字节伪随机。用时间 + 地址做种，够 STUN 事务 ID 用了。
fn fastrand_bytes(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let local = &seed as *const u64 as u64;
    let mut state = seed ^ local.rotate_left(17) ^ 0x2545_F491_4F6C_DD1D;
    for _ in 0..n {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        out.push((state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as u8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cgnat_range_should_be_detected() {
        assert!(is_cgnat("100.64.0.1".parse().unwrap()));
        assert!(is_cgnat("100.127.255.254".parse().unwrap()));
        assert!(!is_cgnat("100.63.0.1".parse().unwrap()));
        assert!(!is_cgnat("100.128.0.1".parse().unwrap()));
    }

    #[test]
    fn virtual_interfaces_should_be_filtered() {
        assert!(is_virtual_if("bridge100"));
        assert!(is_virtual_if("utun3"));
        assert!(is_virtual_if("docker0"));
        assert!(!is_virtual_if("en0"));
        assert!(!is_virtual_if("eth0"));
        assert!(!is_virtual_if("Wi-Fi"));
    }

    #[test]
    fn xor_mapped_address_should_be_decoded() {
        // 手搓一个含 XOR-MAPPED-ADDRESS 的 binding success：
        // 期望映射到 203.0.113.7:54321
        let ip = Ipv4Addr::new(203, 0, 113, 7);
        let port: u16 = 54321;
        let mut msg = Vec::new();
        msg.extend_from_slice(&STUN_BINDING_SUCCESS.to_be_bytes());
        msg.extend_from_slice(&12u16.to_be_bytes()); // 属性区长度
        msg.extend_from_slice(&STUN_MAGIC);
        msg.extend_from_slice(&[0u8; 12]);
        msg.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        msg.extend_from_slice(&8u16.to_be_bytes());
        msg.push(0);
        msg.push(FAMILY_IPV4);
        msg.extend_from_slice(&(port ^ 0x2112).to_be_bytes());
        let o = ip.octets();
        for i in 0..4 {
            msg.push(o[i] ^ STUN_MAGIC[i]);
        }
        assert_eq!(parse_mapped_v4(&msg), Some(ip));
    }

    #[test]
    fn garbage_should_not_parse() {
        assert_eq!(parse_mapped_v4(&[]), None);
        assert_eq!(parse_mapped_v4(&[0u8; 64]), None);
    }

}
