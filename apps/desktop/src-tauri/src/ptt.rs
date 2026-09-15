//! 按键说话：观察式按键监听（不注册系统热键）。
//!
//! `RegisterEventHotKey` 在 macOS 上绑不了单独的字母键（V / CapsLock 会失败），
//! 而且会把按键从游戏里抢走。这里用 `device_query` 轮询 HID 状态：
//! 不消耗按键、支持无修饰键和鼠标侧键，游戏全屏也能用。

use std::str::FromStr;
use std::thread;
use std::time::Duration;

use device_query::{DeviceQuery, DeviceState, Keycode};
use tauri::{AppHandle, Emitter, Manager};

use crate::{lock, AppState};

const POLL_MS: u64 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PttBind {
    Key(Keycode),
    /// `device_query` 的 1-based 鼠标键：1 左 / 2 右 / 3 中 / 4 侧键后退 / 5 侧键前进。
    Mouse(usize),
    Alt,
    Control,
    Shift,
}

impl PttBind {
    pub fn default_v() -> Self {
        Self::Key(Keycode::V)
    }

    pub fn label(self) -> String {
        match self {
            Self::Key(k) => format!("{k:?}"),
            Self::Mouse(n) => format!("Mouse{n}"),
            Self::Alt => "Alt".into(),
            Self::Control => "Control".into(),
            Self::Shift => "Shift".into(),
        }
    }

    fn is_down(self, device: &DeviceState) -> bool {
        match self {
            Self::Key(k) => device.get_keys().contains(&k),
            Self::Mouse(n) => device
                .get_mouse()
                .button_pressed
                .get(n)
                .copied()
                .unwrap_or(false),
            Self::Alt => device.get_keys().iter().any(|k| {
                matches!(
                    k,
                    Keycode::LAlt | Keycode::RAlt | Keycode::LOption | Keycode::ROption
                )
            }),
            Self::Control => device
                .get_keys()
                .iter()
                .any(|k| matches!(k, Keycode::LControl | Keycode::RControl)),
            Self::Shift => device
                .get_keys()
                .iter()
                .any(|k| matches!(k, Keycode::LShift | Keycode::RShift)),
        }
    }
}

pub fn parse_bind(raw: &str) -> Result<PttBind, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("empty key".into());
    }
    if s.contains('+') {
        return Err("按键说话只支持单个按键，不支持组合键".into());
    }
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let lower = compact.to_ascii_lowercase();
    match lower.as_str() {
        "alt" | "option" | "lalt" | "ralt" | "loption" | "roption" | "altleft" | "altright" => {
            return Ok(PttBind::Alt);
        }
        "ctrl" | "control" | "lcontrol" | "rcontrol" | "controlleft" | "controlright" => {
            return Ok(PttBind::Control);
        }
        "shift" | "lshift" | "rshift" | "shiftleft" | "shiftright" => {
            return Ok(PttBind::Shift);
        }
        "mouse4" | "x1" | "xbutton1" | "mousebutton4" => return Ok(PttBind::Mouse(4)),
        "mouse5" | "x2" | "xbutton2" | "mousebutton5" => return Ok(PttBind::Mouse(5)),
        "mouse3" | "middle" | "mousemiddle" => return Ok(PttBind::Mouse(3)),
        "`" | "grave" | "backquote" | "tilde" => return Ok(PttBind::Key(Keycode::Grave)),
        "caps" | "capslock" => return Ok(PttBind::Key(Keycode::CapsLock)),
        "space" | "spacebar" => return Ok(PttBind::Key(Keycode::Space)),
        "esc" | "escape" => return Ok(PttBind::Key(Keycode::Escape)),
        "enter" | "return" => return Ok(PttBind::Key(Keycode::Enter)),
        "tab" => return Ok(PttBind::Key(Keycode::Tab)),
        _ => {}
    }
    if let Some(d) = lower.strip_prefix("digit") {
        if let Ok(k) = Keycode::from_str(&format!("Key{d}")) {
            return Ok(PttBind::Key(k));
        }
    }
    if lower.len() == 1 && lower.as_bytes()[0].is_ascii_digit() {
        if let Ok(k) = Keycode::from_str(&format!("Key{compact}")) {
            return Ok(PttBind::Key(k));
        }
    }
    if let Some(rest) = compact.strip_prefix("Arrow") {
        if let Ok(k) = Keycode::from_str(rest) {
            return Ok(PttBind::Key(k));
        }
    }
    // KeyboardEvent.code `KeyV` → `V`
    let key_name = compact
        .strip_prefix("Key")
        .filter(|r| r.len() == 1)
        .unwrap_or(&compact);
    let upper = key_name.to_ascii_uppercase();
    if let Ok(k) = Keycode::from_str(&upper) {
        return Ok(PttBind::Key(k));
    }
    if let Ok(k) = Keycode::from_str(&compact) {
        return Ok(PttBind::Key(k));
    }
    Err(format!("无法识别按键 {raw}"))
}

pub fn permission_denied_msg() -> &'static str {
    "按键说话需要辅助功能权限：打开「系统设置 → 隐私与安全性 → 辅助功能」，允许 Vivi 后重试。"
}

/// macOS 上 `DeviceState::new()` 没权限会 assert 崩掉；开启 PTT 前先探测。
pub fn ensure_input() -> Result<(), String> {
    open_device()
        .map(|_| ())
        .ok_or_else(|| permission_denied_msg().into())
}

fn open_device() -> Option<DeviceState> {
    DeviceState::checked_new()
}

/// 后台轮询 HID；按住开麦、松开静音。非 tokio 线程，才能 `blocking_send`。
pub fn spawn(app: AppHandle) {
    let _ = thread::Builder::new()
        .name("ptt-watch".into())
        .spawn(move || {
            let mut device: Option<DeviceState> = None;
            let mut was_down = false;
            loop {
                thread::sleep(Duration::from_millis(POLL_MS));
                let Some(state) = app.try_state::<AppState>() else {
                    continue;
                };
                let enabled = *lock(&state.ptt_enabled);
                if !enabled {
                    device = None;
                    if was_down {
                        was_down = false;
                        apply_talk(&app, false);
                    }
                    continue;
                }
                if device.is_none() {
                    device = open_device();
                    if device.is_none() {
                        thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                }
                let Some(dev) = device.as_ref() else {
                    continue;
                };
                let bind = *lock(&state.ptt_bind);
                let down = bind.is_down(dev);
                if down == was_down {
                    continue;
                }
                was_down = down;
                apply_talk(&app, down);
            }
        });
}

fn apply_talk(app: &AppHandle, held: bool) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let handle = lock(&state.session).clone();
    if let Some(h) = handle {
        h.set_muted_blocking(!held);
    }
    let _ = app.emit("ptt-held", held);
}

#[cfg(test)]
mod tests {
    use super::{parse_bind, PttBind};
    use device_query::Keycode;

    #[test]
    fn parse_should_accept_letter_and_key_code() {
        assert_eq!(parse_bind("V").unwrap(), PttBind::Key(Keycode::V));
        assert_eq!(parse_bind("v").unwrap(), PttBind::Key(Keycode::V));
        assert_eq!(parse_bind("KeyV").unwrap(), PttBind::Key(Keycode::V));
    }

    #[test]
    fn parse_should_accept_named_keys() {
        assert_eq!(
            parse_bind("CapsLock").unwrap(),
            PttBind::Key(Keycode::CapsLock)
        );
        assert_eq!(parse_bind("Space").unwrap(), PttBind::Key(Keycode::Space));
        assert_eq!(
            parse_bind("Backquote").unwrap(),
            PttBind::Key(Keycode::Grave)
        );
        assert_eq!(parse_bind("Alt").unwrap(), PttBind::Alt);
        assert_eq!(parse_bind("Mouse4").unwrap(), PttBind::Mouse(4));
        assert_eq!(parse_bind("F1").unwrap(), PttBind::Key(Keycode::F1));
    }

    #[test]
    fn parse_should_reject_combos() {
        assert!(parse_bind("Control+V").is_err());
        assert!(parse_bind("").is_err());
    }

    #[test]
    fn label_should_roundtrip_common_binds() {
        assert_eq!(parse_bind("V").unwrap().label(), "V");
        assert_eq!(parse_bind("Mouse4").unwrap().label(), "Mouse4");
        assert_eq!(parse_bind("Alt").unwrap().label(), "Alt");
    }
}
