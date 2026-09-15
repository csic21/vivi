//! 局域网自动发现（mDNS / DNS-SD）。
//!
//! 设计要点：**只有"正在主持一个房间"的那台会广播**。所以"广播还在"就等价于
//! "房主还开着"；没建房就不广播，也不会在局域网上无条件暴露"这台装了 Vivi"。
//!
//! 广播内容（TXT 里的房号）只当**提示**用。房间到底在不在，永远以
//! `GET /rooms/{id}` 的 HTTP 结果为准——mDNS 有缓存 TTL，房主强杀进程后
//! 对端在 TTL 内仍然能 resolve 到一个死地址。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::net;

/// DNS-SD 服务类型（`_tcp` 是 DNS-SD 的约定写法；我们只用它的地址 + 端口）。
pub const SERVICE_TYPE: &str = "_vivi-signal._tcp.local.";

/// 浏览超时。冷启动要等一次多播往返；命中缓存通常几十毫秒。
pub const BROWSE_TIMEOUT: Duration = Duration::from_millis(2500);

/// TXT 里记录协议版本，将来记录形状变了靠它区分。
const TXT_VERSION: &str = "1";

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscoveredServer {
    /// mDNS 实例全名，用于去重。
    pub fullname: String,
    /// 机器名（去掉 `.local.`），拿来给用户看"发现了哪台"。
    pub label: String,
    /// 候选 IPv4，同网段的排在前面。
    pub addresses: Vec<String>,
    pub port: u16,
    /// TXT 里带的房号。只是提示，**不可信**。
    pub room: Option<String>,
}

pub struct Discovery {
    daemon: ServiceDaemon,
    /// 当前播出去的 fullname；`None` 表示没在广播。
    advertised: Mutex<Option<String>>,
}

impl Discovery {
    pub fn new() -> Result<Self, String> {
        let daemon = ServiceDaemon::new().map_err(|e| format!("mDNS 初始化失败：{e}"))?;
        Ok(Self {
            daemon,
            advertised: Mutex::new(None),
        })
    }

    /// 开始广播"这台机器上主持着 `room`，信令在 `port`"。
    ///
    /// 重复调用会先撤掉旧的（换房号走这条路），所以中途换房号也是安全的。
    pub fn advertise(&self, room: &str, port: u16) -> Result<(), String> {
        self.unadvertise();

        let ips = net::lan_ipv4s();
        if ips.is_empty() {
            return Err("找不到可用的局域网地址，无法广播".into());
        }
        let addr_strs: Vec<String> = ips.iter().map(|ip| ip.to_string()).collect();
        let short = short_hostname();

        // 实例名带端口：天然唯一，不用依赖 mdns-sd 撞名后的自动改名
        // （改名后我们手里的 fullname 会失效，unregister 会打偏、广告泄漏）。
        let instance = format!("{short}-{port}");
        let mut props = HashMap::new();
        props.insert("room".to_string(), room.to_string());
        props.insert("v".to_string(), TXT_VERSION.to_string());

        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &format!("{short}.local."),
            addr_strs.as_slice(),
            port,
            props,
        )
        .map_err(|e| format!("mDNS 记录构造失败：{e}"))?;

        let fullname = info.get_fullname().to_string();
        self.daemon
            .register(info)
            .map_err(|e| format!("mDNS 广播失败：{e}"))?;
        *self.advertised.lock().unwrap_or_else(|e| e.into_inner()) = Some(fullname);
        Ok(())
    }

    /// 停止广播（离房 / 退出）。没在广播时是空操作。
    pub fn unadvertise(&self) {
        let prev = self
            .advertised
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(fullname) = prev {
            let _ = self.daemon.unregister(&fullname);
        }
    }

    /// 浏览局域网里正在广播的 Vivi 信令，最多等 `timeout`。
    ///
    /// 返回的是**候选**：谁真的有这个房间要调用方再探一次 HTTP。
    pub async fn browse(&self, timeout: Duration) -> Vec<DiscoveredServer> {
        let Ok(rx) = self.daemon.browse(SERVICE_TYPE) else {
            return Vec::new();
        };

        let mut found: HashMap<String, DiscoveredServer> = HashMap::new();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(left, rx.recv_async()).await {
                Ok(Ok(ServiceEvent::ServiceResolved(info))) => {
                    found.insert(info.get_fullname().to_string(), to_server(&info));
                }
                // 对端撤销了广播（正常离房），直接剔掉
                Ok(Ok(ServiceEvent::ServiceRemoved(_, fullname))) => {
                    found.remove(&fullname);
                }
                Ok(Ok(_)) => {}
                // 通道关闭 / 等超时：都用已收到的结果
                Ok(Err(_)) | Err(_) => break,
            }
        }
        let _ = self.daemon.stop_browse(SERVICE_TYPE);

        let mut out: Vec<DiscoveredServer> = found.into_values().collect();
        // 按机器名排序，结果稳定（否则同一次发现的顺序会跳，UI 看着像在闪）
        out.sort_by(|a, b| a.label.cmp(&b.label).then(a.port.cmp(&b.port)));
        out
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.unadvertise();
    }
}

fn to_server(info: &mdns_sd::ResolvedService) -> DiscoveredServer {
    DiscoveredServer {
        fullname: info.get_fullname().to_string(),
        label: short_name(info.get_hostname()),
        addresses: order_addresses(info.get_addresses_v4()),
        port: info.get_port(),
        room: info.get_property_val_str("room").map(str::to_string),
    }
}

/// 把候选地址排成"最可能连得上"的顺序：先同网段，其次私有地址，最后其它。
fn order_addresses(addrs: HashSet<std::net::Ipv4Addr>) -> Vec<String> {
    let local = net::primary_lan_ip();
    let same_subnet = |ip: &std::net::Ipv4Addr| match local {
        // 家用网段基本都是 /24，比前三个字节够用了
        Some(l) => l.octets()[..3] == ip.octets()[..3],
        None => false,
    };
    let mut v: Vec<std::net::Ipv4Addr> = addrs.into_iter().filter(|ip| !ip.is_loopback()).collect();
    v.sort_by_key(|ip| (!same_subnet(ip), !ip.is_private(), ip.octets()));
    v.into_iter().map(|ip| ip.to_string()).collect()
}

/// 本机短名。拿不到就退回 "vivi"，只是显示用，不影响功能。
fn short_hostname() -> String {
    let raw = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_default();
    let short = short_name(&raw);
    if short.is_empty() {
        "vivi".to_string()
    } else {
        short
    }
}

/// 去掉 `.local.` / `.local` 后缀；顺手把 DNS-SD 不允许的字符换掉。
fn short_name(host: &str) -> String {
    let trimmed = host
        .trim_end_matches('.')
        .trim_end_matches(".local")
        .trim_end_matches('.');
    let cleaned: String = trimmed
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    cleaned.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_name_should_strip_local_suffix() {
        assert_eq!(short_name("macbook.local."), "macbook");
        assert_eq!(short_name("DESKTOP-ABC123.local."), "DESKTOP-ABC123");
        assert_eq!(short_name("macbook"), "macbook");
    }

    #[test]
    fn short_name_should_escape_dots_and_spaces() {
        assert_eq!(short_name("karl 的 mac.air.local."), "karl---mac-air");
    }

    /// 自己广播、自己浏览：验证整条 mDNS 链路（注册 → 多播 → 解析 → TXT）
    /// 真的通，而不只是能编译。需要本机多播可用，CI 上不可靠，所以默认 ignore：
    ///     cargo test --bin vivi-desktop -- --ignored
    #[tokio::test]
    #[ignore = "依赖本机多播，CI 环境不可靠"]
    async fn discovery_should_see_its_own_advertisement() {
        let d = Discovery::new().expect("mDNS 起不来");
        d.advertise("a1b2c3d4", 18080).expect("广播失败");
        let found = d.browse(Duration::from_secs(4)).await;
        d.unadvertise();

        let hit = found
            .iter()
            .find(|s| s.room.as_deref() == Some("a1b2c3d4") && s.port == 18080)
            .unwrap_or_else(|| panic!("没发现自己广播的记录，实际发现：{found:#?}"));
        assert!(
            !hit.addresses.is_empty(),
            "解析到了服务但没有地址：{hit:#?}"
        );
        assert!(!hit.label.is_empty(), "机器名不该是空的：{hit:#?}");
    }

    #[test]
    fn order_addresses_should_put_private_first() {
        let mut set = HashSet::new();
        set.insert("203.0.113.9".parse().unwrap());
        set.insert("192.168.1.50".parse().unwrap());
        set.insert("127.0.0.1".parse().unwrap());
        let out = order_addresses(set);
        assert!(!out.iter().any(|a| a.starts_with("127.")), "回环不该出现");
        assert_eq!(out[0], "192.168.1.50", "私有地址该排在公网前面");
    }
}
