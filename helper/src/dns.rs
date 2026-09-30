//! Keeps name lookups inside the tunnel on a Mac.
//!
//! A Mac asks the DNS servers of its network service. When that server is on
//! the local network (a home router, an office's or school's gateway), the
//! question takes the local route and never enters the tunnel: the local
//! network still sees every name and answers it, whatever the VPN does.
//! So while the tunnel is up, every network service is pointed at the
//! tunnel's own DNS address, and what was set before is put back afterwards.
//! What was set before is kept in a file, so it is also put back after a
//! crash or a restart of the computer.
//!
//! Windows and Linux need none of this: sing-box keeps DNS in the tunnel
//! there by itself.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::Path;

/// The address after the tunnel's own (172.19.0.1): sing-box answers DNS sent to it.
const TUNNEL_DNS: &str = "172.19.0.2";
const SAVED: &str = "dns-before.json";

/// What each network service had set before: no addresses means "automatic".
type Before = BTreeMap<String, Vec<String>>;

/// Reads `networksetup -listallnetworkservices`: a note on the first line,
/// then one service per line, with a star in front of those turned off.
fn services(text: &str) -> Vec<String> {
    text.lines().skip(1).map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('*')).map(String::from).collect()
}

/// Reads `networksetup -getdnsservers <service>`: one address per line, or a
/// sentence when the service uses what the network hands out.
fn set_servers(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.iter().all(|l| l.parse::<IpAddr>().is_ok()) {
        lines.into_iter().map(String::from).collect()
    } else {
        vec![]
    }
}

fn read(dir: &Path) -> Option<Before> {
    serde_json::from_slice(&std::fs::read(dir.join(SAVED)).ok()?).ok()
}

fn write(dir: &Path, before: &Before) -> bool {
    std::fs::create_dir_all(dir).is_ok() && std::fs::write(dir.join(SAVED), serde_json::to_vec(before).unwrap_or_default()).is_ok()
}

/// Points every network service at the tunnel's DNS. `run` runs
/// `networksetup` with the given arguments and returns what it printed, or
/// nothing when it failed.
fn apply_with(dir: &Path, run: &mut dyn FnMut(&[&str]) -> Option<String>) {
    let Some(list) = run(&["-listallnetworkservices"]) else { return };
    let mut before = read(dir).unwrap_or_default();
    for service in services(&list) {
        let Some(current) = run(&["-getdnsservers", &service]).map(|out| set_servers(&out)) else { continue };
        if current == [TUNNEL_DNS] {
            // Already ours, from a run that ended badly. Without a note of
            // what was there before, "automatic" is the safe thing to go back to.
            before.entry(service).or_default();
            continue;
        }
        // The first note is the real "before": a later one would be our own setting.
        before.entry(service.clone()).or_insert(current);
        // Noted before anything changes, so it can always be undone.
        if !write(dir, &before) {
            return;
        }
        run(&["-setdnsservers", &service, TUNNEL_DNS]);
    }
    write(dir, &before);
}

/// Puts back what every network service had before. A service whose DNS the
/// person changed meanwhile is left as they set it.
fn restore_with(dir: &Path, run: &mut dyn FnMut(&[&str]) -> Option<String>) {
    let Some(before) = read(dir) else { return };
    let mut left = Before::new();
    for (service, servers) in before {
        // A service that is gone has nothing to put back.
        let Some(current) = run(&["-getdnsservers", &service]).map(|out| set_servers(&out)) else { continue };
        if current != [TUNNEL_DNS] {
            continue;
        }
        let servers: Vec<String> = servers.into_iter().filter(|s| s.parse::<IpAddr>().is_ok() && s != TUNNEL_DNS).collect();
        let mut args = vec!["-setdnsservers", service.as_str()];
        if servers.is_empty() {
            args.push("empty");
        } else {
            args.extend(servers.iter().map(String::as_str));
        }
        if run(&args).is_none() {
            left.insert(service, servers);
        }
    }
    if left.is_empty() {
        let _ = std::fs::remove_file(dir.join(SAVED));
    } else {
        write(dir, &left);
    }
}

#[cfg(target_os = "macos")]
fn networksetup(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("/usr/sbin/networksetup").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Names looked up before must not be served from memory afterwards.
#[cfg(target_os = "macos")]
fn forget_cached_names() {
    let _ = std::process::Command::new("/usr/bin/dscacheutil").arg("-flushcache").status();
    let _ = std::process::Command::new("/usr/bin/killall").args(["-HUP", "mDNSResponder"]).status();
}

/// The tunnel is coming up: send this Mac's name lookups into it.
pub fn into_tunnel(dir: &Path) {
    #[cfg(target_os = "macos")]
    {
        apply_with(dir, &mut networksetup);
        forget_cached_names();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = dir;
}

/// The tunnel is down (or the helper just started): give this Mac its own DNS back.
pub fn back_to_normal(dir: &Path) {
    #[cfg(target_os = "macos")]
    if dir.join(SAVED).exists() {
        restore_with(dir, &mut networksetup);
        forget_cached_names();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Mac's network settings, as `networksetup` would show and change them.
    #[derive(Default)]
    struct Mac {
        dns: BTreeMap<String, Vec<String>>,
        off: Vec<String>,
        calls: Vec<String>,
    }

    impl Mac {
        fn new(services: &[(&str, &[&str])]) -> Mac {
            Mac { dns: services.iter().map(|(n, d)| (n.to_string(), d.iter().map(|s| s.to_string()).collect())).collect(), ..Default::default() }
        }

        fn run(&mut self, args: &[&str]) -> Option<String> {
            self.calls.push(args.join(" "));
            match args {
                ["-listallnetworkservices"] => {
                    let mut text = "An asterisk (*) denotes that a network service is disabled.\n".to_string();
                    for name in self.dns.keys() {
                        text += &format!("{}{name}\n", if self.off.contains(name) { "*" } else { "" });
                    }
                    Some(text)
                }
                ["-getdnsservers", service] => {
                    let set = self.dns.get(*service)?;
                    Some(if set.is_empty() { format!("There aren't any DNS Servers set on {service}.\n") } else { set.join("\n") + "\n" })
                }
                ["-setdnsservers", service, servers @ ..] => {
                    let set = self.dns.get_mut(*service)?;
                    *set = if servers == ["empty"] { vec![] } else { servers.iter().map(|s| s.to_string()).collect() };
                    Some(String::new())
                }
                _ => None,
            }
        }

        fn of(&self, service: &str) -> Vec<&str> {
            self.dns[service].iter().map(String::as_str).collect()
        }
    }

    fn dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cakevpn-dns-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_networksetup() {
        let list = "An asterisk (*) denotes that a network service is disabled.\nUSB 10/100/1000 LAN\nWi-Fi\n*Thunderbolt Bridge\n";
        assert_eq!(services(list), ["USB 10/100/1000 LAN", "Wi-Fi"]);
        assert_eq!(set_servers("There aren't any DNS Servers set on Wi-Fi.\n"), Vec::<String>::new());
        assert_eq!(set_servers("8.8.8.8\n2001:4860:4860::8888\n"), ["8.8.8.8", "2001:4860:4860::8888"]);
    }

    #[test]
    fn lookups_go_into_the_tunnel_and_come_back() {
        let dir = dir();
        let mut mac = Mac::new(&[("Wi-Fi", &[]), ("Ethernet", &["192.168.1.1", "8.8.8.8"]), ("Thunderbolt Bridge", &[])]);
        mac.off.push("Thunderbolt Bridge".into());

        apply_with(&dir, &mut |a| mac.run(a));
        assert_eq!(mac.of("Wi-Fi"), [TUNNEL_DNS]);
        assert_eq!(mac.of("Ethernet"), [TUNNEL_DNS]);
        assert!(mac.of("Thunderbolt Bridge").is_empty(), "a service that is turned off is left alone");
        assert!(dir.join(SAVED).exists());

        restore_with(&dir, &mut |a| mac.run(a));
        assert!(mac.of("Wi-Fi").is_empty(), "automatic again");
        assert_eq!(mac.of("Ethernet"), ["192.168.1.1", "8.8.8.8"]);
        assert!(!dir.join(SAVED).exists());
        assert!(mac.calls.iter().any(|c| c == "-setdnsservers Wi-Fi empty"), "{:?}", mac.calls);

        // Nothing saved: nothing is touched.
        let calls = mac.calls.len();
        restore_with(&dir, &mut |a| mac.run(a));
        assert_eq!(mac.calls.len(), calls);
    }

    #[test]
    fn a_crash_in_between_is_undone_later() {
        let dir = dir();
        let mut mac = Mac::new(&[("Wi-Fi", &["10.0.0.1"])]);
        apply_with(&dir, &mut |a| mac.run(a));
        // The helper dies here. When the tunnel comes up again, the note of
        // what was there before must not be replaced by our own address.
        apply_with(&dir, &mut |a| mac.run(a));
        assert_eq!(read(&dir).unwrap()["Wi-Fi"], ["10.0.0.1"]);
        restore_with(&dir, &mut |a| mac.run(a));
        assert_eq!(mac.of("Wi-Fi"), ["10.0.0.1"]);

        // Our address without any note (the file was lost): back to automatic.
        let mut mac = Mac::new(&[("Wi-Fi", &[TUNNEL_DNS])]);
        apply_with(&dir, &mut |a| mac.run(a));
        restore_with(&dir, &mut |a| mac.run(a));
        assert!(mac.of("Wi-Fi").is_empty());
    }

    #[test]
    fn dns_the_person_changed_meanwhile_is_kept() {
        let dir = dir();
        let mut mac = Mac::new(&[("Wi-Fi", &[]), ("Ethernet", &[])]);
        apply_with(&dir, &mut |a| mac.run(a));
        mac.dns.insert("Wi-Fi".into(), vec!["9.9.9.9".into()]);
        mac.dns.remove("Ethernet"); // the adapter was unplugged for good
        restore_with(&dir, &mut |a| mac.run(a));
        assert_eq!(mac.of("Wi-Fi"), ["9.9.9.9"]);
        assert!(!dir.join(SAVED).exists());
    }

    #[test]
    fn a_failed_restore_is_tried_again() {
        let dir = dir();
        let mut mac = Mac::new(&[("Wi-Fi", &["10.0.0.1"])]);
        apply_with(&dir, &mut |a| mac.run(a));
        // networksetup refuses to change anything this time.
        restore_with(&dir, &mut |a| if a[0] == "-setdnsservers" { None } else { mac.run(a) });
        assert_eq!(mac.of("Wi-Fi"), [TUNNEL_DNS]);
        assert_eq!(read(&dir).unwrap()["Wi-Fi"], ["10.0.0.1"], "still noted for the next try");
        restore_with(&dir, &mut |a| mac.run(a));
        assert_eq!(mac.of("Wi-Fi"), ["10.0.0.1"]);
    }
}
