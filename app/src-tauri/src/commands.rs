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
    use tokio::io::AsyncReadExt;
    let safe_path = validate_file_path(&path, &["zip"])?;
    let file = tokio::fs::File::open(&safe_path)
        .await
        .map_err(|e| e.to_string())?;
    const MAX_ZIP_BYTES: usize = 128 * 1024 * 1024;
    let mut bytes = Vec::new();
    file.take((MAX_ZIP_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_ZIP_BYTES {
        tracing::warn!(
            archive_bytes = bytes.len(),
            "desktop ZIP import rejected because it exceeds the size limit"
        );
        return Err("ZIP 文件超过 128 MB 限制".into());
    }
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
    let safe_path = validate_file_path(&path, &["onnx", "bin", "safetensors", "tar", "gz", "zip"])?;
    service
        .import_local_model(&safe_path)
        .await
        .map_err(message)
}

#[tauri::command]
pub async fn move_data_directory(
    app: AppHandle,
    service: State<'_, AppService>,
    path: String,
) -> Result<(), String> {
    let directory = validate_directory_path(&path)?;
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

pub fn validate_file_path(
    path_str: &str,
    allowed_extensions: &[&str],
) -> Result<std::path::PathBuf, String> {
    if path_str.trim().is_empty() {
        return Err("导出路径不能为空".into());
    }
    let raw_path = std::path::Path::new(path_str);

    if let Some(file_name) = raw_path.file_name().and_then(|n| n.to_str()) {
        let stem = raw_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(file_name);
        let reserved = [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        if reserved.iter().any(|&r| r.eq_ignore_ascii_case(stem)) {
            return Err("目标文件名属于系统保留名称".into());
        }
    }

    if !allowed_extensions.is_empty() {
        let ext = raw_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !allowed_extensions
            .iter()
            .any(|&allowed| allowed.eq_ignore_ascii_case(&ext))
        {
            return Err(format!(
                "不支持的文件格式，仅允许：{}",
                allowed_extensions.join(", ")
            ));
        }
    }

    use std::path::Component;
    let mut normalized = std::path::PathBuf::new();
    for component in raw_path.components() {
        match component {
            Component::Prefix(prefix) => {
                #[cfg(target_os = "windows")]
                {
                    if !matches!(prefix.kind(), std::path::Prefix::Disk(_)) {
                        return Err("禁止使用网络共享、命名空间或非标准路径前缀".into());
                    }
                }
                normalized.push(component);
            }
            Component::RootDir => normalized.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err("路径包含非法目录遍历字符 (..)".into());
            }
            Component::Normal(c) => normalized.push(c),
        }
    }

    if !normalized.is_absolute() {
        return Err("路径必须为绝对路径".into());
    }

    let path_str_lower = normalized
        .to_string_lossy()
        .to_ascii_lowercase()
        .replace('/', "\\");

    let is_temp = [
        std::env::var("TEMP").ok(),
        std::env::var("TMP").ok(),
        Some(std::env::temp_dir().to_string_lossy().into_owned()),
    ]
    .into_iter()
    .flatten()
    .any(|tmp| {
        let tmp_lower = tmp.to_ascii_lowercase().replace('/', "\\");
        !tmp_lower.is_empty() && path_str_lower.starts_with(&tmp_lower)
    });

    if !is_temp {
        let forbidden_prefixes = [
            "c:\\windows",
            "c:\\program files",
            "c:\\program files (x86)",
            "c:\\programdata",
        ];
        for forbidden in forbidden_prefixes {
            if path_str_lower.starts_with(forbidden) {
                return Err(format!("禁止访问系统关键目录：{forbidden}"));
            }
        }

        if let Ok(windir) = std::env::var("WINDIR") {
            let windir_lower = windir.to_ascii_lowercase().replace('/', "\\");
            if !windir_lower.is_empty() && path_str_lower.starts_with(&windir_lower) {
                return Err("禁止访问系统 Windows 目录".into());
            }
        }
        if let Ok(appdata) = std::env::var("APPDATA") {
            let appdata_lower = appdata.to_ascii_lowercase().replace('/', "\\");
            if !appdata_lower.is_empty() && path_str_lower.starts_with(&appdata_lower) {
                return Err("禁止访问用户 AppData 目录".into());
            }
        }
        if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
            let localappdata_lower = localappdata.to_ascii_lowercase().replace('/', "\\");
            if !localappdata_lower.is_empty() && path_str_lower.starts_with(&localappdata_lower) {
                return Err("禁止访问用户 LocalAppData 目录".into());
            }
        }

        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            let userprofile_lower = userprofile.to_ascii_lowercase().replace('/', "\\");
            if path_str_lower.starts_with("c:\\users\\")
                && !path_str_lower.starts_with(&userprofile_lower)
            {
                return Err("禁止访问非当前登录用户的目录".into());
            }

            if path_str_lower.starts_with(&userprofile_lower) {
                let rel = &path_str_lower[userprofile_lower.len()..];
                let rel = rel.trim_start_matches('\\');
                if let Some(first_seg) = rel.split('\\').next() {
                    if first_seg.starts_with('.') {
                        return Err(format!("禁止访问用户配置目录 ({first_seg})"));
                    }
                    if first_seg.eq_ignore_ascii_case("appdata") {
                        return Err("禁止访问 AppData 目录".into());
                    }
                }
            }
        }

        if path_str_lower.contains("\\startup") || path_str_lower.contains("\\start menu") {
            return Err("禁止访问系统启动或开始菜单目录".into());
        }
    }

    Ok(normalized)
}

pub fn validate_directory_path(path_str: &str) -> Result<std::path::PathBuf, String> {
    if path_str.trim().is_empty() {
        return Err("目录路径不能为空".into());
    }
    let raw_path = std::path::Path::new(path_str);
    use std::path::Component;
    let mut normalized = std::path::PathBuf::new();
    for component in raw_path.components() {
        match component {
            Component::Prefix(prefix) => {
                #[cfg(target_os = "windows")]
                {
                    if !matches!(prefix.kind(), std::path::Prefix::Disk(_)) {
                        return Err("禁止使用网络共享、命名空间或非标准路径前缀".into());
                    }
                }
                normalized.push(component);
            }
            Component::RootDir => normalized.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err("路径包含非法目录遍历字符 (..)".into());
            }
            Component::Normal(c) => normalized.push(c),
        }
    }

    if !normalized.is_absolute() {
        return Err("路径必须为绝对路径".into());
    }

    let path_str_lower = normalized
        .to_string_lossy()
        .to_ascii_lowercase()
        .replace('/', "\\");

    let is_temp = [
        std::env::var("TEMP").ok(),
        std::env::var("TMP").ok(),
        Some(std::env::temp_dir().to_string_lossy().into_owned()),
    ]
    .into_iter()
    .flatten()
    .any(|tmp| {
        let tmp_lower = tmp.to_ascii_lowercase().replace('/', "\\");
        !tmp_lower.is_empty() && path_str_lower.starts_with(&tmp_lower)
    });

    if !is_temp {
        let forbidden_prefixes = [
            "c:\\windows",
            "c:\\program files",
            "c:\\program files (x86)",
            "c:\\programdata",
        ];
        for forbidden in forbidden_prefixes {
            if path_str_lower.starts_with(forbidden) {
                return Err(format!("禁止访问系统关键目录：{forbidden}"));
            }
        }

        if let Ok(windir) = std::env::var("WINDIR") {
            let windir_lower = windir.to_ascii_lowercase().replace('/', "\\");
            if !windir_lower.is_empty() && path_str_lower.starts_with(&windir_lower) {
                return Err("禁止访问系统 Windows 目录".into());
            }
        }
        if let Ok(appdata) = std::env::var("APPDATA") {
            let appdata_lower = appdata.to_ascii_lowercase().replace('/', "\\");
            if !appdata_lower.is_empty() && path_str_lower.starts_with(&appdata_lower) {
                return Err("禁止访问用户 AppData 目录".into());
            }
        }
        if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
            let localappdata_lower = localappdata.to_ascii_lowercase().replace('/', "\\");
            if !localappdata_lower.is_empty() && path_str_lower.starts_with(&localappdata_lower) {
                return Err("禁止访问用户 LocalAppData 目录".into());
            }
        }

        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            let userprofile_lower = userprofile.to_ascii_lowercase().replace('/', "\\");
            if path_str_lower.starts_with("c:\\users\\")
                && !path_str_lower.starts_with(&userprofile_lower)
            {
                return Err("禁止访问非当前登录用户的目录".into());
            }

            if path_str_lower.starts_with(&userprofile_lower) {
                let rel = &path_str_lower[userprofile_lower.len()..];
                let rel = rel.trim_start_matches('\\');
                if let Some(first_seg) = rel.split('\\').next() {
                    if first_seg.starts_with('.') {
                        return Err(format!("禁止访问用户配置目录 ({first_seg})"));
                    }
                    if first_seg.eq_ignore_ascii_case("appdata") {
                        return Err("禁止访问 AppData 目录".into());
                    }
                }
            }
        }

        if path_str_lower.contains("\\startup") || path_str_lower.contains("\\start menu") {
            return Err("禁止访问系统启动或开始菜单目录".into());
        }
    }

    Ok(normalized)
}

#[tauri::command]
pub async fn write_export_file(path: String, payload: ExportFilePayload) -> Result<(), String> {
    let safe_path = validate_file_path(&path, &["md", "json", "png", "jpg", "jpeg"])?;
    let bytes = match payload.encoding {
        ExportEncoding::Utf8 => payload.data.into_bytes(),
        ExportEncoding::Base64 => STANDARD
            .decode(payload.data)
            .map_err(|error| format!("图片数据无效：{error}"))?,
    };
    tokio::fs::write(safe_path, bytes)
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
    let safe_path = validate_file_path(&path, &["pdf"])?;
    let path = safe_path.to_string_lossy().into_owned();

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
            // SAFETY: WebView2 COM interop via webview2-com 生成的绑定；所有接口指针
            // 均来自框架提供的 controller/environment，且仅在 WebView2 要求的 UI 线程上使用。
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

    #[test]
    fn validates_export_paths_securely() {
        assert!(validate_file_path("", &["md"]).is_err());
        assert!(validate_file_path("C:\\Windows\\System32\\calc.exe", &["md"]).is_err());
        assert!(validate_file_path("C:\\Windows\\System32\\evil.md", &["md"]).is_err());
        assert!(validate_file_path("C:\\Program Files\\app\\test.md", &["md"]).is_err());
        assert!(validate_file_path("C:\\ProgramData\\test.md", &["md"]).is_err());
        assert!(validate_file_path("\\\\evil.com\\share\\doc.md", &["md"]).is_err());
        assert!(validate_file_path("CON.md", &["md"]).is_err());
        assert!(validate_file_path("C:\\test\\..\\..\\Windows\\test.md", &["md"]).is_err());
        assert!(validate_file_path("C:\\test\\export.exe", &["md"]).is_err());
        assert!(validate_file_path(r"\\?\C:\Windows\System32\evil.md", &["md"]).is_err());
        assert!(validate_file_path(r"\\.\COM1", &["md"]).is_err());
        assert!(validate_directory_path(r"C:\Windows\System32").is_err());
        assert!(validate_directory_path(r"\\?\C:\Windows").is_err());
    }
}

#[tauri::command]
pub async fn cancel_semantic_work(service: State<'_, AppService>) -> Result<(), String> {
    service.cancel_semantic_work().await.map_err(message)
}
