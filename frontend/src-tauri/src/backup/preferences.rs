use anyhow::Result;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Runtime};
use tauri_plugin_store::StoreExt;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BackupPreferences {
    pub backup_folder: PathBuf,
    pub auto_backup: bool,
}

impl Default for BackupPreferences {
    fn default() -> Self {
        Self {
            backup_folder: get_default_backup_folder(),
            auto_backup: true,
        }
    }
}

pub fn get_default_backup_folder() -> PathBuf {
    dirs::document_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("meetily-backups")
}

pub fn ensure_backup_directory(path: &PathBuf) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
        info!("Created backup directory: {:?}", path);
    }
    Ok(())
}

pub async fn load_backup_preferences<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<BackupPreferences> {
    let store = match app.store("backup_preferences.json") {
        Ok(store) => store,
        Err(e) => {
            warn!("Failed to access backup store: {}, using defaults", e);
            return Ok(BackupPreferences::default());
        }
    };

    let prefs = if let Some(value) = store.get("preferences") {
        match serde_json::from_value::<BackupPreferences>(value.clone()) {
            Ok(p) => {
                info!("Loaded backup preferences from store: folder={:?}, auto={}", p.backup_folder, p.auto_backup);
                p
            }
            Err(e) => {
                warn!("Failed to deserialize backup preferences: {}, using defaults", e);
                BackupPreferences::default()
            }
        }
    } else {
        info!("No stored backup preferences found, using defaults");
        BackupPreferences::default()
    };

    Ok(prefs)
}

pub async fn save_backup_preferences<R: Runtime>(
    app: &AppHandle<R>,
    preferences: &BackupPreferences,
) -> Result<()> {
    let store = app.store("backup_preferences.json")?;
    let value = serde_json::to_value(preferences)?;
    store.set("preferences".to_string(), value);
    store.save()?;

    info!("Saved backup preferences: folder={:?}, auto={}", preferences.backup_folder, preferences.auto_backup);
    Ok(())
}
