//! Task 1：CPAL 设备枚举（输入/输出/默认设备）与按名查找。
//! cpal 0.18：设备名经 `description().name()` 获取，错误统一为 `cpal::Error`。

use cpal::traits::{DeviceTrait, HostTrait};
use voice_common::{DeviceInfo, DeviceKind, VoiceError};

pub(crate) fn device_name(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_owned())
}

/// 列出输入 + 输出设备，`is_default` 标记默认设备。
pub fn list_devices() -> Result<Vec<DeviceInfo>, VoiceError> {
    let host = cpal::default_host();
    let mut out = Vec::new();

    let default_in = host.default_input_device().and_then(|d| device_name(&d));
    let default_out = host.default_output_device().and_then(|d| device_name(&d));

    if let Ok(devices) = host.input_devices() {
        for d in devices {
            if let Some(name) = device_name(&d) {
                let is_default = Some(name.clone()) == default_in;
                out.push(DeviceInfo {
                    id: format!("in:{name}"),
                    name,
                    kind: DeviceKind::Input,
                    is_default,
                });
            }
        }
    }

    if let Ok(devices) = host.output_devices() {
        for d in devices {
            if let Some(name) = device_name(&d) {
                let is_default = Some(name.clone()) == default_out;
                out.push(DeviceInfo {
                    id: format!("out:{name}"),
                    name,
                    kind: DeviceKind::Output,
                    is_default,
                });
            }
        }
    }

    Ok(out)
}

/// 返回（默认输入，默认输出）名称；缺失则为 None（便于 UI 展示空态）。
pub fn default_devices() -> (Option<String>, Option<String>) {
    let host = cpal::default_host();
    let input = host.default_input_device().and_then(|d| device_name(&d));
    let output = host.default_output_device().and_then(|d| device_name(&d));
    (input, output)
}

/// 按 id（`in:{name}` / `out:{name}`）、精确名或子串查找设备；
/// `query` 为 `None` 时返回默认设备。
pub fn find_device(kind: DeviceKind, query: Option<&str>) -> Result<cpal::Device, VoiceError> {
    let host = cpal::default_host();
    let (label, prefix, default) = match kind {
        DeviceKind::Input => ("input", "in", host.default_input_device()),
        DeviceKind::Output => ("output", "out", host.default_output_device()),
    };
    let Some(q) = query else {
        return default.ok_or_else(|| VoiceError::Device(format!("no default {label} device")));
    };
    let mut devices = match kind {
        DeviceKind::Input => host
            .input_devices()
            .map_err(|e| VoiceError::Device(e.to_string()))?,
        DeviceKind::Output => host
            .output_devices()
            .map_err(|e| VoiceError::Device(e.to_string()))?,
    };
    devices
        .find(|d| {
            device_name(d).is_some_and(|n| n == q || n.contains(q) || format!("{prefix}:{n}") == q)
        })
        .ok_or_else(|| VoiceError::Device(format!("{label} device not found: {q}")))
}
