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
    /// How well the Wi-Fi reaches this computer, when the system says.
    #[serde(skip)]
    pub signal: Option<Signal>,
}

/// The Wi-Fi's strength as the system reports it, in three steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Fine,
    Weak,
    VeryWeak,
}

/// A link slower than this many Mbps is a weak one, whatever the bars say.
const SLOW_LINK_MBPS: f64 = 12.0;

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

/// Reads the signal from `netsh wlan show interfaces`: the one value written
/// as a percentage, and the link speeds, which are the lines naming Mbps.
/// The labels differ per language, so they are not relied on.
fn parse_netsh_signal(text: &str) -> Option<Signal> {
    let values = || text.lines().filter_map(|line| line.split_once(':')).map(|(key, value)| (key, value.trim()));
    let percent = values().find_map(|(_, v)| v.strip_suffix('%')?.trim().parse::<f64>().ok())?;
    let slowest = values()
        .filter(|(key, _)| key.contains("Mb"))
        .filter_map(|(_, v)| v.replace(',', ".").parse::<f64>().ok())
        .fold(f64::INFINITY, f64::min);
    // Windows' percentage runs from -100 dBm (0%) to -50 dBm (100%).
    Some(if percent <= 35.0 {
        Signal::VeryWeak
    } else if percent <= 50.0 || slowest < SLOW_LINK_MBPS {
        Signal::Weak
    } else {
        Signal::Fine
    })
}

/// Reads the signal from `system_profiler SPAirPortDataType -json`: the
/// network in use has "spairport_signal_noise": "-52 dBm / -93 dBm" and its
/// link speed in Mbps.
fn parse_system_profiler(text: &str) -> Option<Signal> {
    fn current(v: &serde_json::Value) -> Option<&serde_json::Value> {
        match v {
            serde_json::Value::Object(map) => {
                map.get("spairport_current_network_information").or_else(|| map.values().find_map(current))
            }
            serde_json::Value::Array(items) => items.iter().find_map(current),
            _ => None,
        }
    }
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let info = current(&json)?;
    let dbm: f64 = info
        .get("spairport_signal_noise")?
        .as_str()?
        .split_whitespace()
        .next()?
        .replace('\u{2212}', "-")
        .parse()
        .ok()?;
    let slow = info.get("spairport_network_rate").and_then(|r| r.as_f64()).is_some_and(|mbps| mbps < SLOW_LINK_MBPS);
    Some(if dbm <= -82.0 {
        Signal::VeryWeak
    } else if dbm <= -75.0 || slow {
        Signal::Weak
    } else {
        Signal::Fine
    })
}

/// Reads the Wi-Fi's name. With `signal` it also reads how strong the Wi-Fi
/// is, which on a Mac takes a slower system tool.
pub fn network(signal: bool) -> Network {
    if cfg!(windows) {
        let text = run("netsh", &["wlan", "show", "interfaces"]);
        Network {
            wifi_name: text.as_deref().and_then(parse_netsh),
            signal: text.as_deref().filter(|_| signal).and_then(parse_netsh_signal),
        }
    } else if cfg!(target_os = "macos") {
        let ports = run("networksetup", &["-listallhardwareports"]).unwrap_or_default();
        let device = ports
            .split("Hardware Port: ")
            .find(|block| block.starts_with("Wi-Fi") || block.starts_with("AirPort"))
            .and_then(|block| block.lines().find_map(|l| l.strip_prefix("Device: ")))
            .unwrap_or("en0")
            .trim()
            .to_string();
        Network {
            wifi_name: run("networksetup", &["-getairportnetwork", &device]).as_deref().and_then(parse_networksetup),
            signal: if signal {
                run("system_profiler", &["SPAirPortDataType", "-json"]).as_deref().and_then(parse_system_profiler)
            } else {
                None
            },
        }
    } else {
        Network::default()
    }
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
    fn reads_windows_signal_in_any_language() {
        let english = "    SSID                   : Home\n    Receive rate (Mbps)    : 866.7\n    Transmit rate (Mbps)   : 866.7\n    Signal                 : 84%\n";
        assert_eq!(parse_netsh_signal(english), Some(Signal::Fine));
        let french = "    SSID                   : Maison\n    Réception (Mbits/s)    : 72,2\n    Transmission (Mbits/s) : 72,2\n    Signal                 : 48%\n";
        assert_eq!(parse_netsh_signal(french), Some(Signal::Weak));
        let far = "    SSID                   : Home\n    Receive rate (Mbps)    : 6.5\n    Transmit rate (Mbps)   : 6.5\n    Signal                 : 21%\n";
        assert_eq!(parse_netsh_signal(far), Some(Signal::VeryWeak));
        // Good bars, but the link crawls.
        let crawling = "    Receive rate (Mbps)    : 5.5\n    Transmit rate (Mbps)   : 130\n    Signal                 : 80%\n";
        assert_eq!(parse_netsh_signal(crawling), Some(Signal::Weak));
        // Not on Wi-Fi: nothing to say.
        assert_eq!(parse_netsh_signal("There is no wireless interface on the system.\n"), None);
    }

    #[test]
    fn reads_mac_signal() {
        let report = |signal: &str, rate: u32| {
            format!(
                r#"{{"SPAirPortDataType":[{{"spairport_airport_interfaces":[{{"_name":"en0",
                    "spairport_current_network_information":{{"_name":"Home","spairport_network_rate":{rate},
                    "spairport_signal_noise":"{signal}"}},
                    "spairport_airport_other_local_wireless_networks":[{{"_name":"Other","spairport_signal_noise":"-90 dBm / -95 dBm"}}]}}]}}]}}"#
            )
        };
        assert_eq!(parse_system_profiler(&report("-52 dBm / -93 dBm", 866)), Some(Signal::Fine));
        assert_eq!(parse_system_profiler(&report("-78 dBm / -93 dBm", 144)), Some(Signal::Weak));
        assert_eq!(parse_system_profiler(&report("-86 dBm / -93 dBm", 26)), Some(Signal::VeryWeak));
        assert_eq!(parse_system_profiler(&report("-60 dBm / -93 dBm", 6)), Some(Signal::Weak));
        // Wi-Fi off or not joined: no current network.
        assert_eq!(parse_system_profiler(r#"{"SPAirPortDataType":[{"spairport_airport_interfaces":[{"_name":"en0"}]}]}"#), None);
        assert_eq!(parse_system_profiler("not json"), None);
    }

    #[test]
    fn reads_mac_wifi_name() {
        assert_eq!(parse_networksetup("Current Wi-Fi Network: Home 5G\n").as_deref(), Some("Home 5G"));
        assert_eq!(parse_networksetup("You are not associated with an AirPort network.\n"), None);
    }
}
