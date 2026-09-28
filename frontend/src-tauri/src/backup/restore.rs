//! Restore a meeting from a backup archive (.zip).
//!
//! A backup zip contains only the meeting folder's files (metadata.json,
//! transcripts.json, summary.json, audio). The database rows are rebuilt here:
//! meetings + transcripts + summary_processes + meeting_backups.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use log::{info, warn};
use serde::Serialize;
use serde_json::Value;
use sqlx::SqlitePool;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use uuid::Uuid;
use zip::ZipArchive;

use crate::audio::audio_processing::sanitize_filename;
use crate::audio::recording_preferences::get_default_recordings_folder;
use crate::database::models::MeetingBackup;
use crate::database::repositories::backup::BackupsRepository;
use crate::database::repositories::meeting::MeetingsRepository;

/// Result of inspecting a backup archive without extracting it.
#[derive(Debug, Clone, Serialize)]
pub struct BackupInspection {
    pub zip_path: String,
    pub title: String,
    /// RFC3339 timestamp taken from metadata.json (or restore time as fallback).
    pub created_at: String,
    pub meeting_id: Option<String>,
    pub has_audio: bool,
    pub has_summary: bool,
    pub has_transcripts: bool,
    pub segment_count: usize,
    pub meeting_id_in_db: bool,
}

/// Result of a successful restore.
#[derive(Debug, Clone, Serialize)]
pub struct RestoreResult {
    pub meeting_id: String,
    pub title: String,
    pub folder_path: String,
    pub segment_count: usize,
    pub restored_summary: bool,
    /// Backup record pointing at the source zip, so the caller can emit
    /// `backup-updated` and refresh sidebar badges / settings stats.
    pub backup: MeetingBackup,
}

/// How to handle a restore request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMode {
    /// No conflict: restore using the original identity (meeting_id if free).
    Fresh,
    /// Conflict: replace the existing meeting (rows rebuilt, old folder renamed aside).
    Replace,
    /// Conflict: restore as a new copy alongside the existing meeting.
    KeepBoth,
}

impl RestoreMode {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "fresh" => Ok(Self::Fresh),
            "replace" => Ok(Self::Replace),
            "keep_both" => Ok(Self::KeepBoth),
            other => Err(anyhow!("Unknown restore mode: {}", other)),
        }
    }
}

/// One transcript row to be inserted into the `transcripts` table.
#[derive(Debug, Clone)]
struct SegmentRow {
    text: String,
    timestamp: String,
    audio_start_time: Option<f64>,
    audio_end_time: Option<f64>,
    duration: Option<f64>,
    speaker: Option<String>,
}

/// Everything readable from the archive without touching the filesystem
/// outside of the zip itself.
#[derive(Debug, Default)]
struct BackupContent {
    /// Root prefix inside the zip ("" or "SomeFolder/") so both flat
    /// Meetily archives and single-folder-wrapped archives restore correctly.
    prefix: String,
    meeting_id: Option<String>,
    meeting_name: Option<String>,
    created_at: Option<DateTime<Utc>>,
    /// The `summary` payload from summary.json, if present and parseable.
    summary: Option<Value>,
    segments: Vec<SegmentRow>,
    has_audio: bool,
    has_transcripts: bool,
}

pub struct RestoreService;

impl RestoreService {
    /// Inspect a backup zip: read metadata, count segments, detect conflicts.
    pub async fn inspect(pool: &SqlitePool, zip_path: &Path) -> Result<BackupInspection> {
        let content = read_backup_content(zip_path)?;
        if content.meeting_id.is_none()
            && !content.has_transcripts
            && content.summary.is_none()
            && content.meeting_name.is_none()
        {
            return Err(anyhow!(
                "Not a Meetily backup archive (no metadata.json, transcripts.json, or summary.json): {:?}",
                zip_path
            ));
        }

        let meeting_id_in_db = match &content.meeting_id {
            Some(id) => meeting_exists(pool, id).await?,
            None => false,
        };

        let created_at = content.created_at.unwrap_or_else(Utc::now);
        let title = derive_title(content.meeting_name.as_deref(), zip_path);

        Ok(BackupInspection {
            zip_path: zip_path.to_string_lossy().to_string(),
            title,
            created_at: created_at.to_rfc3339(),
            meeting_id: content.meeting_id,
            has_audio: content.has_audio,
            has_summary: content.summary.is_some(),
            has_transcripts: content.has_transcripts,
            segment_count: content.segments.len(),
            meeting_id_in_db,
        })
    }

    /// Restore a meeting from a backup zip.
    ///
    /// `base_dir` overrides the target root (tests); production passes `None`
    /// and folders are created under the default recordings folder.
    pub async fn restore(
        pool: &SqlitePool,
        zip_path: &Path,
        mode: RestoreMode,
        base_dir: Option<&Path>,
    ) -> Result<RestoreResult> {
        let content = read_backup_content(zip_path)?;
        if content.meeting_id.is_none()
            && !content.has_transcripts
            && content.summary.is_none()
            && content.meeting_name.is_none()
        {
            return Err(anyhow!(
                "Not a Meetily backup archive (no metadata.json, transcripts.json, or summary.json): {:?}",
                zip_path
            ));
        }

        let base = match base_dir {
            Some(p) => p.to_path_buf(),
            None => get_default_recordings_folder(),
        };
        std::fs::create_dir_all(&base)
            .map_err(|e| anyhow!("Failed to create recordings directory {:?}: {}", base, e))?;

        let created_at = content.created_at.unwrap_or_else(Utc::now);
        let title = derive_title(content.meeting_name.as_deref(), zip_path);
        let existing_id = content.meeting_id.clone();
        let conflict = match &existing_id {
            Some(id) => meeting_exists(pool, id).await?,
            None => false,
        };

        // Resolve identity, title and target folder per mode.
        let preferred = preferred_folder_name(&title, created_at);
        let (meeting_id, title, target_folder) = match mode {
            RestoreMode::Replace => {
                let id = existing_id
                    .clone()
                    .ok_or_else(|| anyhow!("Backup archive has no meeting id to replace"))?;
                if !conflict {
                    return Err(anyhow!(
                        "No existing meeting with id {} to replace",
                        id
                    ));
                }
                let meeting = MeetingsRepository::get_meeting_metadata(pool, &id)
                    .await?
                    .ok_or_else(|| anyhow!("Meeting {} not found", id))?;
                let target = match meeting.folder_path {
                    Some(p) if !p.trim().is_empty() => PathBuf::from(p),
                    _ => unique_dir(&base, &preferred)?,
                };
                (id, title, target)
            }
            RestoreMode::KeepBoth => {
                let (id, title_suffix) = if conflict {
                    (format!("meeting-{}", Uuid::new_v4()), true)
                } else {
                    (
                        existing_id
                            .clone()
                            .unwrap_or_else(|| format!("meeting-{}", Uuid::new_v4())),
                        false,
                    )
                };
                let title = if title_suffix {
                    format!("{} (restored)", title)
                } else {
                    title
                };
                let target = unique_dir(&base, &preferred)?;
                (id, title, target)
            }
            RestoreMode::Fresh => {
                if conflict {
                    return Err(anyhow!(
                        "Meeting {} already exists; use replace or keep_both",
                        existing_id.as_deref().unwrap_or_default()
                    ));
                }
                let id = existing_id
                    .clone()
                    .unwrap_or_else(|| format!("meeting-{}", Uuid::new_v4()));
                let target = unique_dir(&base, &preferred)?;
                (id, title, target)
            }
        };

        // Replace: move the old folder aside (never hard delete), then extract.
        let renamed_to: Option<PathBuf> = if mode == RestoreMode::Replace && target_folder.exists()
        {
            let aside = alternate_folder_path(&target_folder)?;
            std::fs::rename(&target_folder, &aside).map_err(|e| {
                anyhow!(
                    "Failed to move existing folder {:?} aside to {:?}: {}",
                    target_folder,
                    aside,
                    e
                )
            })?;
            info!("Replaced meeting folder moved aside: {:?}", aside);
            Some(aside)
        } else {
            None
        };

        if let Err(e) = extract_zip(zip_path, &target_folder, &content.prefix) {
            // Roll back the folder rename so the existing meeting stays intact.
            if let Some(aside) = &renamed_to {
                let _ = std::fs::rename(aside, &target_folder);
            }
            return Err(e);
        }

        // Replace: drop the old rows before inserting the rebuilt ones.
        if mode == RestoreMode::Replace {
            sqlx::query("DELETE FROM meeting_notes WHERE meeting_id = ?")
                .bind(&meeting_id)
                .execute(pool)
                .await
                .map_err(|e| anyhow!("Failed to delete old meeting notes: {}", e))?;
            sqlx::query("DELETE FROM meeting_backups WHERE meeting_id = ?")
                .bind(&meeting_id)
                .execute(pool)
                .await
                .map_err(|e| anyhow!("Failed to delete old backup record: {}", e))?;
            let deleted = MeetingsRepository::delete_meeting(pool, &meeting_id)
                .await
                .map_err(|e| anyhow!("Failed to delete old meeting rows: {}", e))?;
            if !deleted {
                warn!("Meeting {} vanished before replace restore", meeting_id);
            }
        }

        let folder_path = target_folder.to_string_lossy().to_string();
        let insert_result =
            insert_meeting_rows(pool, &meeting_id, &title, created_at, &folder_path, &content).await;

        if let Err(e) = insert_result {
            if mode != RestoreMode::Replace {
                // Fresh copy: remove the folder we just created to avoid orphans.
                let _ = std::fs::remove_dir_all(&target_folder);
            }
            let mut err = anyhow!("Failed to rebuild database rows: {}", e);
            if let Some(aside) = &renamed_to {
                err = anyhow!(
                    "{} (previous meeting folder was preserved at {:?})",
                    err,
                    aside
                );
            }
            return Err(err);
        }

        let segment_count = content.segments.len();
        let restored_summary = content.summary.is_some();

        let backup_record = MeetingBackup {
            meeting_id: meeting_id.clone(),
            backup_path: zip_path.to_string_lossy().to_string(),
            status: if restored_summary { "ok" } else { "partial" }.to_string(),
            backed_up_at: Utc::now().to_rfc3339(),
            has_audio: content.has_audio,
            has_summary: restored_summary,
        };
        if let Err(e) = BackupsRepository::upsert_backup(pool, &backup_record).await {
            warn!(
                "Restored meeting {} but failed to record backup status: {}",
                meeting_id, e
            );
        }

        info!(
            "Restored meeting '{}' ({}) from {:?} with {} transcript segments, summary={}",
            title, meeting_id, zip_path, segment_count, restored_summary
        );

        Ok(RestoreResult {
            meeting_id,
            title,
            folder_path,
            segment_count,
            restored_summary,
            backup: backup_record,
        })
    }
}

// ---------------------------------------------------------------------------
// Archive reading (no extraction)
// ---------------------------------------------------------------------------

/// Read metadata/transcripts/summary out of the zip without extracting.
fn read_backup_content(zip_path: &Path) -> Result<BackupContent> {
    let file = File::open(zip_path)
        .map_err(|e| anyhow!("Failed to open backup archive {:?}: {}", zip_path, e))?;
    let mut archive =
        ZipArchive::new(file).map_err(|e| anyhow!("Invalid zip archive {:?}: {}", zip_path, e))?;

    let names: Vec<String> = (0..archive.len())
        .map(|i| {
            archive
                .by_index(i)
                .map(|e| e.name().to_string())
                .map_err(|e| anyhow!("Failed to read zip entry: {}", e))
        })
        .collect::<Result<_>>()?;

    let prefix = detect_root_prefix(&names);
    let mut content = BackupContent {
        prefix,
        ..Default::default()
    };

    // metadata.json: lenient parse, only the fields we need.
    if let Some(raw) = read_entry(&mut archive, &format!("{}metadata.json", content.prefix)) {
        if let Ok(v) = serde_json::from_slice::<Value>(&raw) {
            content.meeting_id = v
                .get("meeting_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from);
            content.meeting_name = v
                .get("meeting_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from);
            content.created_at = v
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));
        }
    }
    let created_at = content.created_at.unwrap_or_else(Utc::now);
    let fallback_timestamp = created_at.to_rfc3339();

    // Audio presence (root-level entries under the detected prefix).
    content.has_audio = names.iter().any(|n| {
        let rel = n.strip_prefix(&content.prefix).unwrap_or(n);
        !rel.contains('/')
            && matches!(
                rel.to_ascii_lowercase().as_str(),
                s if s.ends_with(".ogg") || s.ends_with(".mp4") || s.ends_with(".wav")
            )
    });

    // transcripts.json: lenient per-segment parse; missing `timestamp`
    // (recording-flow format) falls back to the meeting's created_at.
    let transcripts_name = format!("{}transcripts.json", content.prefix);
    if names.iter().any(|n| *n == transcripts_name) {
        content.has_transcripts = true;
        if let Some(raw) = read_entry(&mut archive, &transcripts_name) {
            content.segments = parse_segments(&raw, &fallback_timestamp);
        }
    }

    // summary.json: keep the `summary` payload.
    let summary_name = format!("{}summary.json", content.prefix);
    if let Some(raw) = read_entry(&mut archive, &summary_name) {
        if let Ok(v) = serde_json::from_slice::<Value>(&raw) {
            if let Some(summary) = v.get("summary").filter(|s| !s.is_null()) {
                content.summary = Some(summary.clone());
            }
        }
    }

    Ok(content)
}

fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Option<Vec<u8>> {
    let mut entry = archive.by_name(name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Archives are normally flat (files at zip root). If the zip wraps everything
/// in a single top-level folder, treat that folder as the root instead.
fn detect_root_prefix(names: &[String]) -> String {
    const MARKERS: [&str; 3] = ["metadata.json", "transcripts.json", "summary.json"];
    if names.iter().any(|n| MARKERS.contains(&n.as_str())) {
        return String::new();
    }

    let mut firsts: BTreeSet<String> = BTreeSet::new();
    for n in names {
        if let Some((first, _)) = n.split_once('/') {
            if !first.is_empty() && first != ".." {
                firsts.insert(first.to_string());
            }
        }
    }
    if firsts.len() == 1 {
        format!("{}/", firsts.iter().next().unwrap())
    } else {
        String::new()
    }
}

/// Parse `transcripts.json` segments tolerantly; skip entries without text.
fn parse_segments(raw: &[u8], fallback_timestamp: &str) -> Vec<SegmentRow> {
    let Ok(v) = serde_json::from_slice::<Value>(raw) else {
        warn!("Failed to parse transcripts.json during restore");
        return Vec::new();
    };
    let Some(arr) = v.get("segments").and_then(Value::as_array) else {
        return Vec::new();
    };

    arr.iter()
        .filter_map(|seg| {
            let text = seg.get("text").and_then(Value::as_str)?.trim();
            if text.is_empty() {
                return None;
            }
            Some(SegmentRow {
                text: text.to_string(),
                timestamp: seg
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .unwrap_or(fallback_timestamp)
                    .to_string(),
                audio_start_time: seg.get("audio_start_time").and_then(Value::as_f64),
                audio_end_time: seg.get("audio_end_time").and_then(Value::as_f64),
                duration: seg.get("duration").and_then(Value::as_f64),
                speaker: seg.get("speaker").and_then(Value::as_str).map(String::from),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Naming helpers
// ---------------------------------------------------------------------------

/// Title priority: metadata.meeting_name, else derived from the zip filename
/// (`YYYY-MM-DD_Title_backup.zip` -> `Title`).
fn derive_title(meeting_name: Option<&str>, zip_path: &Path) -> String {
    if let Some(name) = meeting_name.map(str::trim).filter(|s| !s.is_empty()) {
        return name.to_string();
    }

    let stem = zip_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut title = stem.trim_end_matches("_backup").to_string();
    // Strip a leading YYYY-MM-DD_ date prefix if present.
    let bytes = title.as_bytes();
    let has_date_prefix = bytes.len() >= 11
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'_'
        && bytes[0..4].iter().all(|c| c.is_ascii_digit())
        && bytes[5..7].iter().all(|c| c.is_ascii_digit())
        && bytes[8..10].iter().all(|c| c.is_ascii_digit());
    if has_date_prefix {
        title = title[11..].to_string();
    }
    if title.trim().is_empty() {
        "Restored meeting".to_string()
    } else {
        title
    }
}

/// Preferred folder name following the `create_meeting_folder` convention:
/// `{sanitized_title}_{YYYY-MM-DD_HH-MM}`.
fn preferred_folder_name(title: &str, created_at: DateTime<Utc>) -> String {
    let ts = created_at
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d_%H-%M");
    format!("{}_{}", sanitize_filename(title), ts)
}

/// Pick a non-existing directory name under `base` by appending `_2`, `_3`, ...
fn unique_dir(base: &Path, preferred: &str) -> Result<PathBuf> {
    for i in 1..=9999u32 {
        let name = if i == 1 {
            preferred.to_string()
        } else {
            format!("{}_{}", preferred, i)
        };
        let candidate = base.join(&name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "Could not find a free folder name under {:?} for {}",
        base,
        preferred
    ))
}

/// `<name>.replaced-<timestamp>` next to the original folder (never delete).
fn alternate_folder_path(folder: &Path) -> Result<PathBuf> {
    let parent = folder
        .parent()
        .ok_or_else(|| anyhow!("Folder has no parent: {:?}", folder))?;
    let name = folder
        .file_name()
        .ok_or_else(|| anyhow!("Folder has no name: {:?}", folder))?
        .to_string_lossy()
        .to_string();
    let ts = chrono::Local::now().format("%Y%m%d%H%M%S");

    for i in 1..=9999u32 {
        let suffix = if i == 1 {
            format!("replaced-{}", ts)
        } else {
            format!("replaced-{}-{}", ts, i)
        };
        let candidate = parent.join(format!("{}.{}", name, suffix));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(anyhow!("Could not find an alternate path for {:?}", folder))
}

// ---------------------------------------------------------------------------
// Extraction & DB insertion
// ---------------------------------------------------------------------------

/// Extract zip entries into `target`, stripping the detected root prefix.
/// Entries with traversal/absolute paths (zip-slip) are skipped.
fn extract_zip(zip_path: &Path, target: &Path, prefix: &str) -> Result<()> {
    let file = File::open(zip_path)
        .map_err(|e| anyhow!("Failed to open backup archive {:?}: {}", zip_path, e))?;
    let mut archive =
        ZipArchive::new(file).map_err(|e| anyhow!("Invalid zip archive {:?}: {}", zip_path, e))?;

    std::fs::create_dir_all(target)
        .map_err(|e| anyhow!("Failed to create target folder {:?}: {}", target, e))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| anyhow!("Failed to read zip entry: {}", e))?;

        let Some(safe_path) = entry.enclosed_name() else {
            warn!("Skipping unsafe zip entry: {}", entry.name());
            continue;
        };

        let rel = safe_path.to_string_lossy().replace('\\', "/");
        let Some(stripped) = rel.strip_prefix(prefix) else {
            // Entry outside the detected root prefix; skip.
            continue;
        };
        if stripped.is_empty() || stripped == "/" {
            continue;
        }

        let out_path = target.join(stripped);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| anyhow!("Failed to create directory {:?}: {}", out_path, e))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("Failed to create directory {:?}: {}", parent, e))?;
        }
        let mut out = File::create(&out_path)
            .map_err(|e| anyhow!("Failed to create file {:?}: {}", out_path, e))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| anyhow!("Failed to extract {:?}: {}", out_path, e))?;
    }

    info!("Extracted backup {:?} to {:?}", zip_path, target);
    Ok(())
}

async fn meeting_exists(pool: &SqlitePool, meeting_id: &str) -> Result<bool> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.is_some())
}

/// Insert meetings/transcripts/summary_processes rows in one transaction.
async fn insert_meeting_rows(
    pool: &SqlitePool,
    meeting_id: &str,
    title: &str,
    created_at: DateTime<Utc>,
    folder_path: &str,
    content: &BackupContent,
) -> std::result::Result<(), sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let mut tx = sqlx::Connection::begin(&mut *conn).await?;

    sqlx::query(
        "INSERT INTO meetings (id, title, created_at, updated_at, folder_path)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(meeting_id)
    .bind(title)
    .bind(created_at)
    .bind(Utc::now())
    .bind(folder_path)
    .execute(&mut *tx)
    .await?;

    for seg in &content.segments {
        sqlx::query(
            "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration, speaker)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(format!("transcript-{}", Uuid::new_v4()))
        .bind(meeting_id)
        .bind(&seg.text)
        .bind(&seg.timestamp)
        .bind(seg.audio_start_time)
        .bind(seg.audio_end_time)
        .bind(seg.duration)
        .bind(&seg.speaker)
        .execute(&mut *tx)
        .await?;
    }

    if let Some(summary) = &content.summary {
        let result_str = serde_json::to_string(summary)
            .map_err(|e| sqlx::Error::Protocol(format!("Failed to serialize summary: {}", e)))?;
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO summary_processes
             (meeting_id, status, created_at, updated_at, start_time, end_time, result, chunk_count, processing_time)
             VALUES (?, 'completed', ?, ?, ?, ?, ?, 0, 0.0)",
        )
        .bind(meeting_id)
        .bind(created_at)
        .bind(now)
        .bind(now)
        .bind(now)
        .bind(result_str)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn write_zip(path: &Path, files: &[(&str, &str)]) {
        let file = File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in files {
            writer.start_file(*name, opts).unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }

    const METADATA: &str = r#"{
        "version": "1.0",
        "meeting_id": "meeting-abc",
        "meeting_name": "Team Standup",
        "created_at": "2026-09-20T08:30:00+00:00",
        "status": "completed"
    }"#;

    const METADATA_NO_ID: &str = r#"{
        "version": "1.0",
        "meeting_name": "Team Standup",
        "created_at": "2026-09-20T08:30:00+00:00",
        "status": "completed"
    }"#;

    const TRANSCRIPTS: &str = r#"{
        "version": "1.0",
        "total_segments": 2,
        "segments": [
            {
                "id": "a",
                "text": "Hello everyone",
                "timestamp": "2026-09-20T08:30:05+00:00",
                "audio_start_time": 0.0,
                "audio_end_time": 1.5,
                "duration": 1.5,
                "speaker": "A"
            },
            {
                "id": "b",
                "text": "Recording-flow segment without timestamp",
                "audio_start_time": 1.5,
                "audio_end_time": 3.0,
                "duration": 1.5,
                "display_time": 1.5,
                "confidence": 0.9,
                "sequence_id": 1
            }
        ]
    }"#;

    const SUMMARY: &str = r##"{
        "meeting_id": "meeting-abc",
        "title": "Team Standup",
        "summary": {"markdown": "# Standup notes"}
    }"##;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        for ddl in [
            "CREATE TABLE meetings (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, folder_path TEXT)",
            "CREATE TABLE transcripts (id TEXT PRIMARY KEY, meeting_id TEXT NOT NULL, transcript TEXT NOT NULL, timestamp TEXT NOT NULL, summary TEXT, action_items TEXT, key_points TEXT, audio_start_time REAL, audio_end_time REAL, duration REAL, speaker TEXT)",
            "CREATE TABLE summary_processes (meeting_id TEXT PRIMARY KEY, status TEXT NOT NULL, created_at TEXT, updated_at TEXT, start_time TEXT, end_time TEXT, result TEXT, chunk_count INTEGER, processing_time REAL, error TEXT)",
            "CREATE TABLE transcript_chunks (meeting_id TEXT PRIMARY KEY, meeting_name TEXT, transcript_text TEXT NOT NULL, model TEXT NOT NULL, model_name TEXT NOT NULL, chunk_size INTEGER, overlap INTEGER, created_at TEXT NOT NULL)",
            "CREATE TABLE meeting_backups (meeting_id TEXT PRIMARY KEY, backup_path TEXT NOT NULL, status TEXT NOT NULL, backed_up_at TEXT NOT NULL, has_audio INTEGER NOT NULL DEFAULT 1, has_summary INTEGER NOT NULL DEFAULT 0)",
            "CREATE TABLE meeting_notes (meeting_id TEXT PRIMARY KEY, notes_markdown TEXT, notes_json TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        ] {
            sqlx::query(ddl).execute(&pool).await.unwrap();
        }
        pool
    }

    fn full_backup_zip(dir: &Path) -> PathBuf {
        let zip_path = dir.join("2026-09-20_Team-Standup_backup.zip");
        write_zip(
            &zip_path,
            &[
                ("metadata.json", METADATA),
                ("transcripts.json", TRANSCRIPTS),
                ("summary.json", SUMMARY),
                ("mic.ogg", "fake-ogg-bytes"),
            ],
        );
        zip_path
    }

    #[test]
    fn test_restore_mode_parse_rejects_unknown() {
        assert_eq!(RestoreMode::parse("fresh").unwrap(), RestoreMode::Fresh);
        assert_eq!(RestoreMode::parse("replace").unwrap(), RestoreMode::Replace);
        assert_eq!(
            RestoreMode::parse("keep_both").unwrap(),
            RestoreMode::KeepBoth
        );
        assert!(RestoreMode::parse("skip").is_err());
    }

    #[test]
    fn test_derive_title_from_zip_filename() {
        let p = PathBuf::from("/backups/2026-09-21_Quarterly-Review_backup.zip");
        assert_eq!(
            derive_title(None, &p),
            "Quarterly-Review",
            "date prefix and _backup suffix stripped"
        );

        let plain = PathBuf::from("/backups/My_Meeting_backup.zip");
        assert_eq!(derive_title(None, &plain), "My_Meeting");

        let fallback = PathBuf::from("/backups/_backup.zip");
        assert_eq!(derive_title(None, &fallback), "Restored meeting");

        assert_eq!(derive_title(Some("From Metadata"), &p), "From Metadata");
    }

    #[test]
    fn test_detect_root_prefix() {
        let flat = vec!["metadata.json".to_string(), "mic.ogg".to_string()];
        assert_eq!(detect_root_prefix(&flat), "");

        let nested = vec![
            "Meeting-2026/metadata.json".to_string(),
            "Meeting-2026/mic.ogg".to_string(),
        ];
        assert_eq!(detect_root_prefix(&nested), "Meeting-2026/");

        let multi = vec![
            "a/metadata.json".to_string(),
            "b/metadata.json".to_string(),
        ];
        assert_eq!(detect_root_prefix(&multi), "");
    }

    #[test]
    fn test_parse_segments_lenient_timestamp_fallback() {
        let segments = parse_segments(TRANSCRIPTS.as_bytes(), "2026-09-20T08:30:00+00:00");
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].timestamp, "2026-09-20T08:30:05+00:00");
        assert_eq!(segments[0].speaker.as_deref(), Some("A"));
        // Recording-flow segments have no `timestamp` field.
        assert_eq!(segments[1].timestamp, "2026-09-20T08:30:00+00:00");
        assert_eq!(segments[1].audio_start_time, Some(1.5));

        // Entries without text are dropped; malformed JSON yields nothing.
        let no_text = r#"{"segments":[{"timestamp":"t"},{"text":"   "}]}"#;
        assert!(parse_segments(no_text.as_bytes(), "x").is_empty());
        assert!(parse_segments(b"not json", "x").is_empty());
    }

    #[tokio::test]
    async fn test_inspect_reads_metadata_segments_and_conflict() {
        let pool = test_pool().await;
        let dir = TempDir::new().unwrap();
        let zip_path = full_backup_zip(dir.path());

        let info = RestoreService::inspect(&pool, &zip_path).await.unwrap();
        assert_eq!(info.title, "Team Standup");
        assert_eq!(info.meeting_id.as_deref(), Some("meeting-abc"));
        assert_eq!(info.created_at, "2026-09-20T08:30:00+00:00");
        assert_eq!(info.segment_count, 2);
        assert!(info.has_audio);
        assert!(info.has_summary);
        assert!(info.has_transcripts);
        assert!(!info.meeting_id_in_db, "no meetings row yet");

        // Seed the meeting -> conflict detected.
        sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('meeting-abc', 'Old', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let info = RestoreService::inspect(&pool, &zip_path).await.unwrap();
        assert!(info.meeting_id_in_db);
    }

    #[tokio::test]
    async fn test_inspect_rejects_foreign_zip_and_reads_nested() {
        let pool = test_pool().await;
        let dir = TempDir::new().unwrap();

        // Not a meeting backup.
        let foreign = dir.path().join("random.zip");
        write_zip(&foreign, &[("readme.txt", "hello")]);
        assert!(RestoreService::inspect(&pool, &foreign).await.is_err());

        // Single top-level folder wrapping the backup.
        let nested = dir.path().join("nested.zip");
        write_zip(
            &nested,
            &[
                ("Meeting-2026/metadata.json", METADATA),
                ("Meeting-2026/transcripts.json", TRANSCRIPTS),
            ],
        );
        let info = RestoreService::inspect(&pool, &nested).await.unwrap();
        assert_eq!(info.title, "Team Standup");
        assert_eq!(info.segment_count, 2);
    }

    #[tokio::test]
    async fn test_restore_fresh_rebuilds_rows_and_folder() {
        let pool = test_pool().await;
        let dir = TempDir::new().unwrap();
        let base = dir.path().join("recordings");
        let zip_path = full_backup_zip(dir.path());

        let result = RestoreService::restore(&pool, &zip_path, RestoreMode::Fresh, Some(&base))
            .await
            .unwrap();

        // Identity preserved (metadata id, no conflict).
        assert_eq!(result.meeting_id, "meeting-abc");
        assert_eq!(result.title, "Team Standup");
        assert!(result.restored_summary);
        assert_eq!(result.segment_count, 2);

        // Folder follows the create_meeting_folder naming convention.
        let folder = PathBuf::from(&result.folder_path);
        assert!(folder.exists());
        assert!(folder.starts_with(&base));
        assert!(folder.join("metadata.json").exists());
        assert!(folder.join("mic.ogg").exists());
        let expected_name = preferred_folder_name("Team Standup", "2026-09-20T08:30:00+00:00".parse::<DateTime<Utc>>().unwrap());
        assert_eq!(folder.file_name().unwrap().to_string_lossy(), expected_name);

        // DB rows rebuilt.
        let title: String =
            sqlx::query_scalar("SELECT title FROM meetings WHERE id = 'meeting-abc'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(title, "Team Standup");

        let transcript_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM transcripts WHERE meeting_id = 'meeting-abc'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(transcript_count.0, 2);

        // The timestamp-less segment falls back to the meeting created_at.
        let ts: String =
            sqlx::query_scalar("SELECT timestamp FROM transcripts WHERE transcript LIKE 'Recording-flow%'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(ts, "2026-09-20T08:30:00+00:00");

        let status: String =
            sqlx::query_scalar("SELECT status FROM summary_processes WHERE meeting_id = 'meeting-abc'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "completed");

        // Backup record points at the source zip with summary-based status.
        assert_eq!(result.backup.backup_path, zip_path.to_string_lossy());
        assert_eq!(result.backup.status, "ok");
        let backup_rows: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM meeting_backups WHERE meeting_id = 'meeting-abc'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(backup_rows.0, 1);
    }

    #[tokio::test]
    async fn test_restore_fresh_twice_gets_unique_folder() {
        let pool = test_pool().await;
        let dir = TempDir::new().unwrap();
        let base = dir.path().join("recordings");
        // Same archive twice, but without a meeting_id: each fresh restore
        // gets a new identity and must not overwrite the first folder.
        let zip_path = dir.path().join("2026-09-20_Team-Standup_backup.zip");
        write_zip(
            &zip_path,
            &[
                ("metadata.json", METADATA_NO_ID),
                ("transcripts.json", TRANSCRIPTS),
            ],
        );

        let first = RestoreService::restore(&pool, &zip_path, RestoreMode::Fresh, Some(&base))
            .await
            .unwrap();
        let second = RestoreService::restore(&pool, &zip_path, RestoreMode::Fresh, Some(&base))
            .await
            .unwrap();

        assert_ne!(first.meeting_id, second.meeting_id, "fresh identity each time");
        assert_ne!(first.folder_path, second.folder_path);
        assert!(
            PathBuf::from(&second.folder_path).join("metadata.json").exists(),
            "second restore must not overwrite the first folder"
        );
        // Both meetings coexist.
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM meetings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 2);
    }

    #[tokio::test]
    async fn test_restore_replace_renames_folder_and_replaces_rows() {
        let pool = test_pool().await;
        let dir = TempDir::new().unwrap();
        let base = dir.path().join("recordings");
        let zip_path = full_backup_zip(dir.path());

        // Existing meeting with its own folder and rows.
        let old_folder = base.join("Old-Standup_2026-01-01_10-00");
        std::fs::create_dir_all(&old_folder).unwrap();
        std::fs::write(old_folder.join("old.txt"), b"old content").unwrap();
        sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at, folder_path)
             VALUES ('meeting-abc', 'Old Standup', '2026-01-01T10:00:00+00:00', '2026-01-01T10:00:00+00:00', ?)",
        )
        .bind(old_folder.to_string_lossy().to_string())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO transcripts (id, meeting_id, transcript, timestamp) VALUES ('t-old', 'meeting-abc', 'old text', '2026-01-01T10:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO meeting_notes (meeting_id, created_at, updated_at) VALUES ('meeting-abc', '2026-01-01', '2026-01-01')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = RestoreService::restore(&pool, &zip_path, RestoreMode::Replace, Some(&base))
            .await
            .unwrap();

        assert_eq!(result.meeting_id, "meeting-abc", "replace keeps the id");
        assert_eq!(result.title, "Team Standup");

        // Old content moved aside, never deleted; new content extracted at
        // the original folder path (which is the meeting's folder_path).
        assert!(
            !old_folder.join("old.txt").exists(),
            "old files no longer at the original path"
        );
        let parent = old_folder.parent().unwrap();
        let replaced: Vec<PathBuf> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .contains(".replaced-")
            })
            .collect();
        assert_eq!(replaced.len(), 1, "exactly one .replaced-* folder");
        assert!(
            replaced[0].join("old.txt").exists(),
            "old files preserved in the .replaced folder"
        );

        // New files live at the original folder path.
        assert!(old_folder.join("metadata.json").exists());
        assert!(old_folder.join("mic.ogg").exists());

        // DB rows replaced; old notes gone.
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM transcripts WHERE meeting_id = 'meeting-abc'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 2, "old transcript rows replaced");
        let notes: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM meeting_notes WHERE meeting_id = 'meeting-abc'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(notes.0, 0, "stale notes removed with replaced content");
        let meetings: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM meetings WHERE id = 'meeting-abc'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(meetings.0, 1, "no duplicate meeting rows");
    }

    #[tokio::test]
    async fn test_restore_replace_errors_without_conflict() {
        let pool = test_pool().await;
        let dir = TempDir::new().unwrap();
        let base = dir.path().join("recordings");
        let zip_path = full_backup_zip(dir.path());

        let err = RestoreService::restore(&pool, &zip_path, RestoreMode::Replace, Some(&base))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("No existing meeting"));
        assert!(
            !base.exists() || std::fs::read_dir(&base).unwrap().next().is_none(),
            "nothing extracted when replace is refused"
        );
    }

    #[tokio::test]
    async fn test_restore_keep_both_creates_suffixed_copy() {
        let pool = test_pool().await;
        let dir = TempDir::new().unwrap();
        let base = dir.path().join("recordings");
        let zip_path = full_backup_zip(dir.path());

        // Existing conflict.
        sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('meeting-abc', 'Team Standup', '2026-09-20T08:30:00+00:00', '2026-09-20T08:30:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = RestoreService::restore(&pool, &zip_path, RestoreMode::KeepBoth, Some(&base))
            .await
            .unwrap();

        assert_ne!(result.meeting_id, "meeting-abc", "fresh identity assigned");
        assert!(result.meeting_id.starts_with("meeting-"));
        assert_eq!(result.title, "Team Standup (restored)");
        assert!(PathBuf::from(&result.folder_path).exists());

        // Original meeting untouched.
        let original: String =
            sqlx::query_scalar("SELECT title FROM meetings WHERE id = 'meeting-abc'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(original, "Team Standup");
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM meetings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 2);
    }

    #[tokio::test]
    async fn test_restore_fresh_errors_on_conflict() {
        let pool = test_pool().await;
        let dir = TempDir::new().unwrap();
        let base = dir.path().join("recordings");
        let zip_path = full_backup_zip(dir.path());

        sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('meeting-abc', 'Team Standup', '2026-09-20T08:30:00+00:00', '2026-09-20T08:30:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let err = RestoreService::restore(&pool, &zip_path, RestoreMode::Fresh, Some(&base))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn test_extract_rejects_zip_slip() {
        let dir = TempDir::new().unwrap();
        let zip_path = dir.path().join("evil.zip");
        write_zip(
            &zip_path,
            &[
                ("../evil.txt", "escape"),
                ("ok.txt", "fine"),
                ("/abs.txt", "absolute"),
            ],
        );

        let target = dir.path().join("target");
        extract_zip(&zip_path, &target, "").unwrap();

        assert!(target.join("ok.txt").exists(), "safe entry extracted");
        assert!(
            !dir.path().join("evil.txt").exists(),
            "traversal entry must not escape the target folder"
        );
        assert!(!Path::new("/abs.txt").exists() || {
            // /abs.txt may pre-exist on some systems; only assert we never wrote it.
            std::fs::metadata("/abs.txt")
                .map(|m| m.len() != 8)
                .unwrap_or(true)
        });
    }
}
