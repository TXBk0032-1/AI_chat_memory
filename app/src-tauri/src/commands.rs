use crate::{
    models::{ApiStatus, AppSettings, ImportResponse, SearchQuery, SessionDetail, SessionList},
    service::AppService,
};
use tauri::State;

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
    service: State<'_, AppService>,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    service.settings.update(settings).await.map_err(message)
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
