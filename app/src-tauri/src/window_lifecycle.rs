use tauri::{Emitter, Manager, WindowEvent};

use crate::{models::CloseBehavior, service::AppService};

pub fn handle(window: &tauri::Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        let app = window.app_handle().clone();
        let service = app.state::<AppService>();
        match service.current_settings().close_behavior {
            CloseBehavior::HideToTray => {
                tracing::info!("main window close requested; hiding to tray");
                api.prevent_close();
                let _ = window.hide();
            }
            CloseBehavior::Exit => {
                tracing::info!("main window close requested; exiting application")
            }
            CloseBehavior::Ask => {
                tracing::info!("main window close requested; awaiting user choice");
                api.prevent_close();
                let _ = window.emit("close-behavior-requested", ());
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
    }
}
