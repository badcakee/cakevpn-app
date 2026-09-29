//! Builds the sing-box configuration for one connection.

use cakevpn_proto::ConnectParams;
use serde_json::{json, Value};

/// How the tunnel takes over traffic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Capture {
    /// A TUN interface that carries all traffic of the computer.
    Tun,
    /// Only a local SOCKS/HTTP port. Used to test the helper without touching routes.
    LocalPort(u16),
}

pub struct Settings<'a> {
    pub params: &'a ConnectParams,
    pub capture: Capture,
    pub api_port: u16,
    pub api_secret: &'a str,
}

/// The tunnel sends all TCP and UDP through the VPN, like a normal network.
/// DNS goes through the tunnel as well, so blocked sites resolve correctly.
/// IPv6 is refused inside the tunnel, which makes apps use IPv4 right away:
/// some IPv6 paths through the server stalled for users in the past.
/// QUIC (UDP 443) is refused too, so browsers and Discord use HTTPS over TCP:
/// the Oracle exits drop UDP packets over about 1390 bytes, which made
/// Discord uploads hang, and QUIC inside a TCP tunnel is slower anyway.
pub fn config(s: &Settings) -> Value {
    let p = s.params;
    let inbound = match s.capture {
        Capture::Tun => {
            let mut tun = json!({
                "type": "tun",
                "tag": "tun-in",
                "address": ["172.19.0.1/30", "fdfe:dcba:9876::1/126"],
                "mtu": 9000,
                "auto_route": true,
                "strict_route": true,
                "stack": "mixed"
            });
            if cfg!(windows) {
                tun["interface_name"] = json!("CakeVPN");
            }
            tun
        }
        Capture::LocalPort(port) => json!({
            "type": "mixed",
            "tag": "local-in",
            "listen": "127.0.0.1",
            "listen_port": port
        }),
    };

    json!({
        "log": { "level": "warn", "timestamp": true },
        "dns": {
            "servers": [
                { "type": "https", "tag": "remote", "server": "1.1.1.1", "detour": "proxy" },
                { "type": "local", "tag": "local" }
            ],
            "final": "remote",
            "strategy": "ipv4_only"
        },
        "inbounds": [inbound],
        "outbounds": [
            {
                "type": "vless",
                "tag": "proxy",
                "server": p.host,
                "server_port": p.port,
                "uuid": p.uuid,
                "flow": p.flow,
                "packet_encoding": "xudp",
                "tls": {
                    "enabled": true,
                    "server_name": p.sni,
                    "utls": { "enabled": true, "fingerprint": p.fingerprint },
                    "reality": { "enabled": true, "public_key": p.public_key, "short_id": p.short_id }
                }
            },
            { "type": "direct", "tag": "direct" }
        ],
        "route": {
            "rules": [
                { "action": "sniff" },
                { "protocol": "dns", "action": "hijack-dns" },
                { "network": "udp", "port": 443, "action": "reject" },
                { "ip_version": 6, "action": "reject" },
                { "ip_is_private": true, "outbound": "direct" }
            ],
            "final": "proxy",
            "auto_detect_interface": true,
            "default_domain_resolver": "local"
        },
        "experimental": {
            "clash_api": {
                "external_controller": format!("127.0.0.1:{}", s.api_port),
                "secret": s.api_secret
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn params() -> ConnectParams {
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
    fn tun_config_sends_everything_through_the_vpn() {
        let p = params();
        let c = config(&Settings { params: &p, capture: Capture::Tun, api_port: 9095, api_secret: "s" });
        assert_eq!(c["inbounds"][0]["type"], "tun");
        assert_eq!(c["inbounds"][0]["auto_route"], true);
        assert_eq!(c["route"]["final"], "proxy");
        assert_eq!(c["outbounds"][0]["flow"], "xtls-rprx-vision");
        assert_eq!(c["outbounds"][0]["tls"]["reality"]["public_key"], p.public_key);
        assert_eq!(c["dns"]["strategy"], "ipv4_only");
        let rules = c["route"]["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["network"] == "udp" && r["port"] == 443 && r["action"] == "reject"), "QUIC is not refused");
    }

    /// Writes both variants for `sing-box check` (see scripts/check-singbox-config.sh).
    #[test]
    fn write_configs_for_sing_box_check() {
        let Ok(dir) = std::env::var("CAKEVPN_WRITE_CONFIGS") else { return };
        let p = params();
        for (name, capture) in [("tun", Capture::Tun), ("local", Capture::LocalPort(11095))] {
            let c = config(&Settings { params: &p, capture, api_port: 9095, api_secret: "s" });
            std::fs::write(format!("{dir}/{name}.json"), serde_json::to_string_pretty(&c).unwrap()).unwrap();
        }
    }
}
