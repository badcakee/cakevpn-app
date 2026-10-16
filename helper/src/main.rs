//! cakevpn-helper runs the CakeVPN tunnel. It needs admin rights to create
//! the network interface, so it runs as a Windows service or a macOS
//! LaunchDaemon and the app talks to it over a local channel.
//!
//! Usage:
//!   cakevpn-helper run          run in the foreground (macOS LaunchDaemon, testing)
//!   cakevpn-helper install      Windows: register the service if needed and (re)start it
//!   cakevpn-helper uninstall    Windows: stop and remove the service
//!
//! For testing without touching routes, set CAKEVPN_HELPER_LOCAL_PORT to get a
//! local SOCKS/HTTP port instead of a TUN interface.

#[cfg(windows)]
mod service_windows;

use cakevpn_helper::{dns, ipc, singbox, tunnel};

use std::path::PathBuf;
use std::sync::Arc;

pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CAKEVPN_HELPER_DIR") {
        return PathBuf::from(dir);
    }
    if cfg!(windows) {
        let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
        PathBuf::from(base).join("CakeVPN")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/CakeVPN")
    } else {
        PathBuf::from("/tmp/cakevpn-helper")
    }
}

fn channel_path() -> String {
    if cfg!(windows) {
        cakevpn_proto::WINDOWS_PIPE.to_string()
    } else {
        std::env::var("CAKEVPN_HELPER_SOCKET").unwrap_or_else(|_| cakevpn_proto::UNIX_SOCKET.to_string())
    }
}

fn sing_box_path() -> PathBuf {
    let name = if cfg!(windows) { "sing-box.exe" } else { "sing-box" };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .unwrap_or_else(|| PathBuf::from(name))
}

fn capture() -> singbox::Capture {
    match std::env::var("CAKEVPN_HELPER_LOCAL_PORT").ok().and_then(|p| p.parse().ok()) {
        Some(port) => singbox::Capture::LocalPort(port),
        None => singbox::Capture::Tun,
    }
}

/// Serves the app until `shutdown` finishes, then takes the tunnel down.
pub async fn serve(shutdown: impl std::future::Future<Output = ()>) -> std::io::Result<()> {
    // Before anything is written there: the folder holds the sign-in to the VPN server.
    #[cfg(windows)]
    if let Err(e) = cakevpn_helper::folder::lock(&data_dir()) {
        eprintln!("cakevpn-helper: cannot protect {}: {e}", data_dir().display());
    }
    // A helper that was ended while the tunnel was up left the Mac's DNS pointing into it.
    if capture() == singbox::Capture::Tun {
        dns::back_to_normal(&data_dir());
    }
    let tunnel = tunnel::Tunnel::new(tunnel::Paths {
        data_dir: data_dir(),
        sing_box: sing_box_path(),
        capture: capture(),
        attach: None,
    });
    let path = channel_path();
    let result = tokio::select! {
        r = ipc::listen(Arc::clone(&tunnel), &path) => r,
        _ = shutdown => Ok(()),
    };
    // Stopping must not hang on a tunnel that is busy; sing-box ends with the helper anyway.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), tunnel.disconnect()).await;
    // A disconnect cut short above may not have got to the Mac's DNS: put it back here.
    if capture() == singbox::Capture::Tun {
        dns::back_to_normal(&data_dir());
    }
    result
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Stack for the helper's threads. Windows gives a program 1 MB unless it
/// asks (Linux gives 8): a deep call must not end the whole helper there.
const STACK: usize = 8 * 1024 * 1024;

/// The runtime everything runs on, with big-stack threads.
pub fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(STACK).build()
}

/// Runs `work` on a thread with a big stack and waits for it; None when it
/// panicked (which helper-crash.log then explains).
pub fn on_big_stack<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    std::thread::Builder::new().stack_size(STACK).spawn(work).ok()?.join().ok()
}

/// How much of helper-crash.log is kept: the newest lines, about a day of
/// crashes even when the helper crashed every minute.
const CRASH_LOG_BYTES: usize = 64 * 1024;

/// The last `keep` bytes of a log, starting at a whole line.
fn last_lines(log: &[u8], keep: usize) -> &[u8] {
    if log.len() <= keep {
        return log;
    }
    let cut = log.len() - keep;
    if log[cut - 1] == b'\n' {
        return &log[cut..];
    }
    match log[cut..].iter().position(|b| *b == b'\n') {
        Some(end) => &log[cut + end + 1..],
        None => &[],
    }
}

/// Writes every panic to helper-crash.log in the data folder (only its
/// newest lines are kept), so the app can show what went wrong.
fn note_crashes() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let when = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let text = info.to_string().replace(['\r', '\n'], " ");
        let dir = data_dir();
        let path = dir.join("helper-crash.log");
        let _ = std::fs::create_dir_all(&dir);
        let old = std::fs::read(&path).unwrap_or_default();
        let mut log = last_lines(&old, CRASH_LOG_BYTES).to_vec();
        log.extend_from_slice(format!("{when} {} {text}\n", env!("CARGO_PKG_VERSION")).as_bytes());
        let _ = std::fs::write(&path, log);
        // The rest of the folder is for SYSTEM and administrators only, but the app reads this.
        #[cfg(windows)]
        let _ = cakevpn_helper::folder::readable_by_users(&path);
        previous(info);
    }));
}

fn run_foreground() {
    let served = on_big_stack(|| runtime().and_then(|runtime| runtime.block_on(serve(shutdown_signal()))));
    match served {
        Some(Ok(())) => {}
        Some(Err(e)) => {
            eprintln!("cakevpn-helper: {e}");
            std::process::exit(1);
        }
        None => std::process::exit(1),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn crash_log_keeps_whole_lines() {
        let log = b"1 first line\n2 second line\n3 third\n";
        assert_eq!(super::last_lines(log, 100), &log[..]);
        assert_eq!(super::last_lines(log, 20), b"3 third\n", "the cut-off second line is left out");
        assert_eq!(super::last_lines(log, 8), b"3 third\n");
        assert_eq!(super::last_lines(b"no end of line", 4), b"");
    }
}

fn main() {
    note_crashes();
    let arg = std::env::args().nth(1).unwrap_or_default();
    match arg.as_str() {
        "run" => run_foreground(),
        "--version" | "version" => println!("cakevpn-helper {}", env!("CARGO_PKG_VERSION")),
        #[cfg(windows)]
        "service" => service_windows::run_service(),
        #[cfg(windows)]
        "install" => service_windows::exit_with(service_windows::install()),
        #[cfg(windows)]
        "uninstall" => service_windows::exit_with(service_windows::uninstall()),
        _ => {
            eprintln!("usage: cakevpn-helper run{}", if cfg!(windows) { " | install | uninstall" } else { "" });
            std::process::exit(2);
        }
    }
}
