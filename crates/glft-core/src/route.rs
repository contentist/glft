//! Which route carries a transfer when a device can be reached more than one way.
//!
//! iroh keeps every working route to a device open (multipath) and lets a "path selector"
//! choose the one that carries the data. Its default prefers the lowest round-trip time
//! with a 3 ms bonus for IPv6, so between two devices on the same Wi-Fi it often picks
//! Yggdrasil, whose addresses are IPv6, although Yggdrasil itself runs over that Wi-Fi:
//! the data is then wrapped twice, at about half the speed (measured 2026-10-18, see
//! decision record 0006).
//!
//! glft's selector ranks routes by class first: the local network, then other ordinary
//! routes, then tunnels (Yggdrasil, WireGuard and other VPNs). The class comes from the
//! network interface the route leaves through, which iroh reports as the path's local
//! address; the address range is the fallback when that interface can't be found.
//! Round-trip time decides only within a class, with the same 5 ms stickiness iroh uses,
//! and a better class wins at once, so a transfer that fell back to Yggdrasil returns to
//! Wi-Fi as soon as Wi-Fi works again.
//!
//! The selector API is behind iroh's `unstable-custom-transports` feature, which is why
//! iroh is pinned to an exact version.

use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use iroh::endpoint::transports::{FourTuple, PathSelection, PathSelectionContext, PathSelector};

/// Switching between two routes of the same class needs at least this much better RTT.
const SWITCHING_MIN: Duration = Duration::from_millis(5);

/// Route classes, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// Directly on the local network (Wi-Fi, Ethernet).
    Local,
    /// An ordinary route that isn't the local network.
    Other,
    /// Through a tunnel: Yggdrasil, WireGuard or another VPN.
    Tunnel,
}

/// What this device knows about one of its network interfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interface {
    pub tunnel: bool,
}

/// The class of a route to `remote` leaving from `local`, given how to look up the
/// interface that owns a local address.
pub fn classify(remote: SocketAddr, local: Option<IpAddr>, interface_of: impl Fn(IpAddr) -> Option<Interface>) -> Class {
    match local.and_then(interface_of) {
        Some(Interface { tunnel: true }) => Class::Tunnel,
        Some(Interface { tunnel: false }) => {
            if is_private(remote.ip()) {
                Class::Local
            } else {
                Class::Other
            }
        }
        // No interface to go by: the address range decides.
        None => {
            if is_mesh(remote.ip()) {
                Class::Tunnel
            } else if is_private(remote.ip()) {
                Class::Local
            } else {
                Class::Other
            }
        }
    }
}

/// Yggdrasil's range, `0200::/7`: the first byte is 0x02 or 0x03. (Not `2000::/3`, the
/// ordinary internet.)
pub fn is_mesh(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V6(v6) if v6.octets()[0] & 0xfe == 0x02)
}

/// Addresses that only make sense on a local network.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            // Link-local fe80::/10, unique local fc00::/7.
            (first & 0xffc0) == 0xfe80 || (first & 0xfe00) == 0xfc00
        }
    }
}

/// The selector installed on every glft endpoint.
#[derive(Debug, Default)]
pub struct LocalFirst;

impl PathSelector for LocalFirst {
    fn select(&self, ctx: &PathSelectionContext<'_>) -> PathSelection {
        let interfaces = Interfaces::read();
        let key = |path: &FourTuple, rtt: Duration| -> Option<(Class, Duration)> {
            match path {
                FourTuple::Ip { remote, local } => Some((classify(*remote, *local, |ip| interfaces.of(ip)), rtt)),
                // glft uses no relays or custom transports; leave such paths to iroh.
                _ => None,
            }
        };
        let current = ctx.current();
        let mut best = None;
        let mut current_key: Option<(Class, Duration)> = None;
        for psd in ctx.paths() {
            // A path whose stats can't be read is closing.
            let Some(stats) = psd.stats() else { continue };
            let Some(k) = key(psd.network_path(), stats.rtt) else { continue };
            if Some(psd.network_path()) == current && current_key.is_none_or(|c| k < c) {
                current_key = Some(k);
            }
            if best.as_ref().is_none_or(|(_, b)| k < *b) {
                best = Some((psd, k));
            }
        }
        let mut selection = PathSelection::none();
        let Some((best_psd, (best_class, best_rtt))) = best else { return selection };
        match current_key {
            // A better class wins at once; within a class, only a clearly better RTT.
            Some((class, rtt)) if class == best_class && best_rtt + SWITCHING_MIN > rtt => {}
            Some((class, _)) if class < best_class => {}
            _ => selection.set(&best_psd),
        }
        selection
    }
}

/// This device's network interfaces, by address.
struct Interfaces(Vec<(IpAddr, Interface)>);

impl Interfaces {
    fn of(&self, ip: IpAddr) -> Option<Interface> {
        self.0.iter().find(|(a, _)| *a == ip).map(|(_, i)| *i)
    }

    /// Reads them now: interfaces come and go (Wi-Fi, a VPN starting), and selection runs
    /// only when paths change, so there is nothing worth caching.
    #[cfg(unix)]
    fn read() -> Self {
        let mut list = Vec::new();
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: getifaddrs fills `head` with a list we free below; every pointer is
        // checked before use and nothing is kept past freeifaddrs.
        unsafe {
            if libc::getifaddrs(&mut head) != 0 {
                return Self(list);
            }
            let mut cur = head;
            while !cur.is_null() {
                let ifa = &*cur;
                cur = ifa.ifa_next;
                let Some(ip) = sockaddr_ip(ifa.ifa_addr) else { continue };
                let name = if ifa.ifa_name.is_null() {
                    String::new()
                } else {
                    std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned()
                };
                let point_to_point = ifa.ifa_flags & libc::IFF_POINTOPOINT as libc::c_uint != 0;
                list.push((ip, Interface { tunnel: point_to_point || tunnel_name(&name) || no_hardware(&name) }));
            }
            libc::freeifaddrs(head);
        }
        Self(list)
    }

    #[cfg(not(unix))]
    fn read() -> Self {
        Self(Vec::new())
    }
}

#[cfg(unix)]
unsafe fn sockaddr_ip(addr: *const libc::sockaddr) -> Option<IpAddr> {
    if addr.is_null() {
        return None;
    }
    // SAFETY: the family says which sockaddr type this is.
    unsafe {
        match (*addr).sa_family as i32 {
            libc::AF_INET => {
                let a = &*(addr as *const libc::sockaddr_in);
                Some(IpAddr::from(u32::from_be(a.sin_addr.s_addr).to_be_bytes()))
            }
            libc::AF_INET6 => {
                let a = &*(addr as *const libc::sockaddr_in6);
                Some(IpAddr::from(a.sin6_addr.s6_addr))
            }
            _ => None,
        }
    }
}

/// Interface names VPNs and meshes use, for systems that don't flag them point-to-point.
fn tunnel_name(name: &str) -> bool {
    ["tun", "tap", "wg", "ygg", "utun", "ppp", "ipsec", "tailscale", "zt"].iter().any(|p| name.starts_with(p))
}

/// Link type 65534 (`ARPHRD_NONE`) is what tun devices report: no hardware below them.
fn no_hardware(name: &str) -> bool {
    // Android denies apps /sys/class/net (and logs each try), so the flag and name decide there.
    if cfg!(target_os = "android") {
        return false;
    }
    !name.is_empty() && std::fs::read_to_string(format!("/sys/class/net/{name}/type")).is_ok_and(|t| t.trim() == "65534")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn yggdrasil_is_0200_slash_7_and_not_the_internet() {
        assert!(is_mesh("200::1".parse().unwrap()));
        assert!(is_mesh("300::1".parse().unwrap()));
        // The laptop and phone measured on 2026-10-18.
        assert!(is_mesh("201:1111:2222:3333:4444:5555:6666:7777".parse().unwrap()));
        assert!(is_mesh("200:aaaa:bbbb:cccc:dddd:eeee:ffff:1".parse().unwrap()));
        assert!(!is_mesh("2001:db8::1".parse().unwrap()));
        assert!(!is_mesh("2001:db8:f500:a7cd::1".parse().unwrap()));
        assert!(!is_mesh("400::1".parse().unwrap()));
        assert!(!is_mesh("192.168.1.8".parse().unwrap()));
    }

    #[test]
    fn the_interface_decides_where_it_is_known() {
        let wifi = |_| Some(Interface { tunnel: false });
        let tun = |_| Some(Interface { tunnel: true });
        let unknown = |_| None;
        let local: Option<IpAddr> = Some("192.168.1.25".parse().unwrap());
        // A WireGuard VPN on 10.x is a tunnel, not the local network.
        assert_eq!(classify(sa("10.8.0.1:47382"), Some("10.8.0.2".parse().unwrap()), tun), Class::Tunnel);
        assert_eq!(classify(sa("192.168.1.8:47382"), local, wifi), Class::Local);
        assert_eq!(classify(sa("[200:33a9::1]:47382"), Some("201:f67::1".parse().unwrap()), tun), Class::Tunnel);
        // Ordinary internet addresses over Wi-Fi.
        assert_eq!(classify(sa("[2001:db8::1]:47382"), local, wifi), Class::Other);
        // No interface known: the ranges.
        assert_eq!(classify(sa("[200:33a9::1]:47382"), None, unknown), Class::Tunnel);
        assert_eq!(classify(sa("192.168.1.8:47382"), None, unknown), Class::Local);
        assert_eq!(classify(sa("[fe80::1]:47382"), None, unknown), Class::Local);
        assert_eq!(classify(sa("[2001:db8::1]:47382"), local, unknown), Class::Other);
    }

    #[test]
    fn this_computers_interfaces_are_read() {
        let found = Interfaces::read();
        assert!(found.of("127.0.0.1".parse().unwrap()).is_some(), "loopback missing: {:?}", found.0);
    }

    #[test]
    fn tunnel_names() {
        for n in ["tun0", "wg0", "ygg0", "utun3", "tailscale0"] {
            assert!(tunnel_name(n), "{n}");
        }
        for n in ["wlp1s0", "eth0", "wlan0", "enp3s0", "docker0"] {
            assert!(!tunnel_name(n), "{n}");
        }
    }
}
