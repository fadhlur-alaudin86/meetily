use std::path::PathBuf;
use tauri::{AppHandle, Runtime, State};

use super::preferences::{load_backup_preferences, save_backup_preferences, BackupPreferences};
use super::service::BackupService;
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
    BackupsRepository::get_backup(pool, &meeting_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn api_get_all_backup_statuses<R: Runtime>(
    _app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<Vec<MeetingBackup>, String> {
    let pool = state.db_manager.pool();
    BackupsRepository::get_all_backups(pool)
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
