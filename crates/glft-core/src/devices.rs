//! Paired devices: the ones remembered on disk, and the ones paired "just this time".
//!
//! Pairing is the security boundary. A connection from a key that is in neither list is
//! closed before anything it sends is read. Remembered devices live in `devices.json` in
//! the data folder, readable by the owner only; session pairings live in memory and end
//! with the program (on the phone: when receiving is switched off).

use std::{
    fs,
    io::Write,
    net::SocketAddr,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use iroh::EndpointId;
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, ErrorKind},
    net,
};

const DEVICES_FILE: &str = "devices.json";

/// A paired device as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// The device's key, as `glft id` prints it.
    pub id: String,
    /// Its name as it last told us, e.g. "Pixel 8 Pro".
    pub name: String,
    /// Where it was last reached; tried along with local network lookup.
    #[serde(default)]
    pub addrs: Vec<SocketAddr>,
    /// Its offers are accepted without asking.
    #[serde(default)]
    pub auto_accept: bool,
    /// Unix seconds.
    pub paired_at: i64,
    #[serde(default)]
    pub last_seen: Option<i64>,
}

impl Device {
    pub fn new(id: EndpointId, name: String, addrs: Vec<SocketAddr>) -> Self {
        Self { id: id.to_string(), name, addrs, auto_accept: false, paired_at: now(), last_seen: None }
    }

    pub fn endpoint_id(&self) -> Option<EndpointId> {
        net::parse_id(&self.id).ok()
    }

    pub fn short_id(&self) -> String {
        self.id.chars().take(10).collect()
    }
}

/// How the person answered "Remember this device?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Remember {
    Yes,
    JustThisTime,
    No,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    devices: Vec<Device>,
}

/// The remembered devices plus this session's pairings.
pub struct DeviceStore {
    path: PathBuf,
    remembered: Vec<Device>,
    session: Vec<Device>,
}

impl DeviceStore {
    /// Loads `devices.json` from `data_dir`; a missing file is an empty store.
    pub fn load(data_dir: &Path) -> Result<Self, Error> {
        let path = data_dir.join(DEVICES_FILE);
        let remembered = match fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice::<File>(&bytes)
                    .map_err(|e| Error::new(ErrorKind::DevicesFileDamaged, format!("{}: {e}", path.display())))?
                    .devices
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(Error::io(ErrorKind::DataDirUnwritable, &path, e)),
        };
        Ok(Self { path, remembered, session: Vec::new() })
    }

    /// Adds or replaces a pairing; `JustThisTime` keeps it in memory only, `No` removes it.
    pub fn pair(&mut self, device: Device, remember: Remember) -> Result<(), Error> {
        self.remembered.retain(|d| d.id != device.id);
        self.session.retain(|d| d.id != device.id);
        match remember {
            Remember::Yes => {
                self.remembered.push(device);
                self.save()
            }
            Remember::JustThisTime => {
                self.session.push(device);
                self.save()
            }
            Remember::No => self.save(),
        }
    }

    /// Whether a key may connect at all.
    pub fn is_paired(&self, id: &EndpointId) -> bool {
        self.get(id).is_some()
    }

    pub fn get(&self, id: &EndpointId) -> Option<&Device> {
        let id = id.to_string();
        self.remembered.iter().chain(&self.session).find(|d| d.id == id)
    }

    /// Whether the device is remembered on disk (not just for this session).
    pub fn is_remembered(&self, id: &EndpointId) -> bool {
        let id = id.to_string();
        self.remembered.iter().any(|d| d.id == id)
    }

    pub fn auto_accept(&self, id: &EndpointId) -> bool {
        self.get(id).is_some_and(|d| d.auto_accept)
    }

    /// Remembered devices first, then session pairings, each with whether it is remembered.
    pub fn list(&self) -> Vec<(Device, bool)> {
        let remembered = self.remembered.iter().map(|d| (d.clone(), true));
        remembered.chain(self.session.iter().map(|d| (d.clone(), false))).collect()
    }

    /// Finds a device by name (ignoring case) or by the start of its id.
    pub fn find(&self, query: &str) -> Result<&Device, Error> {
        let q = query.trim();
        let all: Vec<&Device> = self.remembered.iter().chain(&self.session).collect();
        let by_name: Vec<&Device> = all.iter().copied().filter(|d| d.name.eq_ignore_ascii_case(q)).collect();
        let lower = q.to_lowercase();
        let by_start: Vec<&Device> = all.iter().copied().filter(|d| !q.is_empty() && d.name.to_lowercase().starts_with(&lower)).collect();
        let matches = if !by_name.is_empty() {
            by_name
        } else if !by_start.is_empty() {
            by_start
        } else if q.len() >= 4 {
            let q = q.to_ascii_lowercase();
            all.iter().copied().filter(|d| d.id.starts_with(&q)).collect()
        } else {
            Vec::new()
        };
        match matches.as_slice() {
            [one] => Ok(one),
            [] => Err(Error::new(ErrorKind::UnknownDevice, format!("{q:?}"))),
            many => {
                let names: Vec<String> = many.iter().map(|d| format!("{} ({})", d.name, d.short_id())).collect();
                Err(Error::new(ErrorKind::AmbiguousDevice, format!("{q:?} matches {}", names.join(", "))))
            }
        }
    }

    /// Forgets a device entirely. Returns it if it was paired.
    pub fn forget(&mut self, id: &EndpointId) -> Result<Option<Device>, Error> {
        let id = id.to_string();
        let found = self.remembered.iter().chain(&self.session).find(|d| d.id == id).cloned();
        self.remembered.retain(|d| d.id != id);
        self.session.retain(|d| d.id != id);
        self.save()?;
        Ok(found)
    }

    pub fn set_auto_accept(&mut self, id: &EndpointId, on: bool) -> Result<(), Error> {
        self.update(id, |d| d.auto_accept = on)
    }

    /// Records a successful contact: its current name and the address that worked.
    pub fn seen(&mut self, id: &EndpointId, name: Option<&str>, addrs: &[SocketAddr]) -> Result<(), Error> {
        self.update(id, |d| {
            if let Some(name) = name.filter(|n| !n.is_empty()) {
                d.name = name.to_string();
            }
            if !addrs.is_empty() {
                d.addrs = addrs.to_vec();
            }
            d.last_seen = Some(now());
        })
    }

    /// Ends every session pairing (phone: receiving switched off).
    pub fn end_session(&mut self) {
        self.session.clear();
    }

    fn update(&mut self, id: &EndpointId, f: impl FnOnce(&mut Device)) -> Result<(), Error> {
        let id = id.to_string();
        if let Some(d) = self.remembered.iter_mut().find(|d| d.id == id) {
            f(d);
            return self.save();
        }
        if let Some(d) = self.session.iter_mut().find(|d| d.id == id) {
            f(d);
        }
        Ok(())
    }

    fn save(&self) -> Result<(), Error> {
        let file = File { devices: self.remembered.clone() };
        let bytes = serde_json::to_vec_pretty(&file).expect("devices serialize");
        write_atomic(&self.path, &bytes).map_err(|e| Error::io(ErrorKind::DataDirUnwritable, &self.path, e))
    }
}

/// Writes `bytes` to `path` so that a crash leaves either the old or the new file, never half.
/// The file is readable by its owner only.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    // The rename itself survives a crash only once the folder is synced.
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    fn device(name: &str) -> (EndpointId, Device) {
        let id = SecretKey::generate().public();
        (id, Device::new(id, name.into(), vec![]))
    }

    #[test]
    fn remembered_survive_a_reload_and_session_ones_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = DeviceStore::load(dir.path()).unwrap();
        let (a, da) = device("Pixel 8 Pro");
        let (b, db) = device("Friend's laptop");
        let (c, dc) = device("Stranger");
        store.pair(da, Remember::Yes).unwrap();
        store.pair(db, Remember::JustThisTime).unwrap();
        store.pair(dc, Remember::No).unwrap();
        assert!(store.is_paired(&a) && store.is_paired(&b) && !store.is_paired(&c));

        let store = DeviceStore::load(dir.path()).unwrap();
        assert!(store.is_paired(&a));
        assert!(!store.is_paired(&b), "a session pairing ends with the program");
    }

    #[test]
    fn find_by_name_or_id_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = DeviceStore::load(dir.path()).unwrap();
        let (a, da) = device("Pixel 8 Pro");
        let prefix: String = da.id.chars().take(6).collect();
        store.pair(da, Remember::Yes).unwrap();
        assert_eq!(store.find("pixel 8 pro").unwrap().id, a.to_string());
        assert_eq!(store.find("pixel").unwrap().id, a.to_string(), "the start of a name is enough");
        assert_eq!(store.find(&prefix).unwrap().id, a.to_string());
        assert_eq!(store.find("nope").unwrap_err().kind, ErrorKind::UnknownDevice);

        store.forget(&a).unwrap();
        assert!(!store.is_paired(&a));
    }
}
