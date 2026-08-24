//! 系统托盘图标与菜单的创建及事件处理模块。
//! 所有托盘相关的 UI 和交互逻辑集中在此
use tauri::{
    Manager, Runtime,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

use crate::{
    i18n::native_text,
    models::{SupportedLocale, TrayClickBehavior},
    service::AppService,
};

pub fn build(app: &tauri::App, service: &AppService, locale: SupportedLocale) -> tauri::Result<()> {
    let settings = service.current_settings();
    let menu = build_menu(app, locale)?;
    TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .show_menu_on_left_click(matches!(
            settings.tray_click_behavior,
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
                let behavior = service.current_settings().tray_click_behavior;
                if matches!(behavior, TrayClickBehavior::OpenWindow) {
                    show_main_window(app);
                }
            }
        })
        .build(app)?;
    Ok(())
}

fn build_menu<R: Runtime, M: Manager<R>>(
    manager: &M,
    locale: SupportedLocale,
) -> tauri::Result<Menu<R>> {
    let text = native_text(locale);
    let show = MenuItem::with_id(manager, "show", text.open, true, None::<&str>)?;
    let quit = MenuItem::with_id(manager, "quit", text.quit, true, None::<&str>)?;
    Menu::with_items(manager, &[&show, &quit])
}

pub fn update_locale(app: &tauri::AppHandle, locale: SupportedLocale) -> tauri::Result<()> {
    if let Some(tray) = app.tray_by_id("main-tray") {
        tray.set_menu(Some(build_menu(app, locale)?))?;
    }
    Ok(())
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
