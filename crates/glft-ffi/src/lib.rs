//! The bridge between the Rust core and the Android app, generated with UniFFI.
//!
//! Kotlin calls plain, short functions; long-running work runs on a tokio runtime owned
//! here, and results come back through callbacks. Offers are answered by a separate call
//! ([`Node::answer_offer`]), so Rust never blocks waiting for a person. Nothing in here
//! decides anything: it adapts glft-core's types to ones UniFFI can carry.

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io,
    os::fd::FromRawFd,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use glft_core::{
    Error, Event, ErrorKind,
    events::EventSink,
    identity, net,
    storage::{FsStorage, Partial, ReceiveStorage},
    transfer::{self, Authorize, Decide, IncomingOffer, ReceiveOptions},
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
    fn make_dir(&self, rel: String) -> Result<(), StorageError>;
    fn create_partial(&self, rel: String) -> Result<OpenedPartial, StorageError>;
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
}

// ---------------------------------------------------------------- storage adapters

struct ForeignStorage(Arc<dyn StorageBackend>);

impl ReceiveStorage for ForeignStorage {
    fn exists(&self, rel: &str) -> io::Result<bool> {
        Ok(self.0.exists(rel.into())?)
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
    fn finish(&self, partial: Partial) -> io::Result<()> {
        partial.file.sync_all()?;
        drop(partial.file);
        Ok(self.0.finish(partial.rel, partial.token)?)
    }
    fn discard(&self, partial: Partial) -> io::Result<()> {
        drop(partial.file);
        Ok(self.0.discard(partial.token)?)
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
    fn make_dir(&self, rel: &str) -> io::Result<()> {
        self.fs.make_dir(rel)
    }
    fn create_partial(&self, rel: &str) -> io::Result<Partial> {
        self.fs.create_partial(rel)
    }
    fn finish(&self, partial: Partial) -> io::Result<()> {
        self.fs.finish(partial)
    }
    fn discard(&self, partial: Partial) -> io::Result<()> {
        self.fs.discard(partial)
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

/// This device: its key, the devices allowed to connect, and receiving.
#[derive(uniffi::Object)]
pub struct Node {
    secret: iroh::SecretKey,
    data_dir: PathBuf,
    allowed: Arc<RwLock<HashSet<iroh::EndpointId>>>,
    offers: Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>,
    next_offer: Arc<AtomicU64>,
    running: Mutex<Option<Running>>,
    starting: Mutex<()>,
}

const ALLOWED_FILE: &str = "allowed-devices.txt";

#[uniffi::export]
impl Node {
    /// Loads or creates the device key and the allowed-devices list in `data_dir`.
    /// `device_name` is what other devices see, e.g. "Pixel 8 Pro".
    #[uniffi::constructor]
    pub fn new(data_dir: String, device_name: String) -> Result<Arc<Self>, GlftError> {
        glft_core::paths::set_device_name(device_name);
        let data_dir = PathBuf::from(data_dir);
        let (secret, _) = identity::load_or_create(&data_dir)?;
        let allowed = std::fs::read_to_string(data_dir.join(ALLOWED_FILE))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| net::parse_id(l).ok())
            .collect();
        Ok(Arc::new(Self {
            secret,
            data_dir,
            allowed: Arc::new(RwLock::new(allowed)),
            offers: Default::default(),
            next_offer: Arc::new(AtomicU64::new(1)),
            running: Mutex::new(None),
            starting: Mutex::new(()),
        }))
    }

    /// The device id other devices dial.
    pub fn id(&self) -> String {
        self.secret.public().to_string()
    }

    /// Allows a device to connect. Day 3's QR pairing calls this; until then, debug builds
    /// call it from an adb broadcast.
    pub fn allow_device(&self, id: String) -> Result<(), GlftError> {
        let id = net::parse_id(&id)?;
        let mut allowed = self.allowed.write().unwrap();
        allowed.insert(id);
        let text: String = allowed.iter().map(|k| format!("{k}\n")).collect();
        std::fs::write(self.data_dir.join(ALLOWED_FILE), text)
            .map_err(|e| Error::io(ErrorKind::DataDirUnwritable, &self.data_dir, e))?;
        Ok(())
    }

    pub fn allowed_devices(&self) -> Vec<String> {
        self.allowed.read().unwrap().iter().map(|k| k.to_string()).collect()
    }

    pub fn is_receiving(&self) -> bool {
        self.running.lock().unwrap().as_ref().is_some_and(|r| !r.task.is_finished())
    }

    /// Starts listening for offers. Blocks briefly while the network socket opens; call it
    /// off the main thread.
    pub fn start_receiving(&self, storage: StorageChoice, listener: Arc<dyn NodeListener>) -> Result<(), GlftError> {
        // One start at a time: a second one waits, then replaces the first.
        let _starting = self.starting.lock().unwrap();
        self.stop();
        let storage = open_storage(storage)?;
        let events = sink(listener.clone());
        let endpoint = runtime().block_on(net::bind(self.secret.clone(), None, &events))?;
        let allowed = self.allowed.clone();
        let authorize: Authorize = Arc::new(move |id| allowed.read().unwrap().contains(id));
        let decide = self.decider(listener.clone());
        let opts = ReceiveOptions { storage, decide, authorize, once: false };
        let ep = endpoint.clone();
        let task = runtime().spawn(async move {
            let result = transfer::serve(&ep, &opts, &events).await;
            listener.on_stopped(result.err().map(|e| e.kind.headline().to_string()));
        });
        *self.running.lock().unwrap() = Some(Running { endpoint, task });
        Ok(())
    }

    /// Stops listening and closes the network socket.
    pub fn stop(&self) {
        if let Some(running) = self.running.lock().unwrap().take() {
            runtime().block_on(running.endpoint.close());
            running.task.abort();
        }
        for (_, answer) in self.offers.lock().unwrap().drain() {
            let _ = answer.send(false);
        }
    }

    /// Answers an offer announced through [`NodeListener::on_offer`].
    pub fn answer_offer(&self, id: u64, accept: bool) {
        if let Some(answer) = self.offers.lock().unwrap().remove(&id) {
            let _ = answer.send(accept);
        }
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
    fn decider(&self, listener: Arc<dyn NodeListener>) -> Decide {
        let offers = self.offers.clone();
        let next = self.next_offer.clone();
        Arc::new(move |offer: IncomingOffer| {
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

fn sink(listener: Arc<dyn NodeListener>) -> EventSink {
    Arc::new(move |event: Event| {
        let at_unix_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
        let (is_error, next_step, detail) = match &event {
            Event::Failed(e) => (true, Some(e.kind.next_step().to_string()), Some(format!("[{}] {}", e.kind.id(), e.detail))),
            _ => (false, None, None),
        };
        let sentence = match &event {
            Event::Failed(e) => e.kind.headline().to_string(),
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
                        storage.finish(partial).map_err(|e| err(&rel, e))?;
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

