//! The iroh endpoint, configured to stay on the user's own networks.
//!
//! No relays, no published addresses, no port mapping: devices are found by mDNS on
//! the local network or by addresses the user gives. Connections are QUIC with TLS
//! using raw public keys, so a connection only succeeds with the expected key.

use std::{
    fs,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
};

use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey,
    endpoint::{BindOpts, presets},
};
use iroh_mdns_address_lookup::MdnsAddressLookup;

use crate::{
    error::{Error, ErrorKind},
    events::{Event, EventSink},
};

/// Protocol identifier; bumped on incompatible changes.
pub const ALPN: &[u8] = b"glft/1";

/// The port glft listens on when it is free. Away from LocalSend's 53317.
pub const PREFERRED_PORT: u16 = 47_382;

/// mDNS service name, so glft only sees other glft devices.
const MDNS_SERVICE: &str = "glft";

/// Binds the endpoint on `port` (0 = any), or on the preferred port with fallback.
///
/// If the preferred port is taken, the endpoint takes any free port and the event
/// log names the program holding the preferred one (design rule 3).
pub async fn bind(secret: SecretKey, port: Option<u16>, events: &EventSink) -> Result<Endpoint, Error> {
    let (port, fell_back) = match port {
        Some(p) if p != 0 && UdpSocket::bind((Ipv4Addr::UNSPECIFIED, p)).is_err() => {
            let holder = udp_port_holder(p).unwrap_or_else(|| "a program glft cannot see".into());
            return Err(Error::new(ErrorKind::PortInUse, format!("port {p} is held by {holder}")));
        }
        Some(p) => (p, false),
        None => match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, PREFERRED_PORT)) {
            Ok(probe) => {
                drop(probe);
                (PREFERRED_PORT, false)
            }
            Err(_) => (0, true),
        },
    };
    let endpoint = build(secret, port).await?;
    if fell_back {
        let actual = local_port(&endpoint);
        events(Event::PortInUse { port: PREFERRED_PORT, holder: udp_port_holder(PREFERRED_PORT), actual });
    }
    events(Event::Listening { addrs: reachable_addrs(&endpoint).await });
    Ok(endpoint)
}

/// The addresses other devices can use, e.g. `192.168.1.40:47382`.
///
/// Interface addresses appear shortly after binding; waits up to a second for them.
pub async fn reachable_addrs(endpoint: &Endpoint) -> Vec<SocketAddr> {
    for _ in 0..20 {
        let addrs: Vec<SocketAddr> = endpoint.addr().ip_addrs().copied().collect();
        if !addrs.is_empty() {
            return addrs;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    endpoint.bound_sockets()
}

/// Binds an endpoint that listens on any free port, for sending only.
pub async fn bind_ephemeral(secret: SecretKey) -> Result<Endpoint, Error> {
    build(secret, 0).await
}

/// Binds a short-lived endpoint for pairing on any free port. It is not announced by
/// mDNS (the code carries its addresses) and speaks only the pairing protocol.
pub async fn bind_pairing(secret: SecretKey) -> Result<Endpoint, Error> {
    builder(secret, 0, crate::pairing::PAIR_ALPN)
        .map_err(|e| bind_err(&e))?
        .bind()
        .await
        .map_err(|e| bind_err(&e))
}

/// How long a silent connection lasts. Keep-alives go every 5 s, so a pulled Wi-Fi
/// shows up as a lost connection within this time, and resuming can start.
const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

// Refused outright (EPERM/EACCES) means the platform withholds network access from the
// app, e.g. GrapheneOS's Network permission; anything else is a plain bind failure.
fn bind_err(e: &dyn std::error::Error) -> Error {
    let detail = chain(e);
    let kind = if classify_unreachable(&detail) == ErrorKind::LocalNetworkBlocked {
        ErrorKind::NetworkDenied
    } else {
        ErrorKind::BindFailed
    };
    Error::new(kind, detail)
}

fn builder(secret: SecretKey, port: u16, alpn: &[u8]) -> Result<iroh::endpoint::Builder, iroh::endpoint::InvalidSocketAddr> {
    let transport = iroh::endpoint::QuicTransportConfig::builder()
        .max_idle_timeout(Some(IDLE_TIMEOUT.try_into().expect("15 s fits")))
        .build();
    Endpoint::builder(presets::Minimal)
        .secret_key(secret)
        .alpns(vec![alpn.to_vec()])
        .transport_config(transport)
        .relay_mode(RelayMode::Disabled)
        .clear_ip_transports()
        .bind_addr((Ipv4Addr::UNSPECIFIED, port))?
        .bind_addr_with_opts(
            SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)),
            BindOpts::default().set_is_required(false),
        )
}

/// Orders addresses by how likely they are the home network: 192.168/16, then 10/8,
/// then 172.16/12 (where Docker and other bridges usually sit), then the rest.
pub fn lan_rank(addr: &SocketAddr) -> u8 {
    match addr.ip() {
        std::net::IpAddr::V4(ip) => match ip.octets() {
            [192, 168, ..] => 0,
            [10, ..] => 1,
            [172, b, ..] if (16..32).contains(&b) => 2,
            _ => 3,
        },
        _ => 4,
    }
}

async fn build(secret: SecretKey, port: u16) -> Result<Endpoint, Error> {
    builder(secret, port, ALPN)
        .map_err(|e| bind_err(&e))?
        .address_lookup(MdnsAddressLookup::builder().service_name(MDNS_SERVICE))
        .bind()
        .await
        .map_err(|e| bind_err(&e))
}

fn local_port(endpoint: &Endpoint) -> u16 {
    endpoint.bound_sockets().first().map(|a| a.port()).unwrap_or(0)
}

/// An error with all its causes: "Failed to bind sockets: mDNS: Permission denied".
pub fn chain(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        let part = cause.to_string();
        if !text.contains(&part) {
            text.push_str(": ");
            text.push_str(&part);
        }
        source = cause.source();
    }
    text
}

/// Tells a blocked network apart from a device that is simply not there.
///
/// Android cuts apps off from the local network unless they hold the local network
/// permission: UDP sends then fail with EPERM ("Operation not permitted"), and GrapheneOS's
/// Network permission gives EACCES ("Permission denied").
pub fn classify_unreachable(detail: &str) -> ErrorKind {
    let blocked = ["Operation not permitted", "os error 1)", "Permission denied", "os error 13)"];
    if blocked.iter().any(|b| detail.contains(b)) { ErrorKind::LocalNetworkBlocked } else { ErrorKind::PeerUnreachable }
}

/// Parses a device id as printed by `glft id`.
pub fn parse_id(s: &str) -> Result<EndpointId, Error> {
    s.trim().parse().map_err(|e| Error::new(ErrorKind::BadDeviceId, format!("{s:?}: {e}")))
}

/// Address to dial: the id plus any addresses the user typed.
pub fn peer_addr(id: EndpointId, addrs: &[SocketAddr]) -> EndpointAddr {
    addrs.iter().fold(EndpointAddr::new(id), |a, s| a.with_ip_addr(*s))
}

/// A short form of a device id for the log.
pub fn short_id(id: &EndpointId) -> String {
    id.fmt_short().to_string()
}

/// Names the process holding UDP `port`: "transmission (pid 1234)".
///
/// Reads `/proc/net/udp{,6}` for the socket inode, then looks for it among the
/// open files of processes this user can see. `None` if the holder is not visible.
pub fn udp_port_holder(port: u16) -> Option<String> {
    let inodes: Vec<String> = ["/proc/net/udp", "/proc/net/udp6"]
        .iter()
        .filter_map(|p| fs::read_to_string(p).ok())
        .flat_map(|table| {
            table
                .lines()
                .skip(1)
                .filter_map(|line| {
                    let cols: Vec<&str> = line.split_whitespace().collect();
                    let local_port = cols.get(1)?.rsplit(':').next()?;
                    (u16::from_str_radix(local_port, 16).ok()? == port).then(|| cols.get(9).map(|s| s.to_string()))?
                })
                .collect::<Vec<_>>()
        })
        .filter(|inode| inode != "0")
        .collect();
    let targets: Vec<String> = inodes.iter().map(|i| format!("socket:[{i}]")).collect();
    for proc_entry in fs::read_dir("/proc").ok()?.flatten() {
        let pid = proc_entry.file_name();
        let Some(pid) = pid.to_str().filter(|p| p.bytes().all(|b| b.is_ascii_digit())) else { continue };
        let Ok(fds) = fs::read_dir(proc_entry.path().join("fd")) else { continue };
        for fd in fds.flatten() {
            let Ok(link) = fs::read_link(fd.path()) else { continue };
            if targets.iter().any(|t| link.as_os_str() == t.as_str()) {
                let name = fs::read_to_string(proc_entry.path().join("comm")).unwrap_or_default();
                return Some(format!("{} (pid {pid})", name.trim()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_network_is_named() {
        assert_eq!(classify_unreachable("sendmsg: Operation not permitted (os error 1)"), ErrorKind::LocalNetworkBlocked);
        assert_eq!(classify_unreachable("Permission denied (os error 13)"), ErrorKind::LocalNetworkBlocked);
        assert_eq!(classify_unreachable("timed out"), ErrorKind::PeerUnreachable);
    }

    #[test]
    fn finds_own_socket_as_holder() {
        let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = sock.local_addr().unwrap().port();
        let holder = udp_port_holder(port).expect("own process is visible");
        assert!(holder.contains(&format!("pid {}", std::process::id())), "{holder}");
    }
}
