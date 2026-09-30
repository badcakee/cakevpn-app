mod api;
mod helper;
mod netinfo;
mod store;

use api::{Account, ApiError, Load};
use cakevpn_proto::{Request, Status, TunnelState, HELPER_REVISION, PROTOCOL_VERSION};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, State, WindowEvent};
#[cfg(target_os = "macos")]
use tauri::RunEvent;
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::Mutex;

/// Passed when CakeVPN starts with the computer, so it starts in the tray.
const HIDDEN_ARG: &str = "--hidden";

/// Faster than this (bytes per second, both ways together, 4 Mbps) the line
/// counts as busy: a speed test or a big download fills it, and pings to the
/// router slow down even on a good Wi-Fi.
const BUSY_BYTES_PER_SEC: f64 = 500_000.0;
/// The Wi-Fi is judged on the last 30 seconds of pings, so it waits this long
/// after the line was busy.
const BUSY_HOLD: Duration = Duration::from_secs(35);

/// How much has gone through the tunnel, to tell when the line is busy.
#[derive(Default)]
struct Traffic {
    last: Option<(Instant, u64)>,
    busy_until: Option<Instant>,
}

impl Traffic {
    /// Takes the tunnel's byte count and says whether the line is (or just was) busy.
    fn busy(&mut self, now: Instant, total_bytes: u64) -> bool {
        if let Some((at, bytes)) = self.last {
            let secs = now.duration_since(at).as_secs_f64();
            if total_bytes >= bytes && secs > 0.0 && (total_bytes - bytes) as f64 / secs >= BUSY_BYTES_PER_SEC {
                self.busy_until = Some(now + BUSY_HOLD);
            }
        }
        self.last = Some((now, total_bytes));
        self.busy_until.is_some_and(|until| now < until)
    }
}

struct AppState {
    data_dir: PathBuf,
    account: Mutex<Option<Account>>,
    /// The location the tunnel was opened to, for the load warning.
    location: Mutex<Option<String>>,
    wifi: Mutex<Option<(Instant, netinfo::Network)>>,
    traffic: Mutex<Traffic>,
    /// The connection details last saved to disk, to save them only when they change.
    saved: Mutex<Option<String>>,
    /// False while the window is hidden (CakeVPN in the tray), so the page
    /// can check things less often.
    visible: AtomicBool,
    /// The speed test in progress: its download and upload use one ticket.
    speed: Mutex<Option<api::SpeedTicket>>,
}

/// What the person switched on in Settings, sent along with every connect.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct ConnectOptions {
    block_ads: bool,
    kill_switch: bool,
    bypass_domains: Vec<String>,
    bypass_apps: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    signed_in: bool,
    account: Option<Account>,
    /// The server couldn't be reached, so `account` is the copy saved earlier.
    offline: bool,
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
    window_visible: bool,
}

/// Decides which warning to show, most useful first: a bad Wi-Fi explains a
/// slow VPN, so it wins over the server's load. While the line is busy (a
/// speed test, a big download) slow pings are expected, so the Wi-Fi and
/// speed warnings wait until it calms down.
fn banner(status: &Status, load: Option<&Load>, wifi_name: Option<&str>, busy: bool) -> Option<Banner> {
    if status.state != TunnelState::Connected {
        return None;
    }
    let q = &status.quality;
    let shaky_wifi = !busy
        && (q.gateway_loss.is_some_and(|l| l >= 0.05)
            || q.gateway_jitter_ms.is_some_and(|j| j >= 30.0)
            || q.gateway_rtt_ms.is_some_and(|r| r >= 50.0));
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
    if !busy && (q.tunnel_failures >= 2 || q.tunnel_delay_ms.is_some_and(|d| d >= 400)) {
        return Some(Banner { kind: "slow", message: "Your connection to the VPN is slow right now.".into() });
    }
    None
}

fn not_signed_in() -> ApiError {
    ApiError::new("signed_out", "Enter your code to sign in.")
}

/// The account as it is kept on disk: unlike what the window gets, it
/// includes each location's connection details.
fn account_for_disk(account: &Account) -> serde_json::Value {
    let mut value = serde_json::to_value(account).unwrap_or_default();
    if let Some(locations) = value["locations"].as_array_mut() {
        for (saved, location) in locations.iter_mut().zip(&account.locations) {
            saved["connect"] = serde_json::to_value(&location.connect).unwrap_or_default();
        }
    }
    value
}

/// Saves the account for networks where the server can't be reached. It is
/// written again only when the connection details changed.
async fn remember_account(state: &AppState, token: &str, account: &Account) {
    let on_disk = account_for_disk(account);
    let details = on_disk["locations"].to_string();
    let mut saved = state.saved.lock().await;
    if saved.as_deref() == Some(details.as_str()) {
        return;
    }
    if store::save_account(&state.data_dir, token, on_disk.to_string().as_bytes()).is_ok() {
        *saved = Some(details);
    }
}

async fn fetch_account(state: &AppState) -> Result<Account, ApiError> {
    let token = store::token().ok_or_else(not_signed_in)?;
    match api::account(&token).await {
        Ok(account) => {
            *state.account.lock().await = Some(account.clone());
            remember_account(state, &token, &account).await;
            Ok(account)
        }
        Err(e) => {
            if e.error == "signed_out" {
                // Signed in somewhere else, or the code was deleted: forget the token and stop the tunnel.
                store::clear_token();
                store::forget_account(&state.data_dir);
                *state.saved.lock().await = None;
                *state.account.lock().await = None;
                let _ = helper::ask(Request::Disconnect).await;
            }
            Err(e)
        }
    }
}

/// How long starting and connecting wait for the server before they go on
/// with the saved account.
const SERVER_WAIT: Duration = Duration::from_secs(6);

#[tauri::command]
async fn load_session(state: State<'_, AppState>) -> Result<Session, ApiError> {
    let Some(token) = store::token() else {
        return Ok(Session { signed_in: false, account: None, offline: false });
    };
    let error = match tokio::time::timeout(SERVER_WAIT, fetch_account(&state)).await {
        Ok(Ok(account)) => return Ok(Session { signed_in: true, account: Some(account), offline: false }),
        // Signed out or turned off: the saved copy must not be used.
        Ok(Err(e)) if e.error == "signed_out" || e.error == "code_disabled" => return Err(e),
        Ok(Err(e)) => e,
        Err(_) => ApiError::new("offline", "Can't reach CakeVPN. Check your internet connection and try again."),
    };
    // Some networks block CakeVPN's sign-in port but not the VPN itself, so
    // the account saved last time still lets this device connect.
    let saved = store::saved_account(&state.data_dir, &token)
        .and_then(|raw| serde_json::from_slice::<Account>(&raw).ok());
    match saved {
        Some(account) => {
            *state.account.lock().await = Some(account.clone());
            Ok(Session { signed_in: true, account: Some(account), offline: true })
        }
        None => Err(error),
    }
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

/// Makes an invite code and returns the account with it in the invite list.
#[tauri::command]
async fn create_invite(state: State<'_, AppState>) -> Result<Account, ApiError> {
    let token = store::token().ok_or_else(not_signed_in)?;
    api::create_invite(&token).await?;
    fetch_account(&state).await
}

/// Deletes an unused invite code and returns the account without it.
#[tauri::command]
async fn delete_invite(state: State<'_, AppState>, code: String) -> Result<Account, ApiError> {
    let token = store::token().ok_or_else(not_signed_in)?;
    api::delete_invite(&token, &code).await?;
    fetch_account(&state).await
}

#[tauri::command]
async fn sign_out(state: State<'_, AppState>) -> Result<(), String> {
    let _ = helper::ask(Request::Disconnect).await;
    if let Some(token) = store::token() {
        api::sign_out(&token).await;
    }
    store::clear_token();
    store::forget_account(&state.data_dir);
    *state.saved.lock().await = None;
    *state.account.lock().await = None;
    Ok(())
}

#[tauri::command]
async fn connect(location_id: String, options: Option<ConnectOptions>, state: State<'_, AppState>) -> Result<Status, String> {
    // Use fresh details when the server answers: signing in elsewhere changes them.
    let account = match tokio::time::timeout(SERVER_WAIT, fetch_account(&state)).await {
        Ok(Ok(a)) => a,
        Ok(Err(e)) if e.error != "offline" => return Err(e.message),
        _ => state
            .account
            .lock()
            .await
            .clone()
            .ok_or("Can't reach CakeVPN. Check your internet connection and try again.")?,
    };
    let location = account
        .locations
        .iter()
        .find(|l| l.id == location_id)
        .or_else(|| account.locations.first())
        .ok_or("No locations are available right now.")?;
    let mut params = location.connect.clone().ok_or("This location is offline right now.")?;
    let options = options.unwrap_or_default();
    params.block_ads = options.block_ads;
    params.kill_switch = options.kill_switch;
    params.bypass_domains = options.bypass_domains;
    params.bypass_apps = options.bypass_apps;
    *state.location.lock().await = Some(location.id.clone());
    let response = helper::ask(Request::Connect { params }).await?;
    if response.ok {
        Ok(response.status)
    } else {
        Err(response.error.unwrap_or_else(|| "Could not connect.".into()))
    }
}

/// The last 30 days of traffic for the usage graph.
#[tauri::command]
async fn usage_history() -> Result<Vec<api::DayUsage>, ApiError> {
    let token = store::token().ok_or_else(not_signed_in)?;
    api::usage_history(&token).await
}

/// Starts a speed test against the connected location and measures the
/// download, in Mbps. The server decides whether a test may run now.
#[tauri::command]
async fn speed_test_download(state: State<'_, AppState>) -> Result<f64, ApiError> {
    let token = store::token().ok_or_else(not_signed_in)?;
    let location = state.location.lock().await.clone().unwrap_or_else(|| "main".into());
    let ticket = api::speed_ticket(&token, &location).await?;
    *state.speed.lock().await = Some(ticket.clone());
    api::speed_download(&ticket).await
}

/// The upload half of the speed test started by `speed_test_download`.
#[tauri::command]
async fn speed_test_upload(state: State<'_, AppState>) -> Result<f64, ApiError> {
    let ticket = state.speed.lock().await.take().ok_or_else(|| ApiError::new("speed_test", "Start the speed test again."))?;
    api::speed_upload(&ticket).await
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
            return Ok(Overview {
                helper: "missing",
                status: None,
                banner: None,
                location_id: None,
                window_visible: state.visible.load(Ordering::Relaxed),
            })
        }
        Err(e) => return Err(e),
    };
    let status = response.status;
    // Only a helper change needs a new helper; plain app updates keep the installed one.
    let helper_state =
        if status.protocol != PROTOCOL_VERSION || status.revision < HELPER_REVISION { "outdated" } else { "ok" };
    let location_id = state.location.lock().await.clone();

    let window_visible = state.visible.load(Ordering::Relaxed);
    // Reading the Wi-Fi name runs a system tool, so it is refreshed every 30
    // seconds at most, and not at all while the window is hidden.
    let wifi_name = {
        let mut cached = state.wifi.lock().await;
        let stale = cached.as_ref().is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(30));
        if stale && (window_visible || cached.is_none()) {
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
    let busy = status.state == TunnelState::Connected
        && state.traffic.lock().await.busy(Instant::now(), status.up_bytes + status.down_bytes);
    let banner = banner(&status, load.as_ref(), wifi_name.as_deref(), busy);
    Ok(Overview { helper: helper_state, status: Some(status), banner, location_id, window_visible })
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInfo {
    version: String,
    notes: Option<String>,
}

/// Looks for a newer CakeVPN release on GitHub.
#[tauri::command]
async fn check_update(app: tauri::AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| format!("Could not check for updates: {e}"))?;
    Ok(update.map(|u| UpdateInfo { version: u.version.clone(), notes: u.body.clone() }))
}

/// Downloads and installs the newer release, then restarts CakeVPN.
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| format!("Could not check for updates: {e}"))?
        .ok_or("CakeVPN is already up to date.")?;
    // The installer replaces the helper on Windows, which would cut the tunnel anyway.
    let _ = helper::ask(Request::Disconnect).await;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| format!("The update could not be installed: {e}"))?;
    app.restart()
}

fn show_window(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        state.visible.store(true, Ordering::Relaxed);
    }
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            // Opened at login: stay in the tray. Opened by the person: show the window.
            let shown = !std::env::args().any(|a| a == HIDDEN_ARG);
            if shown {
                show_window(app.handle());
            }
            let data_dir = app.path().app_data_dir()?;
            app.manage(AppState {
                data_dir,
                account: Mutex::new(None),
                location: Mutex::new(None),
                wifi: Mutex::new(None),
                traffic: Mutex::new(Traffic::default()),
                saved: Mutex::new(None),
                visible: AtomicBool::new(shown),
                speed: Mutex::new(None),
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
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
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
            set_autostart,
            create_invite,
            delete_invite,
            usage_history,
            speed_test_download,
            speed_test_upload,
            check_update,
            install_update
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
        assert_eq!(banner(&s, Some(&load("low")), Some("Home"), false), None);
    }

    #[test]
    fn lossy_wifi_is_named() {
        let s = connected(Quality { gateway_loss: Some(0.2), gateway_rtt_ms: Some(4.0), gateway_jitter_ms: Some(2.0), ..Default::default() });
        let b = banner(&s, Some(&load("high")), Some("School Guest"), false).unwrap();
        assert_eq!(b.kind, "wifi");
        assert!(b.message.contains("School Guest"));
    }

    #[test]
    fn busy_location_when_wifi_is_fine() {
        let s = connected(Quality { gateway_loss: Some(0.0), gateway_rtt_ms: Some(4.0), gateway_jitter_ms: Some(2.0), ..Default::default() });
        assert_eq!(banner(&s, Some(&load("high")), None, false).unwrap().kind, "load");
    }

    #[test]
    fn slow_tunnel() {
        let s = connected(Quality { tunnel_delay_ms: Some(900), ..Default::default() });
        assert_eq!(banner(&s, None, None, false).unwrap().kind, "slow");
    }

    #[test]
    fn nothing_while_disconnected() {
        let s = Status { state: TunnelState::Disconnected, quality: Quality { gateway_loss: Some(1.0), ..Default::default() }, ..Default::default() };
        assert_eq!(banner(&s, None, None, false), None);
    }

    #[test]
    fn saved_account_keeps_the_connection_details() {
        let account: Account = serde_json::from_str(
            r#"{"plan":{"id":"free","name":"Free","mbps":25},"usage":{"month":"2026-09","bytes":5},
                "locations":[{"id":"main","name":"France","country":"FR","online":true,"load":null,
                  "connect":{"host":"147.135.128.62","port":443,"uuid":"24c7a93b-bbc7-4f9f-bccb-2bfe437c2fbb",
                    "flow":"xtls-rprx-vision","sni":"www.google.com","publicKey":"k","shortId":"ab","fingerprint":"chrome"}}]}"#,
        )
        .unwrap();
        // The window never gets the details...
        assert!(serde_json::to_value(&account).unwrap()["locations"][0].get("connect").is_none());
        // ...but the copy on disk has them, and reads back as an account.
        let back: Account = serde_json::from_value(account_for_disk(&account)).unwrap();
        assert_eq!(back.locations[0].connect, account.locations[0].connect);
        assert!(back.locations[0].connect.is_some());
    }

    #[test]
    fn busy_line_is_not_a_bad_wifi() {
        let s = connected(Quality { gateway_loss: Some(0.1), gateway_rtt_ms: Some(120.0), gateway_jitter_ms: Some(60.0), tunnel_delay_ms: Some(900), ..Default::default() });
        assert_eq!(banner(&s, None, Some("Home"), true), None);
        assert_eq!(banner(&s, Some(&load("high")), Some("Home"), true).unwrap().kind, "load");
    }

    #[test]
    fn busy_lasts_until_the_ping_window_is_clean() {
        let mut t = Traffic::default();
        let start = Instant::now();
        assert!(!t.busy(start, 0));
        // A speed test: 50 MB in 2 seconds.
        assert!(t.busy(start + Duration::from_secs(2), 50_000_000));
        // Quiet again, but pings from the busy moment are still in the window.
        assert!(t.busy(start + Duration::from_secs(20), 50_000_100));
        assert!(!t.busy(start + Duration::from_secs(40), 50_000_200));
        // Light browsing doesn't count as busy.
        assert!(!t.busy(start + Duration::from_secs(42), 50_300_000));
    }
}
