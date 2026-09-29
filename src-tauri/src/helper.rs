//! Client side of the local channel to cakevpn-helper.

use cakevpn_proto::{Request, Response};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Why the helper could not be asked.
pub const HELPER_MISSING: &str = "helper_missing";

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(stream: S, request: &Request) -> Result<Response, String> {
    let (read, mut write) = tokio::io::split(stream);
    let mut line = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    line.push(b'\n');
    write.write_all(&line).await.map_err(|e| e.to_string())?;
    let mut answer = String::new();
    BufReader::new(read).read_line(&mut answer).await.map_err(|e| e.to_string())?;
    serde_json::from_str(&answer).map_err(|e| format!("the helper sent something unexpected: {e}"))
}

#[cfg(unix)]
async fn open() -> Result<tokio::net::UnixStream, String> {
    tokio::net::UnixStream::connect(cakevpn_proto::UNIX_SOCKET)
        .await
        .map_err(|_| HELPER_MISSING.to_string())
}

#[cfg(windows)]
async fn open() -> Result<tokio::net::windows::named_pipe::NamedPipeClient, String> {
    use tokio::net::windows::named_pipe::ClientOptions;
    const ERROR_PIPE_BUSY: i32 = 231;
    for _ in 0..20 {
        match ClientOptions::new().open(cakevpn_proto::WINDOWS_PIPE) {
            Ok(client) => return Ok(client),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(_) => return Err(HELPER_MISSING.to_string()),
        }
    }
    Err(HELPER_MISSING.to_string())
}

/// Sends one request. Errors are either [`HELPER_MISSING`] or a message.
pub async fn ask(request: Request) -> Result<Response, String> {
    let work = async {
        let stream = open().await?;
        exchange(stream, &request).await
    };
    tokio::time::timeout(Duration::from_secs(20), work)
        .await
        .map_err(|_| "the helper did not answer".to_string())?
}

/// Installs the macOS LaunchDaemon. Asks for an admin password once.
#[cfg(target_os = "macos")]
pub fn install() -> Result<(), String> {
    use cakevpn_proto::MACOS_LABEL;
    let exe_dir = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("cannot find the app folder")?
        .to_path_buf();
    let helper = exe_dir.join("cakevpn-helper");
    let sing_box = exe_dir.join("sing-box");
    for f in [&helper, &sing_box] {
        if !f.exists() {
            return Err(format!("{} is missing from the app", f.display()));
        }
    }
    // Copied out of the app so the root daemon only ever runs root-owned files.
    let dir = format!("/Library/PrivilegedHelperTools/{MACOS_LABEL}");
    let plist = format!("/Library/LaunchDaemons/{MACOS_LABEL}.plist");
    let plist_body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{MACOS_LABEL}</string>
  <key>ProgramArguments</key><array><string>{dir}/cakevpn-helper</string><string>run</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardErrorPath</key><string>/Library/Logs/CakeVPN-helper.log</string>
</dict>
</plist>
"#
    );
    let quote = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    let script = [
        format!("launchctl bootout system/{MACOS_LABEL} 2>/dev/null || true"),
        format!("mkdir -p {}", quote(&dir)),
        format!("cp -f {} {}/cakevpn-helper", quote(&helper.to_string_lossy()), quote(&dir)),
        format!("cp -f {} {}/sing-box", quote(&sing_box.to_string_lossy()), quote(&dir)),
        format!("xattr -dr com.apple.quarantine {} 2>/dev/null || true", quote(&dir)),
        format!("chown -R root:wheel {0} && chmod 755 {0} {0}/cakevpn-helper {0}/sing-box", quote(&dir)),
        format!("printf %s {} > {}", quote(&plist_body), quote(&plist)),
        format!("chown root:wheel {0} && chmod 644 {0}", quote(&plist)),
        format!("launchctl bootstrap system {}", quote(&plist)),
    ]
    .join(" && ");
    let apple_script = format!(
        "do shell script \"{}\" with administrator privileges with prompt \"CakeVPN needs to set up its network helper once.\"",
        script.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(apple_script)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let why = String::from_utf8_lossy(&out.stderr);
        if why.contains("-128") {
            Err("Setup was cancelled.".into())
        } else {
            Err(format!("Setup failed: {}", why.trim()))
        }
    }
}
