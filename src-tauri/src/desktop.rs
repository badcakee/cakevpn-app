//! What only a computer has: the tray icon and its menu, starting at login,
//! updates that install themselves, the keyboard shortcut and the window
//! that hides in the tray.

use crate::{helper, AppState, TrayModel, UpdateInfo};
use cakevpn_proto::Request;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, WindowEvent, Wry};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_updater::UpdaterExt;

/// Passed when CakeVPN starts with the computer, so it starts in the tray.
const HIDDEN_ARG: &str = "--hidden";

pub fn plugins(builder: tauri::Builder<Wry>) -> tauri::Builder<Wry> {
    builder
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec![HIDDEN_ARG])))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        let _ = app.emit("tray-action", "toggle");
                    }
                })
                .build(),
        )
        .on_window_event(|window, event| {
            // Closing the window keeps CakeVPN in the tray, like other VPN apps.
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let to_tray = window.try_state::<AppState>().is_none_or(|s| s.close_to_tray.load(Ordering::Relaxed));
                    if !to_tray {
                        quit(window.app_handle());
                        return;
                    }
                    let _ = window.hide();
                    if let Some(state) = window.try_state::<AppState>() {
                        state.visible.store(false, Ordering::Relaxed);
                    }
                }
                WindowEvent::Focused(true) => {
                    if let Some(state) = window.try_state::<AppState>() {
                        state.visible.store(true, Ordering::Relaxed);
                    }
                }
                _ => {}
            }
        })
}

/// Opened at login: stay in the tray. Opened by the person: show the window.
pub fn starts_shown() -> bool {
    !std::env::args().any(|a| a == HIDDEN_ARG)
}

/// Shows the window (unless started at login) and puts CakeVPN in the tray.
pub fn setup(app: &tauri::App, shown: bool) -> Result<(), Box<dyn std::error::Error>> {
    if shown {
        show_window(app.handle());
    }
    // Until the window describes it (in its language), the menu is the simplest one.
    let open = MenuItem::with_id(app, "open", "Open CakeVPN", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit CakeVPN", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &PredefinedMenuItem::separator(app)?, &quit_item])?;
    let mut tray = TrayIconBuilder::with_id("main").tooltip("CakeVPN").menu(&menu);
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    // Connecting, disconnecting and choosing a location are done by the
    // window, like its own buttons, so it gets told what was picked.
    tray.on_menu_event(|app, event| match event.id.as_ref() {
        "open" => show_window(app),
        "quit" => quit(app),
        "toggle" => {
            let _ = app.emit("tray-action", "toggle");
        }
        id if id.starts_with("loc:") => {
            let _ = app.emit("tray-action", id);
        }
        _ => {}
    })
    .build(app)?;
    Ok(())
}

pub fn autostart_enabled(app: &AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// Starts CakeVPN when the computer starts, hidden in the tray.
pub fn set_autostart(app: &AppHandle, enabled: bool) -> Result<bool, String> {
    let launcher = app.autolaunch();
    let result = if enabled { launcher.enable() } else { launcher.disable() };
    result.map_err(|e| format!("Could not change the startup setting: {e}"))?;
    Ok(launcher.is_enabled().unwrap_or(enabled))
}

/// Looks for a newer CakeVPN release on GitHub.
pub async fn check_update(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = match updater.check().await {
        Ok(update) => update,
        // The newest release has no build for this kind of computer (yet): nothing to update to.
        Err(e) if e.to_string().contains("platforms") => None,
        Err(e) => return Err(format!("Could not check for updates: {e}")),
    };
    Ok(update.map(|u| UpdateInfo { version: u.version.clone(), notes: u.body.clone() }))
}

/// Downloads and installs the newer release, then restarts CakeVPN.
pub async fn install_update(app: &AppHandle, state: &AppState) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| format!("Could not check for updates: {e}"))?
        .ok_or("CakeVPN is already up to date.")?;
    // The installer replaces the helper on Windows, which would cut the tunnel anyway.
    let _ = helper::ask(Request::Disconnect).await;
    let (done, total) = (Arc::clone(&state.update_done), Arc::clone(&state.update_total));
    done.store(0, Ordering::Relaxed);
    total.store(0, Ordering::Relaxed);
    update
        .download_and_install(
            move |chunk, size| {
                done.fetch_add(chunk as u64, Ordering::Relaxed);
                if let Some(size) = size {
                    total.store(size, Ordering::Relaxed);
                }
            },
            || {},
        )
        .await
        .map_err(|e| format!("The update could not be installed: {e}"))?;
    app.restart()
}

/// Sets the keyboard shortcut that turns the VPN on and off ("Control+Alt+Shift+V"), or none.
pub fn set_shortcut(app: &AppHandle, accelerator: Option<String>) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
    let shortcuts = app.global_shortcut();
    shortcuts.unregister_all().map_err(|e| e.to_string())?;
    if let Some(accelerator) = accelerator.filter(|a| !a.is_empty()) {
        let shortcut: Shortcut = accelerator.parse().map_err(|_| "That key combination can't be used.".to_string())?;
        shortcuts
            .register(shortcut)
            .map_err(|_| "That key combination is already used by another app.".to_string())?;
    }
    Ok(())
}

pub fn set_tray(app: &AppHandle, model: TrayModel) -> Result<(), String> {
    let tray = app.tray_by_id("main").ok_or("no tray icon")?;
    let build = || -> tauri::Result<Menu<Wry>> {
        let status = MenuItem::with_id(app, "status", &model.status, false, None::<&str>)?;
        let toggle = MenuItem::with_id(app, "toggle", &model.toggle, true, None::<&str>)?;
        let places = Submenu::with_id(app, "locations", &model.locations_label, !model.locations.is_empty())?;
        for l in &model.locations {
            places.append(&CheckMenuItem::with_id(app, format!("loc:{}", l.id), &l.name, l.enabled, l.chosen, None::<&str>)?)?;
        }
        let open = MenuItem::with_id(app, "open", &model.open, true, None::<&str>)?;
        let quit = MenuItem::with_id(app, "quit", &model.quit, true, None::<&str>)?;
        let line = || PredefinedMenuItem::separator(app);
        Menu::with_items(app, &[&status, &toggle, &places, &line()?, &open, &line()?, &quit])
    };
    tray.set_menu(Some(build().map_err(|e| e.to_string())?)).map_err(|e| e.to_string())?;
    tray.set_tooltip(Some(&model.tooltip)).map_err(|e| e.to_string())?;
    Ok(())
}

/// Disconnects and leaves: leaving would otherwise keep the tunnel up with no way to see it.
fn quit(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = helper::ask(Request::Disconnect).await;
        app.exit(0);
    });
}

pub fn show_window(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        state.visible.store(true, Ordering::Relaxed);
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
