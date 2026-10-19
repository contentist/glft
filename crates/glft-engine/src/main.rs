//! `glftd`: the glft engine. One per user, started on demand by `glft` (through systemd's
//! socket activation once `glft setup` has run). It holds the endpoint, paired devices,
//! pending offers and transfers, and nothing listens on the network unless receiving is
//! switched on, which a desktop notification then shows (design rule 6).

mod notify;
mod tray;

use std::{
    collections::{BTreeMap, VecDeque},
    net::SocketAddr,
    os::{fd::FromRawFd, unix::fs::PermissionsExt},
    path::PathBuf,
    process::ExitCode,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use glft_core::{
    Error, ErrorKind, Event,
    devices::{Device, DeviceStore, Remember},
    events::EventSink,
    identity, net,
    pairing::{self, Host, PairingCode},
    paths,
    resume::Records,
    storage::FsStorage,
    transfer::{self, Authorize, Decide, IncomingOffer, ReceiveOptions, Seen},
};
use glft_engine::{Conn, DeviceInfo, ErrorInfo, Incoming, LogLine, OfferInfo, PING_LIMIT, Receiving, Reply, Request, SendInfo, Status, read_line, write_line};
use iroh::{Endpoint, SecretKey};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{broadcast, mpsc, oneshot, watch},
    task::{AbortHandle, JoinHandle},
};

const RECENT_LINES: usize = 500;

/// The log on disk, in the data folder: one JSON line per entry.
const LOG_FILE: &str = "log.jsonl";
const LOG_FILE_MAX: u64 = 1 << 20;

/// The last [`RECENT_LINES`] lines an earlier engine wrote.
fn earlier_log(data_dir: &std::path::Path) -> VecDeque<LogLine> {
    let text = std::fs::read_to_string(data_dir.join(LOG_FILE)).unwrap_or_default();
    let mut lines: VecDeque<LogLine> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    while lines.len() > RECENT_LINES {
        lines.pop_front();
    }
    lines
}

/// With nothing to do for this long (no client connected, receiving off, no send, no
/// offer), the engine exits; the next client starts it again. Overridden in seconds by
/// `GLFT_IDLE_SECS` (0: never).
const IDLE_LIMIT: Duration = Duration::from_secs(60);

/// An arriving transfer that reports no progress for this long is no longer listed.
const INCOMING_STALE: Duration = Duration::from_secs(20);

fn idle_limit() -> Option<Duration> {
    match std::env::var("GLFT_IDLE_SECS").ok().and_then(|s| s.parse::<f64>().ok()) {
        Some(s) if s <= 0.0 => None,
        Some(s) => Some(Duration::from_secs_f64(s)),
        None => Some(IDLE_LIMIT),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    // Network-level detail for diagnosing, off unless asked for, e.g.
    // `RUST_LOG=iroh=debug,iroh_mdns_address_lookup=debug,glft_core=debug`. It goes to
    // stderr, which systemd puts in the journal.
    if std::env::var_os("RUST_LOG").is_some() {
        tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_ansi(false).init();
    }
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", e.kind.headline());
            eprintln!("What to do: {}", e.kind.next_step());
            eprintln!("details: [{}] {}", e.kind.id(), e.detail);
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Error> {
    let listener = listen().await?;
    let data_dir = paths::data_dir()?;
    let recent = earlier_log(&data_dir);
    let (secret, created) = identity::load_or_create(&data_dir)?;
    let devices = DeviceStore::load(&data_dir)?;
    let engine = Arc::new(Engine {
        secret,
        data_dir,
        devices: Arc::new(Mutex::new(devices)),
        log: broadcast::channel(1024).0,
        recent: Mutex::new(recent),
        receiving: tokio::sync::Mutex::new(None),
        offers: Default::default(),
        next_offer: AtomicU64::new(1),
        sending: Default::default(),
        next_send: AtomicU64::new(1),
        incoming: Default::default(),
        changed: watch::channel(0).0,
        clients: AtomicUsize::new(0),
        quit: tokio::sync::Notify::new(),
    });
    if created {
        (engine.sink())(Event::KeyCreated { id: engine.secret.public().to_string() });
    }
    println!("glft engine {} running (pid {}), id {}", env!("CARGO_PKG_VERSION"), std::process::id(), engine.secret.public());

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
    let limit = idle_limit();
    let mut tick = tokio::time::interval(limit.map(|l| (l / 4).clamp(Duration::from_millis(250), Duration::from_secs(10))).unwrap_or(Duration::from_secs(3600)));
    let mut idle_since = Instant::now();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    idle_since = Instant::now();
                    tokio::spawn(engine.clone().handle(stream));
                }
            }
            _ = tick.tick() => {
                if engine.busy().await {
                    idle_since = Instant::now();
                } else if limit.is_some_and(|l| idle_since.elapsed() >= l) {
                    engine.log_note("Nothing needs the engine now (no window open, receiving off, nothing being sent, no pairing for this session only); it stopped and starts again when needed");
                    break;
                }
            }
            _ = engine.quit.notified() => break,
            _ = term.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    engine.stop_receiving().await;
    Ok(())
}

/// The control socket: handed over by systemd, or bound here when run by hand.
async fn listen() -> Result<UnixListener, Error> {
    let from_systemd = std::env::var("LISTEN_PID").ok().and_then(|p| p.parse::<u32>().ok()) == Some(std::process::id())
        && std::env::var("LISTEN_FDS").ok().and_then(|n| n.parse::<u32>().ok()).unwrap_or(0) >= 1;
    if from_systemd {
        // SAFETY: systemd passes the listening socket as fd 3 and nothing else owns it.
        let std_listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(3) };
        std_listener.set_nonblocking(true).map_err(engine_err)?;
        return UnixListener::from_std(std_listener).map_err(engine_err);
    }
    let path = glft_engine::socket_path()?;
    let dir = path.parent().expect("socket has a folder");
    std::fs::create_dir_all(dir).map_err(engine_err)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(engine_err)?;
    // Single instance: a socket that answers belongs to a running engine.
    if let Ok(stream) = UnixStream::connect(&path).await {
        let mut conn = Conn::new(stream);
        let _ = conn.send(&Request::Status).await;
        let pid = match tokio::time::timeout(glft_engine::HEARTBEAT, conn.recv::<Reply>()).await {
            Ok(Ok(Some(Reply::Ack { pid, .. }))) => pid.to_string(),
            _ => "unknown".into(),
        };
        return Err(Error::new(ErrorKind::EngineAlreadyRunning, format!("pid {pid}, socket {}", path.display())));
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(engine_err)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(engine_err)?;
    Ok(listener)
}

fn engine_err(e: std::io::Error) -> Error {
    Error::new(ErrorKind::EngineNotRunning, e.to_string())
}

struct ReceivingState {
    endpoint: Endpoint,
    task: JoinHandle<()>,
    folder: String,
    /// What shows that receiving is on: the panel icon, or else a notification.
    indicator: Indicator,
}

/// Held for its drop, which takes the icon or notification away.
#[allow(dead_code)]
enum Indicator {
    Icon(tray::Shown),
    Notice(Option<notify::Notice>),
}

struct PendingOffer {
    info: OfferInfo,
    answer: oneshot::Sender<bool>,
}

struct Engine {
    secret: SecretKey,
    data_dir: PathBuf,
    devices: Arc<Mutex<DeviceStore>>,
    log: broadcast::Sender<Reply>,
    recent: Mutex<VecDeque<LogLine>>,
    receiving: tokio::sync::Mutex<Option<ReceivingState>>,
    offers: Arc<Mutex<BTreeMap<u64, PendingOffer>>>,
    next_offer: AtomicU64,
    sending: Mutex<BTreeMap<u64, ActiveSend>>,
    next_send: AtomicU64,
    /// Arriving transfers by sender, with when they last made progress.
    incoming: Mutex<BTreeMap<String, (Incoming, Instant)>>,
    /// Bumped whenever status, offers or devices may have changed, for watching windows.
    changed: watch::Sender<u64>,
    /// Connected clients: a terminal command, a window.
    clients: AtomicUsize,
    quit: tokio::sync::Notify,
}

struct ActiveSend {
    info: SendInfo,
    abort: Option<AbortHandle>,
}

/// Counts a connected client while it lives.
struct ClientGuard<'a>(&'a AtomicUsize);

impl Drop for ClientGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn line(event: &Event) -> LogLine {
    match event {
        Event::Failed(e) => LogLine {
            at_unix_ms: now_ms(),
            sentence: e.kind.headline().into(),
            is_error: true,
            next_step: Some(e.kind.next_step().into()),
            detail: Some(format!("[{}] {}", e.kind.id(), e.detail)),
            id: Some(e.kind.id().into()),
        },
        _ => note(&event.to_string()),
    }
}

fn note(sentence: &str) -> LogLine {
    LogLine { at_unix_ms: now_ms(), sentence: sentence.into(), is_error: false, next_step: None, detail: None, id: None }
}

/// Replies for one client; dropped messages just mean the client has gone.
type ClientTx = mpsc::UnboundedSender<Reply>;

impl Engine {
    /// Every event goes to the engine's log, which `glft log -f` follows.
    fn sink(self: &Arc<Self>) -> EventSink {
        let engine = self.clone();
        Arc::new(move |event: Event| engine.publish(&event))
    }

    /// Events of one request go to its client as well as to the log.
    fn tee(self: &Arc<Self>, client: ClientTx) -> EventSink {
        let engine = self.clone();
        Arc::new(move |event: Event| {
            let reply = reply_of(&event, None);
            let _ = client.send(reply.clone());
            engine.publish_as(&event, reply);
        })
    }

    /// Events of send `id`: its progress is kept for [`Status::sending`] and tagged.
    fn send_sink(self: &Arc<Self>, id: u64, client: Option<ClientTx>) -> EventSink {
        let engine = self.clone();
        Arc::new(move |event: Event| {
            if let Some(a) = engine.sending.lock().unwrap().get_mut(&id) {
                match &event {
                    Event::SendProgress { done, total } => (a.info.done, a.info.total) = (*done, *total),
                    Event::Offered { totals, .. } => (a.info.summary, a.info.total) = (totals.to_string(), totals.bytes),
                    _ => {}
                }
            }
            let reply = reply_of(&event, Some(id));
            if let Some(c) = &client {
                let _ = c.send(reply.clone());
            }
            engine.publish_as(&event, reply);
        })
    }

    fn publish(&self, event: &Event) {
        self.publish_as(event, reply_of(event, None));
    }

    fn publish_as(&self, event: &Event, reply: Reply) {
        match event {
            Event::Received { peer, totals, dest, .. } => {
                notify::received(peer, &totals.to_string(), dest);
                self.incoming.lock().unwrap().remove(peer);
            }
            Event::ReceiveInterrupted { peer, .. } => {
                self.incoming.lock().unwrap().remove(peer);
            }
            Event::Progress { peer, done, total } => {
                let i = Incoming { peer: peer.clone(), done: *done, total: *total };
                self.incoming.lock().unwrap().insert(peer.clone(), (i, Instant::now()));
            }
            _ => {}
        }
        if let Reply::Line(l) = &reply {
            println!("{}", l.sentence);
            self.remember(l);
            self.changed();
        }
        let _ = self.log.send(reply);
    }

    fn log_note(&self, sentence: &str) {
        let l = note(sentence);
        println!("{sentence}");
        self.remember(&l);
        let _ = self.log.send(Reply::Line(l));
        self.changed();
    }

    /// Keeps a log line for `glft log` and windows: in memory, and on disk so the log
    /// outlives the engine stopping when nothing needs it.
    fn remember(&self, l: &LogLine) {
        let mut recent = self.recent.lock().unwrap();
        recent.push_back(l.clone());
        while recent.len() > RECENT_LINES {
            recent.pop_front();
        }
        let path = self.data_dir.join(LOG_FILE);
        if let Ok(mut line) = serde_json::to_vec(l) {
            line.push(b'\n');
            use std::io::Write;
            let appended = std::fs::OpenOptions::new().create(true).append(true).open(&path).and_then(|mut f| f.write_all(&line));
            // Cut back to the recent lines now and then, so the file stays small.
            if appended.is_ok() && std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_FILE_MAX) {
                let kept: Vec<u8> = recent.iter().filter_map(|l| serde_json::to_vec(l).ok()).flat_map(|mut v| { v.push(b'\n'); v }).collect();
                let temp = path.with_extension("jsonl.new");
                if std::fs::write(&temp, kept).is_ok() {
                    let _ = std::fs::rename(&temp, &path);
                }
            }
        }
    }

    /// Tells watching windows to fetch status and devices again.
    fn changed(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Whether anything needs the engine to keep running. A "just this time" pairing
    /// lives in the engine's memory, so the engine stays until logout or `glft quit`.
    async fn busy(&self) -> bool {
        self.clients.load(Ordering::Relaxed) > 0
            || self.devices.lock().unwrap().has_session_pairings()
            || !self.sending.lock().unwrap().is_empty()
            || !self.offers.lock().unwrap().is_empty()
            || self.receiving.lock().await.is_some()
    }

    async fn handle(self: Arc<Self>, stream: UnixStream) {
        self.clients.fetch_add(1, Ordering::Relaxed);
        let _client = ClientGuard(&self.clients);
        let (mut lines, mut write) = Conn::new(stream).split();
        let Ok(Some(request)) = read_line::<Request>(&mut lines).await else { return };
        let ack = Reply::Ack { pid: std::process::id(), version: build_id() };
        if write_line(&mut write, &ack).await.is_err() {
            return;
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<Reply>();
        let writer = tokio::spawn(async move {
            while let Some(reply) = rx.recv().await {
                if write_line(&mut write, &reply).await.is_err() {
                    break;
                }
            }
        });
        // Later lines from the client (pairing answers); the channel closes when it leaves.
        let (from_tx, from_client) = mpsc::unbounded_channel::<Request>();
        let reader = tokio::spawn(async move {
            while let Ok(Some(r)) = read_line::<Request>(&mut lines).await {
                if from_tx.send(r).is_err() {
                    break;
                }
            }
        });
        let result = self.clone().serve_request(request, tx.clone(), from_client).await;
        let _ = tx.send(match &result {
            Ok(()) => Reply::Done,
            Err(e) => Reply::Failed(ErrorInfo::from(e)),
        });
        drop(tx);
        // A client that stopped reading (a frozen window) must not hold the engine.
        let mut writer = writer;
        if tokio::time::timeout(glft_engine::HEARTBEAT, &mut writer).await.is_err() {
            writer.abort();
        }
        reader.abort();
        self.changed();
    }

    async fn serve_request(self: Arc<Self>, request: Request, tx: ClientTx, mut from_client: mpsc::UnboundedReceiver<Request>) -> Result<(), Error> {
        match request {
            Request::Status => {
                let _ = tx.send(Reply::Status(self.status().await));
                Ok(())
            }
            Request::Receive { on: true, dir, port } => self.start_receiving(dir, port, &self.tee(tx.clone())).await,
            Request::Receive { on: false, .. } => {
                if !self.stop_receiving().await {
                    let _ = tx.send(Reply::Line(note("Receiving was already off")));
                }
                Ok(())
            }
            Request::Log { follow } => {
                let mut live = self.log.subscribe();
                let recent: Vec<LogLine> = self.recent.lock().unwrap().iter().cloned().collect();
                for l in recent {
                    let _ = tx.send(Reply::Line(l));
                }
                if follow {
                    loop {
                        tokio::select! {
                            r = live.recv() => match r {
                                Ok(reply) => { let _ = tx.send(reply); }
                                Err(broadcast::error::RecvError::Lagged(_)) => {}
                                Err(_) => break,
                            },
                            gone = from_client.recv() => if gone.is_none() { break },
                        }
                    }
                }
                Ok(())
            }
            Request::Pair => self.host_pairing(tx, from_client).await,
            Request::Join { code } => self.join_pairing(&code, tx, from_client).await,
            Request::PairAnswer { .. } => Err(Error::new(ErrorKind::ProtocolError, "no pairing in progress")),
            Request::Devices => {
                let _ = tx.send(self.devices_reply());
                Ok(())
            }
            Request::Forget { device } => {
                let mut store = self.devices.lock().unwrap();
                let id = store.find(&device)?.endpoint_id();
                let gone = match id {
                    Some(id) => store.forget(&id)?,
                    None => None,
                };
                drop(store);
                if let Some(d) = gone {
                    self.log_note(&format!("Forgot \"{}\"; it can no longer send files here", d.name));
                    let _ = tx.send(Reply::Line(note(&format!("Forgot \"{}\"", d.name))));
                }
                Ok(())
            }
            Request::AutoAccept { device, on } => {
                let mut store = self.devices.lock().unwrap();
                let d = store.find(&device)?.clone();
                if let Some(id) = d.endpoint_id() {
                    store.set_auto_accept(&id, on)?;
                }
                let sentence = if on {
                    format!("Files from \"{}\" will be accepted without asking", d.name)
                } else {
                    format!("Files from \"{}\" will be asked about first", d.name)
                };
                let _ = tx.send(Reply::Line(note(&sentence)));
                Ok(())
            }
            Request::Send { device, addrs, paths, detach } => self.send(device, addrs, paths, detach, tx, from_client).await,
            Request::Offers => {
                let offers = self.offers.lock().unwrap().values().map(|o| o.info.clone()).collect();
                let _ = tx.send(Reply::Offers { offers });
                Ok(())
            }
            Request::Answer { offer, accept } => {
                let pending = self.offers.lock().unwrap().remove(&offer);
                match pending {
                    Some(p) => {
                        let _ = p.answer.send(accept);
                        Ok(())
                    }
                    None => Err(Error::new(ErrorKind::NoSuchOffer, format!("offer {offer}"))),
                }
            }
            Request::CancelSend { id } => {
                let cancelled = self.sending.lock().unwrap().remove(&id);
                match cancelled {
                    Some(a) => {
                        if let Some(abort) = a.abort {
                            abort.abort();
                        }
                        self.log_note(&format!("Sending to \"{}\" cancelled", a.info.peer));
                    }
                    None => {
                        let _ = tx.send(Reply::Line(note("That send had already finished")));
                    }
                }
                Ok(())
            }
            Request::Watch => self.watch(tx, from_client).await,
            Request::Ping => Ok(()),
            Request::Quit => {
                self.quit.notify_one();
                Ok(())
            }
        }
    }

    fn devices_reply(&self) -> Reply {
        let devices = self.devices.lock().unwrap().list();
        Reply::Devices { devices: devices.into_iter().map(|(device, remembered)| DeviceInfo { device, remembered }).collect() }
    }

    /// [`Request::Watch`]: a window's view of the engine, kept up to date.
    async fn watch(self: &Arc<Self>, tx: ClientTx, mut from_client: mpsc::UnboundedReceiver<Request>) -> Result<(), Error> {
        let mut live = self.log.subscribe();
        let mut changes = self.changed.subscribe();
        let _ = tx.send(Reply::Status(self.status().await));
        let _ = tx.send(self.devices_reply());
        let recent: Vec<LogLine> = self.recent.lock().unwrap().iter().cloned().collect();
        for l in recent {
            let _ = tx.send(Reply::Line(l));
        }
        let deadline = tokio::time::sleep(PING_LIMIT);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                r = live.recv() => match r {
                    Ok(reply) => { let _ = tx.send(reply); }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return Ok(()),
                },
                c = changes.changed() => {
                    if c.is_err() {
                        return Ok(());
                    }
                    // Many changes in a burst (5,000 small files) make one update.
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    changes.borrow_and_update();
                    let _ = tx.send(Reply::Status(self.status().await));
                    let _ = tx.send(self.devices_reply());
                }
                m = from_client.recv() => match m {
                    Some(Request::Ping) => deadline.as_mut().reset(tokio::time::Instant::now() + PING_LIMIT),
                    Some(_) => {}
                    None => return Ok(()),
                },
                _ = &mut deadline => {
                    self.log_note(&format!("A glft window stopped answering for {} seconds; the engine no longer counts it as open", PING_LIMIT.as_secs()));
                    return Err(Error::new(ErrorKind::EngineNotAnswering, "the window stopped sending its heartbeat"));
                }
            }
        }
    }

    async fn status(&self) -> Status {
        let receiving = match &*self.receiving.lock().await {
            Some(r) => Some(Receiving { folder: r.folder.clone(), addrs: net::reachable_addrs(&r.endpoint).await }),
            None => None,
        };
        let incoming = {
            let mut incoming = self.incoming.lock().unwrap();
            incoming.retain(|_, (_, at)| at.elapsed() < INCOMING_STALE);
            incoming.values().map(|(i, _)| i.clone()).collect()
        };
        let offers = self.offers.lock().unwrap();
        Status {
            pid: std::process::id(),
            version: build_id(),
            id: self.secret.public().to_string(),
            name: paths::device_name(),
            receiving,
            sending: self.sending.lock().unwrap().values().map(|a| a.info.clone()).collect(),
            incoming,
            offers: offers.len(),
            waiting: offers.values().map(|o| o.info.clone()).collect(),
        }
    }

    // ------------------------------------------------------------ receiving

    async fn start_receiving(self: &Arc<Self>, dir: Option<PathBuf>, port: Option<u16>, events: &EventSink) -> Result<(), Error> {
        let mut receiving = self.receiving.lock().await;
        if let Some(r) = receiving.take() {
            stop(r).await;
        }
        let dir = match dir {
            Some(d) => d,
            None => paths::default_receive_dir()?,
        };
        let storage = FsStorage::new(dir.clone()).map_err(|e| Error::io(ErrorKind::ReceiveDirUnwritable, &dir, e))?;
        let endpoint = net::bind(self.secret.clone(), port, events).await?;
        let folder = glft_core::storage::ReceiveStorage::describe(&storage, "");
        let devices = self.devices.clone();
        let authorize: Authorize = Arc::new(move |id| devices.lock().unwrap().is_paired(id));
        let devices = self.devices.clone();
        let seen: Seen = Arc::new(move |id, name| {
            let _ = devices.lock().unwrap().seen(id, Some(name), &[]);
        });
        let opts = Arc::new(ReceiveOptions {
            storage: Arc::new(storage),
            decide: self.decider(),
            authorize,
            records: Records::new(&self.data_dir),
            seen: Some(seen),
            once: false,
        });
        let (ep, sink) = (endpoint.clone(), self.sink());
        let task = tokio::spawn(async move {
            if let Err(e) = transfer::serve(&ep, opts, &sink).await {
                sink(Event::Failed(e));
            }
        });
        let indicator = match tray::show(self.clone(), folder.clone()).await {
            Some(icon) => Indicator::Icon(icon),
            None => Indicator::Notice(notify::receiving(self.clone(), folder.clone())),
        };
        events(Event::ReceiveFolder { dir: folder.clone() });
        *receiving = Some(ReceivingState { endpoint, task, folder, indicator });
        Ok(())
    }

    /// Returns whether receiving was on.
    async fn stop_receiving(&self) -> bool {
        let Some(r) = self.receiving.lock().await.take() else { return false };
        stop(r).await;
        for (_, offer) in std::mem::take(&mut *self.offers.lock().unwrap()) {
            let _ = offer.answer.send(false);
        }
        self.log_note("Receiving is off; nothing on this computer listens for glft now");
        true
    }

    fn decider(self: &Arc<Self>) -> Decide {
        let engine = self.clone();
        Arc::new(move |offer: IncomingOffer| {
            let engine = engine.clone();
            Box::pin(async move {
                if engine.devices.lock().unwrap().auto_accept(&offer.id) {
                    engine.log_note(&format!("Accepted automatically: {} is set to send without asking", offer.peer));
                    return true;
                }
                let id = engine.next_offer.fetch_add(1, Ordering::Relaxed);
                let info = OfferInfo { id, peer: offer.peer.clone(), summary: offer.totals.to_string(), dest: offer.dest.clone() };
                let (answer, rx) = oneshot::channel();
                engine.offers.lock().unwrap().insert(id, PendingOffer { info: info.clone(), answer });
                engine.changed();
                engine.log_note(&format!("Answer with `glft accept {id}` or `glft decline {id}`, or from the notification"));
                let _ = engine.log.send(Reply::Offer(info.clone()));
                let notice = notify::offer(engine.clone(), info);
                // Dropped unanswered (the sender left): the offer is gone either way.
                let _cleanup = Cleanup(Box::new({
                    let engine = engine.clone();
                    move || {
                        engine.offers.lock().unwrap().remove(&id);
                        drop(notice);
                        engine.changed();
                    }
                }));
                rx.await.unwrap_or(false)
            })
        })
    }

    fn answer(&self, offer: u64, accept: bool) {
        if let Some(p) = self.offers.lock().unwrap().remove(&offer) {
            let _ = p.answer.send(accept);
        }
    }

    async fn receive_addrs(&self) -> Vec<SocketAddr> {
        match &*self.receiving.lock().await {
            Some(r) => net::reachable_addrs(&r.endpoint).await,
            None => Vec::new(),
        }
    }

    // ------------------------------------------------------------ pairing

    async fn host_pairing(self: &Arc<Self>, tx: ClientTx, mut from_client: mpsc::UnboundedReceiver<Request>) -> Result<(), Error> {
        let events = self.tee(tx.clone());
        let host = Host::start(self.secret.clone(), self.receive_addrs().await).await?;
        let _ = tx.send(Reply::PairCode { code: host.code().encode() });
        let pending = tokio::select! {
            p = host.accept(&events) => p?,
            _ = closed(&mut from_client) => return Err(Error::new(ErrorKind::Cancelled, "pairing cancelled")),
        };
        self.finish_pairing(pending, tx, from_client, &events).await
    }

    async fn join_pairing(self: &Arc<Self>, code: &str, tx: ClientTx, from_client: mpsc::UnboundedReceiver<Request>) -> Result<(), Error> {
        let events = self.tee(tx.clone());
        let code = PairingCode::parse(code)?;
        let pending = pairing::join(self.secret.clone(), &code, self.receive_addrs().await, &events).await?;
        self.finish_pairing(pending, tx, from_client, &events).await
    }

    async fn finish_pairing(
        &self,
        pending: pairing::Pending,
        tx: ClientTx,
        mut from_client: mpsc::UnboundedReceiver<Request>,
        events: &EventSink,
    ) -> Result<(), Error> {
        let peer = pending.peer.clone();
        let _ = tx.send(Reply::PairPeer { name: peer.name.clone(), id: peer.id.clone() });
        let (remember, auto_accept) = match from_client.recv().await {
            Some(Request::PairAnswer { remember, auto_accept }) => (remember, auto_accept),
            _ => {
                pending.cancel().await;
                return Err(Error::new(ErrorKind::Cancelled, "pairing cancelled"));
            }
        };
        pending.finish(remember, events).await?;
        let device = Device { auto_accept: remember == Remember::Yes && auto_accept, ..peer };
        self.devices.lock().unwrap().pair(device, remember)
    }

    // ------------------------------------------------------------ sending

    async fn send(
        self: &Arc<Self>,
        device: String,
        extra: Vec<SocketAddr>,
        paths: Vec<PathBuf>,
        detach: bool,
        tx: ClientTx,
        mut from_client: mpsc::UnboundedReceiver<Request>,
    ) -> Result<(), Error> {
        // `paired`: the paired device's name, which the log uses until it tells its own.
        let (id, mut addrs, name, paired) = {
            let store = self.devices.lock().unwrap();
            match store.find(&device) {
                Ok(d) => (d.endpoint_id().ok_or_else(|| Error::new(ErrorKind::BadDeviceId, d.id.clone()))?, d.dial_addrs(), d.name.clone(), Some(d.name.clone())),
                Err(e) => match net::parse_id(&device) {
                    Ok(id) => (id, Vec::new(), net::short_id(&id), None),
                    Err(_) => return Err(e),
                },
            }
        };
        addrs.extend(extra);
        // While receiving, the same endpoint sends; otherwise a temporary one.
        let (endpoint, temporary) = match &*self.receiving.lock().await {
            Some(r) => (r.endpoint.clone(), false),
            None => (net::bind_ephemeral(self.secret.clone()).await?, true),
        };
        let peer = net::peer_addr(id, &addrs);
        let send_id = self.next_send.fetch_add(1, Ordering::Relaxed);
        let info = SendInfo { id: send_id, peer: name.clone(), summary: format!("{} item(s)", paths.len()), done: 0, total: 0 };
        self.sending.lock().unwrap().insert(send_id, ActiveSend { info, abort: None });
        self.changed();
        let events = self.send_sink(send_id, (!detach).then(|| tx.clone()));
        let engine = self.clone();
        let task = tokio::spawn(async move {
            let result = transfer::send_to(&endpoint, peer, paired.as_deref(), paths, &events).await;
            if let Ok(delivered) = &result {
                // The address that worked goes first next time; local network lookup
                // still finds the device if it has moved.
                let _ = engine.devices.lock().unwrap().seen(&id, None, delivered.addr.as_slice());
            }
            if temporary {
                endpoint.close().await;
            }
            engine.sending.lock().unwrap().remove(&send_id);
            engine.changed();
            result.map(|_| ())
        });
        if let Some(a) = self.sending.lock().unwrap().get_mut(&send_id) {
            a.abort = Some(task.abort_handle());
        }
        if detach {
            let _ = tx.send(Reply::Line(note(&format!(
                "Sending to \"{name}\" in the background; `glft status` shows it, `glft log -f` follows it"
            ))));
            return Ok(());
        }
        let abort = task.abort_handle();
        tokio::select! {
            r = task => r.map_err(|e| Error::new(ErrorKind::Cancelled, e.to_string()))?,
            _ = closed(&mut from_client) => {
                abort.abort();
                self.sending.lock().unwrap().remove(&send_id);
                self.log_note(&format!("Sending to \"{name}\" stopped: the terminal that started it was closed"));
                Err(Error::new(ErrorKind::Cancelled, "the client left"))
            }
        }
    }
}

async fn stop(r: ReceivingState) {
    drop(r.indicator);
    r.endpoint.close().await;
    r.task.abort();
}

/// Resolves when the client has gone.
async fn closed(from_client: &mut mpsc::UnboundedReceiver<Request>) {
    while from_client.recv().await.is_some() {}
}

fn reply_of(event: &Event, send: Option<u64>) -> Reply {
    match event {
        Event::Progress { peer, done, total } => Reply::Progress { done: *done, total: *total, sending: false, send: None, peer: Some(peer.clone()) },
        Event::SendProgress { done, total } => Reply::Progress { done: *done, total: *total, sending: true, send, peer: None },
        e => Reply::Line(line(e)),
    }
}

/// Identifies this engine's build: the version and when its binary was written, so a
/// client can tell a rebuilt `glftd` is not the one running.
fn build_id() -> String {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let mtime = std::env::current_exe()
            .and_then(std::fs::metadata)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("{} {mtime}", env!("CARGO_PKG_VERSION"))
    })
    .clone()
}

/// Runs a closure when dropped.
struct Cleanup(Box<dyn FnOnce() + Send>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let f = std::mem::replace(&mut self.0, Box::new(|| {}));
        f();
    }
}
