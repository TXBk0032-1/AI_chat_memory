mod branch;
mod commands;
mod data_directory;
mod database;
mod embedding;
mod error;
mod http_api;
mod local_services;
mod logging;
mod mcp;
mod models;
mod normalizer;
mod semantic;
mod service;
mod settings;
mod tray;
mod window_lifecycle;

use service::AppService;
use settings::SettingsStore;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
            tracing::info!(app_data_dir=%data_dir.display(), "application starting");

            #[cfg(target_os = "windows")]
            if let Some(window) = app.get_webview_window("main") {
                match window_vibrancy::apply_acrylic(&window, Some((18, 18, 18, 110))) {
                    Ok(()) => tracing::info!("acrylic window effect enabled"),
                    Err(error) => tracing::warn!(%error, "acrylic window effect unavailable"),
                }
            }

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

            tray::build(app, &service)?;
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
            commands::import_deepseek_zip,
            commands::get_settings,
            commands::save_settings,
            commands::rotate_secret,
            commands::get_api_status,
            commands::get_semantic_status,
            commands::check_embedding_backend,
            commands::reindex_semantic_search,
            commands::download_local_embedding_model,
            commands::import_local_embedding_model,
            commands::cancel_semantic_work,
            commands::move_data_directory,
            commands::confirm_close_behavior,
            commands::write_export_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
