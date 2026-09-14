//! Task 1 / Task 4 验证：枚举音频设备并打印实际流配置。
//!
//! ```sh
//! cargo run -p voice-core --example devices
//! ```

fn main() -> anyhow::Result<()> {
    let devices = voice_core::audio::list_devices()?;
    println!("devices ({}):", devices.len());
    for d in &devices {
        let dir = match d.kind {
            voice_common::DeviceKind::Input => "in ",
            voice_common::DeviceKind::Output => "out",
        };
        let mark = if d.is_default { " [default]" } else { "" };
        println!("  {dir} {}{mark}  (id: {})", d.name, d.id);
    }

    let (input, output) = voice_core::audio::default_devices();
    println!("default input:  {}", input.as_deref().unwrap_or("<none>"));
    println!("default output: {}", output.as_deref().unwrap_or("<none>"));

    match voice_core::audio::probe_default_input() {
        Ok(cfg) => println!(
            "input stream:  {}Hz ch={} buffer={:?}",
            cfg.sample_rate(),
            cfg.channels(),
            cfg.buffer_size()
        ),
        Err(e) => println!("input stream:  <unavailable: {e}>"),
    }
    match voice_core::audio::probe_default_output() {
        Ok(cfg) => println!(
            "output stream: {}Hz ch={} buffer={:?}",
            cfg.sample_rate(),
            cfg.channels(),
            cfg.buffer_size()
        ),
        Err(e) => println!("output stream: <unavailable: {e}>"),
    }
    Ok(())
}
