#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Tauri IPC 薄层 + 通话会话持有 + Rust 侧 PTT（观察式按键，不经过 JS）。
//!
//! 原则重申：实时音频全在 voice-session；本层只做会话持有、IPC 转发、PTT 监听。
//! PTT 轮询跑在独立线程（按键按下/松开直改静音），渲染进程卡死也不影响收发。

mod discovery;
mod net;
mod ptt;

use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use tauri::{Manager, State};
use voice_common::DeviceInfo;
use voice_session::{AudioMode, Session, SessionConfig, SessionHandle, SessionStats};
use voice_webrtc::PeerConfig;

const DEFAULT_SIGNALING_URL: &str = "ws://127.0.0.1:8080/signal";
const DEFAULT_PTT_KEY: &str = "V";

/// 内嵌信令的端口阶梯。
///
/// 8080 优先（口述地址、防火墙规则都按这个来），往后最多让到 8089 ——
/// 再往后用户既猜不到也没法口述给别人，退让收益为零，不如老实报错。
const PORT_MIN: u16 = 8080;
const PORT_MAX: u16 = 8089;

/// 内嵌信令的启动结果。前端靠它知道"本机信令在哪、能不能被局域网访问"。
#[derive(Clone, Debug, Default, Serialize)]
struct EmbeddedSignaling {
    /// 本机信令端口；`None` = 没起来（8080–8089 都被无关服务占了）
    port: Option<u16>,
    /// `true` = 这个端口是我们自己起的；`false` = 让路复用了已在跑的进程
    embedded: bool,
    /// 局域网里别的机器连不连得上这个端口
    lan_reachable: bool,
}

struct AppState {
    session: Mutex<Option<SessionHandle>>,
    loopback: Mutex<Option<voice_core::audio::LoopbackHandle>>,
    ptt_enabled: Mutex<bool>,
    ptt_key: Mutex<String>,
    ptt_bind: Mutex<ptt::PttBind>,
    signaling_url: Mutex<String>,
    stun_urls: Vec<String>,
    signaling: Mutex<EmbeddedSignaling>,
    /// 懒创建的 mDNS 设备：首次发现/广播时才起 socket，
    /// 平时不占着 5353，也避免没用发现功能的用户看到本机网络权限弹窗。
    discovery: Mutex<Option<Arc<discovery::Discovery>>>,
    /// 正在广播的房间号（广播的生命周期跟着"有没有在主持"走）
    hosting_room: Mutex<Option<String>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[tauri::command]
fn list_audio_devices() -> Result<Vec<DeviceInfo>, String> {
    voice_core::audio::list_devices().map_err(|e| e.to_string())
}

#[tauri::command]
fn default_audio_devices() -> (Option<String>, Option<String>) {
    voice_core::audio::default_devices()
}

#[derive(Debug, Deserialize)]
struct TurnConfig {
    urls: Vec<String>,
    username: String,
    credential: String,
}

#[tauri::command]
async fn join_room(
    state: State<'_, AppState>,
    room_id: String,
    input: Option<String>,
    output: Option<String>,
    turn: Option<TurnConfig>,
    signaling_url: Option<String>,
) -> Result<u64, String> {
    let room_id = room_id.trim().to_lowercase();
    if room_id.is_empty() {
        return Err("empty room id".into());
    }
    // 试听中进房间先停试听（避免通话时扬声器回灌）
    lock(&state.loopback).take();
    // 已在房间则先退（换设备重进也走这条路）
    let prev = lock(&state.session).take();
    if let Some(h) = prev {
        h.leave().await;
    }
    // 前端可按次覆盖信令地址（跨机联调用；空则用当前配置）
    if let Some(url) = signaling_url {
        let url = url.trim().to_owned();
        if !url.is_empty() {
            *lock(&state.signaling_url) = url;
        }
    }
    let signaling_url = lock(&state.signaling_url).clone();
    let user_id = uuid::Uuid::new_v4().as_u64_pair().0;
    let mut peer_config = PeerConfig {
        stun_urls: state.stun_urls.clone(),
        ..PeerConfig::default()
    };
    if let Some(t) = turn {
        peer_config.turn_urls = t.urls;
        peer_config.turn_username = Some(t.username);
        peer_config.turn_credential = Some(t.credential);
    }
    let handle = Session::join(SessionConfig {
        user_id,
        room_id,
        signaling_url: signaling_url.clone(),
        peer_config,
        audio: AudioMode::Live { input, output },
        relay_timeout_secs: 15,
        no_direct: vec![],
    })
    .await
    .map_err(|e| format!("{e:#}"))?;
    // PTT 模式入会即静音，等按键
    if *lock(&state.ptt_enabled) {
        handle.set_muted(true).await;
    }
    *lock(&state.session) = Some(handle);
    Ok(user_id)
}

#[tauri::command]
async fn leave_room(state: State<'_, AppState>) -> Result<(), String> {
    lock(&state.loopback).take();
    let prev = lock(&state.session).take();
    if let Some(h) = prev {
        h.leave().await;
    }
    Ok(())
}

#[tauri::command]
async fn set_muted(state: State<'_, AppState>, muted: bool) -> Result<(), String> {
    let handle = lock(&state.session).clone();
    if let Some(h) = handle {
        h.set_muted(muted).await;
    }
    Ok(())
}

#[tauri::command]
async fn set_deafened(state: State<'_, AppState>, deafened: bool) -> Result<(), String> {
    let handle = lock(&state.session).clone();
    if let Some(h) = handle {
        h.set_deafened(deafened).await;
    }
    Ok(())
}

#[tauri::command]
async fn set_user_gain(state: State<'_, AppState>, user: u64, gain: f32) -> Result<(), String> {
    let handle = lock(&state.session).clone();
    if let Some(h) = handle {
        h.set_user_gain(user, gain).await;
    }
    Ok(())
}

#[tauri::command]
async fn set_mic_gain(state: State<'_, AppState>, gain: f32) -> Result<(), String> {
    let handle = lock(&state.session).clone();
    if let Some(h) = handle {
        h.set_mic_gain(gain).await;
    }
    Ok(())
}

#[tauri::command]
async fn set_speaker_gain(state: State<'_, AppState>, gain: f32) -> Result<(), String> {
    let handle = lock(&state.session).clone();
    if let Some(h) = handle {
        h.set_speaker_gain(gain).await;
    }
    Ok(())
}

#[tauri::command]
async fn set_ns_enabled(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    let handle = lock(&state.session).clone();
    if let Some(h) = handle {
        h.set_ns_enabled(enabled).await;
    }
    Ok(())
}

#[tauri::command]
async fn set_agc_enabled(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    let handle = lock(&state.session).clone();
    if let Some(h) = handle {
        h.set_agc_enabled(enabled).await;
    }
    Ok(())
}

/// 试听麦克风（实时环回，Discord 式 mic test）。
/// 开着进房间会自动停（避免通话时扬声器回灌）。
/// 注意：请戴耳机，否则啸叫。
#[tauri::command]
fn mic_loopback_start(
    state: State<'_, AppState>,
    input: Option<String>,
    output: Option<String>,
) -> Result<(), String> {
    lock(&state.loopback).take();
    let h = voice_core::audio::start_loopback(input.as_deref(), output.as_deref())
        .map_err(|e| e.to_string())?;
    *lock(&state.loopback) = Some(h);
    Ok(())
}

#[tauri::command]
fn mic_loopback_stop(state: State<'_, AppState>) {
    lock(&state.loopback).take();
}

#[tauri::command]
fn get_stats(state: State<'_, AppState>) -> Option<SessionStats> {
    lock(&state.session).as_ref().map(|h| h.stats())
}

/// 本端 mic 实时电平（0.0~1.0，无锁直读；前端 50ms 轮询驱动话筒亮起/律动条）。
/// 试麦环回优先：没进房间也能看到话筒跟着声音亮。
#[tauri::command]
fn mic_level(state: State<'_, AppState>) -> f32 {
    if let Some(h) = lock(&state.loopback).as_ref() {
        return h.level();
    }
    lock(&state.session)
        .as_ref()
        .map(|h| h.mic_level())
        .unwrap_or(0.0)
}

#[derive(Debug, Serialize)]
struct PttState {
    enabled: bool,
    key: String,
}

#[tauri::command]
fn get_ptt(state: State<'_, AppState>) -> PttState {
    PttState {
        enabled: *lock(&state.ptt_enabled),
        key: lock(&state.ptt_key).clone(),
    }
}

#[tauri::command]
async fn set_ptt_enabled(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    if enabled {
        ptt::ensure_input()?;
    }
    *lock(&state.ptt_enabled) = enabled;
    let handle = lock(&state.session).clone();
    if let Some(h) = handle {
        // 按键说话：默认静音等按键；自由说话：开麦。
        h.set_muted(enabled).await;
    }
    Ok(())
}

#[tauri::command]
fn set_ptt_key(state: State<'_, AppState>, key: String) -> Result<String, String> {
    let bind = ptt::parse_bind(&key)?;
    let label = bind.label();
    *lock(&state.ptt_bind) = bind;
    *lock(&state.ptt_key) = label.clone();
    Ok(label)
}

/// 当前信令 WS 地址（前端首页回显，跨机时改成房主 IP）。
#[tauri::command]
fn get_signaling_url(state: State<'_, AppState>) -> String {
    lock(&state.signaling_url).clone()
}

/// 运行时切换信令（跨机联调：两台都指向同一台房主地址，如 ws://192.168.1.10:8080/signal）。
#[tauri::command]
fn set_signaling_url(state: State<'_, AppState>, url: String) -> Result<String, String> {
    let url = url.trim().to_owned();
    if !(url.starts_with("ws://") || url.starts_with("wss://")) {
        return Err("signaling url must start with ws:// or wss://".into());
    }
    *lock(&state.signaling_url) = url.clone();
    Ok(url)
}

// ---------- 内嵌信令的启动 / 状态 ----------

/// 起内嵌信令：按 [`PORT_MIN`]`..=`[`PORT_MAX`] 依次试。
///
/// 端口被占时先探 `/info` 判断是不是我们自己的信令——是就让路复用（保持原有
/// 语义：用户手动跑了 `cargo run -p signaling` 时不重复起），是别的服务就继续
/// 往后试，而不是像以前那样静默放弃。
async fn start_embedded_signaling() -> EmbeddedSignaling {
    for port in PORT_MIN..=PORT_MAX {
        match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
            Ok(listener) => {
                eprintln!("embedded signaling on 0.0.0.0:{port} (LAN reachable)");
                let sig_state = Arc::new(signaling::rooms::AppState::from_env());
                tauri::async_runtime::spawn(async move {
                    let _ = axum::serve(listener, signaling::app(sig_state)).await;
                });
                return EmbeddedSignaling {
                    port: Some(port),
                    embedded: true,
                    lan_reachable: true,
                };
            }
            Err(e) => {
                if net::probe_vivi(port).await {
                    eprintln!("port {port} is another vivi signaling; reusing it");
                    return EmbeddedSignaling {
                        port: Some(port),
                        embedded: false,
                        lan_reachable: probe_lan_reachable(port).await,
                    };
                }
                eprintln!("port {port} busy and not vivi ({e}); trying next");
            }
        }
    }
    EmbeddedSignaling::default()
}

/// 已在跑的那个信令是不是也监听在局域网上。
///
/// 独立跑的 `signaling` 绑的是 `0.0.0.0`，但有人可能改成只绑 `127.0.0.1`。
/// 那种情况下不能对外广播这个地址——否则局域网里所有人都会连到一个死地址。
async fn probe_lan_reachable(port: u16) -> bool {
    let Some(ip) = net::primary_lan_ip() else {
        return false;
    };
    let addr = std::net::SocketAddr::from((ip, port));
    matches!(
        tokio::time::timeout(net::PROBE_TIMEOUT, tokio::net::TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

/// 懒创建 mDNS 设备。同步函数（不跨 await 持锁）。
fn discovery_for(state: &AppState) -> Result<Arc<discovery::Discovery>, String> {
    let mut slot = lock(&state.discovery);
    if let Some(d) = slot.as_ref() {
        return Ok(d.clone());
    }
    let d = Arc::new(discovery::Discovery::new()?);
    *slot = Some(d.clone());
    Ok(d)
}

#[derive(Serialize)]
struct SignalingStatus {
    /// 本机信令的 HTTP base；`None` = 没起来
    local_http: Option<String>,
    port: Option<u16>,
    /// `true` = 我们自己起的；`false` = 复用了已在跑的进程
    embedded: bool,
    lan_reachable: bool,
    lan_ips: Vec<String>,
    primary_lan_ip: Option<String>,
    /// 正在广播的房间号
    advertising_room: Option<String>,
}

/// 本机信令现状。前端拿它渲染状态、拼地址、判断能不能自动发现。
#[tauri::command]
fn get_signaling_status(state: State<'_, AppState>) -> SignalingStatus {
    let sig = lock(&state.signaling).clone();
    SignalingStatus {
        local_http: sig.port.map(|p| format!("http://127.0.0.1:{p}")),
        port: sig.port,
        embedded: sig.embedded,
        lan_reachable: sig.lan_reachable,
        lan_ips: net::lan_ipv4s().iter().map(|ip| ip.to_string()).collect(),
        primary_lan_ip: net::primary_lan_ip().map(|ip| ip.to_string()),
        advertising_room: lock(&state.hosting_room).clone(),
    }
}

#[derive(Serialize)]
struct DiscoveryResult {
    servers: Vec<discovery::DiscoveredServer>,
    /// mDNS 起不来时的原因；前端据此解释"为什么一个都没发现"
    error: Option<String>,
}

/// 浏览局域网里正在广播的 Vivi 信令。
///
/// 返回的是**候选**，不含"哪个真的有这个房间"的判断——那一步由前端并发
/// `GET /rooms/{id}` 决定（mDNS 缓存会撒谎，不能拿它当权威）。
#[tauri::command]
async fn discover_signaling(
    state: State<'_, AppState>,
    timeout_ms: Option<u64>,
) -> Result<DiscoveryResult, String> {
    let device = match discovery_for(&state) {
        Ok(d) => d,
        Err(e) => {
            return Ok(DiscoveryResult {
                servers: Vec::new(),
                error: Some(e),
            })
        }
    };
    let timeout = timeout_ms
        .map(std::time::Duration::from_millis)
        .unwrap_or(discovery::BROWSE_TIMEOUT);
    let servers = device.browse(timeout).await;
    Ok(DiscoveryResult {
        servers,
        error: None,
    })
}

/// 建房成功后开始广播，让同一局域网的人能靠房号找到这台。
#[tauri::command]
fn start_advertising(state: State<'_, AppState>, room_id: String) -> Result<(), String> {
    let sig = lock(&state.signaling).clone();
    let Some(port) = sig.port else {
        return Err("本机信令没起来（8080–8089 都被占用），无法被局域网发现".into());
    };
    if !sig.lan_reachable {
        return Err("本机信令只监听在 localhost，局域网内发现不到".into());
    }
    discovery_for(&state)?.advertise(&room_id, port)?;
    *lock(&state.hosting_room) = Some(room_id);
    Ok(())
}

/// 离房时撤销广播。
#[tauri::command]
fn stop_advertising(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(d) = lock(&state.discovery).as_ref() {
        d.unadvertise();
    }
    *lock(&state.hosting_room) = None;
    Ok(())
}

#[derive(Serialize)]
struct InviteInfo {
    room: String,
    port: Option<u16>,
    lan_ips: Vec<String>,
    primary_lan_ip: Option<String>,
    /// STUN 探到的出口公网地址；只有主动看邀请信息时才会去探
    public_ip: Option<String>,
    /// 出口在运营商大内网（100.64/10）：映射也救不了，得明说
    cgnat: bool,
}

/// 生成邀请所需的候选地址。
///
/// 注意语义：`public_ip` 是 **UDP 出口**的映射地址，和 TCP 8080 的映射
/// 没有任何关系。邀请串里写 `public_ip:port` 的隐含前提是"房主在路由器上
/// 做过 8080/tcp 端口映射"——没做就是连不上，这个只能让队友试一次才知道。
#[tauri::command]
async fn build_invite(state: State<'_, AppState>, room_id: String) -> Result<InviteInfo, String> {
    let sig = lock(&state.signaling).clone();
    let public_ip = net::probe_public_ip(&state.stun_urls, net::PROBE_TIMEOUT).await;
    Ok(InviteInfo {
        room: room_id,
        port: sig.port,
        lan_ips: net::lan_ipv4s().iter().map(|ip| ip.to_string()).collect(),
        primary_lan_ip: net::primary_lan_ip().map(|ip| ip.to_string()),
        cgnat: public_ip.map(net::is_cgnat).unwrap_or(false),
        public_ip: public_ip.map(|ip| ip.to_string()),
    })
}

fn main() {
    // 新 env 优先，旧 GAMEVOICE_ 兼容（重命名前已部署的环境不断连）。
    let signaling_url = std::env::var("VIVI_SIGNALING_URL")
        .or_else(|_| std::env::var("GAMEVOICE_SIGNALING_URL"))
        .unwrap_or_else(|_| DEFAULT_SIGNALING_URL.into());
    let stun_urls = std::env::var("VIVI_STUN_URLS")
        .or_else(|_| std::env::var("GAMEVOICE_STUN_URLS"))
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_owned)
                .collect()
        })
        // 不要再抄一份默认值到这里：唯一事实来源是 PeerConfig::default()
        // （这里以前手抄了一份，改了那边不改这边就等于没改）。
        .unwrap_or_else(|_| PeerConfig::default().stun_urls);

    tauri::Builder::default()
        .manage(AppState {
            session: Mutex::new(None),
            loopback: Mutex::new(None),
            ptt_enabled: Mutex::new(false),
            ptt_key: Mutex::new(DEFAULT_PTT_KEY.into()),
            ptt_bind: Mutex::new(ptt::PttBind::default_v()),
            signaling_url: Mutex::new(signaling_url),
            stun_urls,
            signaling: Mutex::new(EmbeddedSignaling::default()),
            discovery: Mutex::new(None),
            hosting_room: Mutex::new(None),
        })
        .setup(|app| {
            ptt::spawn(app.handle().clone());
            // 内嵌信令：0.0.0.0 监听，局域网另一台也能连。
            // 结果写回 state，前端要靠它知道本机信令到底在哪个端口。
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let result = start_embedded_signaling().await;
                eprintln!(
                    "signaling: port={:?} embedded={} lan_reachable={}",
                    result.port, result.embedded, result.lan_reachable
                );
                if result.port.is_none() {
                    eprintln!("signaling: 8080-{PORT_MAX} 都被无关服务占用，本机信令没起来");
                }
                let state = handle.state::<AppState>();
                *lock(&state.signaling) = result;
            });
            Ok(())
        })
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            list_audio_devices,
            default_audio_devices,
            join_room,
            leave_room,
            set_muted,
            set_deafened,
            set_user_gain,
            set_mic_gain,
            set_speaker_gain,
            set_ns_enabled,
            set_agc_enabled,
            mic_loopback_start,
            mic_loopback_stop,
            get_stats,
            mic_level,
            get_ptt,
            set_ptt_enabled,
            set_ptt_key,
            get_signaling_url,
            set_signaling_url,
            get_signaling_status,
            discover_signaling,
            start_advertising,
            stop_advertising,
            build_invite
        ])
        .run(tauri::generate_context!())
        .expect("failed to run vivi desktop");
}
