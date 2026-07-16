use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{
    models::{
        AppSettings, BranchOverview, DesktopApiStatus, EmbeddingHealth, ImportResponse, Message,
        SearchMode, SearchQuery, SemanticRuntimeStatus, SessionList, SessionOpen, SessionSearchHit,
    },
    service::AppService,
};

fn message(error: impl ToString) -> String {
    error.to_string()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportEncoding {
    Utf8,
    Base64,
}

#[derive(Debug, Deserialize)]
pub struct ExportFilePayload {
    pub encoding: ExportEncoding,
    pub data: String,
}

#[tauri::command]
pub async fn search_sessions(
    service: State<'_, AppService>,
    query: SearchQuery,
) -> Result<SessionList, String> {
    service.list(query).await.map_err(message)
}

#[tauri::command]
pub async fn open_session(
    service: State<'_, AppService>,
    id: String,
    anchor_seq: Option<i64>,
) -> Result<SessionOpen, String> {
    service.open_session(&id, anchor_seq).await.map_err(message)
}

#[tauri::command]
pub async fn get_session_messages(
    service: State<'_, AppService>,
    id: String,
    start_seq: i64,
    limit: i64,
) -> Result<Vec<Message>, String> {
    service
        .session_messages(&id, start_seq, limit)
        .await
        .map_err(message)
}

#[tauri::command]
pub async fn search_session_hits(
    service: State<'_, AppService>,
    id: String,
    query: String,
    mode: Option<SearchMode>,
) -> Result<Vec<SessionSearchHit>, String> {
    service
        .session_search_hits(&id, &query, mode)
        .await
        .map_err(message)
}

#[tauri::command]
pub async fn get_session_branches(
    service: State<'_, AppService>,
    id: String,
) -> Result<BranchOverview, String> {
    service.session_branches(&id).await.map_err(message)
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
        tracing::warn!(
            archive_bytes = metadata.len(),
            "desktop ZIP import rejected because it exceeds the size limit"
        );
        return Err("ZIP 文件超过 128 MB 限制".into());
    }
    let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
    service.import_deepseek_zip(bytes).await.map_err(message)
}

#[tauri::command]
pub async fn get_settings(service: State<'_, AppService>) -> Result<AppSettings, String> {
    Ok(service.settings().await)
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    service: State<'_, AppService>,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    let settings = service.update_settings(settings).await.map_err(message)?;
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
    service.rotate_secret().await.map_err(message)
}

#[tauri::command]
pub async fn get_api_status(service: State<'_, AppService>) -> Result<DesktopApiStatus, String> {
    Ok(service.desktop_api_status().await)
}

#[tauri::command]
pub async fn get_semantic_status(
    service: State<'_, AppService>,
) -> Result<SemanticRuntimeStatus, String> {
    Ok(service.semantic_status().await)
}

#[tauri::command]
pub async fn check_embedding_backend(
    service: State<'_, AppService>,
) -> Result<EmbeddingHealth, String> {
    Ok(service.embedding_healthcheck().await)
}

#[tauri::command]
pub async fn reindex_semantic_search(
    app: AppHandle,
    service: State<'_, AppService>,
) -> Result<usize, String> {
    let emitter = app.clone();
    service
        .reindex_semantic_with_progress(Some(std::sync::Arc::new(move |progress| {
            let _ = emitter.emit("semantic-reindex-progress", progress);
        })))
        .await
        .map_err(message)
}

#[tauri::command]
pub async fn download_local_embedding_model(
    app: AppHandle,
    service: State<'_, AppService>,
) -> Result<(), String> {
    let emitter = app.clone();
    service
        .download_local_model(Some(std::sync::Arc::new(move |progress| {
            let _ = emitter.emit("local-model-download-progress", progress);
        })))
        .await
        .map_err(message)
}

#[tauri::command]
pub async fn import_local_embedding_model(
    service: State<'_, AppService>,
    path: String,
) -> Result<(), String> {
    service
        .import_local_model(std::path::Path::new(&path))
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
    service
        .move_data_directory(&directory)
        .await
        .map_err(message)?;
    app.request_restart();
    Ok(())
}

#[tauri::command]
pub async fn confirm_close_behavior(
    app: AppHandle,
    service: State<'_, AppService>,
    behavior: crate::models::CloseBehavior,
) -> Result<(), String> {
    if matches!(behavior, crate::models::CloseBehavior::Ask) {
        return Err("请选择关闭后的行为".into());
    }
    service
        .set_close_behavior(behavior.clone())
        .await
        .map_err(message)?;
    match behavior {
        crate::models::CloseBehavior::HideToTray => {
            if let Some(window) = app.get_webview_window("main") {
                window.hide().map_err(|error| error.to_string())?;
            }
        }
        crate::models::CloseBehavior::Exit => app.exit(0),
        crate::models::CloseBehavior::Ask => unreachable!(),
    }
    Ok(())
}

#[tauri::command]
pub async fn write_export_file(path: String, payload: ExportFilePayload) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("导出路径不能为空".into());
    }
    let bytes = match payload.encoding {
        ExportEncoding::Utf8 => payload.data.into_bytes(),
        ExportEncoding::Base64 => STANDARD
            .decode(payload.data)
            .map_err(|error| format!("图片数据无效：{error}"))?,
    };
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| format!("写入导出文件失败：{error}"))
}

#[cfg(test)]
mod export_tests {
    use super::*;

    #[tokio::test]
    async fn writes_utf8_and_base64_exports() {
        let root =
            std::env::temp_dir().join(format!("ai-chat-memory-export-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let text_path = root.join("chat.md");
        write_export_file(
            text_path.to_string_lossy().into_owned(),
            ExportFilePayload {
                encoding: ExportEncoding::Utf8,
                data: "聊天".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(tokio::fs::read_to_string(&text_path).await.unwrap(), "聊天");

        let image_path = root.join("chat.png");
        write_export_file(
            image_path.to_string_lossy().into_owned(),
            ExportFilePayload {
                encoding: ExportEncoding::Base64,
                data: "AQID".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(tokio::fs::read(&image_path).await.unwrap(), [1, 2, 3]);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}

#[tauri::command]
pub async fn cancel_semantic_work(service: State<'_, AppService>) -> Result<(), String> {
    service.cancel_semantic_work().await.map_err(message)
}
