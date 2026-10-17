//! Pairing by QR code.
//!
//! 1. The computer shows a code holding its full key, its current addresses and a
//!    one-time secret. The code works once and for five minutes.
//! 2. The phone dials that key. TLS with raw public keys proves it reached the computer
//!    that drew the code; sending the secret proves to the computer that the phone read it.
//! 3. Both sides learn the other's name and ask "Remember this device?", then tell each
//!    other the answer, so a one-sided pairing can be stated plainly.
//!
//! The code carries the whole key on purpose: KDE Connect's 8-character verification
//! code was short enough to brute-force (CVE-2025-32898). It is upper-case base32 so a
//! QR code can use alphanumeric mode, which keeps it small enough to scan off a terminal.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use data_encoding::BASE32_NOPAD;
use iroh::{
    Endpoint, EndpointId, SecretKey,
    endpoint::{Connection, RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};

use crate::{
    devices::{Device, Remember},
    error::{Error, ErrorKind},
    events::{Event, EventSink},
    net::{self, short_id},
    paths::device_name,
    proto::{self, lost},
};

/// Protocol identifier for pairing connections; separate from transfers.
pub const PAIR_ALPN: &[u8] = b"glft-pair/1";

/// How long a code stays valid.
pub const CODE_LIFETIME: Duration = Duration::from_secs(5 * 60);

/// How long either side waits for the other person to answer "Remember this device?".
const CHOICE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

const STEP_TIMEOUT: Duration = Duration::from_secs(15);
const PREFIX: &str = "GLFT1";

/// What the QR code holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    pub id: EndpointId,
    pub secret: [u8; 16],
    pub addrs: Vec<SocketAddr>,
}

impl PairingCode {
    /// `GLFT1/<key>/<secret>/<addresses>`, all upper-case base32.
    pub fn encode(&self) -> String {
        let mut packed = Vec::new();
        for addr in &self.addrs {
            match addr.ip() {
                IpAddr::V4(ip) => {
                    packed.push(4);
                    packed.extend_from_slice(&ip.octets());
                }
                IpAddr::V6(ip) => {
                    packed.push(6);
                    packed.extend_from_slice(&ip.octets());
                }
            }
            packed.extend_from_slice(&addr.port().to_be_bytes());
        }
        format!(
            "{PREFIX}/{}/{}/{}",
            BASE32_NOPAD.encode(self.id.as_bytes()),
            BASE32_NOPAD.encode(&self.secret),
            BASE32_NOPAD.encode(&packed)
        )
    }

    /// Reads a code as scanned or pasted; spaces and case do not matter.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let bad = |why: &str| Error::new(ErrorKind::PairingCodeUnreadable, why.to_string());
        let text: String = text.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_uppercase();
        let mut parts = text.split('/');
        if parts.next() != Some(PREFIX) {
            return Err(bad("it does not start with GLFT1/"));
        }
        let mut field = |name: &str| {
            let part = parts.next().ok_or_else(|| bad(&format!("the {name} is missing")))?;
            BASE32_NOPAD.decode(part.as_bytes()).map_err(|e| bad(&format!("the {name} is damaged: {e}")))
        };
        let key: [u8; 32] = field("key")?.try_into().map_err(|_| bad("the key has the wrong length"))?;
        let secret: [u8; 16] = field("secret")?.try_into().map_err(|_| bad("the secret has the wrong length"))?;
        let packed = field("address list")?;
        let id = EndpointId::from_bytes(&key).map_err(|e| bad(&format!("the key is not valid: {e}")))?;
        let mut addrs = Vec::new();
        let mut rest = &packed[..];
        while let Some((&tag, tail)) = rest.split_first() {
            let ip_len = match tag {
                4 => 4,
                6 => 16,
                _ => return Err(bad("the address list is damaged")),
            };
            if tail.len() < ip_len + 2 {
                return Err(bad("the address list is cut short"));
            }
            let ip = if tag == 4 {
                IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(&tail[..4]).unwrap()))
            } else {
                IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(&tail[..16]).unwrap()))
            };
            let port = u16::from_be_bytes([tail[ip_len], tail[ip_len + 1]]);
            addrs.push(SocketAddr::new(ip, port));
            rest = &tail[ip_len + 2..];
        }
        Ok(Self { id, secret, addrs })
    }
}

#[derive(Serialize, Deserialize)]
struct Hello {
    secret: [u8; 16],
    device_name: String,
    /// Where the joining device can be reached for transfers, if it knows.
    addrs: Vec<SocketAddr>,
}

#[derive(Serialize, Deserialize)]
enum Welcome {
    Ok { device_name: String, addrs: Vec<SocketAddr> },
    WrongCode,
}

#[derive(Serialize, Deserialize)]
struct Choice {
    remember: Remember,
}

/// A pairing whose secret checked out, waiting for "Remember this device?".
pub struct Pending {
    endpoint: Endpoint,
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
    /// The other device, as it introduced itself.
    pub peer: Device,
}

impl Pending {
    /// Sends this side's answer and returns the other side's.
    pub async fn finish(mut self, ours: Remember, events: &EventSink) -> Result<Remember, Error> {
        let name = format!("\"{}\"", self.peer.name);
        let result = async {
            proto::write(&mut self.send, &Choice { remember: ours }).await?;
            let _ = self.send.finish();
            let theirs = tokio::time::timeout(CHOICE_TIMEOUT, proto::read::<Choice>(&mut self.recv))
                .await
                .map_err(|_| Error::new(ErrorKind::PairingAbandoned, "no answer within 5 minutes"))?
                .map_err(|e| Error::new(ErrorKind::PairingAbandoned, e.detail))?;
            let _ = self.recv.read_to_end(64).await;
            // Let the other side read our answer before the connection goes.
            let _ = tokio::time::timeout(Duration::from_secs(2), self.send.stopped()).await;
            Ok(theirs.remember)
        }
        .await;
        self.conn.close(0u32.into(), b"paired");
        self.endpoint.close().await;
        let theirs = result?;
        events(Event::Paired { peer: name, ours, theirs });
        Ok(theirs)
    }

    /// Ends the pairing without an answer (the person cancelled).
    pub async fn cancel(self) {
        self.conn.close(3u32.into(), b"cancelled");
        self.endpoint.close().await;
    }
}

/// The side that shows the code (the computer).
pub struct Host {
    endpoint: Endpoint,
    code: PairingCode,
    receive_addrs: Vec<SocketAddr>,
}

impl Host {
    /// Opens a pairing endpoint and makes a fresh code. `receive_addrs` tells the other
    /// device where to send files later.
    pub async fn start(secret: SecretKey, receive_addrs: Vec<SocketAddr>) -> Result<Self, Error> {
        let endpoint = net::bind_pairing(secret).await?;
        let addrs = code_addrs(&net::reachable_addrs(&endpoint).await);
        let mut secret = [0u8; 16];
        getrandom::fill(&mut secret).map_err(|e| Error::new(ErrorKind::BindFailed, format!("no randomness: {e}")))?;
        let receive_addrs = if receive_addrs.is_empty() { hint_addrs(&addrs) } else { receive_addrs };
        Ok(Self { code: PairingCode { id: endpoint.id(), secret, addrs }, endpoint, receive_addrs })
    }

    pub fn code(&self) -> &PairingCode {
        &self.code
    }

    /// Waits until a device presents the secret, for at most [`CODE_LIFETIME`]. Devices
    /// with a wrong secret are told so and logged, and the code stays valid. After this
    /// returns the code is used up.
    pub async fn accept(self, events: &EventSink) -> Result<Pending, Error> {
        let deadline = tokio::time::Instant::now() + CODE_LIFETIME;
        loop {
            let incoming = match tokio::time::timeout_at(deadline, self.endpoint.accept()).await {
                Err(_) => {
                    self.endpoint.close().await;
                    return Err(Error::new(ErrorKind::PairingExpired, "nobody used the code within 5 minutes"));
                }
                Ok(None) => return Err(Error::new(ErrorKind::PairingAbandoned, "the pairing endpoint closed")),
                Ok(Some(incoming)) => incoming,
            };
            let Ok(Ok(conn)) = tokio::time::timeout(STEP_TIMEOUT, incoming).await else { continue };
            match self.check(conn.clone()).await {
                Ok(Some((send, recv, peer))) => {
                    events(Event::PairingPeer { peer: format!("\"{}\"", peer.name), id: short_id(&conn.remote_id()) });
                    return Ok(Pending { endpoint: self.endpoint, conn, send, recv, peer });
                }
                Ok(None) => {
                    events(Event::PairingWrongCode { peer: short_id(&conn.remote_id()) });
                    let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
                }
                Err(_) => conn.close(1u32.into(), b"protocol"),
            }
        }
    }

    async fn check(&self, conn: Connection) -> Result<Option<(SendStream, RecvStream, Device)>, Error> {
        let step = async {
            let (mut send, mut recv) = conn.accept_bi().await.map_err(lost)?;
            let hello: Hello = proto::read(&mut recv).await?;
            // blake3::Hash compares in constant time.
            if blake3::hash(&hello.secret) != blake3::hash(&self.code.secret) {
                proto::write(&mut send, &Welcome::WrongCode).await?;
                let _ = send.finish();
                return Ok(None);
            }
            let welcome = Welcome::Ok { device_name: device_name(), addrs: self.receive_addrs.clone() };
            proto::write(&mut send, &welcome).await?;
            let peer = Device::new(conn.remote_id(), clean_name(&hello.device_name), hello.addrs);
            Ok(Some((send, recv, peer)))
        };
        tokio::time::timeout(STEP_TIMEOUT, step)
            .await
            .map_err(|_| Error::new(ErrorKind::ConnectionLost, "the other device went quiet"))?
    }
}

/// The side that scans the code (the phone, or `glft join`).
pub async fn join(secret: SecretKey, code: &PairingCode, receive_addrs: Vec<SocketAddr>, events: &EventSink) -> Result<Pending, Error> {
    let endpoint = net::bind_pairing(secret).await?;
    let result = async {
        let peer = net::peer_addr(code.id, &code.addrs);
        events(Event::Connecting { peer: short_id(&code.id) });
        let conn = tokio::time::timeout(STEP_TIMEOUT, endpoint.connect(peer, PAIR_ALPN))
            .await
            .map_err(|_| Error::new(ErrorKind::PairingNoAnswer, "no answer within 15 seconds"))?
            .map_err(|e| {
                let detail = net::chain(&e);
                let kind = match net::classify_unreachable(&detail) {
                    ErrorKind::LocalNetworkBlocked => ErrorKind::LocalNetworkBlocked,
                    _ => ErrorKind::PairingNoAnswer,
                };
                Error::new(kind, detail)
            })?;
        let receive_addrs = if receive_addrs.is_empty() { hint_addrs(&net::reachable_addrs(&endpoint).await) } else { receive_addrs };
        let (mut send, mut recv) = conn.open_bi().await.map_err(lost)?;
        let hello = Hello { secret: code.secret, device_name: device_name(), addrs: receive_addrs };
        proto::write(&mut send, &hello).await?;
        let welcome = tokio::time::timeout(STEP_TIMEOUT, proto::read::<Welcome>(&mut recv))
            .await
            .map_err(|_| Error::new(ErrorKind::PairingNoAnswer, "the computer went quiet"))??;
        match welcome {
            Welcome::WrongCode => Err(Error::new(ErrorKind::PairingCodeWrong, "the computer did not accept the secret")),
            Welcome::Ok { device_name, addrs } => {
                let peer = Device::new(code.id, clean_name(&device_name), addrs);
                events(Event::PairingPeer { peer: format!("\"{}\"", peer.name), id: short_id(&code.id) });
                Ok((conn, send, recv, peer))
            }
        }
    }
    .await;
    match result {
        Ok((conn, send, recv, peer)) => Ok(Pending { endpoint, conn, send, recv, peer }),
        Err(e) => {
            endpoint.close().await;
            Err(e)
        }
    }
}

/// Addresses worth putting in the code: LAN IPv4 first, then routable IPv6, few enough
/// that the QR code stays small.
fn code_addrs(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut v4: Vec<SocketAddr> = addrs.iter().copied().filter(|a| a.is_ipv4() && !a.ip().is_loopback()).collect();
    v4.sort_by_key(net::lan_rank);
    v4.truncate(3);
    let v6 = addrs
        .iter()
        .copied()
        .filter(|a| matches!(a.ip(), IpAddr::V6(ip) if !ip.is_loopback() && (ip.segments()[0] & 0xffc0) != 0xfe80))
        .take(1);
    v4.into_iter().chain(v6).collect()
}

/// Without a receiving endpoint, the best guess of where transfers will arrive: this
/// device's addresses on the preferred port.
fn hint_addrs(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
    code_addrs(addrs).into_iter().map(|a| SocketAddr::new(a.ip(), net::PREFERRED_PORT)).collect()
}

/// Names come from the other device; keep them to one short line.
fn clean_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    let name = name.trim();
    if name.is_empty() { "Unnamed device".into() } else { name.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_round_trips_and_stays_alphanumeric() {
        let code = PairingCode {
            id: SecretKey::generate().public(),
            secret: [7; 16],
            addrs: vec!["192.168.1.40:47382".parse().unwrap(), "[2001:db8::1]:5000".parse().unwrap()],
        };
        let text = code.encode();
        assert!(text.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '/'), "{text}");
        assert_eq!(PairingCode::parse(&text).unwrap(), code);
        assert_eq!(PairingCode::parse(&format!("  {}\n", text.to_lowercase())).unwrap(), code);
    }

    #[test]
    fn damaged_codes_are_explained() {
        for text in ["", "hello", "GLFT1/AAAA", "GLFT1/AAAA/BBBB/CCCC"] {
            assert_eq!(PairingCode::parse(text).unwrap_err().kind, ErrorKind::PairingCodeUnreadable, "{text}");
        }
    }
}
