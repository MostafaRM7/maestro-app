use std::time::Duration;

use maestro_protocol::{Request, Response};
use maestrod::DaemonPaths;
use tauri::{
    App, AppHandle, Manager,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

use super::DesktopHostState;

const SHOW_ID: &str = "maestro-show";
const STOP_ALL_ID: &str = "maestro-stop-all";
const STOP_AND_QUIT_ID: &str = "maestro-stop-and-quit";
const QUIT_KEEP_ID: &str = "maestro-quit-keep";

pub(super) fn install(app: &mut App) -> tauri::Result<()> {
    let status = MenuItem::with_id(
        app,
        "maestro-background-status",
        "No active sessions",
        false,
        None::<&str>,
    )?;
    let show = MenuItem::with_id(app, SHOW_ID, "Show Maestro", true, None::<&str>)?;
    let stop_all = MenuItem::with_id(app, STOP_ALL_ID, "Stop All Sessions", false, None::<&str>)?;
    let stop_and_quit = MenuItem::with_id(
        app,
        STOP_AND_QUIT_ID,
        "Stop All and Quit Maestro",
        true,
        None::<&str>,
    )?;
    let quit_keep = MenuItem::with_id(
        app,
        QUIT_KEEP_ID,
        "Quit Maestro and Keep Sessions",
        true,
        None::<&str>,
    )?;
    let first_separator = PredefinedMenuItem::separator(app)?;
    let second_separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &first_separator,
            &show,
            &stop_all,
            &second_separator,
            &stop_and_quit,
            &quit_keep,
        ],
    )?;
    let mut tray = TrayIconBuilder::with_id("maestro-background")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Maestro agent control center")
        .on_menu_event(|app, event| handle_menu_event(app, &event))
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;

    let app_handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        loop {
            update_status(&app_handle, &status, &stop_all).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    Ok(())
}

fn handle_menu_event(app: &AppHandle, event: &tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        SHOW_ID => show_main_window(app),
        STOP_ALL_ID => stop_all(app.clone(), false),
        STOP_AND_QUIT_ID => stop_all(app.clone(), true),
        QUIT_KEEP_ID => app.exit(0),
        _ => {}
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn stop_all(app: AppHandle, quit_after: bool) {
    tauri::async_runtime::spawn(async move {
        if let Ok(paths) = DaemonPaths::discover() {
            let state = app.state::<DesktopHostState>();
            let _ = state.request_daemon(&paths, Request::StopAllWork).await;
        }
        if quit_after {
            app.exit(0);
        }
    });
}

async fn update_status(
    app: &AppHandle,
    status: &MenuItem<tauri::Wry>,
    stop_all: &MenuItem<tauri::Wry>,
) {
    let Ok(paths) = DaemonPaths::discover() else {
        return;
    };
    let state = app.state::<DesktopHostState>();
    let Ok(Response::SystemSnapshot(snapshot)) =
        state.request_daemon(&paths, Request::SystemSnapshot).await
    else {
        let _ = status.set_text("Maestro service unavailable");
        let _ = stop_all.set_enabled(false);
        return;
    };
    let active = snapshot.active_sessions > 0 || snapshot.active_terminals > 0;
    let label = if active {
        format!(
            "{} agent session(s) · {} terminal(s)",
            snapshot.active_sessions, snapshot.active_terminals
        )
    } else {
        "No active sessions".to_owned()
    };
    let _ = status.set_text(label);
    let _ = stop_all.set_enabled(active);
}
