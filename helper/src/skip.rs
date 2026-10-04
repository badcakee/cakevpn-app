//! Skipped websites, found by their addresses as well as by their names.
//!
//! sing-box knows a skipped website by the name a browser sends when it
//! connects, or else by the name a program looked up just before connecting
//! (sing-box remembers which name each answer was for). Games such as
//! Minecraft send no name, and a lookup doesn't always reach sing-box: the
//! name can lead to another one first (a server behind a protection
//! service), or the computer still remembers the answer from before the VPN
//! was on. So the helper looks the skipped names up itself as well, every few
//! minutes while connected, and keeps their addresses in a small list that
//! sing-box reads again whenever it changes.

use serde_json::json;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::time::{Duration, Instant};

pub const FILE: &str = "skip-addresses.json";
/// How often the names are looked up again while connected (the first two
/// times sooner, so a lookup made while the tunnel was starting is redone).
pub const LOOK_AGAIN: [Duration; 2] = [Duration::from_secs(30), Duration::from_secs(5 * 60)];
/// An address no lookup gave for this long is dropped.
const FORGET_AFTER: Duration = Duration::from_secs(60 * 60);
const MAX_ADDRESSES: usize = 2000;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(4);

/// The addresses found for one skip list, and when each was last seen.
#[derive(Default)]
pub struct Addresses {
    domains: Vec<String>,
    seen: HashMap<IpAddr, Instant>,
}

impl Addresses {
    /// Starts over when the skip list changed, so removed sites stop skipping.
    pub fn use_list(&mut self, domains: &[String]) {
        if self.domains != domains {
            self.domains = domains.to_vec();
            self.seen.clear();
        }
    }

    /// Adds what a lookup found and forgets what wasn't found for an hour.
    /// Returns true when the list changed.
    pub fn update(&mut self, found: &[IpAddr], now: Instant) -> bool {
        let before = self.seen.len();
        self.seen.retain(|_, at| now.duration_since(*at) < FORGET_AFTER);
        let mut changed = self.seen.len() != before;
        for ip in found {
            if self.seen.len() >= MAX_ADDRESSES && !self.seen.contains_key(ip) {
                break;
            }
            changed |= self.seen.insert(*ip, now).is_none();
        }
        changed
    }

    pub fn list(&self) -> Vec<IpAddr> {
        let mut list: Vec<IpAddr> = self.seen.keys().copied().collect();
        list.sort();
        list
    }
}

/// The list as a sing-box rule set. An empty one matches nothing.
pub fn rule_set(addresses: &[IpAddr]) -> serde_json::Value {
    if addresses.is_empty() {
        return json!({ "version": 3, "rules": [] });
    }
    let cidrs: Vec<String> = addresses
        .iter()
        .map(|ip| match ip {
            IpAddr::V4(_) => format!("{ip}/32"),
            IpAddr::V6(_) => format!("{ip}/128"),
        })
        .collect();
    json!({ "version": 3, "rules": [{ "ip_cidr": cidrs }] })
}

/// Replaces the file in one step, so sing-box never reads half of it.
pub fn write(dir: &Path, addresses: &[IpAddr]) -> std::io::Result<()> {
    let temp = dir.join(format!("{FILE}.new"));
    std::fs::write(&temp, serde_json::to_vec(&rule_set(addresses)).unwrap_or_default())?;
    std::fs::rename(&temp, dir.join(FILE))
}

/// Looks every name up the way programs on this device do, all at once.
/// Addresses on the local network are left out: they skip the VPN anyway.
pub async fn look_up(domains: &[String]) -> Vec<IpAddr> {
    let lookups: Vec<_> = domains
        .iter()
        .map(|domain| {
            let domain = domain.clone();
            tokio::spawn(async move {
                match tokio::time::timeout(LOOKUP_TIMEOUT, tokio::net::lookup_host((domain.as_str(), 0))).await {
                    Ok(Ok(found)) => found.map(|a| a.ip()).collect::<Vec<_>>(),
                    _ => vec![],
                }
            })
        })
        .collect();
    let mut all = vec![];
    for lookup in lookups {
        if let Ok(found) = lookup.await {
            all.extend(found.into_iter().filter(|ip| is_public(ip)));
        }
    }
    all.sort();
    all.dedup();
    all
}

fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !(v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast()),
        IpAddr::V6(v6) => !(v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn keeps_addresses_for_an_hour_and_starts_over_for_a_new_list() {
        let mut a = Addresses::default();
        a.use_list(&["donutsmp.net".into()]);
        let start = Instant::now();
        assert!(a.update(&[ip("40.223.14.113"), ip("40.223.14.114")], start));
        assert!(!a.update(&[ip("40.223.14.113")], start + Duration::from_secs(300)), "nothing new");
        // The other address wasn't seen again for an hour: it goes.
        assert!(a.update(&[ip("40.223.14.113")], start + Duration::from_secs(3700)));
        assert_eq!(a.list(), vec![ip("40.223.14.113")]);
        a.use_list(&["donutsmp.net".into()]);
        assert_eq!(a.list().len(), 1, "same list, same addresses");
        a.use_list(&["mybank.com".into()]);
        assert!(a.list().is_empty());
    }

    #[test]
    fn rule_set_lists_each_address() {
        assert_eq!(rule_set(&[]), json!({ "version": 3, "rules": [] }));
        assert_eq!(
            rule_set(&[ip("40.223.14.113"), ip("2001:db8::1")]),
            json!({ "version": 3, "rules": [{ "ip_cidr": ["40.223.14.113/32", "2001:db8::1/128"] }] })
        );
    }

    #[test]
    fn local_addresses_are_left_out() {
        assert!(is_public(&ip("40.223.14.113")));
        for local in ["192.168.1.1", "10.0.0.1", "127.0.0.1", "0.0.0.0", "169.254.1.1", "::1", "fe80::1", "fd00::1"] {
            assert!(!is_public(&ip(local)), "{local}");
        }
    }

    #[test]
    fn writes_the_file_in_one_step() {
        let dir = std::env::temp_dir().join(format!("cakevpn-skip-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir, &[ip("40.223.14.113")]).unwrap();
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(text.contains("40.223.14.113/32"));
        assert!(!dir.join(format!("{FILE}.new")).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
