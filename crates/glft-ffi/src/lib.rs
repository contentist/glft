//! The bridge between the Rust core and the Android app, generated with UniFFI.
//!
//! Kotlin calls plain, short functions; long-running work runs on a tokio runtime owned
//! here, and results come back through callbacks. Offers are answered by a separate call
//! ([`Node::answer_offer`]), so Rust never blocks waiting for a person. Nothing in here
//! decides anything: it adapts glft-core's types to ones UniFFI can carry.

use std::{
    collections::HashMap,
    fs::File,
    io,
    os::fd::FromRawFd,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use glft_core::{
    Error, Event, ErrorKind,
    devices::{Device, DeviceStore, Remember},
    events::EventSink,
    identity,
    manifest::{Item, ItemKind, SourceFiles},
    net,
    pairing::{self, PairingCode},
    resume::Records,
    storage::{FsStorage, Partial, ReceiveStorage},
    transfer::{self, Authorize, Decide, IncomingOffer, ReceiveOptions, Seen},
};
use tokio::{runtime::Runtime, sync::oneshot, task::JoinHandle};

uniffi::setup_scaffolding!();

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("glft")
            .enable_all()
            .build()
            .expect("tokio runtime starts")
    })
}

// ---------------------------------------------------------------- errors

/// An error as the user sees it: a headline, a next step, and the raw detail.
#[derive(Debug, uniffi::Error)]
pub enum GlftError {
    Failed { id: String, headline: String, next_step: String, detail: String },
}

impl std::fmt::Display for GlftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Failed { headline, .. } = self;
        f.write_str(headline)
    }
}

impl From<Error> for GlftError {
    fn from(e: Error) -> Self {
        Self::Failed {
            id: e.kind.id().into(),
            headline: e.kind.headline().into(),
            next_step: e.kind.next_step().into(),
            detail: e.detail,
        }
    }
}

/// A storage failure reported by the Kotlin side.
#[derive(Debug, uniffi::Error)]
pub enum StorageError {
    Failed { reason: String, disk_full: bool, already_exists: bool },
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Failed { reason, .. } = self;
        f.write_str(reason)
    }
}

impl From<uniffi::UnexpectedUniFFICallbackError> for StorageError {
    fn from(e: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::Failed { reason: e.reason, disk_full: false, already_exists: false }
    }
}

impl From<StorageError> for io::Error {
    fn from(e: StorageError) -> Self {
        let StorageError::Failed { reason, disk_full, already_exists } = e;
        let kind = if disk_full {
            io::ErrorKind::StorageFull
        } else if already_exists {
            io::ErrorKind::AlreadyExists
        } else {
            io::ErrorKind::Other
        };
        io::Error::new(kind, reason)
    }
}

// ---------------------------------------------------------------- callbacks

/// A file opened by the platform for Rust to write into.
#[derive(uniffi::Record)]
pub struct OpenedPartial {
    /// An owned file descriptor (`ParcelFileDescriptor.detachFd()`); Rust closes it.
    pub fd: i32,
    /// The platform's handle for publishing it later, e.g. a content URI.
    pub token: String,
}

/// Receive storage implemented in Kotlin (media store or a picked folder).
#[uniffi::export(with_foreign)]
pub trait StorageBackend: Send + Sync {
    fn exists(&self, rel: String) -> Result<bool, StorageError>;
    /// A finished file is at `rel`; files still arriving (pending) do not count.
    fn published(&self, rel: String) -> Result<bool, StorageError>;
    fn make_dir(&self, rel: String) -> Result<(), StorageError>;
    fn create_partial(&self, rel: String) -> Result<OpenedPartial, StorageError>;
    /// Opens a partial from an interrupted transfer by its token, or `None` if it is gone.
    fn reopen_partial(&self, rel: String, token: String) -> Result<Option<OpenedPartial>, StorageError>;
    /// Publishes the partial under `rel`; the fd has already been synced and closed.
    fn finish(&self, rel: String, token: String) -> Result<(), StorageError>;
    fn discard(&self, token: String) -> Result<(), StorageError>;
    fn describe(&self, rel: String) -> String;
}

/// One line of the event log.
#[derive(uniffi::Record)]
pub struct LogLine {
    pub at_unix_ms: i64,
    pub sentence: String,
    pub is_error: bool,
    /// For errors: the next step, and the raw detail for the expandable part.
    pub next_step: Option<String>,
    pub detail: Option<String>,
}

/// An offer waiting for the person's answer.
#[derive(uniffi::Record)]
pub struct OfferInfo {
    pub id: u64,
    pub peer: String,
    pub summary: String,
    pub dest: String,
}

#[uniffi::export(with_foreign)]
pub trait NodeListener: Send + Sync {
    fn on_log(&self, line: LogLine);
    /// Answer with [`Node::answer_offer`]; there is no time limit.
    fn on_offer(&self, offer: OfferInfo);
    /// The offer no longer needs an answer: answered, or `missed` because the sender left.
    fn on_offer_closed(&self, id: u64, missed: bool);
    /// Receiving ended, on request or because of `error`.
    fn on_stopped(&self, error: Option<String>);
    /// Bytes verified so far in the transfer arriving now.
    fn on_progress(&self, done: u64, total: u64);
    /// A transfer arrived complete, every file verified.
    fn on_received(&self, received: ReceivedInfo);
    /// Bytes sent so far in the transfer this phone is sending.
    fn on_send_progress(&self, done: u64, total: u64);
}

/// Files chosen to send, opened by Kotlin when Rust needs them (each time, so thousands of
/// files never hold thousands of descriptors).
#[uniffi::export(with_foreign)]
pub trait SendSource: Send + Sync {
    /// Opens file `index` for reading and returns an owned, detached file descriptor
    /// (`ParcelFileDescriptor.detachFd()`); Rust closes it.
    fn open(&self, index: u32) -> Result<i32, StorageError>;
}

/// Something chosen to send: one file (`file` set), or a folder and everything below it.
#[derive(uniffi::Record)]
pub struct SendItem {
    pub name: String,
    /// The [`SendSource`] index, for a single file.
    pub file: Option<u32>,
    /// For a folder: what's below it.
    pub entries: Vec<FolderEntry>,
}

/// A file or folder below a folder being sent.
#[derive(uniffi::Record)]
pub struct FolderEntry {
    /// Relative to the folder, `/`-separated.
    pub rel: String,
    /// The [`SendSource`] index for a file; `None` for a folder.
    pub file: Option<u32>,
}

/// Which way to reach the other device.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The local network, with the mesh as a fallback if it doesn't answer.
    Auto,
    /// The local network only.
    Local,
    /// The mesh (Yggdrasil) only, never touching the local network.
    Mesh,
}

/// A finished send.
#[derive(uniffi::Record)]
pub struct SendResult {
    pub peer: String,
    pub summary: String,
    pub files: u32,
    pub secs: f64,
    /// The mesh ("Yggdrasil") if that is what carried the data at the end; `None` for the
    /// local network.
    pub via: Option<String>,
}

/// A finished incoming transfer, for the Received list.
#[derive(uniffi::Record)]
pub struct ReceivedInfo {
    pub at_unix_ms: i64,
    pub peer: String,
    pub summary: String,
    pub files: u32,
    pub dest: String,
}

/// "Remember this device?"
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq)]
pub enum RememberChoice {
    Yes,
    JustThisTime,
    No,
}

impl From<RememberChoice> for Remember {
    fn from(c: RememberChoice) -> Self {
        match c {
            RememberChoice::Yes => Remember::Yes,
            RememberChoice::JustThisTime => Remember::JustThisTime,
            RememberChoice::No => Remember::No,
        }
    }
}

impl From<Remember> for RememberChoice {
    fn from(r: Remember) -> Self {
        match r {
            Remember::Yes => RememberChoice::Yes,
            Remember::JustThisTime => RememberChoice::JustThisTime,
            Remember::No => RememberChoice::No,
        }
    }
}

/// The other device in a pairing that is waiting for "Remember this device?".
#[derive(uniffi::Record)]
pub struct PairingPeer {
    pub name: String,
    pub id: String,
}

/// A paired device.
#[derive(uniffi::Record)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    pub auto_accept: bool,
    /// False for a "just this time" pairing, which ends when receiving is switched off.
    pub remembered: bool,
    pub last_seen_unix: Option<i64>,
    /// The mesh it can also be reached over ("Yggdrasil"), if glft knows its address there.
    pub mesh: Option<String>,
}

// ---------------------------------------------------------------- storage adapters

struct ForeignSource(Arc<dyn SendSource>);

impl SourceFiles for ForeignSource {
    fn open(&self, index: usize) -> io::Result<File> {
        let fd = self.0.open(index as u32)?;
        // SAFETY: the Kotlin side detached this descriptor and hands its ownership to us.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

struct ForeignStorage(Arc<dyn StorageBackend>);

impl ReceiveStorage for ForeignStorage {
    fn exists(&self, rel: &str) -> io::Result<bool> {
        Ok(self.0.exists(rel.into())?)
    }
    fn published(&self, rel: &str) -> io::Result<bool> {
        Ok(self.0.published(rel.into())?)
    }
    fn make_dir(&self, rel: &str) -> io::Result<()> {
        Ok(self.0.make_dir(rel.into())?)
    }
    fn create_partial(&self, rel: &str) -> io::Result<Partial> {
        let opened = self.0.create_partial(rel.into())?;
        // SAFETY: the Kotlin side detached this descriptor and hands its ownership to us.
        let file = unsafe { File::from_raw_fd(opened.fd) };
        Ok(Partial { file, rel: rel.into(), token: opened.token })
    }
    fn reopen_partial(&self, rel: &str, token: &str) -> io::Result<Option<Partial>> {
        Ok(self.0.reopen_partial(rel.into(), token.into())?.map(|opened| {
            // SAFETY: as in create_partial, the descriptor's ownership is handed to us.
            let file = unsafe { File::from_raw_fd(opened.fd) };
            Partial { file, rel: rel.into(), token: opened.token }
        }))
    }
    fn finish(&self, partial: Partial, _hash: &[u8; 32]) -> io::Result<()> {
        partial.file.sync_all()?;
        drop(partial.file);
        Ok(self.0.finish(partial.rel, partial.token)?)
    }
    fn discard(&self, partial: Partial) -> io::Result<()> {
        drop(partial.file);
        Ok(self.0.discard(partial.token)?)
    }
    fn discard_abandoned(&self, token: &str) -> io::Result<()> {
        Ok(self.0.discard(token.into())?)
    }
    fn describe(&self, rel: &str) -> String {
        self.0.describe(rel.into())
    }
}

/// Where received files go: a Kotlin backend, or a plain path ("direct" mode).
#[derive(uniffi::Enum)]
pub enum StorageChoice {
    Foreign { backend: Arc<dyn StorageBackend> },
    /// A real directory, e.g. /storage/emulated/0/Download/glft, shown as `label`.
    Path { path: String, label: String },
}

/// FsStorage that reports a friendly name instead of /storage/emulated/0/...
struct LabelledFs {
    fs: FsStorage,
    root: String,
    label: String,
}

impl ReceiveStorage for LabelledFs {
    fn exists(&self, rel: &str) -> io::Result<bool> {
        self.fs.exists(rel)
    }
    fn published(&self, rel: &str) -> io::Result<bool> {
        self.fs.published(rel)
    }
    fn make_dir(&self, rel: &str) -> io::Result<()> {
        self.fs.make_dir(rel)
    }
    fn create_partial(&self, rel: &str) -> io::Result<Partial> {
        self.fs.create_partial(rel)
    }
    fn reopen_partial(&self, rel: &str, token: &str) -> io::Result<Option<Partial>> {
        self.fs.reopen_partial(rel, token)
    }
    fn finish(&self, partial: Partial, hash: &[u8; 32]) -> io::Result<()> {
        self.fs.finish(partial, hash)
    }
    fn discard(&self, partial: Partial) -> io::Result<()> {
        self.fs.discard(partial)
    }
    fn discard_abandoned(&self, token: &str) -> io::Result<()> {
        self.fs.discard_abandoned(token)
    }
    fn describe(&self, rel: &str) -> String {
        let full = self.fs.describe(rel);
        match full.strip_prefix(&self.root) {
            Some(rest) => format!("{}{rest}", self.label),
            None => full,
        }
    }
}

fn open_storage(choice: StorageChoice) -> Result<Arc<dyn ReceiveStorage>, GlftError> {
    Ok(match choice {
        StorageChoice::Foreign { backend } => Arc::new(ForeignStorage(backend)),
        StorageChoice::Path { path, label } => {
            let fs = FsStorage::new(PathBuf::from(&path))
                .map_err(|e| Error::io(ErrorKind::ReceiveDirUnwritable, path.as_ref(), e))?;
            Arc::new(LabelledFs { fs, root: path, label })
        }
    })
}

// ---------------------------------------------------------------- the node

struct Running {
    endpoint: iroh::Endpoint,
    task: JoinHandle<()>,
}

/// This device: its key, the paired devices, pairing and receiving.
#[derive(uniffi::Object)]
pub struct Node {
    secret: iroh::SecretKey,
    data_dir: PathBuf,
    devices: Arc<Mutex<DeviceStore>>,
    offers: Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>,
    next_offer: Arc<AtomicU64>,
    running: Mutex<Option<Running>>,
    starting: Mutex<()>,
    pairing: Mutex<Option<pairing::Pending>>,
    /// Stops the send in progress.
    cancel_send: Mutex<Option<oneshot::Sender<()>>>,
}

#[uniffi::export]
impl Node {
    /// Loads or creates the device key and the paired-device list in `data_dir`.
    /// `device_name` is what other devices see, e.g. "Pixel 8 Pro".
    #[uniffi::constructor]
    pub fn new(data_dir: String, device_name: String) -> Result<Arc<Self>, GlftError> {
        glft_core::paths::set_device_name(device_name);
        let data_dir = PathBuf::from(data_dir);
        let (secret, _) = identity::load_or_create(&data_dir)?;
        // The Day 2 debug list of allowed keys is replaced by pairing.
        let _ = std::fs::remove_file(data_dir.join("allowed-devices.txt"));
        let devices = DeviceStore::load(&data_dir)?;
        Ok(Arc::new(Self {
            secret,
            data_dir,
            devices: Arc::new(Mutex::new(devices)),
            offers: Default::default(),
            next_offer: Arc::new(AtomicU64::new(1)),
            running: Mutex::new(None),
            starting: Mutex::new(()),
            pairing: Mutex::new(None),
            cancel_send: Mutex::new(None),
        }))
    }

    /// The device id other devices dial.
    pub fn id(&self) -> String {
        self.secret.public().to_string()
    }

    pub fn is_receiving(&self) -> bool {
        self.running.lock().unwrap().as_ref().is_some_and(|r| !r.task.is_finished())
    }

    /// Starts listening for offers. Blocks briefly while the network socket opens; call it
    /// off the main thread.
    pub fn start_receiving(&self, storage: StorageChoice, listener: Arc<dyn NodeListener>) -> Result<(), GlftError> {
        // One start at a time: a second one waits, then replaces the first.
        let _starting = self.starting.lock().unwrap();
        self.stop_endpoint();
        let storage = open_storage(storage)?;
        let events = sink(listener.clone());
        let endpoint = runtime().block_on(net::bind(self.secret.clone(), None, &events))?;
        let devices = self.devices.clone();
        let authorize: Authorize = Arc::new(move |id| devices.lock().unwrap().is_paired(id));
        let devices = self.devices.clone();
        let seen: Seen = Arc::new(move |id, name| {
            let _ = devices.lock().unwrap().seen(id, Some(name), &[]);
        });
        let opts = Arc::new(ReceiveOptions {
            storage,
            decide: self.decider(listener.clone()),
            authorize,
            records: Records::new(&self.data_dir),
            seen: Some(seen),
            once: false,
        });
        let ep = endpoint.clone();
        let task = runtime().spawn(async move {
            let result = transfer::serve(&ep, opts, &events).await;
            listener.on_stopped(result.err().map(|e| e.kind.headline().to_string()));
        });
        *self.running.lock().unwrap() = Some(Running { endpoint, task });
        Ok(())
    }

    /// Stops listening and closes the network socket. "Just this time" pairings end here.
    pub fn stop(&self) {
        self.stop_endpoint();
        self.devices.lock().unwrap().end_session();
    }

    /// Answers an offer announced through [`NodeListener::on_offer`].
    pub fn answer_offer(&self, id: u64, accept: bool) {
        if let Some(answer) = self.offers.lock().unwrap().remove(&id) {
            let _ = answer.send(accept);
        }
    }

    /// Uses a pairing code (scanned or pasted): connects to the computer that shows it and
    /// proves this device read it. Blocks until the computer answers (seconds); call it off
    /// the main thread, then ask the person and call [`Node::answer_pairing`].
    pub fn pair(&self, code: String, listener: Arc<dyn NodeListener>) -> Result<PairingPeer, GlftError> {
        let code = PairingCode::parse(&code)?;
        let events = sink(listener);
        let receive_addrs = self.receive_addrs();
        let pending = runtime().block_on(pairing::join(self.secret.clone(), &code, receive_addrs, &events))?;
        let peer = PairingPeer { name: pending.peer.name.clone(), id: pending.peer.id.clone() };
        if let Some(old) = self.pairing.lock().unwrap().replace(pending) {
            runtime().spawn(old.cancel());
        }
        Ok(peer)
    }

    /// Sends this side's answer to "Remember this device?" and returns the other side's.
    /// Blocks until the person at the computer has answered too.
    pub fn answer_pairing(&self, remember: RememberChoice, auto_accept: bool, listener: Arc<dyn NodeListener>) -> Result<RememberChoice, GlftError> {
        let pending = self
            .pairing
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| Error::new(ErrorKind::PairingAbandoned, "no pairing is waiting for an answer"))?;
        let peer = pending.peer.clone();
        let events = sink(listener);
        let ours = Remember::from(remember);
        let theirs = runtime().block_on(pending.finish(ours, &events))?;
        let device = Device { auto_accept: ours == Remember::Yes && auto_accept, ..peer };
        self.devices.lock().unwrap().pair(device, ours)?;
        Ok(theirs.into())
    }

    /// Abandons a pairing waiting for an answer.
    pub fn cancel_pairing(&self) {
        if let Some(p) = self.pairing.lock().unwrap().take() {
            runtime().spawn(p.cancel());
        }
    }

    pub fn devices(&self) -> Vec<PairedDevice> {
        self.devices
            .lock()
            .unwrap()
            .list()
            .into_iter()
            .map(|(d, remembered)| {
                let mesh = d.mesh_addrs.iter().find_map(net::mesh_name).map(String::from);
                PairedDevice { id: d.id, name: d.name, auto_accept: d.auto_accept, remembered, last_seen_unix: d.last_seen, mesh }
            })
            .collect()
    }

    pub fn forget(&self, id: String) -> Result<(), GlftError> {
        let id = net::parse_id(&id)?;
        self.devices.lock().unwrap().forget(&id)?;
        Ok(())
    }

    pub fn set_auto_accept(&self, id: String, on: bool) -> Result<(), GlftError> {
        let id = net::parse_id(&id)?;
        self.devices.lock().unwrap().set_auto_accept(&id, on)?;
        Ok(())
    }

    /// Sends `items` to the paired device `device_id` and blocks until every file is
    /// verified there, the send fails, or [`Node::cancel_send`] is called. A lost
    /// connection is retried. Run it from a background job.
    pub fn send(&self, device_id: String, items: Vec<SendItem>, source: Arc<dyn SendSource>, route: Route, listener: Arc<dyn NodeListener>) -> Result<SendResult, GlftError> {
        let id = net::parse_id(&device_id)?;
        let device = self
            .devices
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::UnknownDevice, device_id.clone()))?;
        let items: Vec<Item> = items
            .into_iter()
            .map(|i| Item {
                name: i.name,
                kind: match i.file {
                    Some(f) => ItemKind::File(f as usize),
                    None => ItemKind::Folder(i.entries.into_iter().map(|e| (e.rel, e.file.map(|f| f as usize))).collect()),
                },
            })
            .collect();
        let events = sink(listener);
        let (cancel, cancelled) = oneshot::channel();
        *self.cancel_send.lock().unwrap() = Some(cancel);
        let addrs = match route {
            Route::Auto => device.dial_addrs(),
            Route::Local => device.addrs.clone(),
            Route::Mesh => device.mesh_addrs.clone(),
        };
        if addrs.is_empty() && route == Route::Mesh {
            return Err(Error::new(ErrorKind::PeerUnreachable, format!("glft knows no mesh address for {}", device.name)).into());
        }
        // Send from the receiving endpoint if there is one, so the other side sees the
        // same address; otherwise from a short-lived one with the same key. Over the mesh,
        // always a separate one that never looks at the local network.
        let receiving = self.running.lock().unwrap().as_ref().map(|r| r.endpoint.clone());
        let secret = self.secret.clone();
        let peer = net::peer_addr(id, &addrs);
        let files: Arc<dyn SourceFiles> = Arc::new(ForeignSource(source));
        let name = device.name.clone();
        let result = runtime().block_on(async move {
            let (endpoint, temporary) = match (route, receiving) {
                (Route::Mesh, _) => (net::bind_without_discovery(secret).await?, true),
                (_, Some(ep)) => (ep, false),
                (_, None) => (net::bind_ephemeral(secret).await?, true),
            };
            let result = tokio::select! {
                r = transfer::send_items(&endpoint, peer, Some(&name), items, files, &events) => r,
                _ = cancelled => Err(Error::new(ErrorKind::Cancelled, "the send was cancelled")),
            };
            if temporary {
                endpoint.close().await;
            }
            result
        });
        *self.cancel_send.lock().unwrap() = None;
        match result {
            Ok(delivered) => {
                let _ = self.devices.lock().unwrap().seen(&id, None, delivered.addr.as_slice());
                Ok(SendResult { peer: device.name, summary: delivered.totals.to_string(), files: delivered.totals.files as u32, secs: delivered.secs, via: delivered.via.map(String::from) })
            }
            Err(e) => Err(explain_network(e).into()),
        }
    }

    /// Stops the send in progress, if any. What already arrived is kept, and sending the
    /// same files again continues from there.
    pub fn cancel_send(&self) {
        if let Some(cancel) = self.cancel_send.lock().unwrap().take() {
            let _ = cancel.send(());
        }
    }

    /// What the phone knows about its network, so failures can be explained: whether a
    /// VPN is active, and whether Android blocks this app from the local network (as
    /// "Block connections without VPN" does).
    pub fn set_network_state(&self, vpn_active: bool, lan_blocked: bool) {
        VPN_ACTIVE.store(vpn_active, Ordering::Relaxed);
        LAN_BLOCKED.store(lan_blocked, Ordering::Relaxed);
    }

    /// Times the storage path the way a real transfer uses it: `files` files of `size`
    /// bytes in folders of 100, each created, written, synced and published.
    pub fn benchmark_storage(&self, storage: StorageChoice, files: u32, size: u32) -> Result<String, GlftError> {
        let storage = open_storage(storage)?;
        runtime()
            .block_on(async move { tokio::task::spawn_blocking(move || bench(storage.as_ref(), files, size)).await })
            .map_err(|e| GlftError::from(Error::new(ErrorKind::ReceiveDirUnwritable, e.to_string())))?
    }
}

impl Node {
    fn stop_endpoint(&self) {
        if let Some(Running { endpoint, task }) = self.running.lock().unwrap().take() {
            let ports: Vec<u16> = endpoint.bound_sockets().iter().map(|a| a.port()).collect();
            // Cancel first: a serve that ends because its endpoint closed reports
            // "stopped", which here would switch the app's receiving switch off while the
            // replacement is already being started.
            runtime().block_on(async {
                task.abort();
                let _ = task.await;
                endpoint.close().await;
            });
            drop(endpoint);
            // The port is free only once every connection has let go of the endpoint. A
            // restart (new Wi-Fi address) should get the same port back, not a random one.
            if let Some(&port) = ports.first() {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).is_err() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        for (_, answer) in self.offers.lock().unwrap().drain() {
            let _ = answer.send(false);
        }
    }

    /// Where this phone receives, to tell a computer during pairing.
    fn receive_addrs(&self) -> Vec<std::net::SocketAddr> {
        let endpoint = self.running.lock().unwrap().as_ref().map(|r| r.endpoint.clone());
        match endpoint {
            Some(ep) => runtime().block_on(net::reachable_addrs(&ep)),
            None => Vec::new(),
        }
    }

    fn decider(&self, listener: Arc<dyn NodeListener>) -> Decide {
        let offers = self.offers.clone();
        let next = self.next_offer.clone();
        let devices = self.devices.clone();
        Arc::new(move |offer: IncomingOffer| {
            if devices.lock().unwrap().auto_accept(&offer.id) {
                return Box::pin(async { true });
            }
            let id = next.fetch_add(1, Ordering::Relaxed);
            let (tx, rx) = oneshot::channel();
            offers.lock().unwrap().insert(id, tx);
            listener.on_offer(OfferInfo { id, peer: offer.peer, summary: offer.totals.to_string(), dest: offer.dest });
            let guard = OfferGuard { id, offers: offers.clone(), listener: listener.clone(), answered: false };
            Box::pin(async move {
                let mut guard = guard;
                let accepted = rx.await.unwrap_or(false);
                guard.answered = true;
                accepted
            })
        })
    }
}

/// Tells Kotlin when an offer no longer needs an answer. If the future is dropped
/// unanswered, the sender left: the offer was missed.
struct OfferGuard {
    id: u64,
    offers: Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>,
    listener: Arc<dyn NodeListener>,
    answered: bool,
}

impl Drop for OfferGuard {
    fn drop(&mut self) {
        self.offers.lock().unwrap().remove(&self.id);
        self.listener.on_offer_closed(self.id, !self.answered);
    }
}

static VPN_ACTIVE: AtomicBool = AtomicBool::new(false);
static LAN_BLOCKED: AtomicBool = AtomicBool::new(false);

/// A VPN in lockdown makes the local network look unreachable; say so instead.
fn explain_network(e: Error) -> Error {
    let unreachable = matches!(e.kind, ErrorKind::PeerUnreachable | ErrorKind::LocalNetworkBlocked | ErrorKind::ConnectionLost);
    if unreachable && VPN_ACTIVE.load(Ordering::Relaxed) && LAN_BLOCKED.load(Ordering::Relaxed) {
        Error::new(ErrorKind::VpnLockdown, format!("{} ({})", e.detail, e.kind.id()))
    } else {
        e
    }
}

fn sink(listener: Arc<dyn NodeListener>) -> EventSink {
    Arc::new(move |event: Event| {
        let at_unix_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
        if let Event::Progress { done, total, .. } = event {
            listener.on_progress(done, total);
            return;
        }
        if let Event::SendProgress { done, total } = event {
            listener.on_send_progress(done, total);
            return;
        }
        if let Event::Received { peer, totals, dest, .. } = &event {
            listener.on_received(ReceivedInfo {
                at_unix_ms,
                peer: peer.clone(),
                summary: totals.to_string(),
                files: totals.files as u32,
                dest: dest.clone(),
            });
        }
        let (is_error, next_step, detail) = match &event {
            Event::Failed(e) => (true, Some(e.kind.next_step().to_string()), Some(format!("[{}] {}", e.kind.id(), e.detail))),
            _ => (false, None, None),
        };
        let sentence = match &event {
            Event::Failed(e) => e.kind.headline().to_string(),
            Event::ViaMesh { mesh, .. } if LAN_BLOCKED.load(Ordering::Relaxed) => {
                format!("The local network is blocked by a VPN (lockdown); sending over {mesh} instead")
            }
            _ => event.to_string(),
        };
        listener.on_log(LogLine { at_unix_ms, sentence, is_error, next_step, detail });
    })
}

/// Files handled at once, as a real transfer does (`transfer::PARALLEL_FILES`).
const BENCH_PARALLEL: u32 = 8;

fn bench(storage: &dyn ReceiveStorage, files: u32, size: u32) -> Result<String, GlftError> {
    use std::io::Write;
    use std::sync::atomic::AtomicU32;
    let err = |rel: &str, e: io::Error| GlftError::from(Error::io(ErrorKind::ReceiveDirUnwritable, rel.as_ref(), e));
    let root = glft_core::manifest::free_name("glft-benchmark", true, |n| storage.exists(n).unwrap_or(true));
    let data = vec![0x5a_u8; size as usize];
    let all = Instant::now();
    let folders = files.div_ceil(100);
    for f in 0..folders {
        let dir = format!("{root}/folder{f:02}");
        storage.make_dir(&dir).map_err(|e| err(&dir, e))?;
    }
    let t_dirs = all.elapsed().as_secs_f64();
    // Per-phase time summed over all workers, in microseconds.
    let (create, write, finish) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    let next = AtomicU32::new(0);
    let first_error = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..BENCH_PARALLEL {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= files || first_error.lock().unwrap().is_some() {
                        return;
                    }
                    let rel = format!("{root}/folder{:02}/file{i:04}.bin", i / 100);
                    let step = || -> Result<(), GlftError> {
                        let t = Instant::now();
                        let mut partial = storage.create_partial(&rel).map_err(|e| err(&rel, e))?;
                        create.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
                        let t = Instant::now();
                        partial.file.write_all(&data).map_err(|e| err(&rel, e))?;
                        write.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
                        let t = Instant::now();
                        storage.finish(partial, &[0; 32]).map_err(|e| err(&rel, e))?;
                        finish.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
                        Ok(())
                    };
                    if let Err(e) = step() {
                        first_error.lock().unwrap().get_or_insert(e);
                    }
                }
            });
        }
    });
    if let Some(e) = first_error.into_inner().unwrap() {
        return Err(e);
    }
    let total = all.elapsed().as_secs_f64();
    let per = |a: &AtomicU64| a.load(Ordering::Relaxed) as f64 / 1000.0 / files.max(1) as f64;
    Ok(format!(
        "{files} files × {size} bytes, {BENCH_PARALLEL} at a time, into {}: total {total:.1} s ({:.1} ms/file) — folders {t_dirs:.2} s; per file: create {:.1} ms, write {:.1} ms, publish {:.1} ms",
        storage.describe(&format!("{root}/x")),
        total * 1000.0 / files.max(1) as f64,
        per(&create),
        per(&write),
        per(&finish),
    ))
}

/// Version of the core, shown in the app's about line.
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// The default receive folder name under Download.
#[uniffi::export]
pub fn default_folder_name() -> String {
    "glft".into()
}


// ---------------------------------------------------------------- debug log

/// Sends the core's and iroh's own logging (`tracing`) to Android's log, tag `glft-net`, so
/// network problems can be followed over `adb logcat`. For debug builds; `filter` is a
/// `RUST_LOG`-style filter such as `iroh=debug,glft_core=debug`.
#[uniffi::export]
pub fn enable_debug_log(filter: String) {
    let filter = tracing_subscriber::EnvFilter::try_new(filter).unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .without_time()
        .with_writer(|| AndroidLog(Vec::new()))
        .try_init();
}

/// One log record, written to Android's log when complete.
struct AndroidLog(Vec<u8>);

impl io::Write for AndroidLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for AndroidLog {
    fn drop(&mut self) {
        let text = String::from_utf8_lossy(&self.0);
        android_log(text.trim_end());
    }
}

#[cfg(target_os = "android")]
fn android_log(text: &str) {
    use std::ffi::{CString, c_char, c_int};
    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }
    const DEBUG: c_int = 3;
    let (Ok(tag), Ok(text)) = (CString::new("glft-net"), CString::new(text.replace('\0', " "))) else { return };
    // SAFETY: both are valid NUL-terminated strings that outlive the call.
    unsafe {
        __android_log_write(DEBUG, tag.as_ptr(), text.as_ptr());
    }
}

#[cfg(not(target_os = "android"))]
fn android_log(text: &str) {
    eprintln!("{text}");
}
