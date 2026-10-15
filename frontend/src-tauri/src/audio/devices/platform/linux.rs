use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait};

use crate::audio::devices::configuration::{AudioDevice, DeviceType};

/// Configure Linux audio devices using ALSA/PulseAudio
pub fn configure_linux_audio(host: &cpal::Host) -> Result<Vec<AudioDevice>> {
    let mut devices = Vec::new();

    // Add input devices
    for device in host.input_devices()? {
        if let Ok(name) = device.name() {
            devices.push(AudioDevice::new(name, DeviceType::Input));
        }
    }

    // Add PulseAudio/PipeWire monitor sources for system audio. cpal has no
    // PulseAudio backend in this build, so monitors — the only way to
    // capture speaker output — are enumerated out-of-band via `pactl`.
    // Classic PulseAudio names them "Monitor of ..." (capital M), PipeWire
    // "*.monitor".
    for name in list_pulse_monitor_sources() {
        if !devices.iter().any(|d| d.name.starts_with(&name)) {
            devices.push(AudioDevice::new(
                format!("{} (System Audio)", name),
                DeviceType::Output
            ));
        }
    }

    Ok(devices)
}

/// List PulseAudio/PipeWire monitor source names via `pactl`.
/// Returns an empty vec when pactl is missing or reports nothing.
pub fn list_pulse_monitor_sources() -> Vec<String> {
    let output = std::process::Command::new("pactl")
        .arg("list")
        .arg("short")
        .arg("sources")
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        // `pactl list short sources` columns: index, name, driver, ...
        .filter_map(|line| line.split('\t').nth(1).map(str::to_string))
        .filter(|name| name.to_lowercase().contains("monitor"))
        .collect()
}

/// Monitor source of the default sink, for "Default System Audio".
/// PipeWire/PulseAudio name it "<sink>.monitor".
pub fn default_sink_monitor_name() -> Option<String> {
    let output = std::process::Command::new("pactl")
        .arg("get-default-sink")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sink = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sink.is_empty() {
        return None;
    }
    Some(format!("{}.monitor", sink))
}

/// Raw name of the PulseAudio/PipeWire default source, if any.
/// Doubles as a pactl-availability probe.
pub fn default_source_name() -> Option<String> {
    let output = std::process::Command::new("pactl")
        .arg("get-default-source")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let source = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if source.is_empty() {
        return None;
    }
    Some(source)
}

/// Monitor of the sink that currently has RUNNING playback, if any.
/// This is what "Default System Audio" should capture: the sink that is
/// actually producing sound, which may differ from the default sink
/// (e.g. a stream routed to a Bluetooth headset).
pub fn active_sink_monitor_name() -> Option<String> {
    let output = std::process::Command::new("pactl")
        .arg("list")
        .arg("short")
        .arg("sinks")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // `pactl list short sinks` columns: id, name, driver, format,
    // channels, rate, state.
    let monitors = list_pulse_monitor_sources();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 7 || cols[6].trim() != "RUNNING" {
            continue;
        }
        let monitor = format!("{}.monitor", cols[1]);
        if monitors.iter().any(|m| m == &monitor) {
            return Some(monitor);
        }
    }
    None
}

/// PipeWire target id (first column of `pactl list short sources`) for a
/// monitor source name. `pw-cat`/`pw-record --target` only link monitor
/// ports reliably with the numeric id — a monitor *name* silently falls
/// back to the default source chain (verified via `pw-link`: name links
/// land on the mic filter with zeros, the id links `...:monitor_FL/FR`).
pub fn pulse_monitor_target_id(monitor_name: &str) -> Option<String> {
    let output = std::process::Command::new("pactl")
        .arg("list")
        .arg("short")
        .arg("sources")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut cols = line.split('\t');
        let (Some(id), Some(name)) = (cols.next(), cols.next()) else {
            continue;
        };
        if name == monitor_name {
            return Some(id.to_string());
        }
    }
    None
}

/// Strip the " (System Audio)" tag added at enumeration, returning the raw
/// PulseAudio/PipeWire monitor source name. `None` for untagged names.
pub fn pulse_monitor_name(tagged: &str) -> Option<String> {
    tagged.strip_suffix(" (System Audio)").map(str::to_string)
}