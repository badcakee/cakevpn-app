//! Android. The tunnel runs inside the app: Android's VpnService (the
//! Kotlin side, VpnPlugin.kt and CakeVpnService.kt in gen/android) hands the
//! app a network interface that carries the phone's traffic, tun2proxy
//! passes what arrives there to sing-box's local port, and sing-box sends it
//! through the VPN. CakeVPN itself is left out of that interface, so
//! sing-box's own connection to the server goes out over the normal network.
//!
//! Also here: what a phone does instead of a computer's tray, start at
//! login, self-installing updates and keyboard shortcut.

use crate::{AppState, TrayModel, UpdateInfo};
use cakevpn_helper::singbox::Capture;
use cakevpn_helper::tunnel::{Attach, Paths, Tunnel};
use cakevpn_proto::ConnectParams;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::plugin::{Builder as PluginBuilder, PluginHandle};
use tauri::{AppHandle, Manager, Wry};

/// The phone's VPN interface; tun2proxy uses the same.
const MTU: u16 = 1500;
const RELEASE_API: &str = "https://api.github.com/repos/badcakee/cakevpn-app/releases/latest";

static TUNNEL: OnceLock<Arc<Tunnel>> = OnceLock::new();
static MODEL: OnceLock<String> = OnceLock::new();
static ANDROID_VERSION: OnceLock<String> = OnceLock::new();
/// The VPN was turned off outside the app (the tile, or Android itself).
static STOPPED_OUTSIDE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn stopped_outside() -> bool {
    STOPPED_OUTSIDE.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn android_version() -> Option<String> {
    ANDROID_VERSION.get().cloned()
}

pub fn tunnel() -> Option<Arc<Tunnel>> {
    TUNNEL.get().cloned()
}

/// The phone's model, like "Pixel 8", for the device name the panel shows.
pub fn model() -> Option<String> {
    MODEL.get().cloned()
}

/// The Kotlin side of the app.
struct Vpn(PluginHandle<Wry>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartArgs {
    /// Package names of apps that skip the VPN.
    skip_apps: Vec<String>,
}

#[derive(Deserialize)]
struct Started {
    fd: i32,
}

#[derive(Deserialize)]
struct Allowed {
    granted: bool,
}

/// What the Kotlin side says about the phone's VPN.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VpnState {
    running: bool,
    stop_requested: bool,
}

#[derive(Deserialize)]
struct Apps {
    apps: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct Action {
    action: Option<String>,
}

pub async fn list_apps(app: &AppHandle) -> Result<Vec<serde_json::Value>, String> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<serde_json::Value>, String> {
        let vpn = app.try_state::<Vpn>().ok_or("The VPN part of CakeVPN didn't start.")?;
        vpn.0.run_mobile_plugin::<Apps>("listApps", ()).map(|a| a.apps).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

pub async fn take_launch_action(app: &AppHandle) -> Option<String> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let vpn = app.try_state::<Vpn>()?;
        vpn.0.run_mobile_plugin::<Action>("takeAction", ()).ok()?.action
    })
    .await
    .ok()
    .flatten()
}

#[derive(Serialize)]
struct OpenArgs {
    url: String,
}

pub fn plugins(builder: tauri::Builder<Wry>) -> tauri::Builder<Wry> {
    builder.plugin(
        PluginBuilder::<Wry>::new("cakevpn")
            .setup(|app, api| {
                let handle = api.register_android_plugin("com.cakevpn.app", "VpnPlugin")?;
                app.manage(Vpn(handle));
                Ok(())
            })
            .build(),
    )
}

pub fn starts_shown() -> bool {
    true
}

/// Where Android unpacked the app's own programs: the folder this library
/// was loaded from. sing-box is shipped there as libsingbox.so, the one
/// place an app may run programs from.
fn native_library_dir() -> Option<PathBuf> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    maps.lines()
        .filter_map(|line| line.split_whitespace().last())
        .find(|path| path.ends_with("/libcakevpn_lib.so"))
        .and_then(|path| PathBuf::from(path).parent().map(PathBuf::from))
}

fn system_property(name: &str) -> Option<String> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut value = [0u8; 92]; // PROP_VALUE_MAX
    let len = unsafe { libc::__system_property_get(name.as_ptr(), value.as_mut_ptr() as *mut libc::c_char) };
    (len > 0).then(|| String::from_utf8_lossy(&value[..len as usize]).trim().to_string()).filter(|v| !v.is_empty())
}

/// Android pauses the app's page whenever it isn't on screen, so the panel's
/// messages are looked for here too (every 3 minutes while CakeVPN runs,
/// which it does while the VPN is on) and shown as notifications.
fn watch_messages(app: AppHandle, dir: PathBuf) {
    use tauri_plugin_notification::{NotificationExt, PermissionState};
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(15)).await;
        // Android 13 and newer ask the person once whether notifications may show.
        let asker = app.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            if matches!(asker.notification().permission_state(), Ok(PermissionState::Prompt | PermissionState::PromptWithRationale)) {
                let _ = asker.notification().request_permission();
            }
        })
        .await;
        let file = dir.join("messages-notified");
        let mut version = 0u64;
        loop {
            if let Some(token) = crate::store::token() {
                if let Ok(account) = crate::api::account(&token).await {
                    let mut told: Vec<i64> = std::fs::read_to_string(&file)
                        .unwrap_or_default()
                        .split(',')
                        .filter_map(|n| n.trim().parse().ok())
                        .collect();
                    let mut changed = false;
                    for message in account.messages.iter().rev() {
                        if !told.contains(&message.id) {
                            if message.notify {
                                let _ = app.notification().builder().title("CakeVPN").body(&message.text).show();
                            }
                            told.push(message.id);
                            changed = true;
                        }
                    }
                    // Announcements count apart from messages (their ids are their own).
                    for a in account.announcements.iter().rev() {
                        let id = -a.id;
                        if !told.contains(&id) {
                            if a.notify {
                                let _ = app.notification().builder().title("CakeVPN").body(&a.text).show();
                            }
                            told.push(id);
                            changed = true;
                        }
                    }
                    if changed {
                        let keep = told.len().saturating_sub(50);
                        let list: Vec<String> = told[keep..].iter().map(|n| n.to_string()).collect();
                        let _ = std::fs::create_dir_all(&dir);
                        let _ = std::fs::write(&file, list.join(","));
                    }
                }
                // Then wait until the panel sends something new; it answers at once when it does.
                loop {
                    match crate::api::news(&token, version).await {
                        Ok(v) if v == version => continue,
                        Ok(v) => {
                            let first = version == 0;
                            version = v;
                            if !first {
                                break;
                            }
                        }
                        Err(_) => {
                            tokio::time::sleep(Duration::from_secs(30)).await;
                            break;
                        }
                    }
                }
            } else {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    });
}

pub fn setup(app: &tauri::App, _shown: bool) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(model) = system_property("ro.product.model") {
        let _ = MODEL.set(model);
    }
    if let Some(version) = system_property("ro.build.version.release") {
        let _ = ANDROID_VERSION.set(version);
    }
    let libs = native_library_dir().ok_or("cannot find the app's own files")?;
    let data_dir = app.path().app_data_dir()?.join("tunnel");
    let tunnel = Tunnel::new(Paths {
        data_dir,
        sing_box: libs.join("libsingbox.so"),
        capture: Capture::Device,
        attach: Some(Arc::new(PhoneAttach { app: app.handle().clone(), running: Mutex::new(None) })),
    });
    let _ = TUNNEL.set(tunnel);
    watch_messages(app.handle().clone(), app.path().app_data_dir()?);
    Ok(())
}

/// tun2proxy, while it carries the phone's traffic.
struct Running {
    stop: tun2proxy::CancellationToken,
    done: tauri::async_runtime::JoinHandle<()>,
}

struct PhoneAttach {
    app: AppHandle,
    running: Mutex<Option<Running>>,
}

impl PhoneAttach {
    fn vpn(&self) -> Result<tauri::State<'_, Vpn>, String> {
        self.app.try_state::<Vpn>().ok_or_else(|| "The VPN part of CakeVPN didn't start. Close CakeVPN and open it again.".to_string())
    }

    /// Stops tun2proxy and waits (briefly) until it has let go of the interface.
    fn stop_forwarding(&self) {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(r) = running {
            r.stop.cancel();
            let _ = tauri::async_runtime::block_on(async { tokio::time::timeout(Duration::from_secs(3), r.done).await });
        }
    }
}

impl Attach for PhoneAttach {
    fn attach(&self, local_port: u16, params: &ConnectParams) -> Result<(), String> {
        self.stop_forwarding();
        let started: Started = self
            .vpn()?
            .0
            .run_mobile_plugin("start", StartArgs { skip_apps: params.bypass_apps.clone() })
            .map_err(|e| format!("Android didn't start the VPN: {e}"))?;

        let proxy = tun2proxy::ProxyParameters::try_from(format!("socks5://127.0.0.1:{local_port}").as_str())
            .map_err(|e| e.to_string())?;
        let mut args = tun2proxy::Args::default();
        args.proxy(proxy)
            .tun_fd(Some(started.fd))
            // The Kotlin side owns the interface and closes it.
            .close_fd_on_drop(false)
            // Name lookups go to sing-box like any other traffic; it answers them itself.
            .dns(tun2proxy::ArgDns::Direct)
            .dns_addr("1.1.1.1".parse().expect("an address"))
            .setup(false)
            .verbosity(tun2proxy::LevelFilter::Off);
        // A phone's browser opens many connections at once.
        args.max_sessions = 2000;

        STOPPED_OUTSIDE.store(false, std::sync::atomic::Ordering::Relaxed);
        let stop = tun2proxy::CancellationToken::new();
        let token = stop.clone();
        let done = tauri::async_runtime::spawn(async move {
            let _ = tun2proxy::general_run_async(args, MTU, false, token.clone()).await;
            if !token.is_cancelled() {
                // It ended by itself: Android took the VPN away (another VPN
                // app started, or the person turned it off in Settings).
                stopped_from_outside();
            }
        });
        // The Quick Settings tile (or Android) may turn the VPN off while the
        // app's window is closed: looked for every second, and the tunnel is
        // then taken down the normal way, from here.
        let watch = stop.clone();
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if watch.is_cancelled() {
                    return;
                }
                let app = app.clone();
                let state = tauri::async_runtime::spawn_blocking(move || {
                    app.try_state::<Vpn>().and_then(|vpn| vpn.0.run_mobile_plugin::<VpnState>("vpnState", ()).ok())
                })
                .await
                .ok()
                .flatten();
                // Taken down by the app itself meanwhile: not from outside.
                if watch.is_cancelled() {
                    return;
                }
                if state.is_some_and(|s| s.stop_requested || !s.running) {
                    stopped_from_outside();
                    return;
                }
            }
        });
        *self.running.lock().unwrap_or_else(|e| e.into_inner()) = Some(Running { stop, done });
        Ok(())
    }

    fn detach(&self) {
        self.stop_forwarding();
        if let Ok(vpn) = self.vpn() {
            let _ = vpn.0.run_mobile_plugin::<serde_json::Value>("stop", ());
        }
    }
}

/// The VPN was turned off outside the app: the tunnel goes down, and the
/// window is told it was meant (not a drop to reconnect from).
fn stopped_from_outside() {
    STOPPED_OUTSIDE.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(tunnel) = tunnel() {
        tauri::async_runtime::spawn(async move { tunnel.disconnect().await });
    }
}

/// Android asks the person once whether CakeVPN may set up a VPN.
pub async fn allow_vpn(app: &AppHandle) -> Result<(), String> {
    let app = app.clone();
    let answer = tauri::async_runtime::spawn_blocking(move || -> Result<Allowed, String> {
        let vpn = app.try_state::<Vpn>().ok_or("The VPN part of CakeVPN didn't start.")?;
        vpn.0.run_mobile_plugin::<Allowed>("prepare", ()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    if answer.granted {
        Ok(())
    } else {
        Err("CakeVPN needs your OK to set up the VPN. Press Connect again and choose OK.".into())
    }
}

pub fn autostart_enabled(_app: &AppHandle) -> bool {
    false
}

pub fn set_autostart(_app: &AppHandle, _enabled: bool) -> Result<bool, String> {
    Err("On a phone, CakeVPN starts when you open it.".into())
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    body: Option<String>,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// "1.8.0" as numbers, for comparing versions.
fn version_parts(v: &str) -> Vec<u32> {
    v.trim_start_matches('v').split(['.', '-']).map_while(|p| p.parse().ok()).collect()
}

/// The newest release, when it is newer than this app and has an Android download.
async fn newer_release() -> Result<Option<(Release, String)>, String> {
    let release: Release = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("CakeVPN/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?
        .get(RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| "Could not check for updates. Check your internet connection.".to_string())?
        .json()
        .await
        .map_err(|e| format!("Could not check for updates: {e}"))?;
    if version_parts(&release.tag_name) <= version_parts(env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let Some(apk) = release.assets.iter().find(|a| a.name.ends_with(".apk")).map(|a| a.browser_download_url.clone()) else {
        return Ok(None);
    };
    Ok(Some((release, apk)))
}

pub async fn check_update(_app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    Ok(newer_release()
        .await?
        .map(|(r, _)| UpdateInfo { version: r.tag_name.trim_start_matches('v').to_string(), notes: r.body }))
}

/// Android installs apps itself: the download opens in the browser, and
/// tapping it installs the new version over this one.
pub async fn install_update(app: &AppHandle, _state: &AppState) -> Result<(), String> {
    let (_, apk) = newer_release().await?.ok_or("CakeVPN is already up to date.")?;
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let vpn = app.try_state::<Vpn>().ok_or("The VPN part of CakeVPN didn't start.")?;
        vpn.0.run_mobile_plugin::<serde_json::Value>("openUrl", OpenArgs { url: apk }).map(|_| ()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

pub fn set_shortcut(_app: &AppHandle, _accelerator: Option<String>) -> Result<(), String> {
    Ok(())
}

pub fn set_tray(_app: &AppHandle, _model: TrayModel) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::version_parts;

    #[test]
    fn compares_versions() {
        assert!(version_parts("v1.10.0") > version_parts("1.9.3"));
        assert!(version_parts("v1.8.0") == version_parts("1.8.0"));
    }
}
