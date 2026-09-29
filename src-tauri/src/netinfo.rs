//! Small facts about the local network for the window: the Wi-Fi name and
//! how quickly each location answers.

use serde::Serialize;
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    /// The Wi-Fi name, when the system lets us read it.
    pub wifi_name: Option<String>,
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let mut command = Command::new(cmd);
    command.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = command.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Reads "SSID : name" from `netsh wlan show interfaces`, skipping the BSSID line.
fn parse_netsh(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "SSID").then(|| value.trim().to_string()).filter(|v| !v.is_empty())
    })
}

/// Reads "Current Wi-Fi Network: name" from `networksetup -getairportnetwork`.
fn parse_networksetup(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.split_once("Network: ").map(|(_, name)| name.trim().to_string()))
        .filter(|v| !v.is_empty())
}

pub fn network() -> Network {
    let wifi_name = if cfg!(windows) {
        run("netsh", &["wlan", "show", "interfaces"]).as_deref().and_then(parse_netsh)
    } else if cfg!(target_os = "macos") {
        let ports = run("networksetup", &["-listallhardwareports"]).unwrap_or_default();
        let device = ports
            .split("Hardware Port: ")
            .find(|block| block.starts_with("Wi-Fi") || block.starts_with("AirPort"))
            .and_then(|block| block.lines().find_map(|l| l.strip_prefix("Device: ")))
            .unwrap_or("en0")
            .trim()
            .to_string();
        run("networksetup", &["-getairportnetwork", &device]).as_deref().and_then(parse_networksetup)
    } else {
        None
    };
    Network { wifi_name }
}

/// Time to open a TCP connection to a location, in milliseconds.
pub async fn tcp_ping(host: &str, port: u16) -> Option<u32> {
    let start = Instant::now();
    let connect = tokio::net::TcpStream::connect((host, port));
    match tokio::time::timeout(Duration::from_secs(3), connect).await {
        Ok(Ok(_)) => Some(start.elapsed().as_millis() as u32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ssid_not_bssid() {
        let text = "    Name                   : Wi-Fi\n    State                  : connected\n    SSID                   : School Guest\n    BSSID                  : aa:bb:cc:dd:ee:ff\n";
        assert_eq!(parse_netsh(text).as_deref(), Some("School Guest"));
    }

    #[test]
    fn reads_mac_wifi_name() {
        assert_eq!(parse_networksetup("Current Wi-Fi Network: Home 5G\n").as_deref(), Some("Home 5G"));
        assert_eq!(parse_networksetup("You are not associated with an AirPort network.\n"), None);
    }
}
