use crate::{
    models::{ApiStatus, AppSettings, ImportResponse, SearchQuery, SessionDetail, SessionList},
    service::AppService,
};
use tauri::{AppHandle, State};

fn message(error: crate::error::AppError) -> String {
    error.to_string()
}

#[tauri::command]
pub async fn search_sessions(
    service: State<'_, AppService>,
    query: SearchQuery,
) -> Result<SessionList, String> {
    service.list(query).await.map_err(message)
}
#[tauri::command]
pub async fn get_session(
    service: State<'_, AppService>,
    id: String,
) -> Result<SessionDetail, String> {
    service.detail(&id).await.map_err(message)
}
#[tauri::command]
pub async fn delete_session(service: State<'_, AppService>, id: String) -> Result<(), String> {
    service.delete(&id).await.map_err(message)
}
#[tauri::command]
pub async fn import_deepseek_zip(
    service: State<'_, AppService>,
    path: String,
) -> Result<ImportResponse, String> {
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| e.to_string())?;
    if metadata.len() > 128 * 1024 * 1024 {
        return Err("ZIP 文件超过 128 MB 限制".into());
    }
    let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
    service.import_deepseek_zip(bytes).await.map_err(message)
}
#[tauri::command]
pub async fn get_settings(service: State<'_, AppService>) -> Result<AppSettings, String> {
    Ok(service.settings.get().await)
}
#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    service: State<'_, AppService>,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    let settings = service.settings.update(settings).await.map_err(message)?;
    if let Some(tray) = app.tray_by_id("main-tray") {
        tray.set_show_menu_on_left_click(matches!(
            settings.tray_click_behavior,
            crate::models::TrayClickBehavior::ShowMenu
        ))
        .map_err(|error| error.to_string())?;
    }
    Ok(settings)
}
#[tauri::command]
pub async fn rotate_secret(service: State<'_, AppService>) -> Result<AppSettings, String> {
    service.settings.rotate_secret().await.map_err(message)
}

#[tauri::command]
pub async fn get_api_status(service: State<'_, AppService>) -> Result<ApiStatus, String> {
    Ok(service.api_status().await)
}

#[tauri::command]
pub async fn migrate_legacy_database(
    service: State<'_, AppService>,
    path: String,
) -> Result<(), String> {
    service
        .migrate_legacy(std::path::Path::new(&path))
        .await
        .map_err(message)
}

#[tauri::command]
pub async fn move_data_directory(
    app: AppHandle,
    service: State<'_, AppService>,
    path: String,
) -> Result<(), String> {
    let directory = std::path::PathBuf::from(path);
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| error.to_string())?;
    let destination = directory.join("chat_memory.db");
    if destination.exists() {
        return Err("目标目录中已存在 chat_memory.db，请选择其他目录".into());
    }
    sqlx::query("VACUUM INTO ?")
        .bind(destination.to_string_lossy().as_ref())
        .execute(&service.pool)
        .await
        .map_err(|error| error.to_string())?;
    let mut settings = service.settings.get().await;
    settings.data_directory = Some(directory.to_string_lossy().into_owned());
    service.settings.update(settings).await.map_err(message)?;
    app.request_restart();
    Ok(())
}
