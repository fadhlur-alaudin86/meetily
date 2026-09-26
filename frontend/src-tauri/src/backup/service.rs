use anyhow::{anyhow, Result};
use chrono::Utc;
use log::{error, info, warn};
use sqlx::SqlitePool;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use tauri::{AppHandle, Runtime};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use super::preferences::{ensure_backup_directory, load_backup_preferences};
use crate::audio::audio_processing::sanitize_filename;
use crate::database::models::MeetingBackup;
use crate::database::repositories::backup::BackupsRepository;
use crate::database::repositories::meeting::MeetingsRepository;

pub struct BackupService;

impl BackupService {
    /// Packages a meeting folder into a standalone .zip archive in the target directory
    pub async fn backup_meeting<R: Runtime>(
        app: &AppHandle<R>,
        pool: &SqlitePool,
        meeting_id: &str,
        custom_target_dir: Option<PathBuf>,
    ) -> Result<MeetingBackup> {
        let meeting = MeetingsRepository::get_meeting_metadata(pool, meeting_id)
            .await?
            .ok_or_else(|| anyhow!("Meeting with id {} not found", meeting_id))?;

        let folder_path_str = meeting
            .folder_path
            .filter(|p| !p.trim().is_empty())
            .ok_or_else(|| anyhow!("Meeting {} has no folder_path", meeting_id))?;

        let meeting_folder = PathBuf::from(&folder_path_str);
        if !meeting_folder.exists() {
            return Err(anyhow!("Meeting folder does not exist: {:?}", meeting_folder));
        }

        // Determine target backup directory
        let backup_dir = match custom_target_dir {
            Some(dir) => dir,
            None => {
                let prefs = load_backup_preferences(app).await?;
                prefs.backup_folder
            }
        };
        ensure_backup_directory(&backup_dir)?;

        // Format zip filename: {date}_{sanitized_title}_backup.zip
        let date_prefix = meeting
            .created_at
            .0
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string();
        let sanitized_title = sanitize_filename(&meeting.title);
        let zip_filename = format!("{}_{}_backup.zip", date_prefix, sanitized_title);
        let target_zip_path = backup_dir.join(&zip_filename);

        info!(
            "Creating backup package for meeting '{}' at {:?}",
            meeting.title, target_zip_path
        );

        let (has_audio, has_summary) = Self::pack_meeting_zip(&meeting_folder, &target_zip_path)?;

        let status = if has_summary {
            "ok".to_string()
        } else {
            "partial".to_string()
        };

        let backup_record = MeetingBackup {
            meeting_id: meeting_id.to_string(),
            backup_path: target_zip_path.to_string_lossy().to_string(),
            status,
            backed_up_at: Utc::now().to_rfc3339(),
            has_audio,
            has_summary,
        };

        BackupsRepository::upsert_backup(pool, &backup_record).await?;

        info!(
            "Successfully created backup for meeting {}: status={}",
            meeting_id, backup_record.status
        );
        Ok(backup_record)
    }

    /// Backs up all meetings in SQLite that have a valid folder_path
    pub async fn backup_all_meetings<R: Runtime>(
        app: &AppHandle<R>,
        pool: &SqlitePool,
    ) -> Result<Vec<MeetingBackup>> {
        let meetings = MeetingsRepository::get_meetings(pool).await?;
        let mut results = Vec::new();

        for meeting in meetings {
            if meeting.folder_path.as_ref().map_or(false, |p| !p.trim().is_empty()) {
                match Self::backup_meeting(app, pool, &meeting.id, None).await {
                    Ok(backup) => results.push(backup),
                    Err(e) => {
                        warn!("Skipped meeting {} during backup_all: {}", meeting.id, e);
                    }
                }
            }
        }

        Ok(results)
    }

    /// Packs all files in `meeting_folder` into a zip archive at `target_zip_path`.
    ///
    /// - Skips `.checkpoints/` sub-directory and any hidden or `.tmp` files.
    /// - Returns `(has_audio, has_summary)` derived from the files packed.
    /// - Writes atomically: fills a `.tmp` file first, then renames on success.
    fn pack_meeting_zip(meeting_folder: &std::path::Path, target_zip_path: &std::path::Path) -> Result<(bool, bool)> {
        let parent = target_zip_path
            .parent()
            .ok_or_else(|| anyhow!("Target zip path has no parent directory"))?;
        let tmp_path = parent.join(format!(
            ".{}.tmp",
            target_zip_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ));

        let tmp_file = File::create(&tmp_path)
            .map_err(|e| anyhow!("Failed to create temp zip file {:?}: {}", tmp_path, e))?;

        let mut zip = ZipWriter::new(tmp_file);
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);

        let mut has_audio = false;
        let mut has_summary = false;

        Self::walk_and_pack(meeting_folder, meeting_folder, &mut zip, &options, &mut has_audio, &mut has_summary)
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                e
            })?;

        zip.finish()
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                anyhow!("Failed to finalize zip archive: {}", e)
            })?;

        // Atomic rename
        std::fs::rename(&tmp_path, target_zip_path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            anyhow!("Failed to rename temp zip to final path {:?}: {}", target_zip_path, e)
        })?;

        info!(
            "Packed meeting folder {:?} -> {:?} (has_audio={}, has_summary={})",
            meeting_folder, target_zip_path, has_audio, has_summary
        );

        Ok((has_audio, has_summary))
    }

    /// Recursively walks `current_dir` relative to `base`, adding files to the zip writer.
    fn walk_and_pack(
        base: &std::path::Path,
        current_dir: &std::path::Path,
        zip: &mut ZipWriter<File>,
        options: &SimpleFileOptions,
        has_audio: &mut bool,
        has_summary: &mut bool,
    ) -> Result<()> {
        for entry in std::fs::read_dir(current_dir)
            .map_err(|e| anyhow!("Failed to read directory {:?}: {}", current_dir, e))?
        {
            let entry = entry.map_err(|e| anyhow!("Failed to read dir entry: {}", e))?;
            let path = entry.path();

            // Compute relative path for the zip entry name
            let rel = path.strip_prefix(base)
                .map_err(|_| anyhow!("Path {:?} is not under base {:?}", path, base))?;
            let rel_str = rel.to_string_lossy().replace('\\', "/");

            if path.is_dir() {
                // Skip .checkpoints - only needed for in-progress encoding, not for portability
                let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                if dir_name == ".checkpoints" {
                    continue;
                }
                // Recurse into other sub-directories
                Self::walk_and_pack(base, &path, zip, options, has_audio, has_summary)?;
            } else {
                let file_name = path.file_name().unwrap_or_default().to_string_lossy();

                // Skip temp files and hidden files
                if file_name.ends_with(".tmp") || file_name.starts_with('.') {
                    continue;
                }

                // Detect audio and summary presence
                let lower = file_name.to_lowercase();
                if lower.ends_with(".ogg") || lower.ends_with(".mp4") || lower.ends_with(".wav") {
                    *has_audio = true;
                }
                if lower == "summary.json" {
                    *has_summary = true;
                }

                // Read and add to zip
                let mut f = File::open(&path)
                    .map_err(|e| anyhow!("Failed to open {:?}: {}", path, e))?;
                zip.start_file(rel_str, *options)
                    .map_err(|e| anyhow!("Failed to start zip entry: {}", e))?;
                let mut buf = Vec::new();
                f.read_to_end(&mut buf)
                    .map_err(|e| anyhow!("Failed to read {:?}: {}", path, e))?;
                zip.write_all(&buf)
                    .map_err(|e| anyhow!("Failed to write zip entry: {}", e))?;
            }
        }
        Ok(())
    }

    /// Background trigger for auto-backup
    pub fn trigger_auto_backup<R: Runtime>(
        app: AppHandle<R>,
        pool: SqlitePool,
        meeting_id: String,
    ) {
        tokio::spawn(async move {
            match load_backup_preferences(&app).await {
                Ok(prefs) => {
                    if !prefs.auto_backup {
                        info!("Auto-backup is disabled, skipping for meeting {}", meeting_id);
                        return;
                    }

                    info!("Auto-backup triggered for meeting {}", meeting_id);
                    if let Err(e) = Self::backup_meeting(&app, &pool, &meeting_id, None).await {
                        error!("Auto-backup failed for meeting {}: {}", meeting_id, e);
                    }
                }
                Err(e) => {
                    error!("Failed to load backup preferences for auto-backup: {}", e);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::TempDir;
    use zip::ZipArchive;

    fn create_file(path: &std::path::Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn test_pack_basic_files() {
        let meeting_dir = TempDir::new().unwrap();
        let out_dir = TempDir::new().unwrap();

        create_file(&meeting_dir.path().join("metadata.json"), b"{\"version\":\"1.0\"}");
        create_file(&meeting_dir.path().join("transcripts.json"), b"[]");

        let zip_path = out_dir.path().join("test_backup.zip");
        let (has_audio, has_summary) =
            BackupService::pack_meeting_zip(meeting_dir.path(), &zip_path).unwrap();

        assert!(zip_path.exists(), "zip file should be created");
        assert!(!has_audio);
        assert!(!has_summary);

        let mut archive = ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains(&"metadata.json".to_string()));
        assert!(names.contains(&"transcripts.json".to_string()));
    }

    #[test]
    fn test_pack_detects_audio_and_summary() {
        let meeting_dir = TempDir::new().unwrap();
        let out_dir = TempDir::new().unwrap();

        create_file(&meeting_dir.path().join("mic.ogg"), b"fake_ogg");
        create_file(&meeting_dir.path().join("system.ogg"), b"fake_ogg");
        create_file(&meeting_dir.path().join("summary.json"), b"{\"markdown\":\"# Summary\"}");
        create_file(&meeting_dir.path().join("transcripts.json"), b"[]");

        let zip_path = out_dir.path().join("test_backup.zip");
        let (has_audio, has_summary) =
            BackupService::pack_meeting_zip(meeting_dir.path(), &zip_path).unwrap();

        assert!(has_audio, "should detect .ogg audio files");
        assert!(has_summary, "should detect summary.json");
    }

    #[test]
    fn test_pack_excludes_checkpoints_and_hidden() {
        let meeting_dir = TempDir::new().unwrap();
        let out_dir = TempDir::new().unwrap();

        create_file(&meeting_dir.path().join("metadata.json"), b"{}");
        create_file(
            &meeting_dir.path().join(".checkpoints").join("chunk_0001.ogg"),
            b"raw_chunk",
        );
        create_file(&meeting_dir.path().join(".hidden_file"), b"hidden");
        create_file(&meeting_dir.path().join("temp.tmp"), b"temp");

        let zip_path = out_dir.path().join("test_backup.zip");
        BackupService::pack_meeting_zip(meeting_dir.path(), &zip_path).unwrap();

        let mut archive = ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();

        assert!(
            !names.iter().any(|n| n.contains(".checkpoints")),
            ".checkpoints must be excluded"
        );
        assert!(!names.contains(&".hidden_file".to_string()));
        assert!(!names.contains(&"temp.tmp".to_string()));
        assert!(names.contains(&"metadata.json".to_string()));
    }

    #[test]
    fn test_pack_file_content_integrity() {
        let meeting_dir = TempDir::new().unwrap();
        let out_dir = TempDir::new().unwrap();

        let expected = b"hello from meetily backup test";
        create_file(&meeting_dir.path().join("transcripts.json"), expected);

        let zip_path = out_dir.path().join("test_backup.zip");
        BackupService::pack_meeting_zip(meeting_dir.path(), &zip_path).unwrap();

        let mut archive = ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
        let mut entry = archive.by_name("transcripts.json").unwrap();
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        assert_eq!(content, expected, "file content must be preserved exactly");
    }
}
