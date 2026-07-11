use crate::{
    models::{AppSettings, ImportResponse, SearchQuery, SessionDetail, SessionList},
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
