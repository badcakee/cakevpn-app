//! Starts and stops sing-box and keeps track of how the tunnel is doing.

use crate::{dns, ping, quality::Tracker, singbox, skip};
use cakevpn_proto::{ConnectParams, PingTarget, Request, Response, Status, TunnelState, HELPER_REVISION, MAX_PING_TARGETS, PROTOCOL_VERSION};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const TEST_URL: &str = "https%3A%2F%2Fwww.gstatic.com%2Fgenerate_204";

/// With the kill switch on, sing-box is started again when it stops, unless
/// it keeps stopping: then the tunnel can't be kept up at all.
const MAX_RESTARTS_PER_MINUTE: usize = 5;

/// After the computer wakes up, the network needs a moment to come back.
/// Nothing is measured for this long, so waking up doesn't look like a problem.
const SETTLE_AFTER_WAKE: Duration = Duration::from_secs(20);
/// A pause between two measurements this much longer than planned means the
/// computer was asleep.
const WAKE_GAP_SECS: u64 = 12;
/// After waking up, how long to wait for the network to come back before
/// sing-box starts over anyway.
const WAIT_FOR_NETWORK_AFTER_WAKE: u64 = 15;
/// Tunnel checks in a row that may fail before sing-box starts over, and
/// how long it waits before doing that again.
const RESTART_AFTER_FAILURES: u32 = 3;
const RESTART_AT_MOST_EVERY: Duration = Duration::from_secs(30);
/// Traffic totals older than this are read again when the app asks.
const FRESH_TOTALS: Duration = Duration::from_millis(900);

const KILL_SWITCH_WAITING: &str =
    "Can't reach the VPN server. The kill switch keeps your internet blocked until it's back, or until you disconnect.";

pub struct Paths {
    /// Holds the generated config and sing-box's log.
    pub data_dir: PathBuf,
    pub sing_box: PathBuf,
    pub capture: singbox::Capture,
    /// With `Capture::Device`: what connects the phone's VPN to sing-box.
    pub attach: Option<Arc<dyn Attach>>,
}

/// Connects the phone's own VPN interface (Android's VpnService) to the
/// local port sing-box listens on, and takes it away again. Both may block
/// while Android does its part, so they run on a blocking thread.
pub trait Attach: Send + Sync + 'static {
    /// Called once traffic gets through the server, before the tunnel counts as connected.
    fn attach(&self, local_port: u16, params: &ConnectParams) -> Result<(), String>;
    /// Called whenever the tunnel stops; must be fine to call when nothing is attached.
    fn detach(&self);
}

#[derive(Default)]
struct Inner {
    state: TunnelState,
    error: Option<String>,
    since: Option<u64>,
    params: Option<ConnectParams>,
    child: Option<Child>,
    api: Option<(u16, String)>,
    /// The port sing-box takes the phone's traffic on (`Capture::Device`).
    device_port: u16,
    config_path: Option<PathBuf>,
    /// When sing-box was started again after stopping (kill switch only).
    restarts: Vec<Instant>,
    /// When sing-box last started over to recover (after a sleep, or when stuck).
    started_over: Option<Instant>,
    /// Goes up on every connect and disconnect, so old background checks stop.
    generation: u64,
    tracker: Tracker,
    gateway: Option<Ipv4Addr>,
    up_bytes: u64,
    down_bytes: u64,
    totals_at: Option<Instant>,
    /// The skipped websites' addresses (see skip.rs), kept between connections.
    skip: skip::Addresses,
}

pub struct Tunnel {
    paths: Paths,
    inner: Mutex<Inner>,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("system random numbers");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn free_local_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

impl Tunnel {
    pub fn new(paths: Paths) -> Arc<Tunnel> {
        Arc::new(Tunnel { paths, inner: Mutex::new(Inner::default()) })
    }

    /// Installs a signed CakeVPN update without asking for permission (Windows).
    async fn install_update(&self, path: String, signature: String) -> Result<(), String> {
        #[cfg(windows)]
        {
            let dir = self.paths.data_dir.clone();
            tokio::task::spawn_blocking(move || crate::update::install(&dir, &path, &signature))
                .await
                .map_err(|e| e.to_string())?
                .map(|_| ())
        }
        #[cfg(not(windows))]
        {
            let _ = (path, signature);
            Err("Updates install themselves only on Windows.".into())
        }
    }

    /// Carries out one request from the app and says how the tunnel is now.
    pub async fn answer(self: &Arc<Self>, request: Request) -> Response {
        let mut pings = None;
        let result = match request {
            Request::Connect { params } => self.connect(params).await,
            Request::Disconnect => {
                self.disconnect().await;
                Ok(())
            }
            Request::Status => Ok(()),
            Request::Ping { targets } => self.ping(targets).await.map(|measured| pings = Some(measured)),
            Request::InstallUpdate { path, signature } => self.install_update(path, signature).await,
        };
        let status = self.status().await;
        match result {
            Ok(()) => Response { ok: true, error: None, status, pings },
            Err(e) => Response { ok: false, error: Some(e), status, pings: None },
        }
    }

    /// The status for the app. Traffic totals are read again when they are
    /// older than a second, so the app's speed display is as fresh as it
    /// asks, and nothing extra runs while nobody is looking.
    pub async fn status(&self) -> Status {
        let stale = {
            let inner = self.inner.lock().await;
            match (inner.state, &inner.api) {
                (TunnelState::Connected, Some(api)) if inner.totals_at.is_none_or(|at| at.elapsed() >= FRESH_TOTALS) => {
                    Some((api.clone(), inner.generation))
                }
                _ => None,
            }
        };
        if let Some(((port, secret), generation)) = stale {
            let totals = tokio::time::timeout(Duration::from_millis(700), read_totals(port, &secret)).await.ok().flatten();
            let mut inner = self.inner.lock().await;
            if let (Some((up, down)), true) = (totals, inner.generation == generation) {
                inner.up_bytes = up;
                inner.down_bytes = down;
                inner.totals_at = Some(Instant::now());
            }
        }
        let inner = self.inner.lock().await;
        Status {
            protocol: PROTOCOL_VERSION,
            revision: HELPER_REVISION,
            version: env!("CARGO_PKG_VERSION").to_string(),
            state: inner.state,
            error: inner.error.clone(),
            connected_since: inner.since,
            host: inner.params.as_ref().map(|p| p.host.clone()),
            quality: inner.tracker.quality(),
            up_bytes: inner.up_bytes,
            down_bytes: inner.down_bytes,
        }
    }

    pub async fn connect(self: &Arc<Self>, params: ConnectParams) -> Result<(), String> {
        params.validate()?;
        let mut inner = self.inner.lock().await;
        self.stop(&mut inner).await;
        inner.generation += 1;
        let generation = inner.generation;
        inner.state = TunnelState::Connecting;
        inner.error = None;
        inner.since = None;
        inner.up_bytes = 0;
        inner.down_bytes = 0;
        inner.totals_at = None;
        inner.tracker.reset();
        // Look up the router before the tunnel changes the routes.
        inner.gateway = tokio::task::spawn_blocking(ping::gateway).await.ok().flatten();

        let port = free_local_port().map_err(|e| format!("no free local port: {e}"))?;
        inner.device_port = match self.paths.capture {
            singbox::Capture::Device => free_local_port().map_err(|e| format!("no free local port: {e}"))?,
            _ => 0,
        };
        let secret = random_hex(16);
        let dir = &self.paths.data_dir;
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let ads_path = dir.join("ads.srs");
        if params.block_ads {
            write_private(&ads_path, singbox::ADS_RULE_SET).map_err(|e| format!("cannot write the ad list: {e}"))?;
        }
        // The addresses known so far; they are looked up again once sing-box runs.
        let skip_path = dir.join(skip::FILE);
        let skips = !params.bypass_domains.is_empty();
        if skips {
            inner.skip.use_list(&params.bypass_domains);
            skip::write(dir, &inner.skip.list()).map_err(|e| format!("cannot write the skip list: {e}"))?;
        }
        let config = singbox::config(&singbox::Settings {
            params: &params,
            capture: self.paths.capture,
            api_port: port,
            api_secret: &secret,
            ads_rule_set: Some(&ads_path),
            skip_rule_set: Some(&skip_path),
            device_port: inner.device_port,
        });
        let config_path = dir.join("config.json");
        write_private(&config_path, &serde_json::to_vec_pretty(&config).unwrap())
            .map_err(|e| format!("cannot write the tunnel settings: {e}"))?;

        let child = self.spawn(&config_path, true).inspect_err(|_| inner.state = TunnelState::Failed)?;
        inner.child = Some(child);
        // On a Mac, names would otherwise still be asked of the local network.
        if self.paths.capture == singbox::Capture::Tun {
            let dir = dir.clone();
            let _ = tokio::task::spawn_blocking(move || dns::into_tunnel(&dir)).await;
        }
        inner.api = Some((port, secret));
        inner.config_path = Some(config_path);
        inner.restarts.clear();
        inner.params = Some(params);
        drop(inner);

        let me = Arc::clone(self);
        tokio::spawn(async move { me.watch(generation).await });
        if skips {
            let me = Arc::clone(self);
            tokio::spawn(async move { me.look_up_skipped(generation).await });
        }
        Ok(())
    }

    /// Keeps the skipped websites' addresses current while this connection
    /// lasts. sing-box reads the file again by itself when it changes.
    async fn look_up_skipped(self: Arc<Self>, generation: u64) {
        let mut waits = skip::LOOK_AGAIN.into_iter().chain(std::iter::repeat(skip::LOOK_AGAIN[1]));
        loop {
            let domains = {
                let inner = self.inner.lock().await;
                let ended = matches!(inner.state, TunnelState::Disconnected | TunnelState::Failed);
                match &inner.params {
                    Some(p) if inner.generation == generation && !ended => p.bypass_domains.clone(),
                    _ => return,
                }
            };
            let found = skip::look_up(&domains).await;
            {
                let mut inner = self.inner.lock().await;
                if inner.generation != generation {
                    return;
                }
                if inner.skip.update(&found, Instant::now()) {
                    let list = inner.skip.list();
                    let _ = skip::write(&self.paths.data_dir, &list);
                }
            }
            tokio::time::sleep(waits.next().unwrap_or(skip::LOOK_AGAIN[1])).await;
        }
    }

    /// Measures the locations while the tunnel is up. The app can't: all it
    /// sends goes through the tunnel. The helper times a TCP connection to
    /// each one on the network the router is on, outside the tunnel, which
    /// is the same measurement the app makes while the tunnel is down.
    pub async fn ping(&self, targets: Vec<PingTarget>) -> Result<HashMap<String, Option<u32>>, String> {
        if targets.len() > MAX_PING_TARGETS {
            return Err("too many locations".into());
        }
        for target in &targets {
            target.validate()?;
        }
        let (gateway, through_tun) = {
            let inner = self.inner.lock().await;
            if inner.state != TunnelState::Connected {
                return Err("not connected".into());
            }
            (inner.gateway, self.paths.capture == singbox::Capture::Tun)
        };
        let tasks: Vec<_> = targets
            .into_iter()
            .map(|target| {
                tokio::spawn(async move {
                    let ms = ping::tcp_outside_tunnel(gateway, &target.host, target.port, through_tun).await;
                    (target.id, ms)
                })
            })
            .collect();
        let mut pings = HashMap::new();
        for task in tasks {
            if let Ok((id, ms)) = task.await {
                pings.insert(id, ms);
            }
        }
        Ok(pings)
    }

    /// Starts sing-box with a config written by `connect`. A fresh start
    /// empties the log; a restart adds to it.
    fn spawn(&self, config_path: &Path, fresh: bool) -> Result<Child, String> {
        let dir = &self.paths.data_dir;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(fresh)
            .append(!fresh)
            .open(dir.join("sing-box.log"))
            .map_err(|e| format!("cannot write the tunnel log: {e}"))?;
        let mut cmd = Command::new(&self.paths.sing_box);
        cmd.arg("run").arg("-c").arg(config_path).arg("-D").arg(dir);
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(log)).kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        cmd.spawn().map_err(|e| format!("cannot start sing-box at {}: {e}", self.paths.sing_box.display()))
    }

    pub async fn disconnect(&self) {
        let mut inner = self.inner.lock().await;
        inner.generation += 1;
        self.stop(&mut inner).await;
        inner.state = TunnelState::Disconnected;
        inner.error = None;
        inner.since = None;
        inner.params = None;
        inner.api = None;
        inner.tracker.reset();
    }

    /// Marks the tunnel failed and stops sing-box, unless a newer connect already took over.
    async fn fail(&self, generation: u64, message: String) {
        let mut inner = self.inner.lock().await;
        if inner.generation != generation {
            return;
        }
        self.stop(&mut inner).await;
        inner.state = TunnelState::Failed;
        inner.error = Some(message);
        inner.since = None;
        inner.api = None;
    }

    /// Ends sing-box and gives the computer its own DNS back (or, on a
    /// phone, takes its VPN away).
    async fn stop(&self, inner: &mut Inner) {
        if let Some(attach) = self.paths.attach.clone() {
            let _ = tokio::task::spawn_blocking(move || attach.detach()).await;
        }
        stop_child(inner).await;
        if self.paths.capture == singbox::Capture::Tun {
            let dir = self.paths.data_dir.clone();
            let _ = tokio::task::spawn_blocking(move || dns::back_to_normal(&dir)).await;
        }
    }

    fn log_tail(&self) -> String {
        let text = std::fs::read_to_string(self.paths.data_dir.join("sing-box.log")).unwrap_or_default();
        text.lines()
            .rev()
            .find(|l| l.contains("FATAL") || l.contains("ERROR"))
            .map(|l| l.split_once(']').map(|x| x.1).unwrap_or(l).trim().to_string())
            .unwrap_or_default()
    }

    /// Returns false once the connection this check belongs to is gone.
    async fn still_current(&self, generation: u64) -> Result<(u16, String), ()> {
        let mut inner = self.inner.lock().await;
        if inner.generation != generation {
            return Err(());
        }
        if let Some(child) = inner.child.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                if kill_switch_on(&inner) {
                    // Start it again right away, so traffic doesn't go around the VPN.
                    let now = Instant::now();
                    inner.restarts.retain(|at| now.duration_since(*at) < Duration::from_secs(60));
                    if inner.restarts.len() < MAX_RESTARTS_PER_MINUTE {
                        if let Some(Ok(child)) = inner.config_path.clone().map(|path| self.spawn(&path, false)) {
                            inner.child = Some(child);
                            inner.restarts.push(now);
                            return inner.api.clone().ok_or(());
                        }
                    }
                }
                drop(inner);
                let why = self.log_tail();
                let message = if why.is_empty() {
                    "The tunnel stopped unexpectedly.".to_string()
                } else {
                    format!("The tunnel stopped: {why}")
                };
                self.fail(generation, message).await;
                return Err(());
            }
        }
        inner.api.clone().ok_or(())
    }

    /// On a phone, points its VPN at sing-box. On failure the tunnel is
    /// marked failed and `false` comes back.
    async fn attach_device(&self, generation: u64) -> bool {
        let Some(attach) = self.paths.attach.clone() else { return true };
        let (port, params) = {
            let inner = self.inner.lock().await;
            if inner.generation != generation {
                return false;
            }
            (inner.device_port, inner.params.clone())
        };
        let Some(params) = params else { return false };
        let why = match tokio::task::spawn_blocking(move || attach.attach(port, &params)).await {
            Ok(Ok(())) => return true,
            Ok(Err(why)) => why,
            Err(_) => "The VPN could not be started.".to_string(),
        };
        self.fail(generation, why).await;
        false
    }

    /// Starts sing-box over with the same settings, while the tunnel stays
    /// "connected". After the computer slept, or the network changed under
    /// it, what sing-box knew (routes, the network card it sent through, its
    /// connections to the server) can be stale, and some computers then pass
    /// nothing at all: every site says there is no internet until the VPN
    /// reconnects. Starting over is that reconnect, done by itself.
    async fn start_over(&self, generation: u64) -> bool {
        let mut inner = self.inner.lock().await;
        if inner.generation != generation {
            return false;
        }
        let Some(path) = inner.config_path.clone() else { return false };
        stop_child(&mut inner).await;
        match self.spawn(&path, false) {
            Ok(child) => inner.child = Some(child),
            Err(why) => {
                drop(inner);
                self.fail(generation, why).await;
                return false;
            }
        }
        inner.started_over = Some(Instant::now());
        inner.tracker.reset();
        let api = inner.api.clone();
        drop(inner);
        if let Some((port, secret)) = api {
            for _ in 0..30 {
                if clash_get(port, &secret, "/version").await.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        }
        // A Mac's DNS goes back into the tunnel (it already points there
        // unless something changed it), and names cached before are forgotten.
        // Not when the person disconnected meanwhile: the DNS would then
        // point into a tunnel that is gone, and no site would open.
        if self.paths.capture == singbox::Capture::Tun {
            let inner = self.inner.lock().await;
            if inner.generation != generation {
                return false;
            }
            let dir = self.paths.data_dir.clone();
            let _ = tokio::task::spawn_blocking(move || dns::into_tunnel(&dir)).await;
            drop(inner);
        }
        true
    }

    async fn watch(self: Arc<Self>, generation: u64) {
        // 1. Wait for sing-box to start.
        let mut started = false;
        for _ in 0..30 {
            let Ok((port, secret)) = self.still_current(generation).await else { return };
            if clash_get(port, &secret, "/version").await.is_some() {
                started = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if !started {
            let why = self.log_tail();
            self.fail(generation, format!("The tunnel did not start. {why}").trim().to_string()).await;
            return;
        }

        // 2. Check that traffic really gets through the server.
        let mut reached = false;
        for _ in 0..3 {
            let Ok((port, secret)) = self.still_current(generation).await else { return };
            if let Some(ms) = delay_test(port, &secret, "proxy").await {
                if !self.attach_device(generation).await {
                    return;
                }
                let mut inner = self.inner.lock().await;
                if inner.generation != generation {
                    return;
                }
                inner.tracker.tunnel_sample(Some(ms));
                inner.state = TunnelState::Connected;
                inner.since = Some(now_secs());
                reached = true;
                break;
            }
        }
        if !reached {
            if !kill_switch_on(&*self.inner.lock().await) {
                self.fail(
                    generation,
                    "Could not reach the VPN server. Check your internet, or try another location.".into(),
                )
                .await;
                return;
            }
            // Kill switch: the tunnel stays up, so nothing goes around it, and keeps trying.
            self.inner.lock().await.error = Some(KILL_SWITCH_WAITING.into());
            loop {
                tokio::time::sleep(Duration::from_secs(3)).await;
                let Ok((port, secret)) = self.still_current(generation).await else { return };
                if let Some(ms) = delay_test(port, &secret, "proxy").await {
                    if !self.attach_device(generation).await {
                        return;
                    }
                    let mut inner = self.inner.lock().await;
                    if inner.generation != generation {
                        return;
                    }
                    inner.tracker.tunnel_sample(Some(ms));
                    inner.state = TunnelState::Connected;
                    inner.since = Some(now_secs());
                    inner.error = None;
                    break;
                }
            }
        }

        // 3. Measure every 2 seconds while connected.
        let mut tick: u64 = 0;
        let mut last = now_secs();
        let mut settle_until: Option<Instant> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            tick += 1;
            let Ok((port, secret)) = self.still_current(generation).await else { return };

            // The clock jumped: the computer was asleep (a closed laptop lid).
            // What was measured before says nothing about now, and the Wi-Fi
            // needs a moment to come back, so start afresh after a pause.
            let now = now_secs();
            let slept = now.saturating_sub(last) >= WAKE_GAP_SECS || now < last;
            last = now;
            if slept {
                // Once the network is back (or after a while anyway), sing-box
                // starts over on it, so sites work right away after opening the lid.
                for _ in 0..WAIT_FOR_NETWORK_AFTER_WAKE {
                    if tokio::task::spawn_blocking(ping::gateway).await.ok().flatten().is_some() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                if !self.start_over(generation).await {
                    return;
                }
                last = now_secs();
                settle_until = Some(Instant::now() + SETTLE_AFTER_WAKE);
            }
            if let Some(until) = settle_until {
                if Instant::now() < until {
                    continue;
                }
                settle_until = None;
                // The laptop may have woken up on another network.
                let found = tokio::task::spawn_blocking(ping::gateway).await.ok().flatten();
                let mut inner = self.inner.lock().await;
                if inner.generation != generation {
                    return;
                }
                if found.is_some() {
                    inner.gateway = found;
                }
                inner.tracker.reset();
            }

            let (gateway, retest) = {
                let inner = self.inner.lock().await;
                (inner.gateway, inner.tracker.tunnel_failing())
            };
            let seq = (tick & 0xffff) as u16;
            // Err means pings cannot be sent here at all, which says nothing about the Wi-Fi.
            let rtt = match gateway {
                Some(ip) => tokio::task::spawn_blocking(move || ping::ping(ip, Duration::from_millis(1000), seq))
                    .await
                    .unwrap_or(Err(())),
                None => Err(()),
            };
            // Every 10 seconds, and right away again after a failed check: the
            // same website through the tunnel and outside it. When only the
            // tunnel fails, the VPN is the problem; when both do, the internet is.
            let delay = if tick % 5 == 0 || retest {
                Some(tokio::join!(delay_test(port, &secret, "proxy"), delay_test(port, &secret, "direct")))
            } else {
                None
            };
            // Traffic totals are not read here: `status` reads them when the
            // app asks, so nothing extra runs while nobody is looking.

            let mut inner = self.inner.lock().await;
            if inner.generation != generation {
                return;
            }
            if let Ok(rtt) = rtt {
                inner.tracker.gateway_sample(rtt);
            }
            if let Some((through_tunnel, outside)) = delay {
                inner.tracker.tunnel_sample(through_tunnel);
                inner.tracker.direct_sample(outside);
            }
            // Nothing gets through any more: start over, like reconnecting by
            // hand would. If the server itself is down, that changes nothing,
            // and the app moves to another location.
            let stuck = inner.tracker.quality().tunnel_failures >= RESTART_AFTER_FAILURES
                && inner.started_over.is_none_or(|at| at.elapsed() >= RESTART_AT_MOST_EVERY);
            if stuck {
                drop(inner);
                if !self.start_over(generation).await {
                    return;
                }
                continue;
            }
            if tick % 30 == 0 {
                // The Wi-Fi may have changed; look the router up again.
                drop(inner);
                let found = tokio::task::spawn_blocking(ping::gateway).await.ok().flatten();
                let mut inner = self.inner.lock().await;
                if found.is_some() && inner.generation == generation && inner.gateway != found {
                    inner.gateway = found;
                    inner.tracker.new_network();
                }
            }
        }
    }
}

fn kill_switch_on(inner: &Inner) -> bool {
    inner.params.as_ref().is_some_and(|p| p.kill_switch)
}

async fn stop_child(inner: &mut Inner) {
    let Some(mut child) = inner.child.take() else { return };
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // Let sing-box remove its routes before it exits.
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        if tokio::time::timeout(Duration::from_secs(5), child.wait()).await.is_ok() {
            return;
        }
    }
    let _ = child.kill().await;
}

fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        std::io::Write::write_all(&mut f, data)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)
    }
}

/// A tiny HTTP/1.0 GET against sing-box's local API. HTTP/1.0 keeps the
/// answer unchunked, so no HTTP library is needed.
async fn clash_get(port: u16, secret: &str, path: &str) -> Option<String> {
    let request = async {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        let head = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {secret}\r\n\r\n");
        stream.write_all(head.as_bytes()).await.ok()?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.ok()?;
        let text = String::from_utf8_lossy(&raw);
        let (status_line, rest) = text.split_once("\r\n")?;
        if !status_line.contains(" 200 ") {
            return None;
        }
        Some(rest.split_once("\r\n\r\n")?.1.to_string())
    };
    tokio::time::timeout(Duration::from_secs(8), request).await.ok().flatten()
}

/// Bytes sent and received through sing-box since it started.
async fn read_totals(port: u16, secret: &str) -> Option<(u64, u64)> {
    let body = clash_get(port, secret, "/connections").await?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    Some((v["uploadTotal"].as_u64()?, v["downloadTotal"].as_u64()?))
}

/// Round trip to a small web page in milliseconds: through the tunnel
/// ("proxy") or outside it ("direct").
async fn delay_test(port: u16, secret: &str, outbound: &str) -> Option<u32> {
    let body = clash_get(port, secret, &format!("/proxies/{outbound}/delay?timeout=5000&url={TEST_URL}")).await?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v["delay"].as_u64().map(|d| d as u32)
}
