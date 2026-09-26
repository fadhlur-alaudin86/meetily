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
        let temp_zip_path = backup_dir.join(format!(".{}.tmp", zip_filename));

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
