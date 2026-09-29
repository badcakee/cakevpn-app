//! cakevpn-helper runs the CakeVPN tunnel. It needs admin rights to create
//! the network interface, so it runs as a Windows service or a macOS
//! LaunchDaemon and the app talks to it over a local channel.
//!
//! Usage:
//!   cakevpn-helper run          run in the foreground (macOS LaunchDaemon, testing)
//!   cakevpn-helper install      Windows: register and start the service
//!   cakevpn-helper uninstall    Windows: stop and remove the service
//!
//! For testing without touching routes, set CAKEVPN_HELPER_LOCAL_PORT to get a
//! local SOCKS/HTTP port instead of a TUN interface.

mod ipc;
mod ping;
mod quality;
mod singbox;
mod tunnel;
#[cfg(windows)]
mod service_windows;

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
    let tunnel = tunnel::Tunnel::new(tunnel::Paths {
        data_dir: data_dir(),
        sing_box: sing_box_path(),
        capture: capture(),
    });
    let path = channel_path();
    let result = tokio::select! {
        r = ipc::listen(Arc::clone(&tunnel), &path) => r,
        _ = shutdown => Ok(()),
    };
    tunnel.disconnect().await;
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

fn run_foreground() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = runtime.block_on(serve(shutdown_signal())) {
        eprintln!("cakevpn-helper: {e}");
        std::process::exit(1);
    }
}

fn main() {
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
