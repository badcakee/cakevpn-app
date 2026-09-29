//! One ICMP echo to the router. The router sits on the local network, so the
//! ping never goes through the tunnel and shows how good the Wi-Fi itself is.

use std::net::Ipv4Addr;
use std::time::Duration;

/// The round trip in milliseconds; `Ok(None)` when no answer came back in
/// time, `Err` when this system does not let us send pings at all.
pub fn ping(target: Ipv4Addr, timeout: Duration, seq: u16) -> Result<Option<f64>, ()> {
    imp::ping(target, timeout, seq)
}

/// The router of the network card that reaches the internet, skipping the
/// tunnel's own interface.
pub fn gateway() -> Option<Ipv4Addr> {
    let tunnel_like = |name: &str| {
        let n = name.to_ascii_lowercase();
        n.starts_with("utun") || n.starts_with("tun") || n.contains("cakevpn") || n.contains("wintun")
    };
    if let Ok(iface) = netdev::get_default_interface() {
        if !tunnel_like(&iface.name) && !tunnel_like(iface.friendly_name.as_deref().unwrap_or("")) {
            if let Some(ip) = iface.gateway.as_ref().and_then(|g| g.ipv4.first()) {
                return Some(*ip);
            }
        }
    }
    netdev::get_interfaces()
        .into_iter()
        .filter(|i| i.is_up() && !tunnel_like(&i.name))
        .find_map(|i| i.gateway.and_then(|g| g.ipv4.first().copied()))
}

#[cfg(unix)]
mod imp {
    use socket2::{Domain, Protocol, SockAddr, Socket, Type};
    use std::mem::MaybeUninit;
    use std::net::{Ipv4Addr, SocketAddrV4};
    use std::time::{Duration, Instant};

    fn checksum(data: &[u8]) -> u16 {
        let mut sum = 0u32;
        for chunk in data.chunks(2) {
            let word = u16::from_be_bytes([chunk[0], *chunk.get(1).unwrap_or(&0)]);
            sum = sum.wrapping_add(word as u32);
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        !(sum as u16)
    }

    pub fn ping(target: Ipv4Addr, timeout: Duration, seq: u16) -> Result<Option<f64>, ()> {
        // Datagram ICMP needs no raw-socket rights; raw is the fallback for root on Linux.
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4))
            .or_else(|_| Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::ICMPV4)))
            .map_err(|_| ())?;
        if socket.send_to(&echo_request(seq), &SockAddr::from(SocketAddrV4::new(target, 0))).is_err() {
            return Err(());
        }
        Ok(wait_for_reply(&socket, timeout, seq))
    }

    fn echo_request(seq: u16) -> [u8; 16] {
        let mut packet = [0u8; 16];
        packet[0] = 8; // echo request
        packet[4..6].copy_from_slice(&0xcafeu16.to_be_bytes());
        packet[6..8].copy_from_slice(&seq.to_be_bytes());
        packet[8..16].copy_from_slice(b"cakevpn!");
        let sum = checksum(&packet);
        packet[2..4].copy_from_slice(&sum.to_be_bytes());
        packet
    }

    fn wait_for_reply(socket: &Socket, timeout: Duration, seq: u16) -> Option<f64> {
        let start = Instant::now();
        let mut buf = [MaybeUninit::<u8>::uninit(); 1500];
        loop {
            let left = timeout.checked_sub(start.elapsed())?;
            socket.set_read_timeout(Some(left.max(Duration::from_millis(1)))).ok()?;
            let n = socket.recv(&mut buf).ok()?;
            let data: Vec<u8> = buf[..n].iter().map(|b| unsafe { b.assume_init() }).collect();
            // Some systems hand back the IP header too.
            let icmp = if !data.is_empty() && data[0] >> 4 == 4 {
                &data[((data[0] & 0x0f) as usize * 4).min(data.len())..]
            } else {
                &data[..]
            };
            if icmp.len() >= 8 && icmp[0] == 0 && icmp[6..8] == seq.to_be_bytes() {
                return Some(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn checksum_of_known_packet() {
            // echo request, id 1, seq 1, no payload
            let p = [8u8, 0, 0, 0, 0, 1, 0, 1];
            assert_eq!(super::checksum(&p), 0xf7fd);
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::net::Ipv4Addr;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho, ICMP_ECHO_REPLY,
    };

    pub fn ping(target: Ipv4Addr, timeout: Duration, _seq: u16) -> Result<Option<f64>, ()> {
        unsafe {
            let handle = IcmpCreateFile();
            if handle == INVALID_HANDLE_VALUE {
                let _ = GetLastError();
                return Err(());
            }
            let payload = *b"cakevpn!";
            let mut reply = vec![0u8; std::mem::size_of::<ICMP_ECHO_REPLY>() + payload.len() + 8];
            let addr = u32::from_ne_bytes(target.octets());
            let n = IcmpSendEcho(
                handle,
                addr,
                payload.as_ptr() as *const _,
                payload.len() as u16,
                std::ptr::null(),
                reply.as_mut_ptr() as *mut _,
                reply.len() as u32,
                timeout.as_millis() as u32,
            );
            IcmpCloseHandle(handle);
            if n == 0 {
                return Ok(None);
            }
            let echo = &*(reply.as_ptr() as *const ICMP_ECHO_REPLY);
            if echo.Status != 0 {
                return Ok(None);
            }
            // Windows rounds to whole milliseconds; a LAN answer often shows as 0.
            Ok(Some(echo.RoundTripTime as f64))
        }
    }
}
