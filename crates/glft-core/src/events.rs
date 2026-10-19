//! The event log: one plain sentence per step.
//!
//! The terminal program prints these as they happen, and the window and the phone
//! will show the same sentences. The sink stamps the time, so events carry none.

use std::{fmt, net::SocketAddr, sync::Arc};

use crate::{
    devices::Remember,
    error::Error,
};

/// Receives every event. Must be cheap; it is called from the transfer tasks.
pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

/// A sink that drops everything, for tests and callers that do not log.
pub fn discard() -> EventSink {
    Arc::new(|_| {})
}

/// What an offer contains, as a person would count it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    pub folders: usize,
    pub files: usize,
    pub bytes: u64,
}

impl fmt::Display for Totals {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.folders > 0 {
            write!(f, "{}, ", plural(self.folders, "folder"))?;
        }
        write!(f, "{}, {}", plural(self.files, "file"), human_bytes(self.bytes))
    }
}

#[derive(Debug)]
pub enum Event {
    KeyCreated { id: String },
    PortInUse { port: u16, holder: Option<String>, actual: u16 },
    Listening { addrs: Vec<SocketAddr> },
    ReceiveFolder { dir: String },
    Hashing { items: usize },
    Connecting { peer: String },
    Connected { peer: String },
    /// The connection runs over a mesh network (Yggdrasil) rather than the local network.
    ViaMesh { peer: String, mesh: &'static str },
    Offered { peer: String, totals: Totals },
    OfferReceived { peer: String, totals: Totals, dest: String },
    Missed { peer: String, totals: Totals },
    Refused { peer: String },
    Accepted { peer: String },
    Declined { peer: String },
    Renamed { from: String, to: String },
    FileVerified { path: String, saved_to: String },
    FileSent { path: String },
    /// Receiving: bytes verified so far in this transfer.
    Progress { peer: String, done: u64, total: u64 },
    /// Bytes sent so far; the receiver verifies them as they arrive.
    SendProgress { done: u64, total: u64 },
    /// Receiving: an interrupted transfer continues without asking again.
    ResumingReceive { peer: String, accepted_at: i64, files_done: usize, files: usize, bytes_left: u64 },
    /// Sending: the receiver already had part of it.
    ResumingSend { peer: String, files_done: usize, files: usize, bytes_left: u64 },
    /// Sending: the connection broke; trying again until it works or is cancelled.
    WaitingToResume { peer: String, files_done: usize, files: usize },
    /// Receiving: the sender's connection dropped mid-transfer; it reconnects by itself.
    ReceiveInterrupted { peer: String, done: u64, total: u64 },
    PairingPeer { peer: String, id: String },
    PairingWrongCode { peer: String },
    Paired { peer: String, ours: Remember, theirs: Remember },
    /// Sending: everything arrived and was verified.
    /// `via` names the mesh (Yggdrasil) if that is what carried the data at the end.
    /// `saved`: where the receiver says it put each item, if it said.
    Finished { peer: String, totals: Totals, secs: f64, via: Option<&'static str>, saved: Vec<String> },
    /// The connection moved to another route while the transfer ran.
    RouteChanged { peer: String, via: &'static str },
    /// Receiving: everything arrived and was verified.
    Received { peer: String, totals: Totals, dest: String, secs: f64, items: Vec<Placed> },
    Failed(Error),
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyCreated { id } => write!(f, "Created this device's key; its id is {id}"),
            Self::PortInUse { port, holder: Some(h), actual } => write!(
                f,
                "Port {port} is in use by {h}, so glft is listening on port {actual} instead"
            ),
            Self::PortInUse { port, holder: None, actual } => write!(
                f,
                "Port {port} is in use by a program glft cannot see (another user's, or the system's), so glft is listening on port {actual} instead"
            ),
            Self::Listening { addrs } => {
                let list: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
                write!(f, "Listening on {}", list.join(", "))
            }
            Self::ReceiveFolder { dir } => write!(f, "Files will be saved to {dir}"),
            Self::Hashing { items } => {
                write!(f, "Reading {} and computing fingerprints", plural(*items, "item"))
            }
            Self::Connecting { peer } => write!(f, "Connecting to {peer}"),
            Self::Connected { peer } => write!(f, "Connected to {peer}; its key matches"),
            Self::ViaMesh { peer, mesh } => write!(f, "Reaching {peer} over {mesh}, not the local network"),
            Self::Offered { peer, totals } => write!(f, "Offered {totals} to {peer}; waiting for an answer"),
            Self::OfferReceived { peer, totals, dest } => write!(f, "{peer} offers {totals}, to be saved to {dest}"),
            Self::Missed { peer, totals } => write!(f, "Missed: {peer} stopped waiting before {totals} could be accepted"),
            Self::Refused { peer } => {
                write!(f, "Refused a connection from {peer}, a device this one is not paired with")
            }
            Self::Accepted { peer } => write!(f, "{peer} accepted"),
            Self::Declined { peer } => write!(f, "{peer} declined"),
            Self::Renamed { from, to } => write!(f, "\"{from}\" already exists here; saving as \"{to}\""),
            Self::FileVerified { path, saved_to } => write!(f, "{path} verified, saved to {saved_to}"),
            Self::FileSent { path } => write!(f, "{path} sent"),
            Self::Progress { done, total, .. } => write!(f, "{} of {} verified", human_bytes(*done), human_bytes(*total)),
            Self::SendProgress { done, total } => write!(f, "{} of {} sent", human_bytes(*done), human_bytes(*total)),
            Self::ResumingReceive { peer, accepted_at, files_done, files, bytes_left } => write!(
                f,
                "Resuming the transfer from {peer} you accepted at {}: {files_done} of {} already here, {} left",
                local_time(*accepted_at),
                plural(*files, "file"),
                human_bytes(*bytes_left)
            ),
            Self::ResumingSend { peer, files_done, files, bytes_left } => write!(
                f,
                "Resuming: {peer} already has {files_done} of {}; {} left",
                plural(*files, "file"),
                human_bytes(*bytes_left)
            ),
            Self::ReceiveInterrupted { peer, done, total } => write!(
                f,
                "Connection to {peer} lost with {} of {} here; it continues by itself when {peer} reconnects, nothing to do here",
                human_bytes(*done),
                human_bytes(*total)
            ),
            Self::WaitingToResume { peer, files_done, files } => write!(
                f,
                "Connection to {peer} lost with {files_done} of {} done; waiting to resume",
                plural(*files, "file")
            ),
            Self::PairingPeer { peer, id } => write!(f, "{peer} ({id}) used the code; its key matches"),
            Self::PairingWrongCode { peer } => {
                write!(f, "A device ({peer}) tried to pair with a wrong code; the code still works")
            }
            Self::Paired { peer, ours, theirs } => {
                match ours {
                    Remember::Yes => write!(f, "Paired with {peer}; this device remembers it")?,
                    Remember::JustThisTime => write!(f, "Paired with {peer} for this session only")?,
                    Remember::No => write!(f, "Did not pair with {peer}")?,
                }
                match (ours, theirs) {
                    (Remember::No, _) => Ok(()),
                    (_, Remember::No) => write!(f, ". {peer} chose not to pair, so it will refuse files from this device"),
                    (_, Remember::JustThisTime) => write!(f, ". {peer} paired for its current session only"),
                    (_, Remember::Yes) => write!(f, ". {peer} remembers this device too"),
                }
            }
            Self::Finished { peer, totals, secs, via, saved } => {
                let rate = if *secs > 0.0 { totals.bytes as f64 / secs } else { 0.0 };
                let over = via.map(|v| format!(" over {v}")).unwrap_or_default();
                write!(
                    f,
                    "{} verified with {peer}{over} ({}, {:.1} s, {}/s)",
                    all_files(totals.files),
                    human_bytes(totals.bytes),
                    secs,
                    human_bytes(rate as u64)
                )?;
                if !saved.is_empty() {
                    write!(f, "; saved on {peer} as {}", saved.join(", "))?;
                }
                Ok(())
            }
            Self::RouteChanged { peer, via } => write!(f, "The connection to {peer} moved to {via}"),
            Self::Received { peer, totals, dest, secs, items } => {
                let rate = if *secs > 0.0 { totals.bytes as f64 / secs } else { 0.0 };
                let places: Vec<&str> = items.iter().map(|i| i.place.as_str()).collect();
                let at = if places.is_empty() { dest.clone() } else { places.join(", ") };
                write!(
                    f,
                    "{} from {peer} verified; saved as {at} ({}, {:.1} s, {}/s)",
                    all_files(totals.files),
                    human_bytes(totals.bytes),
                    secs,
                    human_bytes(rate as u64)
                )
            }
            Self::Failed(e) => write!(f, "{} {}", e.kind.headline(), e.kind.next_step()),
        }
    }
}

/// One item of a received transfer and where it is now.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Placed {
    /// The name it arrived under ("Album (2)" if "Album" was taken).
    pub name: String,
    pub is_dir: bool,
    /// Where it is, as people say it: "~/Downloads/glft/Album".
    pub place: String,
    /// The same as a path on this device, if it has one, for "Show in folder".
    pub path: Option<std::path::PathBuf>,
}

/// "14:03" today, "Mon 14:03" otherwise.
pub fn local_time(unix: i64) -> String {
    use chrono::{Local, TimeZone};
    let Some(t) = Local.timestamp_opt(unix, 0).single() else { return "an earlier time".into() };
    if t.date_naive() == Local::now().date_naive() { t.format("%H:%M").to_string() } else { t.format("%a %d %b %H:%M").to_string() }
}

/// "The file" or "All 14 files".
fn all_files(n: usize) -> String {
    if n == 1 { "The file".into() } else { format!("All {n} files") }
}

pub fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}

/// Decimal units, as file managers on Linux and Android show them.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return format!("{n} bytes");
    }
    let mut v = n as f64;
    let mut unit = "";
    for u in UNITS {
        v /= 1000.0;
        unit = u;
        if v < 1000.0 {
            break;
        }
    }
    if v < 10.0 { format!("{v:.1} {unit}") } else { format!("{v:.0} {unit}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_read_like_the_plan() {
        let t = Totals { folders: 1, files: 14, bytes: 412_000_000 };
        assert_eq!(t.to_string(), "1 folder, 14 files, 412 MB");
        let t = Totals { folders: 0, files: 1, bytes: 999 };
        assert_eq!(t.to_string(), "1 file, 999 bytes");
    }

    #[test]
    fn bytes() {
        assert_eq!(human_bytes(1_500), "1.5 KB");
        assert_eq!(human_bytes(10_000_000_000), "10 GB");
    }
}
