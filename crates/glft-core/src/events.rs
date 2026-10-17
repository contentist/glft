//! The event log: one plain sentence per step.
//!
//! The terminal program prints these as they happen, and the window and the phone
//! will show the same sentences. The sink stamps the time, so events carry none.

use std::{fmt, net::SocketAddr, sync::Arc};

use crate::error::Error;

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
    Offered { peer: String, totals: Totals },
    OfferReceived { peer: String, totals: Totals, dest: String },
    Missed { peer: String, totals: Totals },
    Refused { peer: String },
    Accepted { peer: String },
    Declined { peer: String },
    Renamed { from: String, to: String },
    FileVerified { path: String, saved_to: String },
    FileSent { path: String },
    Finished { peer: String, totals: Totals, secs: f64 },
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
            Self::Finished { peer, totals, secs } => {
                let rate = if *secs > 0.0 { totals.bytes as f64 / secs } else { 0.0 };
                write!(
                    f,
                    "All {} verified with {peer} ({}, {:.1} s, {}/s)",
                    plural(totals.files, "file"),
                    human_bytes(totals.bytes),
                    secs,
                    human_bytes(rate as u64)
                )
            }
            Self::Failed(e) => write!(f, "{} {}", e.kind.headline(), e.kind.next_step()),
        }
    }
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
