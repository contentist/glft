//! The engine's local protocol, shared by `glftd` (the engine) and `glft` (its client).
//!
//! One engine runs per user. It holds the network endpoint, the paired devices, pending
//! offers and transfers, so they outlive any one terminal command. Clients talk to it over
//! a Unix socket in the user's runtime folder, readable by the user only: one JSON
//! request per line, answered by JSON replies, one per line. Every request is
//! acknowledged straight away; a client that hears nothing within two seconds says the
//! engine is not answering (design rule 1, the heartbeat).

use std::{
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use glft_core::{
    Error, ErrorKind,
    devices::{Device, Remember},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
};

/// Overrides the runtime folder holding the socket, so two engines can run side by side
/// (tests, a second "computer"). With it set, clients start `glftd` directly, not
/// through systemd.
pub const RUNTIME_DIR_ENV: &str = "GLFT_RUNTIME_DIR";

/// How long a client waits for the engine's acknowledgement.
pub const HEARTBEAT: Duration = Duration::from_secs(2);

pub fn runtime_dir() -> Result<PathBuf, Error> {
    if let Some(dir) = std::env::var_os(RUNTIME_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|d| PathBuf::from(d).join("glft"))
        .ok_or_else(|| Error::new(ErrorKind::EngineNotRunning, "XDG_RUNTIME_DIR is not set"))
}

pub fn socket_path() -> Result<PathBuf, Error> {
    Ok(runtime_dir()?.join("engine.sock"))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    Status,
    /// Switch receiving on (into `dir`, default ~/Downloads/glft) or off.
    Receive { on: bool, dir: Option<PathBuf>, port: Option<u16> },
    /// Recent log lines; with `follow`, then everything new until the client leaves.
    Log { follow: bool },
    /// Show a pairing code and wait for a device to use it.
    Pair,
    /// Use another device's pairing code.
    Join { code: String },
    /// Sent during `Pair` or `Join`, after [`Reply::PairPeer`].
    PairAnswer { remember: Remember, auto_accept: bool },
    Devices,
    Forget { device: String },
    AutoAccept { device: String, on: bool },
    /// Send files. Paths must be absolute. Without `detach`, the transfer is cancelled
    /// when the client goes away.
    Send { device: String, addrs: Vec<SocketAddr>, paths: Vec<PathBuf>, detach: bool },
    /// Offers waiting for an answer.
    Offers,
    Answer { offer: u64, accept: bool },
    Quit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case")]
pub enum Reply {
    Ack { pid: u32, version: String },
    Line(LogLine),
    Progress { done: u64, total: u64 },
    Status(Status),
    Devices { devices: Vec<DeviceInfo> },
    PairCode { code: String },
    PairPeer { name: String, id: String },
    Offers { offers: Vec<OfferInfo> },
    Offer(OfferInfo),
    /// The request finished.
    Done,
    /// The request failed.
    Failed(ErrorInfo),
}

/// One line of the event log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub at_unix_ms: i64,
    pub sentence: String,
    pub is_error: bool,
    pub next_step: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub id: String,
    pub headline: String,
    pub next_step: String,
    pub detail: String,
}

impl From<&Error> for ErrorInfo {
    fn from(e: &Error) -> Self {
        Self {
            id: e.kind.id().into(),
            headline: e.kind.headline().into(),
            next_step: e.kind.next_step().into(),
            detail: e.detail.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub pid: u32,
    pub version: String,
    pub id: String,
    pub name: String,
    /// While receiving: the folder, and the addresses others can use.
    pub receiving: Option<Receiving>,
    pub sending: Vec<String>,
    pub offers: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receiving {
    pub folder: String,
    pub addrs: Vec<SocketAddr>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    #[serde(flatten)]
    pub device: Device,
    /// False for a "just this time" pairing.
    pub remembered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferInfo {
    pub id: u64,
    pub peer: String,
    pub summary: String,
    pub dest: String,
}

// ---------------------------------------------------------------- framing

/// One side of a connection: JSON values, one per line.
pub struct Conn {
    lines: Lines<BufReader<OwnedReadHalf>>,
    write: OwnedWriteHalf,
}

impl Conn {
    pub fn new(stream: UnixStream) -> Self {
        let (read, write) = stream.into_split();
        Self { lines: BufReader::new(read).lines(), write }
    }

    pub fn split(self) -> (Lines<BufReader<OwnedReadHalf>>, OwnedWriteHalf) {
        (self.lines, self.write)
    }

    pub async fn send<T: Serialize>(&mut self, value: &T) -> io::Result<()> {
        write_line(&mut self.write, value).await
    }

    /// The next value, or `None` when the other side has gone.
    pub async fn recv<T: DeserializeOwned>(&mut self) -> io::Result<Option<T>> {
        read_line(&mut self.lines).await
    }
}

pub async fn write_line<T: Serialize>(write: &mut OwnedWriteHalf, value: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
    line.push(b'\n');
    write.write_all(&line).await
}

pub async fn read_line<T: DeserializeOwned>(lines: &mut Lines<BufReader<OwnedReadHalf>>) -> io::Result<Option<T>> {
    match lines.next_line().await? {
        Some(line) => serde_json::from_str(&line).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------- systemd units

/// Where `glft setup` puts the user units.
pub fn unit_dir() -> Result<PathBuf, Error> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(|| Error::new(ErrorKind::NoHomeDir, "neither XDG_CONFIG_HOME nor HOME is set"))?;
    Ok(config.join("systemd/user"))
}

pub const SOCKET_UNIT: &str = "\
[Unit]
Description=glft engine socket (local control only)

[Socket]
ListenStream=%t/glft/engine.sock
SocketMode=0600
DirectoryMode=0700

[Install]
WantedBy=sockets.target
";

/// The service unit, running `glftd` from `engine`.
pub fn service_unit(engine: &Path) -> String {
    format!(
        "\
[Unit]
Description=glft engine: sends and receives files between your devices
Requires=glft.socket

[Service]
ExecStart=\"{}\"
"
,
        engine.display()
    )
}
