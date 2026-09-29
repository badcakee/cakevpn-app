//! Messages between the CakeVPN app and `cakevpn-helper`, the background
//! service that runs the tunnel with admin rights.
//!
//! Every message is one line of JSON. The app sends a [`Request`] and the
//! helper answers with a [`Response`].

use serde::{Deserialize, Serialize};

/// Bumped when the app and helper stop understanding each other.
pub const PROTOCOL_VERSION: u32 = 1;

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
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum Request {
    Connect { params: ConnectParams },
    Disconnect,
    Status,
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
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub protocol: u32,
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
        }
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
        ];
        for change in cases {
            let mut p = good();
            change(&mut p);
            assert!(p.validate().is_err(), "{p:?} was accepted");
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
