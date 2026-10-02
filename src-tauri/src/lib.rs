mod api;
#[cfg(desktop)]
mod desktop;
#[cfg(desktop)]
mod helper;
#[cfg(mobile)]
#[path = "helper_phone.rs"]
mod helper;
mod netinfo;
#[cfg(mobile)]
mod phone;
mod store;

// What differs between a computer and a phone, under the same names.
#[cfg(desktop)]
use desktop as platform;
#[cfg(mobile)]
use phone as platform;

use api::{Account, ApiError, Load};
use cakevpn_proto::{PingTarget, Request, Status, TunnelState, MIN_HELPER_REVISION, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_notification::NotificationExt;
#[cfg(target_os = "macos")]
use tauri::RunEvent;
use tokio::sync::Mutex;

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

/// What was last read about the Wi-Fi, and when. Reading it runs system
/// tools, so it is done seldom: the name every 30 seconds, the signal every
/// minute and only while connected.
#[derive(Default)]
struct WifiSeen {
    network: netinfo::Network,
    name_at: Option<Instant>,
    signal_at: Option<Instant>,
}

pub(crate) struct AppState {
    data_dir: PathBuf,
    account: Mutex<Option<Account>>,
    /// The location the tunnel was opened to, for the load warning.
    location: Mutex<Option<String>>,
    wifi: Mutex<WifiSeen>,
    /// How far each location last measured, for judging what "slow" is there.
    pings: Mutex<HashMap<String, u32>>,
    slow: Mutex<Slowness>,
    /// Where names are looked up, checked once per connection: the moment
    /// the connection was made, and what the check found (None while it runs).
    dns: Arc<Mutex<Option<(u64, Option<netinfo::DnsPath>)>>>,
    traffic: Mutex<Traffic>,
    /// The connection details last saved to disk, to save them only when they change.
    saved: Mutex<Option<String>>,
    /// False while the window is hidden (CakeVPN in the tray), so the page
    /// can check things less often.
    pub(crate) visible: AtomicBool,
    /// Closing the window keeps CakeVPN in the tray (true) or quits it.
    pub(crate) close_to_tray: AtomicBool,
    /// The speed test in progress: its download and upload use one ticket.
    speed: Mutex<Option<api::SpeedTicket>>,
    /// Bytes of the update downloaded so far, and its size (0 while unknown).
    pub(crate) update_done: Arc<AtomicU64>,
    pub(crate) update_total: Arc<AtomicU64>,
}

/// How far the update download is, for the progress bar.
#[derive(Serialize)]
struct UpdateProgress {
    downloaded: u64,
    /// 0 while the size isn't known yet.
    total: u64,
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
    /// Android: the VPN was turned off outside the app (the Quick Settings
    /// tile, or Android itself), so it isn't a drop to reconnect from.
    stopped_outside: bool,
}

/// What the app knows besides the helper's own checks.
struct Around<'a> {
    load: Option<&'a Load>,
    wifi: &'a netinfo::Network,
    /// The line is full (a speed test, a big download), so slow answers are expected.
    busy: bool,
    /// The checks through the VPN, and outside it, have been slow for a
    /// while (see [`Slowness`]).
    tunnel_slow: bool,
    direct_slow: bool,
    /// Names are being looked up by the local network instead of through the VPN.
    dns_outside: bool,
}

/// A check outside the VPN slower than this means the connection itself is slow.
const SLOW_DIRECT_MS: u32 = 500;
/// A check through the VPN slower than this is slow for a location nearby.
const SLOW_TUNNEL_MS: u32 = 800;
/// One slow check says little: the next is often quick again (the check
/// looks a name up first now and then, the line hiccups). Slow counts once
/// it has lasted this long, which is three checks in a row.
const SLOW_FOR: Duration = Duration::from_secs(25);

/// From how many milliseconds a check through the VPN is slow. Each check
/// makes several round trips, so a far location answers later than a near
/// one without anything being wrong.
fn slow_tunnel_from(ping_ms: Option<u32>) -> u32 {
    SLOW_TUNNEL_MS.max(ping_ms.unwrap_or(0).saturating_mul(6) + 250)
}

/// Since when a check has been slow without a quick one in between, and when
/// it was last looked at.
#[derive(Default)]
struct SlowSince {
    since: Option<Instant>,
    looked: Option<Instant>,
}

/// The app looks at the checks every 10 seconds or more often. A longer
/// break means it wasn't looking (the window was minimized, the computer
/// slept), and what it saw before the break says nothing about now.
const LOOKED_AWAY: Duration = Duration::from_secs(15);

impl SlowSince {
    /// Notes whether the check is slow now, and tells whether it has been,
    /// every time the app looked, for long enough to say so.
    fn lasting(&mut self, slow_now: bool, now: Instant) -> bool {
        if self.looked.is_some_and(|looked| now.duration_since(looked) > LOOKED_AWAY) {
            self.since = None;
        }
        self.looked = Some(now);
        if !slow_now {
            self.since = None;
            return false;
        }
        now.duration_since(*self.since.get_or_insert(now)) >= SLOW_FOR
    }
}

/// The checks through the VPN and outside it.
#[derive(Default)]
struct Slowness {
    tunnel: SlowSince,
    direct: SlowSince,
}

/// Decides which warning to show. One only appears when something is
/// actually wrong for the person: the VPN isn't getting through or has been
/// slow for a while, or the connection is slow even outside the VPN. It then
/// says plainly whose side it is on: "Your Wi-Fi is unstable" when it is the
/// person's own connection (a weak signal, pings to the router lost or slow,
/// or the same check failing or crawling outside the tunnel too), or "The
/// VPN is unstable" when it is the location. Without such trouble, it says
/// when names are looked up outside the VPN, and when the location is busy.
///
/// While the line is full slow answers are expected, so it takes more to
/// count as a problem.
fn banner(status: &Status, around: &Around) -> Option<Banner> {
    if status.state != TunnelState::Connected {
        return None;
    }
    let q = &status.quality;
    let busy = around.busy;
    let not_through = q.tunnel_failures >= if busy { 4 } else { 2 };
    let slow = !busy && around.tunnel_slow;
    let direct_down = q.direct_failures >= 2;
    let direct_slow = !busy && around.direct_slow;
    let trouble = not_through || slow || direct_down || direct_slow;

    // One lost or slow ping now and then is normal; over a quarter of them is
    // not, and neither is a router that takes 40 ms to answer on average.
    let lagging = q.gateway_bad.is_some_and(|bad| bad >= if busy { 0.5 } else { 0.27 })
        || (!busy && q.gateway_rtt_ms.is_some_and(|rtt| rtt >= 40.0));
    // Their own connection is called Wi-Fi when they are on one.
    let own_side = || {
        let wifi = &around.wifi;
        if wifi.wifi_name.is_none() && wifi.signal.is_none() {
            return Banner { kind: "internet", message: "Your internet is unstable.".into() };
        }
        let name = wifi.wifi_name.as_deref().map(|n| format!(" ({n})")).unwrap_or_default();
        Banner { kind: "wifi", message: format!("Your Wi-Fi{name} is unstable.") }
    };

    // A weak signal is named when it shows: the router answers late, or
    // websites do. On its own it may belong to a Wi-Fi this computer isn't
    // using (one on a cable, say).
    let felt = trouble
        || lagging
        || (!busy
            && (q.gateway_rtt_ms.is_some_and(|rtt| rtt >= 20.0)
                || q.gateway_bad.is_some_and(|bad| bad >= 0.13)
                || q.direct_delay_ms.is_some_and(|d| d >= 300)));
    match around.wifi.signal {
        Some(netinfo::Signal::VeryWeak) if felt => return Some(own_side()),
        Some(netinfo::Signal::Weak) if trouble || lagging => return Some(own_side()),
        _ => {}
    }

    let high_load = around.load.is_some_and(|l| l.level == "high");
    if !trouble {
        if around.dns_outside {
            return Some(Banner {
                kind: "dns",
                message: "The names of the sites you open are still looked up by the network you're on, not through the VPN, so it can see them. Disconnect and connect again; if this stays, update CakeVPN.".into(),
            });
        }
        return high_load
            .then(|| Banner { kind: "load", message: "The location you're in is experiencing high load.".into() });
    }
    // The router lags, or websites fail or crawl outside the VPN as well: not the VPN.
    if lagging || direct_down || (!not_through && direct_slow) {
        return Some(own_side());
    }
    Some(Banner { kind: "vpn", message: "The VPN is unstable.".into() })
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
        // Signed out, turned off or too old: the saved copy must not be used.
        Ok(Err(e)) if e.error == "signed_out" || e.error == "code_disabled" || e.error == "update_required" => return Err(e),
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

/// The system CakeVPN runs on, like "Windows 10.0.26100 (x86_64)".
async fn os_description() -> String {
    let arch = std::env::consts::ARCH;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let ver = tokio::task::spawn_blocking(|| {
            std::process::Command::new("cmd.exe").args(["/c", "ver"]).creation_flags(0x0800_0000).output().ok()
        })
        .await
        .ok()
        .flatten()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
        let number = ver.split("Version").nth(1).unwrap_or("").trim_matches(|c: char| !c.is_ascii_digit() && c != '.').to_string();
        return format!("Windows {number} ({arch})");
    }
    #[cfg(target_os = "macos")]
    {
        let ver = tokio::task::spawn_blocking(|| std::process::Command::new("sw_vers").arg("-productVersion").output().ok())
            .await
            .ok()
            .flatten()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        return format!("macOS {ver} ({arch})");
    }
    #[cfg(target_os = "android")]
    {
        return format!("Android {} · {} ({arch})", phone::android_version().unwrap_or_default(), phone::model().unwrap_or_default());
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "android")))]
    {
        format!("{} ({arch})", std::env::consts::OS)
    }
}

/// What a problem report says about this computer and the VPN, besides
/// what the window adds. Shown to the person before it is sent.
#[tauri::command]
async fn problem_details() -> String {
    let mut lines = vec![format!("CakeVPN {}", env!("CARGO_PKG_VERSION")), format!("System: {}", os_description().await)];
    match helper::ask(Request::Status).await {
        Ok(r) => {
            let s = r.status;
            let q = s.quality;
            lines.push(format!(
                "Service: {} (revision {}), tunnel {:?}{}",
                s.version,
                s.revision,
                s.state,
                s.error.map(|e| format!(": {e}")).unwrap_or_default()
            ));
            let ms = |v: Option<u32>| v.map(|v| format!("{v} ms")).unwrap_or_else(|| "-".into());
            lines.push(format!(
                "Checks: through the VPN {} ({} failed), outside {} ({} failed)",
                ms(q.tunnel_delay_ms),
                q.tunnel_failures,
                ms(q.direct_delay_ms),
                q.direct_failures
            ));
        }
        Err(e) => lines.push(format!("Service: can't be reached ({e})")),
    }
    #[cfg(windows)]
    {
        let problem = windows_helper_problem().await;
        if problem.kind != "unknown" || !problem.detail.is_empty() {
            lines.push(format!("Windows service: {} · {}", problem.kind, problem.detail));
        }
        let crashes = tokio::task::spawn_blocking(helper_crash_notes).await.unwrap_or_default();
        if !crashes.is_empty() {
            lines.push(format!("Crashes: {crashes}"));
        }
    }
    lines.join("\n")
}

/// Sends a problem report: what the person wrote and the details they saw.
#[tauri::command]
async fn report_problem(text: String, details: String) -> Result<(), ApiError> {
    let token = store::token().ok_or_else(not_signed_in)?;
    api::report(&token, &text, &details).await
}

/// Android: the installed apps, for choosing which skip the VPN.
#[tauri::command]
async fn list_apps(app: AppHandle) -> Result<Vec<serde_json::Value>, String> {
    #[cfg(mobile)]
    return phone::list_apps(&app).await;
    #[cfg(desktop)]
    {
        let _ = app;
        Ok(vec![])
    }
}

/// Android: what the Quick Settings tile asked for when it opened the app ("connect"), once.
#[tauri::command]
async fn take_launch_action(app: AppHandle) -> Option<String> {
    #[cfg(mobile)]
    return phone::take_launch_action(&app).await;
    #[cfg(desktop)]
    {
        let _ = app;
        None
    }
}

/// The person closed one of the panel's messages for them.
#[tauri::command]
async fn close_message(id: i64) -> Result<(), ApiError> {
    let token = store::token().ok_or_else(not_signed_in)?;
    api::close_message(&token, id).await
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
async fn connect(
    location_id: String,
    options: Option<ConnectOptions>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Status, String> {
    #[cfg(mobile)]
    phone::allow_vpn(&app).await?;
    #[cfg(desktop)]
    let _ = &app;
    // Use fresh details when the server answers: signing in elsewhere changes them.
    let account = match tokio::time::timeout(SERVER_WAIT, fetch_account(&state)).await {
        Ok(Ok(a)) => a,
        // The window updates CakeVPN when it sees this.
        Ok(Err(e)) if e.error == "update_required" => {
            return Err(format!("update_required:{}", e.version.unwrap_or_default()))
        }
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
                stopped_outside: false,
            })
        }
        Err(e) => return Err(e),
    };
    let status = response.status;
    // Only a helper change needs a new helper; plain app updates keep the installed one.
    let helper_state =
        if status.protocol != PROTOCOL_VERSION || status.revision < MIN_HELPER_REVISION { "outdated" } else { "ok" };
    let location_id = state.location.lock().await.clone();

    let window_visible = state.visible.load(Ordering::Relaxed);
    let connected = status.state == TunnelState::Connected;
    // Reading about the Wi-Fi runs system tools, so it is done seldom, and
    // not at all while the window is hidden.
    let wifi = {
        let mut seen = state.wifi.lock().await;
        let older = |at: Option<Instant>, secs| at.is_none_or(|at| at.elapsed() > Duration::from_secs(secs));
        let signal = connected && older(seen.signal_at, 60);
        if (window_visible || seen.name_at.is_none()) && (signal || older(seen.name_at, 30)) {
            let mut network = tokio::task::spawn_blocking(move || netinfo::network(signal)).await.unwrap_or_default();
            let now = Instant::now();
            if signal {
                seen.signal_at = Some(now);
            } else {
                // The signal read earlier still holds until its minute is over.
                network.signal = seen.network.signal.filter(|_| connected);
            }
            seen.name_at = Some(now);
            seen.network = network;
        }
        seen.network.clone()
    };
    let load = {
        let account = state.account.lock().await;
        account
            .as_ref()
            .and_then(|a| a.locations.iter().find(|l| Some(&l.id) == location_id.as_ref()))
            .and_then(|l| l.load.clone())
    };
    let busy = connected && state.traffic.lock().await.busy(Instant::now(), status.up_bytes + status.down_bytes);
    let ping_ms = match &location_id {
        Some(id) => state.pings.lock().await.get(id).copied(),
        None => None,
    };
    let (tunnel_slow, direct_slow) = {
        let mut slow = state.slow.lock().await;
        let q = &status.quality;
        let now = Instant::now();
        let measuring = connected && !busy;
        (
            slow.tunnel.lasting(measuring && q.tunnel_delay_ms.is_some_and(|d| d >= slow_tunnel_from(ping_ms)), now),
            slow.direct.lasting(measuring && q.direct_delay_ms.is_some_and(|d| d >= SLOW_DIRECT_MS), now),
        )
    };
    let dns_outside = dns_outside(&state, &status).await;
    let banner = banner(&status, &Around { load: load.as_ref(), wifi: &wifi, busy, tunnel_slow, direct_slow, dns_outside });
    #[cfg(mobile)]
    let stopped_outside = phone::stopped_outside();
    #[cfg(desktop)]
    let stopped_outside = false;
    Ok(Overview { helper: helper_state, status: Some(status), banner, location_id, window_visible, stopped_outside })
}

/// Once per connection, a little after it is made, checks in the background
/// where this computer's name lookups go. True once a check found them
/// outside the VPN.
async fn dns_outside(state: &AppState, status: &Status) -> bool {
    // On a phone CakeVPN itself is left out of its VPN (so sing-box can reach
    // the server), so its own lookups always go outside: the check can't tell.
    if cfg!(mobile) {
        return false;
    }
    let (TunnelState::Connected, Some(since)) = (status.state, status.connected_since) else {
        return false;
    };
    let mut dns = state.dns.lock().await;
    match *dns {
        Some((checked, found)) if checked == since => found == Some(netinfo::DnsPath::Outside),
        _ => {
            let connected_for = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0).saturating_sub(since);
            if connected_for >= 8 {
                *dns = Some((since, None));
                let shared = Arc::clone(&state.dns);
                tokio::spawn(async move {
                    let found = netinfo::dns_path().await;
                    let mut dns = shared.lock().await;
                    if matches!(*dns, Some((checked, None)) if checked == since) {
                        *dns = Some((since, Some(found)));
                    }
                });
            }
            false
        }
    }
}

/// Where this computer's name lookups go right now: "vpn", "outside" or
/// "unknown". Settings shows it while the VPN is connected.
#[tauri::command]
async fn dns_check() -> &'static str {
    match netinfo::dns_path().await {
        netinfo::DnsPath::Vpn => "vpn",
        netinfo::DnsPath::Outside => "outside",
        netinfo::DnsPath::Unknown => "unknown",
    }
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
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(helper::install).await.map_err(|e| e.to_string())??;
        for _ in 0..40 {
            if helper::ask(Request::Status).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err("The CakeVPN service still isn't running. Restart your PC and install CakeVPN again.".into())
    }
    #[cfg(target_os = "android")]
    {
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", windows, target_os = "android")))]
    {
        Err("The CakeVPN helper is not running.".into())
    }
}

/// Why the app can't use the helper, for the fix screen on Windows: the
/// same "isn't running" covers very different problems.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct HelperProblem {
    /// "file_missing", "not_installed", "stopped", "starting", "elsewhere",
    /// "outdated", "not_answering", "crashed" or "unknown".
    kind: &'static str,
    /// What Windows says, for when someone asks for help.
    detail: String,
}

/// Runs `sc.exe` and returns its exit code and output. No admin rights needed.
#[cfg(windows)]
fn sc(args: &[&str]) -> Option<(i32, String)> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("sc.exe").args(args).creation_flags(0x0800_0000).output().ok()?;
    Some((out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// The value after "NAME :" in sc.exe's output (the names are not translated).
#[cfg_attr(not(windows), allow(dead_code))]
fn sc_field(text: &str, name: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| l.starts_with(name) && l[name.len()..].trim_start().starts_with(':'))
        .map(|l| l.split_once(':').map(|x| x.1).unwrap_or("").trim().to_string())
}

/// What Windows and the helper noted about the helper stopping in the last
/// day: its own crash note (helper-crash.log), Windows' "Application Error"
/// (with the exception code) and the service manager's "terminated
/// unexpectedly". Empty when there is nothing.
#[cfg(windows)]
fn helper_crash_notes() -> String {
    use std::os::windows::process::CommandExt;
    let mut notes = vec![];
    let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    let log = std::path::Path::new(&base).join("CakeVPN").join("helper-crash.log");
    let recent = std::fs::metadata(&log)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().is_ok_and(|age| age.as_secs() < 86_400));
    if recent {
        if let Some(last) = std::fs::read_to_string(&log).ok().and_then(|t| t.lines().last().map(str::to_string)) {
            notes.push(last.chars().take(300).collect::<String>());
        }
    }
    let wevtutil = |log: &str, query: &str| -> String {
        std::process::Command::new("wevtutil.exe")
            .args(["qe", log, &format!("/q:{query}"), "/c:20", "/rd:true", "/f:text"])
            .creation_flags(0x0800_0000)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let day = "TimeCreated[timediff(@SystemTime) <= 86400000]";
    let errors = wevtutil("Application", &format!("*[System[Provider[@Name='Application Error'] and {day}]]"));
    let crashes: Vec<&str> = errors.split("Event[").filter(|e| e.to_lowercase().contains("cakevpn-helper")).collect();
    if let Some(first) = crashes.first() {
        let code = first
            .split(|c: char| !c.is_ascii_alphanumeric())
            .find(|w| w.len() == 10 && w.to_lowercase().starts_with("0xc"))
            .unwrap_or("?");
        notes.push(format!("Windows: helper crashed {} time(s) today, code {code}", crashes.len()));
    }
    let stops = wevtutil("System", &format!("*[System[Provider[@Name='Service Control Manager'] and (EventID=7031 or EventID=7034) and {day}]]"));
    let ended = stops.split("Event[").filter(|e| e.contains("CakeVPN")).count();
    if ended > 0 {
        notes.push(format!("service ended unexpectedly {ended} time(s) today"));
    }
    notes.join(" · ")
}

#[tauri::command]
async fn helper_problem() -> HelperProblem {
    #[cfg(windows)]
    {
        let crashes = tokio::task::spawn_blocking(helper_crash_notes).await.unwrap_or_default();
        let mut problem = windows_helper_problem().await;
        if !crashes.is_empty() {
            if matches!(problem.kind, "stopped" | "starting" | "not_answering" | "unknown") {
                problem.kind = "crashed";
            }
            problem.detail = if problem.detail.is_empty() { crashes } else { format!("{} · {crashes}", problem.detail) };
        }
        problem
    }
    #[cfg(not(windows))]
    {
        HelperProblem { kind: "unknown", ..Default::default() }
    }
}

#[cfg(windows)]
async fn windows_helper_problem() -> HelperProblem {
    {
        let dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf()));
        for file in ["cakevpn-helper.exe", "sing-box.exe"] {
            if dir.as_ref().is_some_and(|d| !d.join(file).exists()) {
                return HelperProblem { kind: "file_missing", detail: file.to_string() };
            }
        }
        let service = cakevpn_proto::WINDOWS_SERVICE;
        let Some((code, query)) = tokio::task::spawn_blocking(move || sc(&["query", service])).await.ok().flatten() else {
            return HelperProblem { kind: "unknown", detail: "sc.exe did not run".into() };
        };
        if code == 1060 {
            return HelperProblem { kind: "not_installed", detail: "the service does not exist".into() };
        }
        let state = sc_field(&query, "STATE").unwrap_or_default();
        let exit = sc_field(&query, "WIN32_EXIT_CODE").unwrap_or_default();
        let detail = format!("STATE {state} · exit {exit}");
        let path = tokio::task::spawn_blocking(move || sc(&["qc", service])).await.ok().flatten()
            .and_then(|(_, qc)| sc_field(&qc, "BINARY_PATH_NAME"));
        if let (Some(path), Some(dir)) = (&path, &dir) {
            let here = dir.join("cakevpn-helper.exe").to_string_lossy().to_lowercase();
            if !path.to_lowercase().contains(&here) {
                return HelperProblem { kind: "elsewhere", detail: path.clone() };
            }
        }
        let number = state.split_whitespace().next().unwrap_or("");
        let kind = match number {
            "1" | "3" => "stopped",
            "2" => "starting",
            "4" => match helper::ask(Request::Status).await {
                Ok(r) if r.status.protocol != PROTOCOL_VERSION || r.status.revision < MIN_HELPER_REVISION => "outdated",
                Ok(_) => "unknown",
                Err(_) => "not_answering",
            },
            _ => "unknown",
        };
        HelperProblem { kind, detail }
    }
}

/// Measures how far each location is, in milliseconds. While the tunnel is
/// down the app connects to them itself. While it is up, everything the app
/// sends goes through the tunnel, so the helper measures outside it instead.
#[tauri::command]
async fn ping_locations(state: State<'_, AppState>) -> Result<HashMap<String, Option<u32>>, String> {
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
    if let Ok(r) = helper::ask(Request::Status).await {
        match r.status.state {
            TunnelState::Connected => {
                let targets = targets.into_iter().map(|(id, host, port)| PingTarget { id, host, port }).collect();
                let pings = helper::ask(Request::Ping { targets }).await?.pings.unwrap_or_default();
                remember_pings(&state, &pings).await;
                return Ok(pings);
            }
            // Routes are changing; a measurement now would mean nothing.
            TunnelState::Connecting => return Ok(HashMap::new()),
            _ => {}
        }
    }
    let mut pings = HashMap::new();
    for (id, host, port) in targets {
        pings.insert(id, netinfo::tcp_ping(&host, port).await);
    }
    remember_pings(&state, &pings).await;
    Ok(pings)
}

/// Keeps the newest answer per location; one that didn't answer keeps its last.
async fn remember_pings(state: &AppState, pings: &HashMap<String, Option<u32>>) {
    let mut known = state.pings.lock().await;
    for (id, ms) in pings {
        if let Some(ms) = ms {
            known.insert(id.clone(), *ms);
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsInfo {
    version: &'static str,
    autostart: bool,
}

#[tauri::command]
fn settings_info(app: tauri::AppHandle) -> SettingsInfo {
    SettingsInfo { version: env!("CARGO_PKG_VERSION"), autostart: platform::autostart_enabled(&app) }
}

/// Starts CakeVPN when the computer starts, hidden in the tray.
#[tauri::command]
fn set_autostart(enabled: bool, app: tauri::AppHandle) -> Result<bool, String> {
    platform::set_autostart(&app, enabled)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateInfo {
    pub(crate) version: String,
    pub(crate) notes: Option<String>,
}

/// Looks for a newer CakeVPN release on GitHub.
#[tauri::command]
async fn check_update(app: tauri::AppHandle) -> Result<Option<UpdateInfo>, String> {
    platform::check_update(&app).await
}

/// Installs the newer release: on a computer by itself, on a phone by
/// opening the download for Android's installer.
#[tauri::command]
async fn install_update(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    platform::install_update(&app, &state).await
}

#[tauri::command]
fn update_progress(state: State<'_, AppState>) -> UpdateProgress {
    UpdateProgress {
        downloaded: state.update_done.load(Ordering::Relaxed),
        total: state.update_total.load(Ordering::Relaxed),
    }
}

/// Whether this computer is on a Wi-Fi, and which, for connecting by itself on untrusted ones.
#[tauri::command]
async fn current_network() -> netinfo::CurrentNetwork {
    tokio::task::spawn_blocking(netinfo::current_network).await.unwrap_or_default()
}

/// A desktop notification.
#[tauri::command]
fn notify(app: AppHandle, title: String, body: String) {
    let _ = app.notification().builder().title(title).body(body).show();
}

/// Sets the keyboard shortcut that turns the VPN on and off ("Control+Alt+Shift+V"), or none.
#[tauri::command]
fn set_shortcut(app: AppHandle, accelerator: Option<String>) -> Result<(), String> {
    platform::set_shortcut(&app, accelerator)
}

/// What closing the window does: keep CakeVPN in the tray, or quit it.
#[tauri::command]
fn set_close_to_tray(state: State<'_, AppState>, enabled: bool) {
    state.close_to_tray.store(enabled, Ordering::Relaxed);
}

/// The tray menu, as the window describes it (in its language) whenever it changes.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(mobile, allow(dead_code))]
pub(crate) struct TrayModel {
    pub(crate) status: String,
    pub(crate) toggle: String,
    pub(crate) locations_label: String,
    pub(crate) locations: Vec<TrayLocation>,
    pub(crate) open: String,
    pub(crate) quit: String,
    pub(crate) tooltip: String,
}

#[derive(Deserialize)]
#[cfg_attr(mobile, allow(dead_code))]
pub(crate) struct TrayLocation {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) chosen: bool,
    pub(crate) enabled: bool,
}

#[tauri::command]
fn set_tray(app: AppHandle, model: TrayModel) -> Result<(), String> {
    platform::set_tray(&app, model)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_notification::init());
    let builder = platform::plugins(builder);
    let app = builder
        .setup(|app| {
            let shown = platform::starts_shown();
            let data_dir = app.path().app_data_dir()?;
            store::use_dir(&data_dir);
            app.manage(AppState {
                data_dir,
                account: Mutex::new(None),
                location: Mutex::new(None),
                wifi: Mutex::new(WifiSeen::default()),
                pings: Mutex::new(HashMap::new()),
                slow: Mutex::new(Slowness::default()),
                dns: Arc::new(Mutex::new(None)),
                traffic: Mutex::new(Traffic::default()),
                saved: Mutex::new(None),
                visible: AtomicBool::new(shown),
                close_to_tray: AtomicBool::new(true),
                speed: Mutex::new(None),
                update_done: Arc::new(AtomicU64::new(0)),
                update_total: Arc::new(AtomicU64::new(0)),
            });
            platform::setup(app, shown)?;
            Ok(())
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
            helper_problem,
            ping_locations,
            settings_info,
            set_autostart,
            create_invite,
            delete_invite,
            close_message,
            problem_details,
            report_problem,
            list_apps,
            take_launch_action,
            usage_history,
            dns_check,
            current_network,
            notify,
            set_shortcut,
            set_close_to_tray,
            set_tray,
            speed_test_download,
            speed_test_upload,
            check_update,
            install_update,
            update_progress
        ])
        .build(tauri::generate_context!())
        .expect("error while starting CakeVPN");

    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = event {
            desktop::show_window(app);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (app, event);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sc_exe() {
        let query = "\r\nSERVICE_NAME: CakeVPNHelper \r\n        TYPE               : 10  WIN32_OWN_PROCESS  \r\n        STATE              : 1  STOPPED \r\n        WIN32_EXIT_CODE    : 1  (0x1)\r\n        SERVICE_EXIT_CODE  : 0  (0x0)\r\n";
        assert_eq!(sc_field(query, "STATE").as_deref(), Some("1  STOPPED"));
        assert_eq!(sc_field(query, "WIN32_EXIT_CODE").as_deref(), Some("1  (0x1)"));
        let qc = "        BINARY_PATH_NAME   : \"C:\\Program Files\\CakeVPN\\cakevpn-helper.exe\" service\r\n";
        assert_eq!(sc_field(qc, "BINARY_PATH_NAME").as_deref(), Some("\"C:\\Program Files\\CakeVPN\\cakevpn-helper.exe\" service"));
        assert_eq!(sc_field(query, "SERVICE_NAME").as_deref(), Some("CakeVPNHelper"));
    }
    use cakevpn_proto::Quality;

    fn connected(q: Quality) -> Status {
        Status { state: TunnelState::Connected, quality: q, ..Default::default() }
    }

    /// What the app would pass for these measurements once slowness has lasted.
    fn around<'a>(status: &Status, load: Option<&'a Load>, wifi: &'a netinfo::Network, busy: bool, ping_ms: Option<u32>) -> Around<'a> {
        let q = &status.quality;
        Around {
            load,
            wifi,
            busy,
            tunnel_slow: q.tunnel_delay_ms.is_some_and(|d| d >= slow_tunnel_from(ping_ms)),
            direct_slow: q.direct_delay_ms.is_some_and(|d| d >= SLOW_DIRECT_MS),
            dns_outside: false,
        }
    }

    /// The banner for someone near the location, whose Wi-Fi signal is unknown.
    fn seen(status: &Status, load: Option<&Load>, wifi_name: Option<&str>, busy: bool) -> Option<Banner> {
        let wifi = netinfo::Network { wifi_name: wifi_name.map(str::to_string), signal: None };
        banner(status, &around(status, load, &wifi, busy, Some(20)))
    }

    fn load(level: &str) -> Load {
        Load { percent: 90, level: level.into() }
    }

    /// Pings to the router: `bad` is the share lost or slow.
    fn wifi(bad: f64) -> Quality {
        Quality { gateway_rtt_ms: Some(4.0), gateway_jitter_ms: Some(2.0), gateway_loss: Some(0.0), gateway_bad: Some(bad), ..Default::default() }
    }

    #[test]
    fn quiet_when_all_is_well() {
        let s = connected(Quality { tunnel_delay_ms: Some(40), direct_delay_ms: Some(20), ..wifi(0.0) });
        assert_eq!(seen(&s, Some(&load("low")), Some("Home"), false), None);
    }

    #[test]
    fn shaky_pings_alone_are_not_a_warning() {
        // The router drops half its pings, but the VPN works: nothing is wrong for the person.
        let s = connected(Quality { tunnel_delay_ms: Some(60), ..wifi(0.5) });
        assert_eq!(seen(&s, Some(&load("low")), Some("Home"), false), None);
    }

    #[test]
    fn one_bad_ping_does_not_blame_the_wifi() {
        let s = connected(Quality { tunnel_failures: 2, direct_delay_ms: Some(30), ..wifi(1.0 / 15.0) });
        assert_eq!(seen(&s, None, Some("Home"), false).unwrap().kind, "vpn");
    }

    #[test]
    fn bad_wifi_is_named_when_the_vpn_suffers() {
        let s = connected(Quality { tunnel_failures: 2, direct_failures: 2, ..wifi(0.4) });
        let b = seen(&s, Some(&load("high")), Some("School Guest"), false).unwrap();
        assert_eq!(b.kind, "wifi");
        assert_eq!(b.message, "Your Wi-Fi (School Guest) is unstable.");
    }

    #[test]
    fn internet_down_is_not_the_vpns_fault() {
        let s = connected(Quality { tunnel_failures: 3, direct_failures: 3, ..wifi(0.0) });
        // On a Wi-Fi, their own connection is called the Wi-Fi...
        let b = seen(&s, None, Some("Home"), false).unwrap();
        assert_eq!((b.kind, b.message.as_str()), ("wifi", "Your Wi-Fi (Home) is unstable."));
        // ...and on a cable, the internet.
        let b = seen(&s, None, None, false).unwrap();
        assert_eq!((b.kind, b.message.as_str()), ("internet", "Your internet is unstable."));
    }

    #[test]
    fn vpn_problem_is_called_a_vpn_problem() {
        // The Wi-Fi is fine and the internet works outside the tunnel.
        let s = connected(Quality { tunnel_failures: 2, direct_delay_ms: Some(25), ..wifi(0.0) });
        let b = seen(&s, None, Some("Home"), false).unwrap();
        assert_eq!(b.kind, "vpn");
        assert_eq!(b.message, "The VPN is unstable.");
        let slow = connected(Quality { tunnel_delay_ms: Some(900), direct_delay_ms: Some(25), ..wifi(0.0) });
        let b = seen(&slow, None, Some("Home"), false).unwrap();
        assert_eq!(b.kind, "vpn");
        assert_eq!(b.message, "The VPN is unstable.");
    }

    #[test]
    fn slow_everywhere_is_the_internet() {
        let s = connected(Quality { tunnel_delay_ms: Some(900), direct_delay_ms: Some(800), ..wifi(0.0) });
        assert_eq!(seen(&s, None, None, false).unwrap().kind, "internet");
    }

    #[test]
    fn busy_location_with_and_without_trouble() {
        let fine = connected(Quality { tunnel_delay_ms: Some(50), ..wifi(0.0) });
        assert_eq!(seen(&fine, Some(&load("high")), None, false).unwrap().kind, "load");
        let slow = connected(Quality { tunnel_delay_ms: Some(900), direct_delay_ms: Some(30), ..wifi(0.0) });
        let b = seen(&slow, Some(&load("high")), None, false).unwrap();
        assert_eq!((b.kind, b.message.as_str()), ("vpn", "The VPN is unstable."));
    }

    #[test]
    fn slow_outside_the_vpn_is_said_even_when_the_vpn_check_passes() {
        // The check through the VPN stays under its limit, but without the VPN websites crawl too.
        let s = connected(Quality { tunnel_delay_ms: Some(550), direct_delay_ms: Some(520), ..wifi(0.0) });
        let b = seen(&s, None, Some("Home"), false).unwrap();
        assert_eq!((b.kind, b.message.as_str()), ("wifi", "Your Wi-Fi (Home) is unstable."));
        // The router itself answers late: it is the Wi-Fi.
        let mut q = Quality { tunnel_delay_ms: Some(550), direct_delay_ms: Some(520), ..wifi(0.1) };
        q.gateway_rtt_ms = Some(65.0);
        let b = seen(&connected(q), None, Some("Home"), false).unwrap();
        assert_eq!(b.kind, "wifi");
        assert_eq!(b.message, "Your Wi-Fi (Home) is unstable.");
    }

    #[test]
    fn weak_signal_is_named_when_it_shows() {
        let say = |s: &Status, signal| {
            let net = netinfo::Network { wifi_name: Some("Home".into()), signal: Some(signal) };
            banner(s, &around(s, None, &net, false, Some(20)))
        };

        // All is quick: a weak signal is not worth a warning, and it may not even be the Wi-Fi in use.
        let fine = connected(Quality { tunnel_delay_ms: Some(60), direct_delay_ms: Some(40), ..wifi(0.0) });
        assert_eq!(say(&fine, netinfo::Signal::VeryWeak), None);

        // Very weak, and websites outside the VPN already take a while.
        let sluggish = connected(Quality { tunnel_delay_ms: Some(400), direct_delay_ms: Some(330), ..wifi(0.0) });
        let b = say(&sluggish, netinfo::Signal::VeryWeak).unwrap();
        assert_eq!(b.kind, "wifi");
        assert_eq!(b.message, "Your Wi-Fi (Home) is unstable.");

        // Only weak: said once something is really slow, and then before blaming the VPN.
        assert_eq!(say(&sluggish, netinfo::Signal::Weak), None);
        let slow = connected(Quality { tunnel_delay_ms: Some(900), direct_delay_ms: Some(200), ..wifi(0.0) });
        let b = say(&slow, netinfo::Signal::Weak).unwrap();
        assert_eq!(b.kind, "wifi");
        assert_eq!(b.message, "Your Wi-Fi (Home) is unstable.");

        // A fine signal changes nothing: that one is on the VPN's side.
        assert_eq!(say(&slow, netinfo::Signal::Fine).unwrap().kind, "vpn");
    }

    #[test]
    fn a_far_location_may_answer_later() {
        // 900 ms is slow for a location 20 ms away, and normal for one 120 ms away.
        let s = connected(Quality { tunnel_delay_ms: Some(900), direct_delay_ms: Some(40), ..wifi(0.0) });
        let net = netinfo::Network::default();
        let say = |status: &Status, ping_ms| banner(status, &around(status, None, &net, false, ping_ms));
        assert_eq!(say(&s, Some(20)).unwrap().kind, "vpn");
        assert_eq!(say(&s, Some(120)), None);
        assert_eq!(say(&s, None).unwrap().kind, "vpn");
        let very = connected(Quality { tunnel_delay_ms: Some(1200), direct_delay_ms: Some(40), ..wifi(0.0) });
        assert_eq!(say(&very, Some(120)).unwrap().kind, "vpn");
        // Under the limit for a near location: nothing to say.
        let ok = connected(Quality { tunnel_delay_ms: Some(700), direct_delay_ms: Some(40), ..wifi(0.0) });
        assert_eq!(say(&ok, Some(20)), None);
    }

    #[test]
    fn one_slow_check_is_not_a_slow_vpn() {
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);
        let mut slow = SlowSince::default();
        // Slow once, then quick again: never said.
        assert!(!slow.lasting(true, start));
        assert!(!slow.lasting(false, at(10)));
        // Slow check after slow check: said once it has lasted three checks.
        assert!(!slow.lasting(true, at(20)));
        assert!(!slow.lasting(true, at(30)));
        assert!(slow.lasting(true, at(40)) || slow.lasting(true, at(46)));
        // A quick one ends it at once.
        assert!(!slow.lasting(false, at(50)));
        assert!(!slow.lasting(true, at(60)));
    }

    #[test]
    fn a_window_that_was_minimized_starts_counting_afresh() {
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);
        let mut slow = SlowSince::default();
        // One slow check is seen, then the window is minimized and nothing is looked at for minutes.
        assert!(!slow.lasting(true, start));
        // Back again, and the check happens to be slow at that moment too:
        // that is two checks minutes apart, not a slow VPN.
        assert!(!slow.lasting(true, at(300)));
        assert!(!slow.lasting(true, at(301)));
        assert!(!slow.lasting(true, at(310)));
        // Only if it stays slow while being watched is it said.
        assert!(!slow.lasting(true, at(320)));
        assert!(slow.lasting(true, at(326)));
    }

    #[test]
    fn lookups_outside_the_vpn_are_said_when_nothing_else_is_wrong() {
        let net = netinfo::Network::default();
        let fine = connected(Quality { tunnel_delay_ms: Some(60), direct_delay_ms: Some(40), ..wifi(0.0) });
        let mut a = around(&fine, None, &net, false, Some(20));
        a.dns_outside = true;
        let b = banner(&fine, &a).unwrap();
        assert_eq!(b.kind, "dns");
        assert!(b.message.contains("not through the VPN"), "{}", b.message);
        // Something that is broken right now comes first.
        let down = connected(Quality { tunnel_failures: 3, direct_delay_ms: Some(30), ..wifi(0.0) });
        let mut a = around(&down, None, &net, false, Some(20));
        a.dns_outside = true;
        assert_eq!(banner(&down, &a).unwrap().kind, "vpn");
    }

    #[test]
    fn nothing_while_disconnected() {
        let s = Status { state: TunnelState::Disconnected, quality: Quality { tunnel_failures: 9, ..wifi(1.0) }, ..Default::default() };
        assert_eq!(seen(&s, None, None, false), None);
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
    fn busy_line_needs_more_to_count_as_trouble() {
        // A speed test: answers are slow, a couple of checks time out, pings lag.
        let s = connected(Quality { tunnel_delay_ms: Some(900), tunnel_failures: 3, ..wifi(0.4) });
        assert_eq!(seen(&s, None, Some("Home"), true), None);
        assert_eq!(seen(&s, Some(&load("high")), Some("Home"), true).unwrap().kind, "load");
        // Nothing gets through for a good while: that is a real problem, even then.
        let stuck = connected(Quality { tunnel_failures: 4, direct_delay_ms: Some(30), ..wifi(0.1) });
        assert_eq!(seen(&stuck, None, Some("Home"), true).unwrap().kind, "vpn");
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
