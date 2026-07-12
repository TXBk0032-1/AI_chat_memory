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
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
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
            let legacy = find_legacy_database();
            let service = tauri::async_runtime::block_on(async {
                let settings = Arc::new(SettingsStore::load(settings_path).await?);
                let mut settings_value = settings.get().await;
                let database_dir = settings_value
                    .data_directory
                    .as_ref()
                    .map(std::path::PathBuf::from)
                    .unwrap_or(data_dir);
                if settings_value.data_directory.is_none() {
                    settings_value.data_directory = Some(database_dir.to_string_lossy().into_owned());
                    settings.update(settings_value).await?;
                }
                tokio::fs::create_dir_all(&database_dir).await?;
                let database_path = database_dir.join("chat_memory.db");
                let pool = database::connect(&database_path).await?;
                tracing::info!(path=%database_path.display(), "application database ready");
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
            app.manage(service.clone());
            let http_service = service.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = http_api::serve(http_service.clone()).await {
                    http_service
                        .set_api_status(crate::models::ApiStatus::Failed(error.to_string()))
                        .await;
                    tracing::error!(%error,"local API stopped");
                }
            });

            let show = MenuItem::with_id(app, "show", "打开对话归档", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::with_id("main-tray")
                .menu(&menu)
                .show_menu_on_left_click(matches!(
                    tauri::async_runtime::block_on(service.settings.get()).tray_click_behavior,
                    crate::models::TrayClickBehavior::ShowMenu
                ))
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
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        let service = app.state::<AppService>();
                        let behavior = tauri::async_runtime::block_on(service.settings.get())
                            .tray_click_behavior;
                        if matches!(behavior, crate::models::TrayClickBehavior::OpenWindow)
                            && let Some(window) = app.get_webview_window("main")
                        {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle().clone();
                let service = app.state::<AppService>();
                match tauri::async_runtime::block_on(service.settings.get()).close_behavior {
                    crate::models::CloseBehavior::HideToTray => {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                    crate::models::CloseBehavior::Exit => {}
                    crate::models::CloseBehavior::Ask => {
                        api.prevent_close();
                        let window = window.clone();
                        app.dialog()
                            .message("以后关闭窗口时要隐藏到系统托盘吗？选择“退出应用”将直接结束本地同步服务。")
                            .title("关闭窗口")
                            .buttons(MessageDialogButtons::OkCancelCustom(
                                "隐藏到托盘".into(),
                                "退出应用".into(),
                            ))
                            .show(move |hide| {
                                let app = window.app_handle().clone();
                                tauri::async_runtime::spawn(async move {
                                    let service = app.state::<AppService>();
                                    let mut settings = service.settings.get().await;
                                    settings.close_behavior = if hide {
                                        crate::models::CloseBehavior::HideToTray
                                    } else {
                                        crate::models::CloseBehavior::Exit
                                    };
                                    if let Err(error) = service.settings.update(settings).await {
                                        tracing::error!(%error, "failed to save close behavior");
                                        return;
                                    }
                                    if hide {
                                        let _ = window.hide();
                                    } else {
                                        app.exit(0);
                                    }
                                });
                            });
                    }
                }
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
            ,commands::move_data_directory
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
