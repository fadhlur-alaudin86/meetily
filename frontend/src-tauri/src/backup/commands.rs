use log::warn;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Runtime, State};

use super::preferences::{load_backup_preferences, save_backup_preferences, BackupPreferences};
use super::service::BackupService;
use super::{BackupInspection, RestoreMode, RestoreResult, RestoreService};
use crate::database::models::MeetingBackup;
use crate::database::repositories::backup::BackupsRepository;
use crate::state::AppState;

#[tauri::command]
pub async fn api_backup_meeting<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    meeting_id: String,
    target_dir: Option<String>,
) -> Result<MeetingBackup, String> {
    let pool = state.db_manager.pool();
    let custom_target = target_dir.map(PathBuf::from);

    BackupService::backup_meeting(&app, pool, &meeting_id, custom_target)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn api_backup_all_meetings<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<Vec<MeetingBackup>, String> {
    let pool = state.db_manager.pool();
    BackupService::backup_all_meetings(&app, pool)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn api_get_backup_status<R: Runtime>(
    _app: AppHandle<R>,
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Option<MeetingBackup>, String> {
    let pool = state.db_manager.pool();
    let backup = BackupsRepository::get_backup(pool, &meeting_id)
        .await
        .map_err(|e| e.to_string())?;

    let Some(backup) = backup else {
        return Ok(None);
    };

    Ok(BackupService::reconcile_backup_records(pool, vec![backup])
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .next())
}

#[tauri::command]
pub async fn api_get_all_backup_statuses<R: Runtime>(
    _app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<Vec<MeetingBackup>, String> {
    let pool = state.db_manager.pool();
    let backups = BackupsRepository::get_all_backups(pool)
        .await
        .map_err(|e| e.to_string())?;
    BackupService::reconcile_backup_records(pool, backups)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn api_get_backup_preferences<R: Runtime>(
    app: AppHandle<R>,
) -> Result<BackupPreferences, String> {
    load_backup_preferences(&app)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn api_save_backup_preferences<R: Runtime>(
    app: AppHandle<R>,
    preferences: BackupPreferences,
) -> Result<(), String> {
    save_backup_preferences(&app, &preferences)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn api_select_backup_folder<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let folder_path = app.dialog().file().blocking_pick_folder();

    if let Some(path) = folder_path {
        let path_str = path.to_string();
        if let Ok(mut prefs) = load_backup_preferences(&app).await {
            prefs.backup_folder = PathBuf::from(&path_str);
            let _ = save_backup_preferences(&app, &prefs).await;
        }
        Ok(Some(path_str))
    } else {
        Ok(None)
    }
}

#[tauri::command]
pub async fn api_open_backup_folder<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let prefs = load_backup_preferences(&app).await.map_err(|e| e.to_string())?;
    super::preferences::ensure_backup_directory(&prefs.backup_folder).map_err(|e| e.to_string())?;

    let folder_path = prefs.backup_folder.to_string_lossy().to_string();

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&folder_path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&folder_path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&folder_path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    Ok(())
}

#[tauri::command]
pub async fn api_reset_backup_folder_to_default<R: Runtime>(
    app: AppHandle<R>,
) -> Result<String, String> {
    let default_folder = super::preferences::get_default_backup_folder();
    let default_str = default_folder.to_string_lossy().to_string();

    if let Ok(mut prefs) = load_backup_preferences(&app).await {
        prefs.backup_folder = default_folder;
        let _ = save_backup_preferences(&app, &prefs).await;
    }

    Ok(default_str)
}

/// Opens a file picker filtered to .zip archives; returns the selected path.
#[tauri::command]
pub async fn api_select_backup_zip<R: Runtime>(app: AppHandle<R>) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let picked = app
        .dialog()
        .file()
        .add_filter("Meeting backup archive", &["zip"])
        .blocking_pick_file();

    Ok(picked.map(|p| p.to_string()))
}

/// Reads a backup archive without extracting it (title, contents, conflicts).
#[tauri::command]
pub async fn api_inspect_backup<R: Runtime>(
    _app: AppHandle<R>,
    state: State<'_, AppState>,
    zip_path: String,
) -> Result<BackupInspection, String> {
    let pool = state.db_manager.pool();
    RestoreService::inspect(pool, &PathBuf::from(&zip_path))
        .await
        .map_err(|e| e.to_string())
}

/// Restores a meeting from a backup archive: extracts the folder and rebuilds
/// meetings/transcripts/summary rows. `mode` is "fresh" | "replace" | "keep_both".
#[tauri::command]
pub async fn api_restore_meeting<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    zip_path: String,
    mode: String,
) -> Result<RestoreResult, String> {
    let pool = state.db_manager.pool();
    let mode = RestoreMode::parse(&mode).map_err(|e| e.to_string())?;

    let result = RestoreService::restore(pool, &PathBuf::from(&zip_path), mode, None)
        .await
        .map_err(|e| e.to_string())?;

    // Refresh sidebar badges and settings stats for the restored meeting.
    if let Err(e) = app.emit("backup-updated", &result.backup) {
        warn!(
            "Failed to emit backup-updated after restore of {}: {}",
            result.meeting_id, e
        );
    }

    Ok(result)
}

