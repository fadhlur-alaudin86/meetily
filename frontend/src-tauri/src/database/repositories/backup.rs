use crate::database::models::MeetingBackup;
use sqlx::{Error as SqlxError, SqlitePool};
use tracing::{error, info};

pub struct BackupsRepository;

impl BackupsRepository {
    pub async fn upsert_backup(
        pool: &SqlitePool,
        backup: &MeetingBackup,
    ) -> Result<(), SqlxError> {
        sqlx::query(
            r#"
            INSERT INTO meeting_backups (meeting_id, backup_path, status, backed_up_at, has_audio, has_summary)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(meeting_id) DO UPDATE SET
                backup_path = excluded.backup_path,
                status = excluded.status,
                backed_up_at = excluded.backed_up_at,
                has_audio = excluded.has_audio,
                has_summary = excluded.has_summary
            "#,
        )
        .bind(&backup.meeting_id)
        .bind(&backup.backup_path)
        .bind(&backup.status)
        .bind(&backup.backed_up_at)
        .bind(backup.has_audio)
        .bind(backup.has_summary)
        .execute(pool)
        .await
        .map_err(|e| {
            error!("Failed to upsert backup for meeting {}: {}", backup.meeting_id, e);
            e
        })?;

        info!("Backup status updated for meeting {}", backup.meeting_id);
        Ok(())
    }

    pub async fn get_backup(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<Option<MeetingBackup>, SqlxError> {
        let backup = sqlx::query_as::<_, MeetingBackup>(
            "SELECT * FROM meeting_backups WHERE meeting_id = ?",
        )
        .bind(meeting_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| {
            error!("Failed to fetch backup status for meeting {}: {}", meeting_id, e);
            e
        })?;

        Ok(backup)
    }

    pub async fn get_all_backups(pool: &SqlitePool) -> Result<Vec<MeetingBackup>, SqlxError> {
        let backups = sqlx::query_as::<_, MeetingBackup>(
            "SELECT * FROM meeting_backups ORDER BY backed_up_at DESC",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| {
            error!("Failed to fetch all backups: {}", e);
            e
        })?;

        Ok(backups)
    }

    pub async fn delete_backup(pool: &SqlitePool, meeting_id: &str) -> Result<bool, SqlxError> {
        let result = sqlx::query("DELETE FROM meeting_backups WHERE meeting_id = ?")
            .bind(meeting_id)
            .execute(pool)
            .await
            .map_err(|e| {
                error!("Failed to delete backup for meeting {}: {}", meeting_id, e);
                e
            })?;

        Ok(result.rows_affected() > 0)
    }
}
