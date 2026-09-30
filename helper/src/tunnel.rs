//! Starts and stops sing-box and keeps track of how the tunnel is doing.

use crate::{ping, quality::Tracker, singbox};
use cakevpn_proto::{ConnectParams, Status, TunnelState, HELPER_REVISION, PROTOCOL_VERSION};
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

const KILL_SWITCH_WAITING: &str =
    "Can't reach the VPN server. The kill switch keeps your internet blocked until it's back, or until you disconnect.";

pub struct Paths {
    /// Holds the generated config and sing-box's log.
    pub data_dir: PathBuf,
    pub sing_box: PathBuf,
    pub capture: singbox::Capture,
}

#[derive(Default)]
struct Inner {
    state: TunnelState,
    error: Option<String>,
    since: Option<u64>,
    params: Option<ConnectParams>,
    child: Option<Child>,
    api: Option<(u16, String)>,
    config_path: Option<PathBuf>,
    /// When sing-box was started again after stopping (kill switch only).
    restarts: Vec<Instant>,
    /// Goes up on every connect and disconnect, so old background checks stop.
    generation: u64,
    tracker: Tracker,
    gateway: Option<Ipv4Addr>,
    up_bytes: u64,
    down_bytes: u64,
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

    pub async fn status(&self) -> Status {
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
        stop_child(&mut inner).await;
        inner.generation += 1;
        let generation = inner.generation;
        inner.state = TunnelState::Connecting;
        inner.error = None;
        inner.since = None;
        inner.up_bytes = 0;
        inner.down_bytes = 0;
        inner.tracker.reset();
        // Look up the router before the tunnel changes the routes.
        inner.gateway = tokio::task::spawn_blocking(ping::gateway).await.ok().flatten();

        let port = free_local_port().map_err(|e| format!("no free local port: {e}"))?;
        let secret = random_hex(16);
        let dir = &self.paths.data_dir;
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let ads_path = dir.join("ads.srs");
        if params.block_ads {
            write_private(&ads_path, singbox::ADS_RULE_SET).map_err(|e| format!("cannot write the ad list: {e}"))?;
        }
        let config = singbox::config(&singbox::Settings {
            params: &params,
            capture: self.paths.capture,
            api_port: port,
            api_secret: &secret,
            ads_rule_set: Some(&ads_path),
        });
        let config_path = dir.join("config.json");
        write_private(&config_path, &serde_json::to_vec_pretty(&config).unwrap())
            .map_err(|e| format!("cannot write the tunnel settings: {e}"))?;

        let child = self.spawn(&config_path, true).inspect_err(|_| inner.state = TunnelState::Failed)?;
        inner.child = Some(child);
        inner.api = Some((port, secret));
        inner.config_path = Some(config_path);
        inner.restarts.clear();
        inner.params = Some(params);
        drop(inner);

        let me = Arc::clone(self);
        tokio::spawn(async move { me.watch(generation).await });
        Ok(())
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
        stop_child(&mut inner).await;
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
        stop_child(&mut inner).await;
        inner.state = TunnelState::Failed;
        inner.error = Some(message);
        inner.since = None;
        inner.api = None;
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
            if let Some(ms) = delay_test(port, &secret).await {
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
                if let Some(ms) = delay_test(port, &secret).await {
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
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            tick += 1;
            let Ok((port, secret)) = self.still_current(generation).await else { return };
            let gateway = self.inner.lock().await.gateway;
            let seq = (tick & 0xffff) as u16;
            // Err means pings cannot be sent here at all, which says nothing about the Wi-Fi.
            let rtt = match gateway {
                Some(ip) => tokio::task::spawn_blocking(move || ping::ping(ip, Duration::from_millis(1000), seq))
                    .await
                    .unwrap_or(Err(())),
                None => Err(()),
            };
            let delay = if tick % 5 == 0 { Some(delay_test(port, &secret).await) } else { None };
            let totals = clash_get(port, &secret, "/connections").await.and_then(|body| {
                let v: serde_json::Value = serde_json::from_str(&body).ok()?;
                Some((v["uploadTotal"].as_u64()?, v["downloadTotal"].as_u64()?))
            });

            let mut inner = self.inner.lock().await;
            if inner.generation != generation {
                return;
            }
            if let Ok(rtt) = rtt {
                inner.tracker.gateway_sample(rtt);
            }
            if let Some(result) = delay {
                inner.tracker.tunnel_sample(result);
            }
            if let Some((up, down)) = totals {
                inner.up_bytes = up;
                inner.down_bytes = down;
            }
            if tick % 30 == 0 {
                // The Wi-Fi may have changed; look the router up again.
                drop(inner);
                let found = tokio::task::spawn_blocking(ping::gateway).await.ok().flatten();
                let mut inner = self.inner.lock().await;
                if found.is_some() && inner.generation == generation {
                    inner.gateway = found;
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

/// Round trip through the tunnel to a small web page, in milliseconds.
async fn delay_test(port: u16, secret: &str) -> Option<u32> {
    let body = clash_get(port, secret, &format!("/proxies/proxy/delay?timeout=5000&url={TEST_URL}")).await?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v["delay"].as_u64().map(|d| d as u32)
}
