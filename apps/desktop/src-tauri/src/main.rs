#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Tauri IPC 薄层 + 通话会话持有 + Rust 侧 PTT（系统级热键，不经过 JS）。
//!
//! 原则重申：实时音频全在 voice-session；本层只做会话持有、IPC 转发、PTT 热键。
//! PTT 回调跑在 backend（按键按下/松开直改静音），渲染进程卡死也不影响收发。

use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use voice_common::DeviceInfo;
use voice_session::{AudioMode, Session, SessionConfig, SessionHandle, SessionStats};
use voice_webrtc::PeerConfig;

const DEFAULT_SIGNALING_URL: &str = "ws://127.0.0.1:8080/signal";
const DEFAULT_PTT_KEY: &str = "V";

struct AppState {
    session: Mutex<Option<SessionHandle>>,
    loopback: Mutex<Option<voice_core::audio::LoopbackHandle>>,
    ptt_enabled: Mutex<bool>,
    ptt_key: Mutex<String>,
    signaling_url: String,
    stun_urls: Vec<String>,
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
) -> Result<u64, String> {
    if room_id.trim().is_empty() {
        return Err("empty room id".into());
    }
    // 试听中进房间先停试听（避免通话时扬声器回灌）
    lock(&state.loopback).take();
    // 已在房间则先退（换设备重进也走这条路）
    let prev = lock(&state.session).take();
    if let Some(h) = prev {
        h.leave().await;
    }
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
        signaling_url: state.signaling_url.clone(),
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
async fn set_ptt_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    *lock(&state.ptt_enabled) = enabled;
    if enabled {
        // 开启即注册当前键并静音等按键
        let key = lock(&state.ptt_key).clone();
        register_ptt_key(&app, "", &key)?;
        let handle = lock(&state.session).clone();
        if let Some(h) = handle {
            h.set_muted(true).await;
        }
    }
    Ok(())
}

#[tauri::command]
fn set_ptt_key(app: AppHandle, state: State<'_, AppState>, key: String) -> Result<(), String> {
    let key = key.trim().to_owned();
    if key.is_empty() {
        return Err("empty shortcut".into());
    }
    // 先解析再换绑：新键非法则保留旧键
    let new_sc: Shortcut = key.parse().map_err(|e| format!("bad shortcut: {e}"))?;
    let old_key = lock(&state.ptt_key).clone();
    if old_key != key {
        if let Ok(old_sc) = old_key.parse::<Shortcut>() {
            let _ = app.global_shortcut().unregister(old_sc);
        }
        app.global_shortcut()
            .on_shortcut(new_sc, ptt_handler)
            .map_err(|e| e.to_string())?;
        *lock(&state.ptt_key) = key;
    }
    Ok(())
}

/// PTT 按键回调（backend 线程）：按下开麦，松开静音；非 PTT 模式/无会话直接忽略。
/// 不经过 JS，UI 卡死也不影响。
fn ptt_handler(
    app: &AppHandle,
    _shortcut: &Shortcut,
    event: tauri_plugin_global_shortcut::ShortcutEvent,
) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if !*lock(&state.ptt_enabled) {
        return;
    }
    let handle = lock(&state.session).clone();
    let Some(h) = handle else { return };
    match event.state {
        ShortcutState::Pressed => h.set_muted_blocking(false),
        ShortcutState::Released => h.set_muted_blocking(true),
    }
}

/// 注册 PTT 键（`old_key` 为空表示首次注册，不解绑）。
fn register_ptt_key(app: &AppHandle, old_key: &str, new_key: &str) -> Result<(), String> {
    if !old_key.is_empty() {
        if let Ok(old_sc) = old_key.parse::<Shortcut>() {
            let _ = app.global_shortcut().unregister(old_sc);
        }
    }
    let sc: Shortcut = new_key.parse().map_err(|e| format!("bad shortcut: {e}"))?;
    app.global_shortcut()
        .on_shortcut(sc, ptt_handler)
        .map_err(|e| e.to_string())
}

fn main() {
    let signaling_url =
        std::env::var("GAMEVOICE_SIGNALING_URL").unwrap_or_else(|_| DEFAULT_SIGNALING_URL.into());
    let stun_urls = std::env::var("GAMEVOICE_STUN_URLS")
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_else(|_| {
            vec![
                "stun:stun.l.google.com:19302".into(),
                "stun:stun1.l.google.com:19302".into(),
                "stun:stun2.l.google.com:19302".into(),
                "stun:stun3.l.google.com:19302".into(),
                "stun:stun4.l.google.com:19302".into(),
            ]
        });

    tauri::Builder::default()
        .manage(AppState {
            session: Mutex::new(None),
            loopback: Mutex::new(None),
            ptt_enabled: Mutex::new(false),
            ptt_key: Mutex::new(DEFAULT_PTT_KEY.into()),
            signaling_url,
            stun_urls,
        })
        .setup(|_app| {
            // 内嵌信令：本机 8080 有.room/WS/TURN 凭证全套，开箱即用；
            // 端口被占（比如手动跑了 signaling）就让路，用现成的。
            tauri::async_runtime::spawn(async {
                match tokio::net::TcpListener::bind("127.0.0.1:8080").await {
                    Ok(listener) => {
                        eprintln!("embedded signaling on 127.0.0.1:8080");
                        let state = std::sync::Arc::new(signaling::rooms::AppState::from_env());
                        let _ = axum::serve(listener, signaling::app(state)).await;
                    }
                    Err(e) => {
                        eprintln!("embedded signaling skipped (port busy?): {e}");
                    }
                }
            });
            Ok(())
        })
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
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
            set_ptt_key
        ])
        .run(tauri::generate_context!())
        .expect("failed to run gamevoice desktop");
}
