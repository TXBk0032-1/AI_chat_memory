mod branch;
mod commands;
mod data_directory;
mod database;
mod embedding;
mod error;
mod http_api;
mod i18n;
mod import_history;
mod local_services;
mod logging;
mod mcp;
mod models;
mod normalizer;
mod semantic;
mod service;
mod settings;
pub mod sync;
mod tray;
mod window_lifecycle;

use rmcp::ServiceExt;
use service::AppService;
use settings::SettingsStore;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "windows")]
    // SAFETY: SetErrorMode 是无指针参数的 Win32 调用，仅改变进程错误弹窗模式；
    // 两个标志为文档化常量，不会引入内存安全问题。
    unsafe {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn SetErrorMode(uMode: u32) -> u32;
        }
        const SEM_FAILCRITICALERRORS: u32 = 0x0001;
        const SEM_NOOPENFILEERRORBOX: u32 = 0x8000;
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            tracing::info!("second instance requested; focusing main window");
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            match logging::init(&data_dir) {
                Ok(log_guard) => {
                    app.manage(log_guard);
                }
                Err(error) => {
                    eprintln!("failed to initialize file logging: {error}");
                }
            }
            #[cfg(target_os = "windows")]
            if let Some(window) = app.get_webview_window("main") {
                match window_vibrancy::apply_acrylic(&window, Some((18, 18, 18, 110))) {
                    Ok(()) => tracing::info!("acrylic window effect enabled"),
                    Err(error) => tracing::warn!(%error, "acrylic window effect unavailable"),
                }
            }
            tracing::info!(app_data_dir=%data_dir.display(), "application starting");

            let executable_dir = std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(std::path::Path::to_path_buf));
            let working_dir = std::env::current_dir().ok();
            let settings_path = data_dir.join("settings.json");
            let service = tauri::async_runtime::block_on(async {
                let started = std::time::Instant::now();
                let settings = Arc::new(SettingsStore::load(settings_path).await?);
                let settings_value = settings.get().await;
                let configured_dir = settings_value
                    .data_directory
                    .as_ref()
                    .map(std::path::PathBuf::from);
                let database_dir = data_directory::prepare_database_directory(
                    configured_dir.as_deref(),
                    executable_dir.as_deref(),
                    working_dir.as_deref(),
                    &data_dir,
                )
                .await;
                tokio::fs::create_dir_all(&database_dir).await?;
                let database_path = database_dir.join("chat_memory.db");
                let pool = database::connect(&database_path).await?;
                tracing::info!(
                    path=%database_path.display(),
                    elapsed_ms = started.elapsed().as_millis(),
                    "application database ready"
                );
                let service = AppService::new(pool, settings, database_dir).await?;
                tracing::info!(
                    elapsed_ms = started.elapsed().as_millis(),
                    "application service ready"
                );
                Ok::<_, crate::error::AppError>(service)
            })?;
            app.manage(service.clone());

            let manager = local_services::LocalServiceManager::new();
            let mcp_service = service.clone();
            tauri::async_runtime::block_on(async {
                manager
                    .register(local_services::LocalServiceSpec {
                        id: local_services::LocalServiceId::Mcp,
                        bind: std::net::SocketAddr::from(([127, 0, 0, 1], mcp::server::MCP_PORT)),
                        build: Arc::new(move || mcp::http::build_mcp_router(mcp_service.clone())),
                    })
                    .await;
                let enabled = service.settings().await.mcp_enabled;
                manager
                    .apply_desired(local_services::LocalServiceId::Mcp, enabled)
                    .await;
            });
            app.manage(manager);

            let http_service = service.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = http_api::serve(http_service.clone()).await {
                    http_service
                        .set_api_status(crate::models::ApiStatus::Failed(error.to_string()))
                        .await;
                    tracing::error!(%error,"local API stopped");
                }
            });

            let initial_locale =
                crate::i18n::resolve_native_locale(service.current_settings().language);
            if let Some(window) = app.get_webview_window("main") {
                window.set_title(crate::i18n::native_text(initial_locale).app_title)?;
            }
            tray::build(app, &service, initial_locale)?;
            Ok(())
        })
        .on_window_event(window_lifecycle::handle)
        .invoke_handler(tauri::generate_handler![
            commands::search_sessions,
            commands::open_session,
            commands::get_session_messages,
            commands::search_session_hits,
            commands::get_session_branches,
            commands::delete_session,
            commands::import_history,
            commands::get_settings,
            commands::set_native_locale,
            commands::save_settings,
            commands::rotate_secret,
            commands::get_cloud_sync_status,
            commands::test_cloud_sync_connection,
            commands::sync_now,
            commands::rewrite_cloud_archive,
            commands::remove_cloud_device_record,
            commands::get_api_status,
            commands::get_semantic_status,
            commands::check_embedding_backend,
            commands::reindex_semantic_search,
            commands::download_local_embedding_model,
            commands::import_local_embedding_model,
            commands::cancel_semantic_work,
            commands::move_data_directory,
            commands::confirm_close_behavior,
            commands::write_export_file,
            commands::print_to_pdf
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}

/// `mcp_enabled` 门槛：stdio 传输在开关关闭时必须启动即报错退出。
pub fn ensure_mcp_enabled(enabled: bool) -> error::Result<()> {
    if enabled {
        Ok(())
    } else {
        Err(error::AppError::Configuration(
            "MCP 服务未启用：请在桌面应用设置中开启 MCP 后再使用 stdio 传输".into(),
        ))
    }
}

/// stdio 传输入口（供独立 bin target `ai-chat-memory-mcp` 调用）。
///
/// 安全模型差异：HTTP 端点（127.0.0.1:19821）依赖 secret + Origin 白名单中间件；
/// stdio 没有网络面，不经过任何 HTTP 中间件，信任边界等价于桌面应用——
/// 能启动本进程的本地用户。stdout 全程保留给 MCP JSON-RPC 协议帧，
/// 日志仅写文件与 stderr。与桌面应用并发打开同一 SQLite（WAL + busy_timeout）。
pub async fn run_mcp_stdio() -> error::Result<()> {
    let data_dir = data_directory::fallback_app_data_dir()?;
    let _log_guard = match logging::init(&data_dir) {
        Ok(guard) => Some(guard),
        Err(err) => {
            eprintln!("failed to initialize file logging: {err}");
            None
        }
    };

    let started = std::time::Instant::now();
    let executable_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf));
    let working_dir = std::env::current_dir().ok();
    let settings = Arc::new(SettingsStore::load(data_dir.join("settings.json")).await?);
    let settings_value = settings.get().await;
    // Fast-fail before opening the database: an opted-out MCP invocation must
    // not pay for a full service bootstrap (SQLite open + embedding manager).
    ensure_mcp_enabled(settings_value.mcp_enabled)?;
    let configured_dir = settings_value
        .data_directory
        .as_ref()
        .map(std::path::PathBuf::from);
    let database_dir = data_directory::prepare_database_directory(
        configured_dir.as_deref(),
        executable_dir.as_deref(),
        working_dir.as_deref(),
        &data_dir,
    )
    .await;
    tokio::fs::create_dir_all(&database_dir).await?;
    let database_path = database_dir.join("chat_memory.db");
    let pool = database::connect(&database_path).await?;
    tracing::info!(
        path = %database_path.display(),
        elapsed_ms = started.elapsed().as_millis(),
        "stdio MCP database ready"
    );
    let service = AppService::new_for_mcp_stdio(pool, settings, database_dir).await?;

    let running = mcp::server::ChatMemoryMcp::new(service)
        .serve(rmcp::transport::io::stdio())
        .await
        .map_err(|err| error::AppError::Configuration(format!("MCP stdio 初始化失败：{err}")))?;
    tracing::info!("stdio MCP server started");
    running
        .waiting()
        .await
        .map_err(|err| error::AppError::Configuration(format!("MCP stdio 服务异常退出：{err}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ensure_mcp_enabled;

    #[test]
    fn mcp_enabled_gate_rejects_disabled_and_accepts_enabled() {
        assert!(ensure_mcp_enabled(true).is_ok());
        let err = ensure_mcp_enabled(false).unwrap_err();
        assert!(err.to_string().contains("MCP 服务未启用"), "{err}");
    }
}
