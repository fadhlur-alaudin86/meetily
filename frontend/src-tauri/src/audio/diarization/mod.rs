// audio/diarization/mod.rs
//
// Speaker diarization module for batch (import/retranscription) and live streaming.
// Uses polyvoice (tract-onnx pure Rust) for Pyannote segmentation and embedding extraction,
// with graceful fallback if models cannot be initialized.

use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A diarized speaker turn with time bounds and speaker label
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiarizationSegment {
    pub start: f64,
    pub end: f64,
    pub speaker: String,
}

/// Helper to get model cache directory for diarization models
pub fn get_diarization_cache_dir() -> PathBuf {
    if let Some(data_dir) = dirs::data_dir() {
        data_dir.join("com.meetily.ai").join("models").join("diarization")
    } else {
        PathBuf::from("/tmp/meetily-diarization")
    }
}

/// Batch diarizer for full audio files (used in import & retranscription)
pub struct BatchDiarizer;

impl BatchDiarizer {
    /// Diarize complete 16kHz audio samples into speaker turns
    pub fn diarize(samples: &[f32], sample_rate: u32) -> Result<Vec<DiarizationSegment>, String> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }

        // Resample to 16kHz if not already
        let samples_16k = if sample_rate != 16000 {
            crate::audio::audio_processing::resample_audio(samples, sample_rate, 16000)
        } else {
            samples.to_vec()
        };

        let cache_dir = get_diarization_cache_dir();
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            warn!("Failed to create diarization cache dir {:?}: {}", cache_dir, e);
        }

        info!(
            "Running batch diarization on {} samples ({:.1}s) with cache at {:?}",
            samples_16k.len(),
            samples_16k.len() as f64 / 16000.0,
            cache_dir
        );

        let registry = match polyvoice::models::ModelRegistry::with_cache_dir(&cache_dir) {
            Ok(r) => r,
            Err(e) => {
                warn!("Failed to initialize polyvoice model registry: {}", e);
                return Err(format!("Registry initialization error: {}", e));
            }
        };

        let pipeline = match polyvoice::Pipeline::builder()
            .profile(polyvoice::types::Profile::Fast)
            .with_models_from(registry)
            .build()
        {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to build polyvoice diarization pipeline: {}", e);
                return Err(format!("Pipeline build error: {}", e));
            }
        };

        match pipeline.run(&samples_16k, polyvoice::types::SampleRate::default()) {
            Ok(result) => {
                info!("Diarization completed: {} turns identified", result.turns.len());
                let segments: Vec<DiarizationSegment> = result
                    .turns
                    .into_iter()
                    .map(|turn| DiarizationSegment {
                        start: turn.time.start,
                        end: turn.time.end,
                        speaker: format!("SPEAKER_{:02}", turn.speaker.0),
                    })
                    .collect();
                Ok(segments)
            }
            Err(e) => {
                warn!("Polyvoice diarization execution failed: {}", e);
                Err(format!("Diarization execution error: {}", e))
            }
        }
    }
}

/// Find the speaker label covering the midpoint of a time range.
/// Returns None if no speaker turn covers the midpoint or range.
pub fn get_speaker_for_time_range(
    segments: &[DiarizationSegment],
    start_sec: f64,
    end_sec: f64,
) -> Option<String> {
    if segments.is_empty() {
        return None;
    }

    let midpoint = (start_sec + end_sec) / 2.0;

    // 1. Primary check: turn that contains the midpoint
    for seg in segments {
        if midpoint >= seg.start && midpoint <= seg.end {
            return Some(seg.speaker.clone());
        }
    }

    // 2. Secondary check: turn with largest overlap
    let mut best_speaker: Option<String> = None;
    let mut best_overlap = 0.0f64;

    for seg in segments {
        let overlap_start = start_sec.max(seg.start);
        let overlap_end = end_sec.min(seg.end);
        let overlap = overlap_end - overlap_start;

        if overlap > best_overlap && overlap > 0.05 {
            best_overlap = overlap;
            best_speaker = Some(seg.speaker.clone());
        }
    }

    best_speaker
}

/// Live speaker tracker for System Audio stream.
/// Clusters system audio turns into SPEAKER_00, SPEAKER_01, etc. based on acoustic energy & spectral profile.
#[derive(Clone)]
pub struct LiveSystemDiarizer {
    speakers: Arc<Mutex<Vec<SpeakerProfile>>>,
    active_speaker: Arc<Mutex<Option<(usize, u64)>>>, // (speaker_idx, last_seen_chunk_id)
    speaker_counter: Arc<Mutex<usize>>,
}

struct SpeakerProfile {
    id: usize,
    spectral_centroid: f32,
    energy_level: f32,
    sample_count: usize,
}

impl LiveSystemDiarizer {
    pub fn new() -> Self {
        Self {
            speakers: Arc::new(Mutex::new(Vec::new())),
            active_speaker: Arc::new(Mutex::new(None)),
            speaker_counter: Arc::new(Mutex::new(0)),
        }
    }

    /// Reset diarizer for a new recording session
    pub fn reset(&self) {
        if let Ok(mut speakers) = self.speakers.lock() {
            speakers.clear();
        }
        if let Ok(mut active) = self.active_speaker.lock() {
            *active = None;
        }
        if let Ok(mut counter) = self.speaker_counter.lock() {
            *counter = 0;
        }
    }

    /// Process a system audio speech segment and assign a speaker label
    pub fn identify_speaker(&self, samples: &[f32], chunk_id: u64) -> String {
        if samples.is_empty() {
            return "SPEAKER_00".to_string();
        }

        // Calculate simple spectral features (Zero Crossing Rate & Energy)
        let energy = (samples.iter().map(|&x| x * x).sum::<f32>() / samples.len() as f32).sqrt();
        let mut zcr_count = 0usize;
        for i in 1..samples.len() {
            if (samples[i] >= 0.0 && samples[i - 1] < 0.0) || (samples[i] < 0.0 && samples[i - 1] >= 0.0) {
                zcr_count += 1;
            }
        }
        let zcr = zcr_count as f32 / samples.len() as f32;

        let mut speakers = self.speakers.lock().unwrap_or_else(|e| e.into_inner());
        let mut active = self.active_speaker.lock().unwrap_or_else(|e| e.into_inner());

        // Check if continuing recent turn (< 3 chunks apart)
        if let Some((current_idx, last_chunk)) = *active {
            if chunk_id.saturating_sub(last_chunk) <= 2 && current_idx < speakers.len() {
                // Update profile slightly
                let profile = &mut speakers[current_idx];
                profile.spectral_centroid = 0.9 * profile.spectral_centroid + 0.1 * zcr;
                profile.energy_level = 0.9 * profile.energy_level + 0.1 * energy;
                profile.sample_count += 1;
                *active = Some((current_idx, chunk_id));
                return format!("SPEAKER_{:02}", profile.id);
            }
        }

        // Compare against existing speaker profiles
        let mut best_idx = None;
        let mut best_distance = f32::MAX;

        for (idx, profile) in speakers.iter().enumerate() {
            let dist = (profile.spectral_centroid - zcr).abs() * 2.0 + (profile.energy_level - energy).abs();
            if dist < best_distance {
                best_distance = dist;
                best_idx = Some(idx);
            }
        }

        // Threshold for creating a new speaker vs matching existing
        if let Some(idx) = best_idx {
            if best_distance < 0.25 {
                let profile = &mut speakers[idx];
                profile.spectral_centroid = 0.8 * profile.spectral_centroid + 0.2 * zcr;
                profile.energy_level = 0.8 * profile.energy_level + 0.2 * energy;
                profile.sample_count += 1;
                *active = Some((idx, chunk_id));
                return format!("SPEAKER_{:02}", profile.id);
            }
        }

        // Create new speaker if limit (< 10) not reached
        let mut counter = self.speaker_counter.lock().unwrap_or_else(|e| e.into_inner());
        let new_id = *counter;
        if *counter < 10 {
            *counter += 1;
        }

        let new_speaker = SpeakerProfile {
            id: new_id,
            spectral_centroid: zcr,
            energy_level: energy,
            sample_count: 1,
        };
        let new_idx = speakers.len();
        speakers.push(new_speaker);
        *active = Some((new_idx, chunk_id));

        format!("SPEAKER_{:02}", new_id)
    }
}

static GLOBAL_LIVE_SYSTEM_DIARIZER: std::sync::OnceLock<LiveSystemDiarizer> = std::sync::OnceLock::new();

/// Get or initialize the global live system diarizer
pub fn get_live_system_diarizer() -> &'static LiveSystemDiarizer {
    GLOBAL_LIVE_SYSTEM_DIARIZER.get_or_init(LiveSystemDiarizer::new)
}

/// Reset the global live system diarizer state
pub fn reset_live_system_diarizer() {
    if let Some(diarizer) = GLOBAL_LIVE_SYSTEM_DIARIZER.get() {
        diarizer.reset();
    }
}
