use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_store::StoreExt;

/// Storage key in preferences.json. A missing or invalid value means "ask".
const QUIT_BEHAVIOR_KEY: &str = "quit_behavior";

/// Stored quit behavior: "ask" | "background" | "kill".
/// Anything missing or invalid falls back to "ask".
pub fn get_quit_behavior<R: Runtime>(app: &AppHandle<R>) -> String {
    app.store("preferences.json")
        .ok()
        .and_then(|store| store.get(QUIT_BEHAVIOR_KEY))
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|s| s == "background" || s == "kill" || s == "ask")
        .unwrap_or_else(|| "ask".to_string())
}

#[tauri::command]
pub async fn api_get_quit_behavior<R: Runtime>(app: AppHandle<R>) -> Result<String, String> {
    Ok(get_quit_behavior(&app))
}

#[tauri::command]
pub async fn api_set_quit_behavior<R: Runtime>(
    app: AppHandle<R>,
    behavior: String,
) -> Result<(), String> {
    if !["ask", "background", "kill"].contains(&behavior.as_str()) {
        return Err(format!("Invalid quit behavior: {}", behavior));
    }
    let store = app
        .store("preferences.json")
        .map_err(|e| format!("Failed to access preferences store: {}", e))?;
    store.set(QUIT_BEHAVIOR_KEY, serde_json::json!(behavior));
    store
        .save()
        .map_err(|e| format!("Failed to save preferences: {}", e))?;
    Ok(())
}

/// Hide the main window (quit to background, app keeps running in tray).
#[tauri::command]
pub async fn api_quit_background<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    app.get_webview_window("main")
        .ok_or_else(|| "Main window not found".to_string())?
        .hide()
        .map_err(|e| e.to_string())
}

/// Terminate the app process (quit kill).
#[tauri::command]
pub async fn api_quit_app<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    app.exit(0);
    #[allow(unreachable_code)]
    Ok(())
}
