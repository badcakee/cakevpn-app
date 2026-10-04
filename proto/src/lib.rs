//! Messages between the CakeVPN app and `cakevpn-helper`, the background
//! service that runs the tunnel with admin rights.
//!
//! Every message is one line of JSON. The app sends a [`Request`] and the
//! helper answers with a [`Response`].

use serde::{Deserialize, Serialize};

/// Bumped when the app and helper stop understanding each other.
pub const PROTOCOL_VERSION: u32 = 1;

/// Bumped whenever the helper's behavior changes, so the app can ask for the
/// helper to be reinstalled on macOS. Windows reinstalls it with every update.
///
/// 2: QUIC (UDP 443) is refused in the tunnel.
/// 3: IPv6 is let through for users with their own IPv6 exit.
/// 4: ad blocking, the kill switch and split tunneling.
/// 5: checks the internet outside the tunnel too, measures afresh after the
///    computer wakes up, and reports traffic as fresh as the app asks.
/// 6: a much bigger ad and tracker list, and pings to the locations while
///    the tunnel is up.
/// 7: on a Mac, name lookups go into the tunnel even when the network's own
///    DNS server is on the local network.
/// 8: sing-box starts over after the computer wakes up, and whenever nothing
///    gets through the tunnel any more (sites said "no internet" after
///    opening the lid).
/// 9: on Windows, installs signed CakeVPN updates without asking (InstallUpdate).
/// 10: skipped websites skip the VPN in games and other programs too, not
///     only in browsers (found by the name looked up and by address).
pub const HELPER_REVISION: u32 = 10;

/// The oldest helper the app still works with. Revision 9 only adds
/// silent updates on Windows, where the installer updates the helper anyway,
/// so a Mac doesn't have to run "Set up" again for it. Neither for 10: a Mac
/// with skipped websites is offered "Set up" in those settings instead.
pub const MIN_HELPER_REVISION: u32 = 8;

/// Most locations one ping request can name.
pub const MAX_PING_TARGETS: usize = 32;

/// Most websites and apps one person can set to skip the VPN.
pub const MAX_BYPASS_DOMAINS: usize = 100;
pub const MAX_BYPASS_APPS: usize = 50;

pub const WINDOWS_PIPE: &str = r"\\.\pipe\cakevpn-helper";
pub const UNIX_SOCKET: &str = "/var/run/cakevpn-helper.sock";
pub const WINDOWS_SERVICE: &str = "CakeVPNHelper";
pub const MACOS_LABEL: &str = "com.cakevpn.helper";

/// Where to connect. It comes from the panel's `/account` answer.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    pub uuid: String,
    pub flow: String,
    pub sni: String,
    pub public_key: String,
    pub short_id: String,
    pub fingerprint: String,
    /// The user has their own IPv6 exit here, so IPv6 may go through the tunnel.
    #[serde(default)]
    pub ipv6: bool,
    /// Blocks ads and trackers by name, for every app on the computer.
    #[serde(default)]
    pub block_ads: bool,
    /// While the server can't be reached, the tunnel stays up and keeps
    /// trying, so nothing leaves outside the VPN until the person disconnects.
    #[serde(default)]
    pub kill_switch: bool,
    /// Websites (domain names, which include their subdomains) that skip the VPN.
    #[serde(default)]
    pub bypass_domains: Vec<String>,
    /// Apps (process names, like "steam.exe") that skip the VPN.
    #[serde(default)]
    pub bypass_apps: Vec<String>,
}

/// A domain name like "mybank.com": letters, digits, dashes and dots, with at least one dot.
pub fn is_domain(d: &str) -> bool {
    d.len() <= 253
        && d.contains('.')
        && d.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// A program's file name like "steam.exe" or "Discord", never a path.
pub fn is_app_name(a: &str) -> bool {
    !a.is_empty()
        && a.len() <= 100
        && a.trim() == a
        && a.chars().all(|c| c.is_alphanumeric() || " ._-+()".contains(c))
}

impl ConnectParams {
    /// Rejects anything that is not plain connection data, so a local program
    /// cannot use the helper to feed arbitrary settings to a root process.
    pub fn validate(&self) -> Result<(), String> {
        fn only(s: &str, max: usize, ok: impl Fn(char) -> bool) -> bool {
            !s.is_empty() && s.len() <= max && s.chars().all(ok)
        }
        let host_char = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == ':';
        if !only(&self.host, 253, host_char) {
            return Err("bad server address".into());
        }
        if self.port == 0 {
            return Err("bad server port".into());
        }
        let is_uuid = self.uuid.len() == 36
            && self.uuid.chars().enumerate().all(|(i, c)| match i {
                8 | 13 | 18 | 23 => c == '-',
                _ => c.is_ascii_hexdigit(),
            });
        if !is_uuid {
            return Err("bad user id".into());
        }
        if self.flow != "xtls-rprx-vision" {
            return Err("unsupported flow".into());
        }
        if !only(&self.sni, 253, |c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
            return Err("bad server name".into());
        }
        if !only(&self.public_key, 64, |c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err("bad public key".into());
        }
        if self.short_id.len() > 16 || !self.short_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("bad short id".into());
        }
        const FINGERPRINTS: [&str; 8] =
            ["chrome", "firefox", "safari", "ios", "android", "edge", "360", "qq"];
        if !FINGERPRINTS.contains(&self.fingerprint.as_str()) {
            return Err("unsupported fingerprint".into());
        }
        if self.bypass_domains.len() > MAX_BYPASS_DOMAINS || !self.bypass_domains.iter().all(|d| is_domain(d)) {
            return Err("bad website in the skip list".into());
        }
        if self.bypass_apps.len() > MAX_BYPASS_APPS || !self.bypass_apps.iter().all(|a| is_app_name(a)) {
            return Err("bad app in the skip list".into());
        }
        Ok(())
    }
}

/// A location to measure while the tunnel is up. The helper connects to
/// `host:port` outside the tunnel, which the app itself cannot do then.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PingTarget {
    pub id: String,
    pub host: String,
    pub port: u16,
}

impl PingTarget {
    /// The same kind of check as for connect details: nothing but plain values.
    pub fn validate(&self) -> Result<(), String> {
        let plain = |s: &str, max: usize, ok: &dyn Fn(char) -> bool| !s.is_empty() && s.len() <= max && s.chars().all(ok);
        if !plain(&self.id, 40, &|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err("bad location id".into());
        }
        if !plain(&self.host, 253, &|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') || self.port == 0 {
            return Err("bad location address".into());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum Request {
    Connect { params: ConnectParams },
    Disconnect,
    Status,
    /// Measures the locations outside the tunnel. Only answered while connected.
    Ping { targets: Vec<PingTarget> },
    /// Windows: checks the CakeVPN installer at `path` against the update key
    /// (`signature` as in latest.json) and runs it silently. Revision 9 and newer.
    InstallUpdate { path: String, signature: String },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TunnelState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Failed,
}

/// Network measurements over the last 30 seconds. `None` means not measured yet.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Quality {
    /// Ping to the router, which shows how good the Wi-Fi or cable is.
    pub gateway_rtt_ms: Option<f64>,
    pub gateway_jitter_ms: Option<f64>,
    /// Share of pings to the router that got no answer, 0.0 to 1.0.
    pub gateway_loss: Option<f64>,
    /// Round trip through the tunnel to a website.
    pub tunnel_delay_ms: Option<u32>,
    /// Tunnel checks in a row that failed.
    pub tunnel_failures: u32,
    /// Share of recent pings to the router that were lost or slow, 0.0 to
    /// 1.0. One bad ping in a while is normal, so this is what Wi-Fi is judged on.
    #[serde(default)]
    pub gateway_bad: Option<f64>,
    /// Round trip to the same website outside the tunnel, which shows
    /// whether the internet itself works when the tunnel doesn't.
    #[serde(default)]
    pub direct_delay_ms: Option<u32>,
    /// Checks outside the tunnel in a row that failed.
    #[serde(default)]
    pub direct_failures: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub protocol: u32,
    /// Helpers from before revisions existed send nothing, which reads as 0.
    #[serde(default)]
    pub revision: u32,
    pub version: String,
    pub state: TunnelState,
    /// Why the tunnel failed, in words a person can read.
    pub error: Option<String>,
    /// Seconds since 1970 when the tunnel came up.
    pub connected_since: Option<u64>,
    /// The server the tunnel goes to.
    pub host: Option<String>,
    pub quality: Quality,
    /// Bytes through the tunnel since it came up.
    pub up_bytes: u64,
    pub down_bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub status: Status,
    /// The answer to `Ping`: milliseconds per location id, `None` where it didn't answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pings: Option<std::collections::HashMap<String, Option<u32>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> ConnectParams {
        ConnectParams {
            host: "147.135.128.62".into(),
            port: 443,
            uuid: "24c7a93b-bbc7-4f9f-bccb-2bfe437c2fbb".into(),
            flow: "xtls-rprx-vision".into(),
            sni: "www.google.com".into(),
            public_key: "_OU5fl_xBw2CuAdTyvqgl4jDgxo0PN1DiaxEd7JQslE".into(),
            short_id: "26bd688ca4".into(),
            fingerprint: "chrome".into(),
            ipv6: false,
            block_ads: false,
            kill_switch: false,
            bypass_domains: vec![],
            bypass_apps: vec![],
        }
    }

    #[test]
    fn accepts_skip_lists() {
        let mut p = good();
        p.bypass_domains = vec!["mybank.com".into(), "play.donutsmp.net".into(), "xn--80ak6aa92e.com".into()];
        p.bypass_apps = vec!["steam.exe".into(), "Discord".into(), "Microsoft Teams (work)".into()];
        assert_eq!(p.validate(), Ok(()));
    }

    #[test]
    fn accepts_panel_params() {
        assert_eq!(good().validate(), Ok(()));
    }

    #[test]
    fn rejects_values_that_could_smuggle_settings() {
        let cases: Vec<Box<dyn Fn(&mut ConnectParams)>> = vec![
            Box::new(|p| p.host = "a\",\"x\":\"y".into()),
            Box::new(|p| p.host = String::new()),
            Box::new(|p| p.port = 0),
            Box::new(|p| p.uuid = "not-a-uuid".into()),
            Box::new(|p| p.flow = "".into()),
            Box::new(|p| p.sni = "www.google.com/evil".into()),
            Box::new(|p| p.public_key = "abc\"".into()),
            Box::new(|p| p.short_id = "xyz".into()),
            Box::new(|p| p.fingerprint = "random".into()),
            Box::new(|p| p.bypass_domains = vec!["localhost".into()]),
            Box::new(|p| p.bypass_domains = vec!["evil.com\",\"outbound\":\"x".into()]),
            Box::new(|p| p.bypass_domains = vec!["Upper.com".into()]),
            Box::new(|p| p.bypass_domains = vec!["-bad.com".into()]),
            Box::new(|p| p.bypass_domains = (0..=MAX_BYPASS_DOMAINS).map(|i| format!("site{i}.com")).collect()),
            Box::new(|p| p.bypass_apps = vec!["/usr/bin/curl".into()]),
            Box::new(|p| p.bypass_apps = vec![r"C:\evil.exe".into()]),
            Box::new(|p| p.bypass_apps = vec!["".into()]),
            Box::new(|p| p.bypass_apps = vec!["app\"".into()]),
        ];
        for change in cases {
            let mut p = good();
            change(&mut p);
            assert!(p.validate().is_err(), "{p:?} was accepted");
        }
    }

    #[test]
    fn ping_targets_are_plain_values() {
        let good = PingTarget { id: "node-1".into(), host: "194.156.89.230".into(), port: 443 };
        assert_eq!(good.validate(), Ok(()));
        for bad in [
            PingTarget { host: "a/b?x=1".into(), ..good.clone() },
            PingTarget { host: "a b".into(), ..good.clone() },
            PingTarget { host: String::new(), ..good.clone() },
            PingTarget { port: 0, ..good.clone() },
            PingTarget { id: "x\"y".into(), ..good.clone() },
        ] {
            assert!(bad.validate().is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn requests_are_tagged_json() {
        assert_eq!(serde_json::to_string(&Request::Status).unwrap(), r#"{"cmd":"status"}"#);
        let back: Request =
            serde_json::from_str(&serde_json::to_string(&Request::Connect { params: good() }).unwrap()).unwrap();
        assert!(matches!(back, Request::Connect { params } if params == good()));
    }
}
