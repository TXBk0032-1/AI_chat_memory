//! 系统托盘图标与菜单的创建及事件处理模块。
//! 所有托盘相关的 UI 和交互逻辑集中在此
use tauri::{
    Manager,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

use crate::{models::TrayClickBehavior, service::AppService};

pub fn build(app: &tauri::App, service: &AppService) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "打开对话归档", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .show_menu_on_left_click(matches!(
            tauri::async_runtime::block_on(service.settings()).tray_click_behavior,
            TrayClickBehavior::ShowMenu
        ))
        .icon(app.default_window_icon().unwrap().clone())
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => {
                tracing::info!("application exit requested from tray");
                app.exit(0)
            }
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
                let behavior =
                    tauri::async_runtime::block_on(service.settings()).tray_click_behavior;
                if matches!(behavior, TrayClickBehavior::OpenWindow) {
                    show_main_window(app);
                }
            }
        })
        .build(app)?;
    Ok(())
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
