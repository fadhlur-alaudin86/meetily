use std::sync::Arc;
use anyhow::Result;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, Stream, SupportedStreamConfig};
use log::{error, info, warn};
use tokio::sync::mpsc;

use super::devices::{AudioDevice, get_device_and_config};
use super::pipeline::AudioCapture;
use super::recording_state::{RecordingState, DeviceType};
use super::capture::{AudioCaptureBackend, get_current_backend};

#[cfg(target_os = "macos")]
use super::capture::CoreAudioCapture;

/// Stream backend implementation
pub enum StreamBackend {
    /// CPAL-based stream (ScreenCaptureKit or default)
    Cpal(Stream),
    /// Core Audio direct implementation (macOS only)
    #[cfg(target_os = "macos")]
    CoreAudio {
        task: Option<tokio::task::JoinHandle<()>>,
    },
    /// Linux PulseAudio/PipeWire monitor capture via a helper subprocess
    /// (pw-cat/pw-record/ffmpeg) piping raw PCM into the pipeline
    /// (cpal has no PulseAudio backend).
    #[cfg(target_os = "linux")]
    MonitorPipe {
        task: Option<tokio::task::JoinHandle<()>>,
        child: Option<tokio::process::Child>,
    },
}

// SAFETY: While Stream doesn't implement Send, we ensure it's only accessed
// from the same thread context by using spawn_blocking for operations that cross thread boundaries
unsafe impl Send for StreamBackend {}

/// Simplified audio stream wrapper with multi-backend support
pub struct AudioStream {
    device: Arc<AudioDevice>,
    backend: StreamBackend,
}

// SAFETY: AudioStream contains StreamBackend which we've marked as Send
unsafe impl Send for AudioStream {}

impl AudioStream {
    /// Create a new audio stream for the given device
    pub async fn create(
        device: Arc<AudioDevice>,
        state: Arc<RecordingState>,
        device_type: DeviceType,
        recording_sender: Option<mpsc::UnboundedSender<super::recording_state::AudioChunk>>,
    ) -> Result<Self> {
        // Get current backend from global config
        let backend_type = get_current_backend();
        Self::create_with_backend(device, state, device_type, recording_sender, backend_type).await
    }

    /// Create a new audio stream with explicit backend selection
    pub async fn create_with_backend(
        device: Arc<AudioDevice>,
        state: Arc<RecordingState>,
        device_type: DeviceType,
        recording_sender: Option<mpsc::UnboundedSender<super::recording_state::AudioChunk>>,
        backend_type: AudioCaptureBackend,
    ) -> Result<Self> {
        info!("🎵 Stream: Creating audio stream for device: {} with backend: {:?}, device_type: {:?}",
              device.name, backend_type, device_type);

        // For system audio devices, use the selected backend
        // For microphone devices, always use CPAL
        #[cfg(target_os = "macos")]
        let use_core_audio = device_type == DeviceType::System
            && backend_type == AudioCaptureBackend::CoreAudio;

        #[cfg(not(target_os = "macos"))]
        let use_core_audio = false;

        #[cfg(target_os = "macos")]
        info!("🎵 Stream: use_core_audio = {}, device_type == System: {}, backend == CoreAudio: {}",
              use_core_audio,
              device_type == DeviceType::System,
              backend_type == AudioCaptureBackend::CoreAudio);

        #[cfg(not(target_os = "macos"))]
        info!("🎵 Stream: use_core_audio = {}, device_type == System: {}",
              use_core_audio,
              device_type == DeviceType::System);

        #[cfg(target_os = "macos")]
        if use_core_audio {
            info!("🎵 Stream: Using Core Audio backend (cidre) for system audio");
            return Self::create_core_audio_stream(device, state, device_type, recording_sender).await;
        }

        // Default path: use CPAL
        #[cfg(target_os = "macos")]
        let backend_name = if backend_type == AudioCaptureBackend::ScreenCaptureKit {
            "ScreenCaptureKit"
        } else {
            "CPAL (default)"
        };

        #[cfg(not(target_os = "macos"))]
        let backend_name = "CPAL";

        info!("🎵 Stream: Using CPAL backend ({}) for device: {}", backend_name, device.name);
        Self::create_cpal_stream(device, state, device_type, recording_sender).await
    }

    /// Create a CPAL-based stream (ScreenCaptureKit on macOS)
    async fn create_cpal_stream(
        device: Arc<AudioDevice>,
        state: Arc<RecordingState>,
        device_type: DeviceType,
        recording_sender: Option<mpsc::UnboundedSender<super::recording_state::AudioChunk>>,
    ) -> Result<Self> {
        info!("Creating CPAL stream for device: {}", device.name);

        // Linux PulseAudio/PipeWire monitors cannot be opened through cpal
        // (no PulseAudio backend), so tagged monitor devices are captured
        // via a helper subprocess instead.
        #[cfg(target_os = "linux")]
        if let Some(monitor) = super::devices::platform::pulse_monitor_name(&device.name) {
            return Self::create_monitor_pipe_stream(
                device,
                state,
                device_type,
                recording_sender,
                &monitor,
            )
            .await;
        }

        // Get the underlying cpal device and config
        let (cpal_device, config) = get_device_and_config(&device).await?;

        info!("Audio config - Sample rate: {}, Channels: {}, Format: {:?}",
              config.sample_rate().0, config.channels(), config.sample_format());

        // Create audio capture processor
        let capture = AudioCapture::new(
            device.clone(),
            state.clone(),
            config.sample_rate().0,
            config.channels(),
            device_type,
            recording_sender,
        );

        // Build the appropriate stream based on sample format
        let stream = Self::build_stream(&cpal_device, &config, capture.clone())?;

        // Start the stream
        stream.play()?;
        info!("CPAL stream started for device: {}", device.name);

        Ok(Self {
            device,
            backend: StreamBackend::Cpal(stream),
        })
    }

    /// Probe whether an ffmpeg binary can open a PulseAudio monitor.
    /// Opens the monitor briefly and discards the output; success means the
    /// binary has PulseAudio input support and can reach the sound server.
    #[cfg(target_os = "linux")]
    async fn ffmpeg_supports_pulse(ffmpeg: &std::path::Path, monitor: &str) -> bool {
        let status = tokio::process::Command::new(ffmpeg)
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-f")
            .arg("pulse")
            .arg("-i")
            .arg(monitor)
            .arg("-t")
            .arg("0.2")
            .arg("-f")
            .arg("null")
            .arg("-")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        matches!(status, Ok(s) if s.success())
    }

    /// Create a PulseAudio/PipeWire monitor capture stream (Linux only).
    ///
    /// Captures the monitor as 48kHz stereo float32 and feeds the raw PCM
    /// into the normal pipeline (which downmixes to mono itself). Prefers
    /// PipeWire-native `pw-cat`/`pw-record` (always present where monitors
    /// exist), falling back to ffmpeg only if it probes OK for PulseAudio
    /// input (the bundled static build lacks it). Mirrors the chunking in
    /// `SystemAudioStreamManager::create`.
    #[cfg(target_os = "linux")]
    async fn create_monitor_pipe_stream(
        device: Arc<AudioDevice>,
        state: Arc<RecordingState>,
        device_type: DeviceType,
        recording_sender: Option<mpsc::UnboundedSender<super::recording_state::AudioChunk>>,
        monitor: &str,
    ) -> Result<Self> {
        use tokio::io::AsyncReadExt;

        info!("Creating monitor pipe capture for: {}", monitor);

        // pw-* --target needs the numeric source id; a monitor name
        // silently misses and captures zeros. Ids shift on reconnect, so
        // resolve fresh per recording; fall back to the name on failure.
        let target =
            super::devices::platform::pulse_monitor_target_id(monitor).unwrap_or_else(|| {
                warn!(
                    "Could not resolve target id for monitor '{}', falling back to name",
                    monitor
                );
                monitor.to_string()
            });

        // (binary, args). pw-* default to an AU container on stdout, so we
        // pass --container=raw for header-less f32le; ffmpeg is raw anyway.
        let mut sources: Vec<(std::path::PathBuf, Vec<String>)> = vec![
            (
                std::path::PathBuf::from("pw-cat"),
                vec![
                    format!("--target={}", target),
                    "-r".to_string(),
                    "--format=f32".to_string(),
                    "--rate=48000".to_string(),
                    "--channels=2".to_string(),
                    "--container=raw".to_string(),
                    "-".to_string(),
                ],
            ),
            (
                std::path::PathBuf::from("pw-record"),
                vec![
                    format!("--target={}", target),
                    "--format=f32".to_string(),
                    "--rate=48000".to_string(),
                    "--channels=2".to_string(),
                    "--container=raw".to_string(),
                    "-".to_string(),
                ],
            ),
        ];
        if let Some(ffmpeg) = super::ffmpeg::find_ffmpeg_path() {
            sources.push((
                ffmpeg,
                vec![
                    "-hide_banner".to_string(),
                    "-loglevel".to_string(),
                    "error".to_string(),
                    "-f".to_string(),
                    "pulse".to_string(),
                    "-i".to_string(),
                    monitor.to_string(),
                    "-f".to_string(),
                    "f32le".to_string(),
                    "-ac".to_string(),
                    "2".to_string(),
                    "-ar".to_string(),
                    "48000".to_string(),
                    "-".to_string(),
                ],
            ));
        }
        // Last resort: ffmpeg resolved via PATH.
        sources.push((
            std::path::PathBuf::from("ffmpeg"),
            vec![
                "-hide_banner".to_string(),
                "-loglevel".to_string(),
                "error".to_string(),
                "-f".to_string(),
                "pulse".to_string(),
                "-i".to_string(),
                monitor.to_string(),
                "-f".to_string(),
                "f32le".to_string(),
                "-ac".to_string(),
                "2".to_string(),
                "-ar".to_string(),
                "48000".to_string(),
                "-".to_string(),
            ],
        ));

        let mut child_opt: Option<tokio::process::Child> = None;
        for (bin, args) in &sources {
            // ffmpeg without PulseAudio input support fails instantly; probe it.
            // (The bundled static build lacks it.)
            if bin.to_string_lossy().contains("ffmpeg")
                && !Self::ffmpeg_supports_pulse(bin, monitor).await
            {
                warn!("{:?} cannot open Pulse monitor '{}', trying next", bin, monitor);
                continue;
            }
            match tokio::process::Command::new(bin)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(child) => {
                    info!("Monitor capture for '{}' via {:?}", monitor, bin);
                    child_opt = Some(child);
                    break;
                }
                Err(e) => {
                    warn!("Failed to spawn {:?} for monitor capture: {}", bin, e);
                }
            }
        }
        let mut child = child_opt.ok_or_else(|| {
            anyhow::anyhow!(
                "No working monitor capture backend (pw-cat/pw-record/ffmpeg) for '{}'",
                monitor
            )
        })?;

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("monitor capture stdout unavailable"))?;

        // 48kHz stereo float32 little-endian, matching the ffmpeg arguments.
        let capture = AudioCapture::new(
            device.clone(),
            state.clone(),
            48000,
            2,
            device_type,
            recording_sender,
        );

        let device_name = device.name.clone();
        let task = tokio::spawn({
            let capture = capture.clone();
            async move {
                // Raw f32le stereo @48kHz, no container (pw-* run with
                // --container=raw, ffmpeg with -f f32le).
                let mut byte_buf = vec![0u8; 4096 * 4];
                let mut samples: Vec<f32> = Vec::with_capacity(4096);
                let mut total_bytes: u64 = 0;

                loop {
                    match stdout.read_exact(&mut byte_buf).await {
                        Ok(_) => {
                            total_bytes += byte_buf.len() as u64;
                            samples.extend(byte_buf.chunks_exact(4).map(|c| {
                                f32::from_le_bytes([c[0], c[1], c[2], c[3]])
                            }));
                            if samples.len() >= 4096 {
                                capture.process_audio_data(&samples);
                                samples.clear();
                            }
                        }
                        Err(e) => {
                            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                                info!("Monitor pipe closed for {}", device_name);
                            } else {
                                warn!("Monitor pipe read error for {}: {}", device_name, e);
                            }
                            break;
                        }
                    }
                }

                // Process any remaining samples
                if !samples.is_empty() {
                    capture.process_audio_data(&samples);
                }

                if total_bytes == 0 {
                    warn!(
                        "Monitor pipe for {} captured 0 bytes — monitor may be silent or the capture backend lost the PipeWire connection",
                        device_name
                    );
                }
                info!("Monitor pipe task ended for {}", device_name);
            }
        });

        info!("Monitor pipe capture started for device: {}", device.name);

        Ok(Self {
            device,
            backend: StreamBackend::MonitorPipe {
                task: Some(task),
                child: Some(child),
            },
        })
    }

    /// Create a Core Audio stream (macOS only)
    #[cfg(target_os = "macos")]
    async fn create_core_audio_stream(
        device: Arc<AudioDevice>,
        state: Arc<RecordingState>,
        device_type: DeviceType,
        recording_sender: Option<mpsc::UnboundedSender<super::recording_state::AudioChunk>>,
    ) -> Result<Self> {
        info!("🔊 Stream: Creating Core Audio stream for device: {}", device.name);

        // Create Core Audio capture
        info!("🔊 Stream: Calling CoreAudioCapture::new()...");
        let capture_impl = CoreAudioCapture::new()
            .map_err(|e| {
                error!("❌ Stream: CoreAudioCapture::new() failed: {}", e);
                anyhow::anyhow!("Failed to create Core Audio capture: {}", e)
            })?;

        info!("✅ Stream: CoreAudioCapture created, calling stream()...");
        let core_stream = capture_impl.stream()
            .map_err(|e| {
                error!("❌ Stream: capture_impl.stream() failed: {}", e);
                anyhow::anyhow!("Failed to create Core Audio stream: {}", e)
            })?;

        let sample_rate = core_stream.sample_rate();
        info!("✅ Stream: Core Audio stream created with sample rate: {} Hz", sample_rate);

        // Create audio capture processor for pipeline integration
        // CRITICAL: Core Audio tap is MONO (with_mono_global_tap_excluding_processes)
        let capture = AudioCapture::new(
            device.clone(),
            state.clone(),
            sample_rate,
            1, // Core Audio tap is MONO (not stereo!)
            device_type,
            recording_sender,
        );

        // Spawn task to process Core Audio stream samples
        // The stream needs to be polled continuously to produce samples
        let device_name = device.name.clone();
        info!("🔊 Stream: Spawning tokio task to poll Core Audio stream...");
        let task = tokio::spawn({
            let capture = capture.clone();
            let mut stream = core_stream;

            async move {
                use futures_util::StreamExt;

                let mut buffer = Vec::new();
                let mut frame_count = 0;
                let frames_per_chunk = 1024; // Process in chunks of 1024 samples

                info!("✅ Stream: Core Audio processing task started for {}", device_name);

                let mut _sample_count = 0u64;
                while let Some(sample) = stream.next().await {
                    _sample_count += 1;
                    // if _sample_count % 48000 == 0 {
                    //     info!("📊 Stream: Received {} samples from Core Audio stream", _sample_count);
                    // }

                    buffer.push(sample);
                    frame_count += 1;

                    // Process when we have enough samples
                    if frame_count >= frames_per_chunk {
                        capture.process_audio_data(&buffer);
                        buffer.clear();
                        frame_count = 0;
                    }
                }

                // Process any remaining samples
                if !buffer.is_empty() {
                    capture.process_audio_data(&buffer);
                }

                info!("⚠️ Stream: Core Audio processing task ended for {}", device_name);
            }
        });

        info!("✅ Stream: Core Audio stream fully initialized for device: {}", device.name);

        Ok(Self {
            device: device.clone(),
            backend: StreamBackend::CoreAudio {
                task: Some(task),
            },
        })
    }

    /// Build stream based on sample format
    fn build_stream(
        device: &Device,
        config: &SupportedStreamConfig,
        capture: AudioCapture,
    ) -> Result<Stream> {
        let config_copy = config.clone();

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                let capture_clone = capture.clone();
                device.build_input_stream(
                    &config_copy.into(),
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        capture.process_audio_data(data);
                    },
                    move |err| {
                        capture_clone.handle_stream_error(err);
                    },
                    None,
                )?
            }
            cpal::SampleFormat::I16 => {
                let capture_clone = capture.clone();
                device.build_input_stream(
                    &config_copy.into(),
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        let f32_data: Vec<f32> = data.iter()
                            .map(|&sample| sample as f32 / i16::MAX as f32)
                            .collect();
                        capture.process_audio_data(&f32_data);
                    },
                    move |err| {
                        capture_clone.handle_stream_error(err);
                    },
                    None,
                )?
            }
            cpal::SampleFormat::I32 => {
                let capture_clone = capture.clone();
                device.build_input_stream(
                    &config_copy.into(),
                    move |data: &[i32], _: &cpal::InputCallbackInfo| {
                        let f32_data: Vec<f32> = data.iter()
                            .map(|&sample| sample as f32 / i32::MAX as f32)
                            .collect();
                        capture.process_audio_data(&f32_data);
                    },
                    move |err| {
                        capture_clone.handle_stream_error(err);
                    },
                    None,
                )?
            }
            cpal::SampleFormat::I8 => {
                let capture_clone = capture.clone();
                device.build_input_stream(
                    &config_copy.into(),
                    move |data: &[i8], _: &cpal::InputCallbackInfo| {
                        let f32_data: Vec<f32> = data.iter()
                            .map(|&sample| sample as f32 / i8::MAX as f32)
                            .collect();
                        capture.process_audio_data(&f32_data);
                    },
                    move |err| {
                        capture_clone.handle_stream_error(err);
                    },
                    None,
                )?
            }
            _ => {
                return Err(anyhow::anyhow!("Unsupported sample format: {:?}", config.sample_format()));
            }
        };

        Ok(stream)
    }

    /// Get device info
    pub fn device(&self) -> &AudioDevice {
        &self.device
    }

    /// Stop the stream
    pub fn stop(self) -> Result<()> {
        info!("Stopping audio stream for device: {}", self.device.name);

        match self.backend {
            StreamBackend::Cpal(stream) => {
                // CRITICAL: Pause the stream first to stop callbacks immediately
                // This ensures closures stop executing before we drop the stream,
                // allowing Arc references captured in callbacks to be released
                if let Err(e) = stream.pause() {
                    warn!("Failed to pause stream before drop: {}", e);
                }
                info!("Stream paused, now dropping to release callbacks");
                drop(stream);
            }
            #[cfg(target_os = "macos")]
            StreamBackend::CoreAudio { task } => {
                // Abort the processing task and wait briefly for cleanup
                if let Some(task_handle) = task {
                    info!("Aborting Core Audio task...");
                    task_handle.abort();
                    // Give the runtime a moment to clean up the aborted task
                    // This helps ensure Arc references in the closure are dropped
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    info!("Core Audio task aborted");
                }
            }
            #[cfg(target_os = "linux")]
            StreamBackend::MonitorPipe { task, child } => {
                // Stop the pump task first so it releases the pipe, then kill
                // the ffmpeg subprocess.
                if let Some(task_handle) = task {
                    info!("Aborting monitor pipe task...");
                    task_handle.abort();
                }
                if let Some(mut child) = child {
                    if let Err(e) = child.start_kill() {
                        warn!("Failed to kill monitor capture child process: {}", e);
                    }
                    // Reap the child so it doesn't linger as a zombie.
                    for _ in 0..10 {
                        match child.try_wait() {
                            Ok(Some(_)) => break,
                            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                            Err(e) => {
                                warn!("Failed to reap monitor capture child process: {}", e);
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Explicitly drop self.device Arc reference
        drop(self.device);
        info!("Audio stream stopped and device reference dropped");
        Ok(())
    }
}

/// Audio stream manager for handling multiple streams
pub struct AudioStreamManager {
    microphone_stream: Option<AudioStream>,
    system_stream: Option<AudioStream>,
    state: Arc<RecordingState>,
}

// SAFETY: AudioStreamManager contains AudioStream which we've marked as Send
unsafe impl Send for AudioStreamManager {}

impl AudioStreamManager {
    pub fn new(state: Arc<RecordingState>) -> Self {
        Self {
            microphone_stream: None,
            system_stream: None,
            state,
        }
    }

    /// Start audio streams for the given devices
    pub async fn start_streams(
        &mut self,
        microphone_device: Option<Arc<AudioDevice>>,
        system_device: Option<Arc<AudioDevice>>,
        recording_sender: Option<mpsc::UnboundedSender<super::recording_state::AudioChunk>>,
    ) -> Result<()> {
        use super::capture::get_current_backend;
        let backend = get_current_backend();
        info!("🎙️ Starting audio streams with backend: {:?}", backend);

        // Start microphone stream
        if let Some(mic_device) = microphone_device {
            info!("🎤 Creating microphone stream: {} (always uses CPAL)", mic_device.name);
            match AudioStream::create(mic_device.clone(), self.state.clone(), DeviceType::Microphone, recording_sender.clone()).await {
                Ok(stream) => {
                    self.state.set_microphone_device(mic_device);
                    self.microphone_stream = Some(stream);
                    info!("✅ Microphone stream created successfully");
                }
                Err(e) => {
                    error!("❌ Failed to create microphone stream: {}", e);
                    return Err(e);
                }
            }
        } else {
            info!("ℹ️ No microphone device specified, skipping microphone stream");
        }

        // Start system audio stream
        if let Some(sys_device) = system_device {
            info!("🔊 Creating system audio stream: {} (backend: {:?})", sys_device.name, backend);
            match AudioStream::create(sys_device.clone(), self.state.clone(), DeviceType::System, recording_sender.clone()).await {
                Ok(stream) => {
                    self.state.set_system_device(sys_device);
                    self.system_stream = Some(stream);
                    info!("✅ System audio stream created with {:?} backend", backend);
                }
                Err(e) => {
                    warn!("⚠️ Failed to create system audio stream: {}", e);
                    // Don't fail if only system audio fails
                }
            }
        } else {
            info!("ℹ️ No system device specified, skipping system audio stream");
        }

        // Ensure at least one stream was created
        if self.microphone_stream.is_none() && self.system_stream.is_none() {
            return Err(anyhow::anyhow!("No audio streams could be created"));
        }

        Ok(())
    }

    /// Stop all audio streams
    pub fn stop_streams(&mut self) -> Result<()> {
        info!("Stopping all audio streams");

        let mut errors = Vec::new();

        // Stop microphone stream
        if let Some(mic_stream) = self.microphone_stream.take() {
            if let Err(e) = mic_stream.stop() {
                error!("Failed to stop microphone stream: {}", e);
                errors.push(e);
            }
        }

        // Stop system stream
        if let Some(sys_stream) = self.system_stream.take() {
            if let Err(e) = sys_stream.stop() {
                error!("Failed to stop system stream: {}", e);
                errors.push(e);
            }
        }

        if !errors.is_empty() {
            Err(anyhow::anyhow!("Failed to stop some streams: {:?}", errors))
        } else {
            info!("All audio streams stopped successfully");
            Ok(())
        }
    }

    /// Take the microphone stream OUT of the manager, keeping system audio
    /// running. The caller stops/drops it OUTSIDE any lock — a cpal teardown
    /// of a dead BT device can stall and must not block a held mutex.
    pub fn take_mic_stream(&mut self) -> Option<AudioStream> {
        self.microphone_stream.take()
    }

    /// Set a new microphone stream (used after hot-swap creation).
    pub fn set_mic_stream(&mut self, stream: AudioStream) {
        self.microphone_stream = Some(stream);
    }

    /// Get stream count
    pub fn active_stream_count(&self) -> usize {
        let mut count = 0;
        if self.microphone_stream.is_some() {
            count += 1;
        }
        if self.system_stream.is_some() {
            count += 1;
        }
        count
    }

    /// Check if any streams are active
    pub fn has_active_streams(&self) -> bool {
        self.microphone_stream.is_some() || self.system_stream.is_some()
    }
}

impl Drop for AudioStreamManager {
    fn drop(&mut self) {
        if let Err(e) = self.stop_streams() {
            error!("Error stopping streams during drop: {}", e);
        }
    }
}