//! Builds the sing-box configuration for one connection.

use cakevpn_proto::ConnectParams;
use serde_json::{json, Value};
use std::path::Path;

/// The ad and tracker list (see assets/README.md), written next to the
/// config when blocking is on.
pub const ADS_RULE_SET: &[u8] = include_bytes!("../assets/ads.srs");

/// How the tunnel takes over traffic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Capture {
    /// A TUN interface that carries all traffic of the computer.
    Tun,
    /// Only a local SOCKS/HTTP port. Used to test the helper without touching routes.
    LocalPort(u16),
    /// Android: the phone's own VPN interface (see `tunnel::Attach`) passes
    /// everything to a local port picked for each connection.
    Device,
}

pub struct Settings<'a> {
    pub params: &'a ConnectParams,
    pub capture: Capture,
    pub api_port: u16,
    pub api_secret: &'a str,
    /// Where ADS_RULE_SET was written; needed when `params.block_ads` is on.
    pub ads_rule_set: Option<&'a Path>,
    /// The local port for `Capture::Device`.
    pub device_port: u16,
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
    let device = s.capture == Capture::Device;
    // Users with their own IPv6 exit get IPv6 too, but IPv4 stays first: all
    // IPv6 leaves through one Oracle VM, so it shouldn't carry everything.
    let dns_strategy = if p.ipv6 { "prefer_ipv4" } else { "ipv4_only" };
    let mut rules = vec![json!({ "action": "sniff" }), json!({ "protocol": "dns", "action": "hijack-dns" })];
    let mut dns_rules = vec![];
    let mut rule_sets = vec![];

    // Split tunneling: these sites and apps go straight out, and their names
    // are looked up outside the VPN too, so they get nearby servers.
    if !p.bypass_domains.is_empty() {
        rules.push(json!({ "domain_suffix": p.bypass_domains, "outbound": "direct" }));
        dns_rules.push(json!({ "domain_suffix": p.bypass_domains, "server": "local" }));
    }
    // On Android, apps skip the VPN by being left out of the VPN itself.
    if !p.bypass_apps.is_empty() && !device {
        rules.push(json!({ "process_name": p.bypass_apps, "outbound": "direct" }));
        dns_rules.push(json!({ "process_name": p.bypass_apps, "server": "local" }));
    }

    // Ad blocking: ad names don't resolve, and connections that still name
    // one (found by sniffing) are refused.
    if let (true, Some(path)) = (p.block_ads, s.ads_rule_set) {
        rule_sets.push(json!({ "type": "local", "tag": "ads", "format": "binary", "path": path }));
        dns_rules.push(json!({ "rule_set": "ads", "action": "predefined", "rcode": "NXDOMAIN" }));
        rules.push(json!({ "rule_set": "ads", "action": "reject" }));
    }

    rules.push(json!({ "network": "udp", "port": 443, "action": "reject" }));
    if !p.ipv6 {
        rules.push(json!({ "ip_version": 6, "action": "reject" }));
    }
    rules.push(json!({ "ip_is_private": true, "outbound": "direct" }));
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
        Capture::Device => json!({
            "type": "mixed",
            "tag": "device-in",
            "listen": "127.0.0.1",
            "listen_port": s.device_port
        }),
    };
    // sing-box on Android has no system resolver to ask (it runs as a plain
    // program there), so names outside the VPN go to Cloudflare directly.
    // The app's own traffic is left out of the phone's VPN, so this doesn't loop.
    let local_dns = if device {
        json!({ "type": "udp", "tag": "local", "server": "1.1.1.1" })
    } else {
        json!({ "type": "local", "tag": "local" })
    };

    json!({
        "log": { "level": "warn", "timestamp": true },
        "dns": {
            "servers": [
                { "type": "https", "tag": "remote", "server": "1.1.1.1", "detour": "proxy" },
                local_dns
            ],
            "rules": dns_rules,
            "final": "remote",
            "strategy": dns_strategy
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
            "rules": rules,
            "rule_set": rule_sets,
            "final": "proxy",
            "auto_detect_interface": !device,
            "find_process": !p.bypass_apps.is_empty() && !device,
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
            ipv6: false,
            block_ads: false,
            kill_switch: false,
            bypass_domains: vec![],
            bypass_apps: vec![],
        }
    }

    fn settings(p: &ConnectParams) -> Settings<'_> {
        Settings { params: p, capture: Capture::Tun, api_port: 9095, api_secret: "s", ads_rule_set: Some(Path::new("/x/ads.srs")), device_port: 0 }
    }

    #[test]
    fn nothing_extra_by_default() {
        let c = config(&settings(&params()));
        assert_eq!(c["dns"]["rules"], json!([]));
        assert_eq!(c["route"]["rule_set"], json!([]));
        assert_eq!(c["route"]["find_process"], false);
    }

    #[test]
    fn ads_are_blocked_by_name_and_connection() {
        let mut p = params();
        p.block_ads = true;
        let c = config(&settings(&p));
        assert_eq!(c["route"]["rule_set"][0]["path"], "/x/ads.srs");
        assert!(c["dns"]["rules"].as_array().unwrap().iter().any(|r| r["rule_set"] == "ads" && r["rcode"] == "NXDOMAIN"));
        let rules = c["route"]["rules"].as_array().unwrap();
        let ads = rules.iter().position(|r| r["rule_set"] == "ads" && r["action"] == "reject").expect("no ad rule");
        let final_rules = rules.iter().position(|r| r["ip_is_private"] == true).unwrap();
        assert!(ads < final_rules);
    }

    #[test]
    fn skipped_sites_and_apps_go_direct() {
        let mut p = params();
        p.bypass_domains = vec!["mybank.com".into()];
        p.bypass_apps = vec!["steam.exe".into()];
        let c = config(&settings(&p));
        let rules = c["route"]["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["domain_suffix"] == json!(["mybank.com"]) && r["outbound"] == "direct"));
        assert!(rules.iter().any(|r| r["process_name"] == json!(["steam.exe"]) && r["outbound"] == "direct"));
        assert_eq!(c["route"]["find_process"], true);
        assert!(c["dns"]["rules"].as_array().unwrap().iter().any(|r| r["domain_suffix"] == json!(["mybank.com"]) && r["server"] == "local"));
    }

    #[test]
    fn tun_config_sends_everything_through_the_vpn() {
        let p = params();
        let c = config(&settings(&p));
        assert_eq!(c["inbounds"][0]["type"], "tun");
        assert_eq!(c["inbounds"][0]["auto_route"], true);
        assert_eq!(c["route"]["final"], "proxy");
        assert_eq!(c["outbounds"][0]["flow"], "xtls-rprx-vision");
        assert_eq!(c["outbounds"][0]["tls"]["reality"]["public_key"], p.public_key);
        assert_eq!(c["dns"]["strategy"], "ipv4_only");
        let rules = c["route"]["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["network"] == "udp" && r["port"] == 443 && r["action"] == "reject"), "QUIC is not refused");
    }

    #[test]
    fn android_takes_traffic_on_a_local_port() {
        let mut p = params();
        p.bypass_apps = vec!["com.example.bank".into()];
        let mut s = settings(&p);
        s.capture = Capture::Device;
        s.device_port = 40000;
        let c = config(&s);
        assert_eq!(c["inbounds"][0]["type"], "mixed");
        assert_eq!(c["inbounds"][0]["listen"], "127.0.0.1");
        assert_eq!(c["inbounds"][0]["listen_port"], 40000);
        assert_eq!(c["route"]["auto_detect_interface"], false);
        assert_eq!(c["route"]["find_process"], false);
        assert_eq!(c["dns"]["servers"][1]["type"], "udp");
        let rules = c["route"]["rules"].as_array().unwrap();
        assert!(!rules.iter().any(|r| r.get("process_name").is_some()), "Android leaves skipped apps out of the VPN instead");
        assert!(rules.iter().any(|r| r["action"] == "hijack-dns"));
    }

    #[test]
    fn ipv6_only_for_users_with_their_own_address() {
        let mut p = params();
        let without = config(&settings(&p));
        assert_eq!(without["dns"]["strategy"], "ipv4_only");
        assert!(without["route"]["rules"].as_array().unwrap().iter().any(|r| r["ip_version"] == 6));
        p.ipv6 = true;
        let with = config(&settings(&p));
        assert_eq!(with["dns"]["strategy"], "prefer_ipv4");
        assert!(!with["route"]["rules"].as_array().unwrap().iter().any(|r| r["ip_version"] == 6));
    }

    /// Writes both variants for `sing-box check` (see scripts/check-singbox-config.sh).
    #[test]
    fn write_configs_for_sing_box_check() {
        let Ok(dir) = std::env::var("CAKEVPN_WRITE_CONFIGS") else { return };
        let p = params();
        let mut v6 = params();
        v6.ipv6 = true;
        let mut all = params();
        all.block_ads = true;
        all.bypass_domains = vec!["mybank.com".into(), "donutsmp.net".into()];
        all.bypass_apps = vec!["steam.exe".into(), "Discord".into()];
        let ads = format!("{dir}/ads.srs");
        std::fs::write(&ads, ADS_RULE_SET).unwrap();
        for (name, params, capture) in [
            ("tun", &p, Capture::Tun),
            ("local", &p, Capture::LocalPort(11095)),
            ("tun-ipv6", &v6, Capture::Tun),
            ("tun-all-options", &all, Capture::Tun),
            ("android", &all, Capture::Device),
        ] {
            let c = config(&Settings { params, capture, api_port: 9095, api_secret: "s", ads_rule_set: Some(Path::new(&ads)), device_port: 11096 });
            std::fs::write(format!("{dir}/{name}.json"), serde_json::to_string_pretty(&c).unwrap()).unwrap();
        }
    }
}
