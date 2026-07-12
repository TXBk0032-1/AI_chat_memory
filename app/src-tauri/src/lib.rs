mod commands;
mod database;
mod error;
mod http_api;
mod models;
mod normalizer;
mod service;
mod settings;

use service::AppService;
use settings::SettingsStore;
use std::sync::Arc;
use tauri::{
    Manager, WindowEvent,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};
use tokio::sync::RwLock;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let settings_path = data_dir.join("settings.json");
            let database_path = data_dir.join("chat_memory.db");
            let legacy = find_legacy_database();
            let service = tauri::async_runtime::block_on(async {
                let settings = Arc::new(SettingsStore::load(settings_path).await?);
                let pool = database::connect(&database_path).await?;
                Ok::<_, crate::error::AppError>(AppService {
                    pool,
                    settings,
                    api_status: Arc::new(RwLock::new(crate::models::ApiStatus::Starting)),
                })
            })?;
            if legacy.exists()
                && !tauri::async_runtime::block_on(service.settings.get()).migrated_legacy_database
                && let Err(error) = tauri::async_runtime::block_on(service.migrate_legacy(&legacy))
            {
                tracing::error!(%error, "automatic legacy migration failed");
            }
            tracing::info!(path=%database_path.display(), "application database ready");
            app.manage(service.clone());
            tauri::async_runtime::spawn(async move {
                if let Err(error) = http_api::serve(service.clone()).await {
                    service
                        .set_api_status(crate::models::ApiStatus::Failed(error.to_string()))
                        .await;
                    tracing::error!(%error,"local API stopped");
                }
            });

            let show = MenuItem::with_id(app, "show", "打开对话归档", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::new()
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::search_sessions,
            commands::get_session,
            commands::delete_session,
            commands::import_deepseek_zip,
            commands::get_settings,
            commands::save_settings,
            commands::rotate_secret,
            commands::get_api_status,
            commands::migrate_legacy_database
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}

fn find_legacy_database() -> std::path::PathBuf {
    let start = std::env::current_dir().unwrap_or_default();
    for directory in start.ancestors() {
        let candidate = directory.join("legacy/python/server/data/chat_memory.db");
        if candidate.exists() {
            return candidate;
        }
        let original = directory.join("server/data/chat_memory.db");
        if original.exists() {
            return original;
        }
    }
    start.join("legacy/python/server/data/chat_memory.db")
}
