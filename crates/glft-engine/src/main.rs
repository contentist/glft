//! `glftd`: the glft engine. One per user, started on demand by `glft` (through systemd's
//! socket activation once `glft setup` has run). It holds the endpoint, paired devices,
//! pending offers and transfers, and nothing listens on the network unless receiving is
//! switched on, which a desktop notification then shows (design rule 6).

mod notify;

use std::{
    collections::{BTreeMap, VecDeque},
    net::SocketAddr,
    os::{fd::FromRawFd, unix::fs::PermissionsExt},
    path::PathBuf,
    process::ExitCode,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
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
use glft_engine::{Conn, DeviceInfo, ErrorInfo, LogLine, OfferInfo, Receiving, Reply, Request, Status, read_line, write_line};
use iroh::{Endpoint, SecretKey};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};

const RECENT_LINES: usize = 500;

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
    let (secret, created) = identity::load_or_create(&data_dir)?;
    let devices = DeviceStore::load(&data_dir)?;
    let engine = Arc::new(Engine {
        secret,
        data_dir,
        devices: Arc::new(Mutex::new(devices)),
        log: broadcast::channel(1024).0,
        recent: Mutex::new(VecDeque::new()),
        receiving: tokio::sync::Mutex::new(None),
        offers: Default::default(),
        next_offer: AtomicU64::new(1),
        sending: Default::default(),
        next_send: AtomicU64::new(1),
        quit: tokio::sync::Notify::new(),
    });
    if created {
        (engine.sink())(Event::KeyCreated { id: engine.secret.public().to_string() });
    }
    println!("glft engine {} running (pid {}), id {}", env!("CARGO_PKG_VERSION"), std::process::id(), engine.secret.public());

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    tokio::spawn(engine.clone().handle(stream));
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
    notice: Option<notify::Notice>,
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
    sending: Mutex<BTreeMap<u64, String>>,
    next_send: AtomicU64,
    quit: tokio::sync::Notify,
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
        },
        _ => note(&event.to_string()),
    }
}

fn note(sentence: &str) -> LogLine {
    LogLine { at_unix_ms: now_ms(), sentence: sentence.into(), is_error: false, next_step: None, detail: None }
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
            let _ = client.send(reply_of(&event));
            engine.publish(&event);
        })
    }

    fn publish(&self, event: &Event) {
        if let Event::Received { peer, totals, dest, .. } = event {
            notify::received(peer, &totals.to_string(), dest);
        }
        let reply = reply_of(event);
        if let Reply::Line(l) = &reply {
            println!("{}", l.sentence);
            let mut recent = self.recent.lock().unwrap();
            recent.push_back(l.clone());
            while recent.len() > RECENT_LINES {
                recent.pop_front();
            }
        }
        let _ = self.log.send(reply);
    }

    fn log_note(&self, sentence: &str) {
        let l = note(sentence);
        println!("{sentence}");
        self.recent.lock().unwrap().push_back(l.clone());
        let _ = self.log.send(Reply::Line(l));
    }

    async fn handle(self: Arc<Self>, stream: UnixStream) {
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
        let _ = writer.await;
        reader.abort();
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
                let devices = self.devices.lock().unwrap().list();
                let devices = devices.into_iter().map(|(device, remembered)| DeviceInfo { device, remembered }).collect();
                let _ = tx.send(Reply::Devices { devices });
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
            Request::Quit => {
                self.quit.notify_one();
                Ok(())
            }
        }
    }

    async fn status(&self) -> Status {
        let receiving = match &*self.receiving.lock().await {
            Some(r) => Some(Receiving { folder: r.folder.clone(), addrs: net::reachable_addrs(&r.endpoint).await }),
            None => None,
        };
        Status {
            pid: std::process::id(),
            version: build_id(),
            id: self.secret.public().to_string(),
            name: paths::device_name(),
            receiving,
            sending: self.sending.lock().unwrap().values().cloned().collect(),
            offers: self.offers.lock().unwrap().len(),
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
        let notice = notify::receiving(self.clone(), folder.clone());
        events(Event::ReceiveFolder { dir: folder.clone() });
        *receiving = Some(ReceivingState { endpoint, task, folder, notice });
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
                engine.log_note(&format!("Answer with `glft accept {id}` or `glft decline {id}`, or from the notification"));
                let _ = engine.log.send(Reply::Offer(info.clone()));
                let notice = notify::offer(engine.clone(), info);
                // Dropped unanswered (the sender left): the offer is gone either way.
                let _cleanup = Cleanup(Box::new({
                    let engine = engine.clone();
                    move || {
                        engine.offers.lock().unwrap().remove(&id);
                        drop(notice);
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
        let (id, mut addrs, name) = {
            let store = self.devices.lock().unwrap();
            match store.find(&device) {
                Ok(d) => (d.endpoint_id().ok_or_else(|| Error::new(ErrorKind::BadDeviceId, d.id.clone()))?, d.dial_addrs(), d.name.clone()),
                Err(e) => match net::parse_id(&device) {
                    Ok(id) => (id, Vec::new(), net::short_id(&id)),
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
        self.sending.lock().unwrap().insert(send_id, format!("to \"{name}\": {} item(s)", paths.len()));
        let events = if detach { self.sink() } else { self.tee(tx.clone()) };
        let engine = self.clone();
        let task = tokio::spawn(async move {
            let result = transfer::send(&endpoint, peer, paths, &events).await;
            if let Ok(delivered) = &result {
                // The address that worked goes first next time; local network lookup
                // still finds the device if it has moved.
                let _ = engine.devices.lock().unwrap().seen(&id, None, delivered.addr.as_slice());
            }
            if temporary {
                endpoint.close().await;
            }
            engine.sending.lock().unwrap().remove(&send_id);
            result.map(|_| ())
        });
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
    drop(r.notice);
    r.endpoint.close().await;
    r.task.abort();
}

/// Resolves when the client has gone.
async fn closed(from_client: &mut mpsc::UnboundedReceiver<Request>) {
    while from_client.recv().await.is_some() {}
}

fn reply_of(event: &Event) -> Reply {
    match event {
        Event::Progress { done, total } => Reply::Progress { done: *done, total: *total, sending: false },
        Event::SendProgress { done, total } => Reply::Progress { done: *done, total: *total, sending: true },
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
