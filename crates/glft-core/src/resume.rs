//! Records of incoming transfers, so an interrupted one continues where it stopped.
//!
//! The receiver keeps one small file per unfinished transfer in `transfers/` in its data
//! folder: who sent it, which manifest, where it is being saved, and for each file whether
//! it is waiting, partly here (with how many bytes are verified and synced), or done. The
//! same sender offering the same manifest again continues from the record without asking:
//! the person already accepted it.
//!
//! A record is saved at most once a second, and a verified offset is recorded only after
//! the data before it has been synced, so after a crash the partial file is cut back to
//! the recorded offset and nothing unverified is kept.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use iroh::EndpointId;
use serde::{Deserialize, Serialize};

use crate::{
    devices::{now, write_atomic},
    manifest::Manifest,
};

/// Unfinished transfers older than this are removed with their partial files. Matches
/// how long Android keeps pending media store entries.
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

const SAVE_EVERY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileState {
    Waiting,
    /// Partly here: the first `verified` bytes are checked and on disk.
    Partial { token: String, verified: u64 },
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub sender: String,
    /// BLAKE3 of the manifest, hex.
    pub manifest: String,
    /// The receive folder as `ReceiveStorage::describe("")` gave it.
    pub storage: String,
    pub accepted_at: i64,
    pub updated_at: i64,
    /// Offered top-level names and the names they are saved under ("Album" → "Album (2)").
    pub renames: Vec<(String, String)>,
    pub files: Vec<FileState>,
}

/// The fingerprint that identifies "the same transfer".
pub fn manifest_hash(manifest: &Manifest) -> String {
    let bytes = postcard::to_stdvec(manifest).expect("manifest serializes");
    blake3::hash(&bytes).to_hex().to_string()
}

/// The `transfers/` folder.
#[derive(Debug, Clone)]
pub struct Records {
    dir: PathBuf,
}

impl Records {
    pub fn new(data_dir: &Path) -> Self {
        Self { dir: data_dir.join("transfers") }
    }

    fn path(&self, sender: &str, manifest: &str) -> PathBuf {
        let sender: String = sender.chars().take(16).collect();
        let manifest: String = manifest.chars().take(32).collect();
        self.dir.join(format!("{sender}-{manifest}.json"))
    }

    pub fn load(&self, sender: &EndpointId, manifest: &str) -> Option<Record> {
        let bytes = fs::read(self.path(&sender.to_string(), manifest)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn remove(&self, record: &Record) {
        let _ = fs::remove_file(self.path(&record.sender, &record.manifest));
    }

    /// Records not updated for [`MAX_AGE`], removed from disk; the caller discards their
    /// partial files.
    pub fn take_expired(&self) -> Vec<Record> {
        let Ok(entries) = fs::read_dir(&self.dir) else { return Vec::new() };
        let cutoff = now() - MAX_AGE.as_secs() as i64;
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            match fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Record>(&b).ok()) {
                Some(r) if r.updated_at < cutoff => {
                    let _ = fs::remove_file(&path);
                    out.push(r);
                }
                Some(_) => {}
                None => {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        out
    }

    /// Starts tracking a transfer, saving the record straight away.
    pub fn live(&self, record: Record) -> Live {
        let live = Live {
            path: self.path(&record.sender, &record.manifest),
            state: Mutex::new(State { record, last_save: Instant::now(), dirty: true, finished: false }),
        };
        live.flush();
        live
    }
}

struct State {
    record: Record,
    last_save: Instant,
    dirty: bool,
    finished: bool,
}

/// A record being updated while files arrive. Saved at most once a second, and once
/// more when dropped, so a cancelled transfer keeps its progress.
pub struct Live {
    path: PathBuf,
    state: Mutex<State>,
}

impl Live {
    pub fn get(&self, index: usize) -> FileState {
        self.state.lock().unwrap().record.files[index].clone()
    }

    pub fn renames(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().record.renames.clone()
    }

    pub fn set(&self, index: usize, file: FileState) {
        let mut state = self.state.lock().unwrap();
        state.record.files[index] = file;
        state.dirty = true;
        if state.last_save.elapsed() >= SAVE_EVERY {
            Self::save(&self.path, &mut state);
        }
    }

    pub fn flush(&self) {
        Self::save(&self.path, &mut self.state.lock().unwrap());
    }

    /// The transfer is complete: the record goes.
    pub fn finish(&self) {
        let mut state = self.state.lock().unwrap();
        state.finished = true;
        let _ = fs::remove_file(&self.path);
    }

    fn save(path: &Path, state: &mut State) {
        if !state.dirty || state.finished {
            return;
        }
        state.record.updated_at = now();
        let bytes = serde_json::to_vec(&state.record).expect("record serializes");
        // A record that cannot be saved costs only the ability to resume.
        if write_atomic(path, &bytes).is_ok() {
            state.dirty = false;
        }
        state.last_save = Instant::now();
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        Self::save(&self.path, self.state.get_mut().unwrap());
    }
}
