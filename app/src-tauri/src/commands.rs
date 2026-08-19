use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{
    models::{
        AppSettings, BranchOverview, CloudConnectionTestResult, CloudCredentialInput,
        CloudSyncSettings, CloudSyncStatus, DesktopApiStatus, EmbeddingHealth, ImportResponse,
        Message, SearchMode, SearchQuery, SemanticRuntimeStatus, SessionList, SessionOpen,
        SessionSearchHit, SupportedLocale,
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
pub fn set_native_locale(app: AppHandle, locale: SupportedLocale) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window
            .set_title(crate::i18n::native_text(locale).app_title)
            .map_err(message)?;
    }
    crate::tray::update_locale(&app, locale).map_err(message)
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    service: State<'_, AppService>,
    manager: State<'_, std::sync::Arc<crate::local_services::LocalServiceManager>>,
    settings: AppSettings,
    cloud_sync_credentials: Option<CloudCredentialInput>,
) -> Result<AppSettings, String> {
    let previous = service.settings().await;
    let settings = service
        .update_settings_with_cloud_credentials(settings, cloud_sync_credentials)
        .await
        .map_err(message)?;
    if previous.mcp_enabled != settings.mcp_enabled {
        manager
            .apply_desired(
                crate::local_services::LocalServiceId::Mcp,
                settings.mcp_enabled,
            )
            .await;
    }
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
pub async fn get_cloud_sync_status(
    service: State<'_, AppService>,
) -> Result<CloudSyncStatus, String> {
    Ok(service.cloud_sync_status().await)
}

#[tauri::command]
pub async fn test_cloud_sync_connection(
    service: State<'_, AppService>,
    cloud_sync: CloudSyncSettings,
    credentials: CloudCredentialInput,
) -> Result<CloudConnectionTestResult, String> {
    service
        .test_cloud_sync_connection(cloud_sync, credentials)
        .await
        .map_err(message)
}

#[tauri::command]
pub async fn sync_now(service: State<'_, AppService>) -> Result<CloudSyncStatus, String> {
    service.sync_now().await.map_err(message)
}

#[tauri::command]
pub async fn rewrite_cloud_archive(
    service: State<'_, AppService>,
) -> Result<CloudSyncStatus, String> {
    service.rewrite_cloud_archive().await.map_err(message)
}

#[tauri::command]
pub async fn remove_cloud_device_record(
    service: State<'_, AppService>,
    device_id: String,
) -> Result<CloudSyncStatus, String> {
    service
        .remove_cloud_device_record(device_id)
        .await
        .map_err(message)
}

#[tauri::command]
pub async fn get_api_status(
    service: State<'_, AppService>,
    manager: State<'_, std::sync::Arc<crate::local_services::LocalServiceManager>>,
) -> Result<DesktopApiStatus, String> {
    let mut status = service.desktop_api_status().await;
    status.mcp = manager
        .status(crate::local_services::LocalServiceId::Mcp)
        .await;
    status.mcp_url = crate::mcp::server::MCP_URL.to_string();
    Ok(status)
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

#[cfg(any(target_os = "windows", test))]
const MILLIMETERS_PER_INCH: f64 = 25.4;
#[cfg(any(target_os = "windows", test))]
const A4_PAGE_WIDTH_INCHES: f64 = 210.0 / MILLIMETERS_PER_INCH;
#[cfg(any(target_os = "windows", test))]
const A4_PAGE_HEIGHT_INCHES: f64 = 297.0 / MILLIMETERS_PER_INCH;

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone, Copy, PartialEq)]
struct PdfPrintLayout {
    page_width_inches: f64,
    page_height_inches: f64,
    margin_inches: f64,
}

#[cfg(any(target_os = "windows", test))]
const fn pdf_print_layout(compact: bool) -> PdfPrintLayout {
    PdfPrintLayout {
        page_width_inches: A4_PAGE_WIDTH_INCHES,
        page_height_inches: A4_PAGE_HEIGHT_INCHES,
        margin_inches: if compact { 0.3 } else { 0.6 },
    }
}

#[tauri::command]
pub async fn print_to_pdf(
    window: tauri::WebviewWindow,
    path: String,
    compact: bool,
) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("导出路径不能为空".into());
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::sync::Mutex;
        use tokio::sync::oneshot;
        use webview2_com::Microsoft::Web::WebView2::Win32::*;
        use windows_core::{BOOL, HRESULT, Interface, PCWSTR, implement};

        #[implement(ICoreWebView2PrintToPdfCompletedHandler)]
        struct PrintHandler(Mutex<Option<oneshot::Sender<Result<(), String>>>>);

        impl ICoreWebView2PrintToPdfCompletedHandler_Impl for PrintHandler_Impl {
            fn Invoke(&self, error_code: HRESULT, is_successful: BOOL) -> windows_core::Result<()> {
                if let Some(tx) = self.0.lock().ok().and_then(|mut lock| lock.take()) {
                    if error_code.is_ok() && is_successful.as_bool() {
                        let _ = tx.send(Ok(()));
                    } else {
                        let _ = tx.send(Err(format!("PDF 写入失败 (错误码: {error_code:?})")));
                    }
                }
                Ok(())
            }
        }

        let (tx, rx) = oneshot::channel();
        let target_path = path.clone();

        window
            .with_webview(move |webview| unsafe {
                let controller = match webview.controller().CoreWebView2() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Err(format!("获取 WebView2 实例失败：{e}")));
                        return;
                    }
                };

                let core10: ICoreWebView2_10 = match controller.cast() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Err(format!("当前系统 WebView2 不支持 PrintToPdf：{e}")));
                        return;
                    }
                };

                let env6: ICoreWebView2Environment6 = match webview.environment().cast() {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = tx.send(Err(format!("获取 WebView2 环境失败：{e}")));
                        return;
                    }
                };

                let settings = match env6.CreatePrintSettings() {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(Err(format!("创建打印配置失败：{e}")));
                        return;
                    }
                };
                let layout = pdf_print_layout(compact);
                let configure_result: windows_core::Result<()> = (|| {
                    settings.SetOrientation(COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT)?;
                    settings.SetPageWidth(layout.page_width_inches)?;
                    settings.SetPageHeight(layout.page_height_inches)?;
                    settings.SetMarginTop(layout.margin_inches)?;
                    settings.SetMarginBottom(layout.margin_inches)?;
                    settings.SetMarginLeft(layout.margin_inches)?;
                    settings.SetMarginRight(layout.margin_inches)?;
                    settings.SetShouldPrintBackgrounds(true)?;
                    Ok(())
                })();
                if let Err(error) = configure_result {
                    let _ = tx.send(Err(format!("配置 PDF 打印参数失败：{error}")));
                    return;
                }

                let wide_path: Vec<u16> = std::ffi::OsStr::new(&target_path)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();

                let handler: ICoreWebView2PrintToPdfCompletedHandler =
                    PrintHandler(Mutex::new(Some(tx))).into();

                if let Err(e) =
                    core10.PrintToPdf(PCWSTR(wide_path.as_ptr()), &settings, Some(&handler))
                {
                    tracing::error!(error = %e, "PrintToPdf 启动失败");
                }
            })
            .map_err(|e| format!("调度 WebView2 打印失败：{e}"))?;

        rx.await.map_err(|_| "打印等待通道中断".to_string())?
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (window, compact);
        Err("当前平台暂不支持原生 PDF 导出".into())
    }
}

#[cfg(test)]
mod export_tests {
    use super::*;

    #[test]
    fn pdf_print_layout_uses_a4_and_density_specific_margins() {
        assert_eq!(
            pdf_print_layout(true),
            PdfPrintLayout {
                page_width_inches: 210.0 / 25.4,
                page_height_inches: 297.0 / 25.4,
                margin_inches: 0.3,
            }
        );
        assert_eq!(
            pdf_print_layout(false),
            PdfPrintLayout {
                page_width_inches: 210.0 / 25.4,
                page_height_inches: 297.0 / 25.4,
                margin_inches: 0.6,
            }
        );
    }

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
