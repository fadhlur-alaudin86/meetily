// Platform-specific audio device implementations

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub mod linux;

// Re-export platform-specific functions
#[cfg(target_os = "windows")]
pub use windows::{configure_windows_audio, get_windows_device};

#[cfg(target_os = "macos")]
pub use macos::configure_macos_audio;

#[cfg(target_os = "linux")]
pub use linux::{
    active_sink_monitor_name, configure_linux_audio, default_sink_monitor_name,
    default_source_name, list_pulse_monitor_sources, pulse_monitor_name,
    pulse_monitor_target_id,
};