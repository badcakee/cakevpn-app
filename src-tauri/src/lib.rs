mod api;
mod helper;
mod netinfo;
mod store;

use api::{Account, ApiError, Load};
use cakevpn_proto::{Request, Status, TunnelState, PROTOCOL_VERSION};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, State, WindowEvent};
#[cfg(target_os = "macos")]
use tauri::RunEvent;
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tokio::sync::Mutex;

/// Passed when CakeVPN starts with the computer, so it starts in the tray.
const HIDDEN_ARG: &str = "--hidden";

struct AppState {
    data_dir: PathBuf,
    account: Mutex<Option<Account>>,
    /// The location the tunnel was opened to, for the load warning.
    location: Mutex<Option<String>>,
    wifi: Mutex<Option<(Instant, netinfo::Network)>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    signed_in: bool,
    account: Option<Account>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Banner {
    kind: &'static str,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Overview {
    /// "ok", "missing" (the helper is not installed or not running) or "outdated".
    helper: &'static str,
    status: Option<Status>,
    banner: Option<Banner>,
    location_id: Option<String>,
}

/// Decides which warning to show, most useful first: a bad Wi-Fi explains a
/// slow VPN, so it wins over the server's load.
fn banner(status: &Status, load: Option<&Load>, wifi_name: Option<&str>) -> Option<Banner> {
    if status.state != TunnelState::Connected {
        return None;
    }
    let q = &status.quality;
    let shaky_wifi = q.gateway_loss.is_some_and(|l| l >= 0.05)
        || q.gateway_jitter_ms.is_some_and(|j| j >= 30.0)
        || q.gateway_rtt_ms.is_some_and(|r| r >= 50.0);
    if shaky_wifi {
        let message = match wifi_name {
            Some(name) => format!("The Wi-Fi you're connected to ({name}) is unstable."),
            None => "The Wi-Fi or network you're connected to is unstable.".to_string(),
        };
        return Some(Banner { kind: "wifi", message });
    }
    if load.is_some_and(|l| l.level == "high") {
        return Some(Banner { kind: "load", message: "The location you're in is experiencing high load.".into() });
    }
    if q.tunnel_failures >= 2 || q.tunnel_delay_ms.is_some_and(|d| d >= 400) {
        return Some(Banner { kind: "slow", message: "Your connection to the VPN is slow right now.".into() });
    }
    None
}

fn not_signed_in() -> ApiError {
    ApiError::new("signed_out", "Enter your code to sign in.")
}

async fn fetch_account(state: &AppState) -> Result<Account, ApiError> {
    let token = store::token().ok_or_else(not_signed_in)?;
    match api::account(&token).await {
        Ok(account) => {
            *state.account.lock().await = Some(account.clone());
            Ok(account)
        }
        Err(e) => {
            if e.error == "signed_out" {
                // Signed in somewhere else, or the code was deleted: forget the token and stop the tunnel.
                store::clear_token();
                *state.account.lock().await = None;
                let _ = helper::ask(Request::Disconnect).await;
            }
            Err(e)
        }
    }
}

#[tauri::command]
async fn load_session(state: State<'_, AppState>) -> Result<Session, ApiError> {
    if store::token().is_none() {
        return Ok(Session { signed_in: false, account: None });
    }
    let account = fetch_account(&state).await?;
    Ok(Session { signed_in: true, account: Some(account) })
}

#[tauri::command]
async fn redeem(code: String, state: State<'_, AppState>) -> Result<Account, ApiError> {
    let token = api::redeem(&code, &store::device_id(&state.data_dir), &store::device_name()).await?;
    store::save_token(&token).map_err(|m| ApiError::new("keychain", m))?;
    fetch_account(&state).await
}

#[tauri::command]
async fn refresh_account(state: State<'_, AppState>) -> Result<Account, ApiError> {
    fetch_account(&state).await
}

#[tauri::command]
async fn sign_out(state: State<'_, AppState>) -> Result<(), String> {
    let _ = helper::ask(Request::Disconnect).await;
    if let Some(token) = store::token() {
        api::sign_out(&token).await;
    }
    store::clear_token();
    *state.account.lock().await = None;
    Ok(())
}

#[tauri::command]
async fn connect(location_id: String, state: State<'_, AppState>) -> Result<Status, String> {
    // Always use fresh details: signing in elsewhere changes them.
    let account = match fetch_account(&state).await {
        Ok(a) => a,
        Err(e) if e.error == "offline" => state.account.lock().await.clone().ok_or(e.message)?,
        Err(e) => return Err(e.message),
    };
    let location = account
        .locations
        .iter()
        .find(|l| l.id == location_id)
        .or_else(|| account.locations.first())
        .ok_or("No locations are available right now.")?;
    let params = location.connect.clone().ok_or("This location is offline right now.")?;
    *state.location.lock().await = Some(location.id.clone());
    let response = helper::ask(Request::Connect { params }).await?;
    if response.ok {
        Ok(response.status)
    } else {
        Err(response.error.unwrap_or_else(|| "Could not connect.".into()))
    }
}

#[tauri::command]
async fn disconnect() -> Result<Status, String> {
    Ok(helper::ask(Request::Disconnect).await?.status)
}

#[tauri::command]
async fn overview(state: State<'_, AppState>) -> Result<Overview, String> {
    let response = match helper::ask(Request::Status).await {
        Ok(r) => r,
        Err(e) if e == helper::HELPER_MISSING => {
            return Ok(Overview { helper: "missing", status: None, banner: None, location_id: None })
        }
        Err(e) => return Err(e),
    };
    let status = response.status;
    // Only a protocol change needs a new helper; plain app updates keep the installed one.
    let helper_state = if status.protocol != PROTOCOL_VERSION { "outdated" } else { "ok" };
    let location_id = state.location.lock().await.clone();

    // Reading the Wi-Fi name runs a system tool, so it is refreshed every 30 seconds at most.
    let wifi_name = {
        let mut cached = state.wifi.lock().await;
        let stale = cached.as_ref().is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(30));
        if stale {
            let net = tokio::task::spawn_blocking(netinfo::network).await.unwrap_or_default();
            *cached = Some((Instant::now(), net));
        }
        cached.as_ref().and_then(|(_, n)| n.wifi_name.clone())
    };
    let load = {
        let account = state.account.lock().await;
        account
            .as_ref()
            .and_then(|a| a.locations.iter().find(|l| Some(&l.id) == location_id.as_ref()))
            .and_then(|l| l.load.clone())
    };
    let banner = banner(&status, load.as_ref(), wifi_name.as_deref());
    Ok(Overview { helper: helper_state, status: Some(status), banner, location_id })
}

#[tauri::command]
async fn install_helper() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        tokio::task::spawn_blocking(helper::install).await.map_err(|e| e.to_string())??;
        // Give launchd a moment to start it.
        for _ in 0..20 {
            if helper::ask(Request::Status).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err("The helper was installed but did not start. Restart your Mac and try again.".into())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("The CakeVPN service is not running. Reinstall CakeVPN to fix it.".into())
    }
}

/// How fast each location answers, measured outside the tunnel. While the
/// tunnel is up the numbers would be wrong, so nothing is measured then.
#[tauri::command]
async fn ping_locations(state: State<'_, AppState>) -> Result<HashMap<String, Option<u32>>, String> {
    if let Ok(r) = helper::ask(Request::Status).await {
        if r.status.state != TunnelState::Disconnected && r.status.state != TunnelState::Failed {
            return Ok(HashMap::new());
        }
    }
    let targets: Vec<(String, String, u16)> = state
        .account
        .lock()
        .await
        .as_ref()
        .map(|a| {
            a.locations
                .iter()
                .filter_map(|l| l.connect.as_ref().map(|c| (l.id.clone(), c.host.clone(), c.port)))
                .collect()
        })
        .unwrap_or_default();
    let mut pings = HashMap::new();
    for (id, host, port) in targets {
        pings.insert(id, netinfo::tcp_ping(&host, port).await);
    }
    Ok(pings)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsInfo {
    version: &'static str,
    autostart: bool,
}

#[tauri::command]
fn settings_info(app: tauri::AppHandle) -> SettingsInfo {
    SettingsInfo {
        version: env!("CARGO_PKG_VERSION"),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
    }
}

/// Starts CakeVPN when the computer starts, hidden in the tray.
#[tauri::command]
fn set_autostart(enabled: bool, app: tauri::AppHandle) -> Result<bool, String> {
    let launcher = app.autolaunch();
    let result = if enabled { launcher.enable() } else { launcher.disable() };
    result.map_err(|e| format!("Could not change the startup setting: {e}"))?;
    Ok(launcher.is_enabled().unwrap_or(enabled))
}

fn show_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec![HIDDEN_ARG])))
        .setup(|app| {
            // Opened at login: stay in the tray. Opened by the person: show the window.
            if !std::env::args().any(|a| a == HIDDEN_ARG) {
                show_window(app.handle());
            }
            let data_dir = app.path().app_data_dir()?;
            app.manage(AppState {
                data_dir,
                account: Mutex::new(None),
                location: Mutex::new(None),
                wifi: Mutex::new(None),
            });

            let open = MenuItem::with_id(app, "open", "Open CakeVPN", true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "disconnect", "Disconnect", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit CakeVPN", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &stop, &PredefinedMenuItem::separator(app)?, &quit])?;
            let mut tray = TrayIconBuilder::with_id("main").tooltip("CakeVPN").menu(&menu);
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.on_menu_event(|app, event| match event.id.as_ref() {
                "open" => show_window(app),
                "disconnect" => {
                    tauri::async_runtime::spawn(async {
                        let _ = helper::ask(Request::Disconnect).await;
                    });
                }
                "quit" => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        // Leaving would otherwise keep the tunnel up with no way to see it.
                        let _ = helper::ask(Request::Disconnect).await;
                        app.exit(0);
                    });
                }
                _ => {}
            })
            .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps CakeVPN in the tray, like other VPN apps.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            load_session,
            redeem,
            refresh_account,
            sign_out,
            connect,
            disconnect,
            overview,
            install_helper,
            ping_locations,
            settings_info,
            set_autostart
        ])
        .build(tauri::generate_context!())
        .expect("error while starting CakeVPN");

    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = event {
            show_window(app);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (app, event);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use cakevpn_proto::Quality;

    fn connected(q: Quality) -> Status {
        Status { state: TunnelState::Connected, quality: q, ..Default::default() }
    }

    fn load(level: &str) -> Load {
        Load { percent: 90, level: level.into() }
    }

    #[test]
    fn quiet_when_all_is_well() {
        let s = connected(Quality { gateway_rtt_ms: Some(3.0), gateway_jitter_ms: Some(1.0), gateway_loss: Some(0.0), tunnel_delay_ms: Some(40), tunnel_failures: 0 });
        assert_eq!(banner(&s, Some(&load("low")), Some("Home")), None);
    }

    #[test]
    fn lossy_wifi_is_named() {
        let s = connected(Quality { gateway_loss: Some(0.2), gateway_rtt_ms: Some(4.0), gateway_jitter_ms: Some(2.0), ..Default::default() });
        let b = banner(&s, Some(&load("high")), Some("School Guest")).unwrap();
        assert_eq!(b.kind, "wifi");
        assert!(b.message.contains("School Guest"));
    }

    #[test]
    fn busy_location_when_wifi_is_fine() {
        let s = connected(Quality { gateway_loss: Some(0.0), gateway_rtt_ms: Some(4.0), gateway_jitter_ms: Some(2.0), ..Default::default() });
        assert_eq!(banner(&s, Some(&load("high")), None).unwrap().kind, "load");
    }

    #[test]
    fn slow_tunnel() {
        let s = connected(Quality { tunnel_delay_ms: Some(900), ..Default::default() });
        assert_eq!(banner(&s, None, None).unwrap().kind, "slow");
    }

    #[test]
    fn nothing_while_disconnected() {
        let s = Status { state: TunnelState::Disconnected, quality: Quality { gateway_loss: Some(1.0), ..Default::default() }, ..Default::default() };
        assert_eq!(banner(&s, None, None), None);
    }
}
