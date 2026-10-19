//! Sending and receiving, over iroh connections with BLAKE3 verified streaming.
//!
//! See `docs/decisions/0001-transfer-layer.md` for why this is our own small layer
//! on `bao-tree` rather than iroh-blobs. The receiver pulls each file and checks
//! every 16 KiB chunk group against the sender's fingerprint as it lands; data
//! goes straight into a partial file inside the receive folder, which is moved into
//! place only once all of it has been verified.
//!
//! A broken connection does not end a transfer. The sender keeps trying to reconnect and
//! offers the same manifest again; the receiver recognises it from its record
//! ([`crate::resume`]) and asks only for what is missing.

use std::{
    collections::HashSet,
    future::Future,
    net::SocketAddr,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use bao_tree::{
    BaoTree, ChunkNum, ChunkRanges,
    io::{
        BaoContentItem, DecodeError,
        fsm::{ResponseDecoder, ResponseDecoderNext, encode_ranges_validated},
        outboard::PreOrderMemOutboard,
    },
};
use bytes::BytesMut;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, TransportAddr,
    endpoint::{Connection, RecvStream, SendStream},
};
use n0_future::StreamExt;
use iroh_io::{TokioStreamReader, TokioStreamWriter};

use crate::{
    error::{Error, ErrorKind},
    events::{Event, EventSink, Placed, Totals},
    manifest::{self, FileEntry, Manifest, Source},
    net::{self, ALPN, short_id},
    paths::device_name,
    proto::{self, Answer, Done, FileRequest, Offer, Resumed, Saved, lost},
    resume::{self, FileState, Live, Record, Records},
    storage::{Partial, ReceiveStorage},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the local network gets on its own before mesh addresses are tried as well.
const LOCAL_FIRST: Duration = Duration::from_secs(4);
/// How long an accepted transfer may go without any progress before it reconnects.
///
/// A connection can stop carrying data while it still looks alive: on Android, a VPN tunnel
/// (such as Yggdrasil's) refuses the batched sends iroh starts each connection with, and the
/// connection keeps trying them until QUIC gives up on it about 35 seconds later. A new
/// connection learns not to batch, so reconnecting early ends the stall. Anything else
/// that hangs a connection is caught the same way; resuming loses nothing.
const STALL: Duration = Duration::from_secs(8);
/// Verified data is gathered into writes of this size.
const WRITE_BUFFER: usize = 4 * 1024 * 1024;
/// Files fetched at the same time.
const PARALLEL_FILES: usize = 8;
/// A partial file is synced and its verified length recorded this often.
const CHECKPOINT: u64 = 8 * 1024 * 1024;
/// Waits between reconnection attempts grow to this.
const MAX_BACKOFF: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------- sending

/// Files ready to send: the manifest and where each file's data and outboard are.
pub struct Outgoing {
    pub manifest: Manifest,
    sources: Vec<Source>,
}

impl Outgoing {
    pub fn totals(&self) -> Totals {
        self.manifest.totals()
    }
}

/// Reads and fingerprints `paths`. This is the slow part for big folders, done once per
/// transfer however often it has to reconnect.
pub async fn prepare(paths: Vec<PathBuf>, events: &EventSink) -> Result<Arc<Outgoing>, Error> {
    events(Event::Hashing { items: paths.len() });
    let (manifest, sources) = tokio::task::spawn_blocking(move || manifest::build(&paths))
        .await
        .map_err(|e| Error::new(ErrorKind::SourceUnreadable, e.to_string()))??;
    Ok(Arc::new(Outgoing { manifest, sources }))
}

/// Like [`prepare`], for items chosen on a device without paths (the phone).
pub async fn prepare_items(items: Vec<manifest::Item>, files: Arc<dyn manifest::SourceFiles>, events: &EventSink) -> Result<Arc<Outgoing>, Error> {
    events(Event::Hashing { items: items.len() });
    let (manifest, sources) = tokio::task::spawn_blocking(move || manifest::build_items(&items, files))
        .await
        .map_err(|e| Error::new(ErrorKind::SourceUnreadable, e.to_string()))??;
    Ok(Arc::new(Outgoing { manifest, sources }))
}

/// Sends items chosen on a device without paths (the phone) to `peer`.
pub async fn send_items(
    endpoint: &Endpoint,
    peer: EndpointAddr,
    name: Option<&str>,
    items: Vec<manifest::Item>,
    files: Arc<dyn manifest::SourceFiles>,
    events: &EventSink,
) -> Result<Delivered, Error> {
    let outgoing = prepare_items(items, files, events).await?;
    deliver(endpoint, peer, name, outgoing, events).await
}

/// Sends `paths` to `peer` and returns what was delivered.
pub async fn send(endpoint: &Endpoint, peer: EndpointAddr, paths: Vec<PathBuf>, events: &EventSink) -> Result<Delivered, Error> {
    send_to(endpoint, peer, None, paths, events).await
}

/// Like [`send`], naming the peer in events by `name` (a paired device's name) until it
/// tells its own.
pub async fn send_to(endpoint: &Endpoint, peer: EndpointAddr, name: Option<&str>, paths: Vec<PathBuf>, events: &EventSink) -> Result<Delivered, Error> {
    let outgoing = prepare(paths, events).await?;
    deliver(endpoint, peer, name, outgoing, events).await
}

/// Errors after which an accepted transfer is worth trying again.
fn retryable(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::ConnectionLost | ErrorKind::PeerUnreachable | ErrorKind::LocalNetworkBlocked)
}

/// A finished delivery.
#[derive(Debug, Clone)]
pub struct Delivered {
    pub totals: Totals,
    /// The receiver's address the transfer ended on, worth trying first next time.
    pub addr: Option<SocketAddr>,
    /// Seconds from acceptance to the last file verified.
    pub secs: f64,
    /// The mesh (Yggdrasil) if that is what carried the data at the end; `None` for the
    /// local network.
    pub via: Option<&'static str>,
    /// Where the receiver saved each item ("~/Downloads/glft/Album"), in the order sent;
    /// empty if it didn't say (a version from before it did).
    pub saved: Vec<String>,
}

/// What the local network is called in route sentences.
const LOCAL_NETWORK: &str = "the local network";

/// The route a connection is using now: a mesh's name, or [`LOCAL_NETWORK`].
fn route_of(paths: iroh::endpoint::PathList<'_>) -> Option<&'static str> {
    paths.into_iter().find(|p| p.is_selected()).and_then(|p| match p.remote_addr() {
        TransportAddr::Ip(addr) => Some(net::mesh_name(addr).unwrap_or(LOCAL_NETWORK)),
        _ => None,
    })
}

/// Follows a connection's route, says when it changes, and keeps the latest in `route`.
/// iroh moves a connection to a better route by itself (from Yggdrasil to Wi-Fi when both
/// devices turn out to share one), so where it started is not where the data went.
fn watch_route(conn: Connection, peer: String, route: Arc<Mutex<Option<&'static str>>>, events: EventSink) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut paths = conn.paths_stream();
        while let Some(list) = paths.next().await {
            let Some(now) = route_of(list) else { continue };
            let before = route.lock().unwrap().replace(now);
            if before.is_some_and(|b| b != now) {
                events(Event::RouteChanged { peer: peer.clone(), via: now });
            }
        }
    })
}

/// Delivers prepared files. Before the receiver has accepted, any failure ends it. Once
/// accepted, a lost connection is retried until it works or the caller drops this future.
///
/// While no connection is up, the local network lookup is watched as well: a receiver that
/// comes back on a new address (a phone after Wi-Fi reconnects) is dialled as soon as it
/// announces itself, instead of after the attempt on its old address times out.
pub async fn deliver(endpoint: &Endpoint, mut peer: EndpointAddr, name: Option<&str>, outgoing: Arc<Outgoing>, events: &EventSink) -> Result<Delivered, Error> {
    let totals = outgoing.totals();
    let reached = Arc::new(AtomicBool::new(false));
    let mut s = SendState {
        label: name.map(|n| format!("\"{n}\"")).unwrap_or_else(|| short_id(&peer.id)),
        told_connecting: false,
        saved: Vec::new(),
        accepted: false,
        connected: false,
        reached: reached.clone(),
        addr: None,
        via_mesh: None,
        route: Arc::new(Mutex::new(None)),
        sent: Arc::new(Mutex::new(HashSet::new())),
        progress: Arc::new(SendProgress::new(totals.bytes, events.clone())),
        started: Instant::now(),
    };
    let mut backoff = Duration::from_secs(1);
    loop {
        s.connected = false;
        reached.store(false, Ordering::Relaxed);
        let result = {
            let run = attempt(endpoint, peer.clone(), &outgoing, events, &mut s);
            tokio::pin!(run);
            let mut lookup = std::pin::pin!(new_addrs(endpoint, &peer));
            let mut watching = true;
            loop {
                tokio::select! {
                    r = &mut run => break Ok(r),
                    more = &mut lookup, if watching => {
                        if reached.load(Ordering::Relaxed) {
                            // Connected already: a new address must not cut a working
                            // connection, or offer again while the person decides.
                            watching = false;
                        } else {
                            tracing::debug!(addrs = ?more.ip_addrs().collect::<Vec<_>>(), "receiver announced a new address; dialling it now");
                            break Err(more);
                        }
                    }
                }
            }
        };
        let result = match result {
            Ok(r) => r,
            Err(more) => {
                peer = more;
                continue;
            }
        };
        match result {
            Ok(()) => {
                s.progress.set(totals.bytes);
                let secs = s.started.elapsed().as_secs_f64();
                let via = (*s.route.lock().unwrap()).filter(|r| *r != LOCAL_NETWORK);
                events(Event::Finished { peer: s.label, totals, secs, via, saved: s.saved.clone() });
                return Ok(Delivered { totals, addr: s.addr, secs, via, saved: s.saved });
            }
            Err(e) if s.accepted && retryable(e.kind) => {
                tracing::debug!(peer = %s.label, kind = e.kind.id(), detail = %e.detail, ?backoff, "attempt failed; retrying");
                if s.connected {
                    // A fresh loss: say so once, then retry quietly.
                    let files_done = s.sent.lock().unwrap().len();
                    events(Event::WaitingToResume { peer: s.label.clone(), files_done, files: totals.files });
                    backoff = Duration::from_secs(1);
                }
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => backoff = (backoff * 2).min(MAX_BACKOFF),
                    more = new_addrs(endpoint, &peer) => {
                        tracing::debug!(addrs = ?more.ip_addrs().collect::<Vec<_>>(), "receiver announced a new address; dialling it now");
                        peer = more;
                        backoff = Duration::from_secs(1);
                    }
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// Waits until the local network lookup reports an address for `peer` that is not in it
/// yet, and returns where to dial now. Never returns if nothing new turns up.
///
/// What the device announces replaces what was known: old addresses are dead after a
/// reconnect, and each one dialled takes a share of the connection attempts, so a phone
/// back on Wi-Fi was otherwise tried on its new address only after many tries at stale
/// ones. Its private IPv4 addresses are preferred, as the ones this network can reach
/// (mobile data addresses are announced too), and its mesh addresses are kept.
async fn new_addrs(endpoint: &Endpoint, peer: &EndpointAddr) -> EndpointAddr {
    let Ok(lookup) = endpoint.address_lookup() else { return std::future::pending().await };
    let known: HashSet<SocketAddr> = peer.ip_addrs().copied().collect();
    loop {
        // A lookup ends after a few seconds; start another until something new turns up.
        let mut found = lookup.resolve(peer.id);
        while let Some(item) = found.next().await {
            let Ok(Ok(item)) = item else { continue };
            let announced: Vec<SocketAddr> = item.to_endpoint_addr().ip_addrs().copied().collect();
            let lan: Vec<SocketAddr> = announced.iter().filter(|a| net::lan_rank(a) < 3).copied().collect();
            let dial = if lan.is_empty() { announced } else { lan };
            if dial.iter().any(|a| !known.contains(a)) {
                // Mesh addresses stay: they work wherever the device is, and are the way
                // round a VPN that blocks the local network.
                let mesh = peer.ip_addrs().filter(|a| net::mesh_name(a).is_some()).copied();
                let dial: Vec<SocketAddr> = dial.into_iter().chain(mesh).collect();
                return net::peer_addr(peer.id, &dial);
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

struct SendState {
    label: String,
    /// Where the receiver says it saved each item, once it has said.
    saved: Vec<String>,
    /// "Connecting to …" was said; redialling a new address doesn't say it again.
    told_connecting: bool,
    /// The receiver accepted at least once, so this transfer is now worth resuming.
    accepted: bool,
    /// This attempt got as far as an answer.
    connected: bool,
    /// This attempt has a connection (it may still be waiting for the answer).
    reached: Arc<AtomicBool>,
    /// The receiver's address in use when the transfer finished.
    addr: Option<SocketAddr>,
    /// The mesh network the last connection went over, said once in the log.
    via_mesh: Option<&'static str>,
    /// The route the current connection is using, kept up to date by [`watch_route`].
    route: Arc<Mutex<Option<&'static str>>>,
    /// Files the receiver has confirmed, across attempts.
    sent: Arc<Mutex<HashSet<u32>>>,
    progress: Arc<SendProgress>,
    /// When the receiver first accepted.
    started: Instant,
}

async fn attempt(endpoint: &Endpoint, peer: EndpointAddr, outgoing: &Arc<Outgoing>, events: &EventSink, s: &mut SendState) -> Result<(), Error> {
    let totals = outgoing.totals();
    if !s.accepted && !s.told_connecting {
        s.told_connecting = true;
        events(Event::Connecting { peer: s.label.clone() });
    }
    let dialled_lan = peer.ip_addrs().copied().filter(|a| net::lan_rank(a) < 3).min_by_key(net::lan_rank);
    // The local network first: a mesh such as Yggdrasil is the way round a VPN that blocks
    // it, not the first choice. Mesh addresses join after a few seconds, or at once if the
    // local network refuses (as a VPN in lockdown makes it).
    let (mesh, local): (Vec<SocketAddr>, Vec<SocketAddr>) = peer.ip_addrs().partition(|a| net::mesh_name(a).is_some());
    let local_first = if mesh.is_empty() || local.is_empty() {
        None
    } else {
        match tokio::time::timeout(LOCAL_FIRST, endpoint.connect(net::peer_addr(peer.id, &local), ALPN)).await {
            Ok(Ok(conn)) => Some(conn),
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "local network refused; trying mesh addresses too");
                None
            }
            Err(_) => {
                tracing::debug!("no answer on the local network yet; trying mesh addresses too");
                None
            }
        }
    };
    let conn = match local_first {
        Some(conn) => conn,
        None => tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(peer, ALPN))
            .await
            .map_err(|_| Error::new(ErrorKind::PeerUnreachable, "no answer within 15 seconds"))?
            .map_err(|e| Error::new(net::classify_unreachable(&e.to_string()), e.to_string()))?,
    };
    s.reached.store(true, Ordering::Relaxed);
    if !s.accepted {
        events(Event::Connected { peer: s.label.clone() });
    }
    let start = route_of(conn.paths());
    let mesh = start.filter(|r| *r != LOCAL_NETWORK);
    if mesh != s.via_mesh {
        if let Some(mesh) = mesh {
            events(Event::ViaMesh { peer: s.label.clone(), mesh });
        }
        s.via_mesh = mesh;
    }
    *s.route.lock().unwrap() = start;
    let route_watch = AbortOnDrop(watch_route(conn.clone(), s.label.clone(), s.route.clone(), events.clone()));

    // A receiver that does not know this device closes the connection straight away.
    let label = s.label.clone();
    let accepted = s.accepted;
    let answer = async {
        let (mut ctl_send, mut ctl_recv) = conn.open_bi().await.map_err(lost)?;
        proto::write(&mut ctl_send, &Offer { device_name: device_name(), manifest: outgoing.manifest.clone() }).await?;
        if !accepted {
            events(Event::Offered { peer: label.clone(), totals });
        }
        let answer = proto::read::<Answer>(&mut ctl_recv).await?;
        Ok::<_, Error>((answer, ctl_recv))
    }
    .await
    .map_err(|e| match conn.close_reason() {
        Some(reason) if format!("{reason:?}").contains("not-paired") => {
            Error::new(ErrorKind::NotPaired, format!("{label} closed the connection: {reason}"))
        }
        _ => e,
    })?;
    let (answer, mut ctl_recv) = answer;
    s.connected = true;

    match answer {
        Answer::Decline => {
            events(Event::Declined { peer: s.label.clone() });
            conn.close(0u32.into(), b"declined");
            return Err(Error::new(ErrorKind::Declined, format!("{} declined", s.label)));
        }
        Answer::Accept { device_name, resumed } => {
            s.label = format!("\"{device_name}\"");
            match resumed {
                Some(Resumed { files_done, bytes_left }) => {
                    events(Event::ResumingSend { peer: s.label.clone(), files_done: files_done as usize, files: totals.files, bytes_left });
                    s.progress.set(totals.bytes.saturating_sub(bytes_left));
                }
                None => {
                    events(Event::Accepted { peer: s.label.clone() });
                    s.progress.set(0);
                }
            }
            if !s.accepted {
                // Time the transfer, not the person deciding.
                s.started = Instant::now();
            }
            s.accepted = true;
        }
    }

    let server = tokio::spawn(serve_requests(conn.clone(), outgoing.clone(), s.sent.clone(), s.progress.clone(), events.clone()));
    s.progress.touch();
    let done = tokio::select! {
        done = proto::read::<Done>(&mut ctl_recv) => done,
        () = s.progress.stalled(STALL) => {
            tracing::debug!(peer = %s.label, "no progress for {STALL:?}; reconnecting");
            Err(Error::new(ErrorKind::ConnectionLost, format!("no progress for {} seconds", STALL.as_secs())))
        }
    };
    // Then where it was saved, from a receiver new enough to say; an older one just closes.
    if done.is_ok() {
        let saved = tokio::time::timeout(Duration::from_secs(2), proto::read::<Saved>(&mut ctl_recv)).await;
        if let Ok(Ok(saved)) = saved {
            s.saved = saved.places;
        }
    }
    server.abort();
    drop(route_watch);
    // Remember a private IPv4 address if the connection had one, or else the one dialled
    // (the connection may have moved to IPv6 by now): a phone's IPv6 addresses change
    // with every Wi-Fi reconnect, while its IPv4 lease often survives.
    let ip = |p: &iroh::endpoint::Path<'_>| match p.remote_addr() {
        TransportAddr::Ip(addr) => Some(*addr),
        _ => None,
    };
    let paths = conn.paths();
    s.addr = paths
        .clone()
        .into_iter()
        .filter_map(|p| ip(&p))
        .min_by_key(net::lan_rank)
        .filter(|a| net::lan_rank(a) < 3)
        .or(dialled_lan)
        .or_else(|| paths.into_iter().find(|p| p.is_selected()).and_then(|p| ip(&p)));
    conn.close(0u32.into(), b"done");
    let done = done?;
    if done.files as usize != totals.files {
        return Err(Error::new(
            ErrorKind::ProtocolError,
            format!("receiver confirmed {} of {} files", done.files, totals.files),
        ));
    }
    Ok(())
}

/// Answers file requests until the connection closes.
async fn serve_requests(conn: Connection, outgoing: Arc<Outgoing>, sent: Arc<Mutex<HashSet<u32>>>, progress: Arc<SendProgress>, events: EventSink) {
    while let Ok((send, recv)) = conn.accept_bi().await {
        let (outgoing, sent, progress, events) = (outgoing.clone(), sent.clone(), progress.clone(), events.clone());
        tokio::spawn(async move {
            match serve_file(send, recv, &outgoing, &progress, &events).await {
                Ok(index) => {
                    progress.touch();
                    sent.lock().unwrap().insert(index);
                }
                // A lost connection is reported once by the transfer, not per file.
                Err(e) if e.kind == ErrorKind::ConnectionLost => {}
                Err(e) => events(Event::Failed(e)),
            }
        });
    }
}

async fn serve_file(mut send: SendStream, mut recv: RecvStream, outgoing: &Outgoing, progress: &SendProgress, events: &EventSink) -> Result<u32, Error> {
    let req: FileRequest = proto::read(&mut recv).await?;
    let (entry, source) = outgoing
        .manifest
        .files
        .get(req.index as usize)
        .zip(outgoing.sources.get(req.index as usize))
        .ok_or_else(|| Error::new(ErrorKind::ProtocolError, format!("request for file {}", req.index)))?;
    let what = source.file.describe();
    let opener = source.file.clone();
    let file = tokio::task::spawn_blocking(move || opener.open())
        .await
        .map_err(|e| Error::new(ErrorKind::SourceUnreadable, e.to_string()))?
        .map_err(|e| Error::new(ErrorKind::SourceUnreadable, format!("{what}: {e}")))?;
    let file = iroh_io::File::from_std(file);
    let outboard = PreOrderMemOutboard { root: source.outboard.root, tree: source.outboard.tree, data: &source.outboard.data[..] };
    let ranges = ChunkRanges::from(ChunkNum(req.from_chunk)..);
    encode_ranges_validated(file, outboard, &ranges, TokioStreamWriter(Counted { inner: &mut send, progress }))
        .await
        .map_err(|e| match e {
            bao_tree::io::EncodeError::Io(e) if e.kind() != std::io::ErrorKind::InvalidData => lost(e),
            e => Error::new(ErrorKind::VerificationFailed, format!("{}: changed while being sent ({e})", entry.path)),
        })?;
    send.finish().map_err(lost)?;
    // Wait until the receiver has everything before reporting it sent.
    send.stopped().await.map_err(lost)?;
    events(Event::FileSent { path: entry.path.clone() });
    Ok(req.index)
}

/// Aborts a task when dropped, so it ends with the attempt however that ends.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Bytes on their way to the receiver, reported at most a few times a second.
struct SendProgress {
    done: AtomicU64,
    total: u64,
    last: Mutex<Option<Instant>>,
    /// When something last moved: bytes written to the connection, or a file confirmed.
    active: Mutex<Instant>,
    events: EventSink,
}

impl SendProgress {
    fn new(total: u64, events: EventSink) -> Self {
        Self { done: AtomicU64::new(0), total, last: Mutex::new(None), active: Mutex::new(Instant::now()), events }
    }

    /// Notes that the transfer moved forward.
    fn touch(&self) {
        *self.active.lock().unwrap() = Instant::now();
    }

    /// Resolves once nothing has moved for `limit`.
    async fn stalled(&self, limit: Duration) {
        loop {
            let idle = self.active.lock().unwrap().elapsed();
            if idle >= limit {
                return;
            }
            tokio::time::sleep(limit - idle).await;
        }
    }

    /// Starts counting from what the receiver already has, and reports it.
    fn set(&self, done: u64) {
        self.done.store(done, Ordering::Relaxed);
        *self.last.lock().unwrap() = Some(Instant::now());
        (self.events)(Event::SendProgress { done: done.min(self.total), total: self.total });
    }

    fn add(&self, n: u64) {
        self.touch();
        let done = self.done.fetch_add(n, Ordering::Relaxed) + n;
        let mut last = self.last.lock().unwrap();
        if last.is_none_or(|t| t.elapsed() >= Duration::from_millis(250)) {
            *last = Some(Instant::now());
            drop(last);
            // What goes out includes the fingerprints that let the receiver check each
            // piece (about 0.4%), so the count can pass the total just before the end.
            (self.events)(Event::SendProgress { done: done.min(self.total), total: self.total });
        }
    }
}

/// Counts what is written through it.
struct Counted<'a, W> {
    inner: W,
    progress: &'a SendProgress,
}

impl<W: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for Counted<'_, W> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let std::task::Poll::Ready(Ok(n)) = &polled {
            self.progress.add(*n as u64);
        }
        polled
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

// ---------------------------------------------------------------- receiving

/// An offer as shown to the person deciding.
#[derive(Debug, Clone)]
pub struct IncomingOffer {
    pub id: EndpointId,
    pub peer: String,
    pub totals: Totals,
    /// Where it will be saved, as a person reads it.
    pub dest: String,
}

/// Decides whether to accept an offer; may ask the user and take as long as they need.
pub type Decide = Arc<dyn Fn(IncomingOffer) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// Whether a device key may connect at all. Pairing fills this; anything else is refused
/// before an offer is read.
pub type Authorize = Arc<dyn Fn(&EndpointId) -> bool + Send + Sync>;

/// Told about every finished transfer from a device: its id and current name.
pub type Seen = Arc<dyn Fn(&EndpointId, &str) + Send + Sync>;

pub struct ReceiveOptions {
    pub storage: Arc<dyn ReceiveStorage>,
    pub decide: Decide,
    pub authorize: Authorize,
    /// Where unfinished transfers are recorded.
    pub records: Records,
    pub seen: Option<Seen>,
    /// Stop after the first offer, accepted or not.
    pub once: bool,
}

/// Reason sent when closing a connection from a key that is not allowed.
const NOT_PAIRED: &[u8] = b"not-paired";

/// Accepts connections until the endpoint closes. Each connection is handled on its own,
/// so one device's transfer does not keep another waiting.
pub async fn serve(endpoint: &Endpoint, opts: Arc<ReceiveOptions>, events: &EventSink) -> Result<(), Error> {
    events(Event::ReceiveFolder { dir: opts.storage.describe("") });
    let (records, storage) = (opts.records.clone(), opts.storage.clone());
    let _ = blocking(move || {
        discard_expired(&records, storage.as_ref());
        Ok(())
    })
    .await;
    let active: Active = Default::default();
    let mut handlers = tokio::task::JoinSet::new();
    while let Some(incoming) = endpoint.accept().await {
        let conn = match incoming.await {
            Ok(conn) => conn,
            Err(e) => {
                events(Event::Failed(Error::new(ErrorKind::ConnectionLost, e.to_string())));
                continue;
            }
        };
        if !(opts.authorize)(&conn.remote_id()) {
            events(Event::Refused { peer: short_id(&conn.remote_id()) });
            conn.close(2u32.into(), NOT_PAIRED);
            continue;
        }
        let (opts2, events2, active2) = (opts.clone(), events.clone(), active.clone());
        let handle = async move {
            if let Err(e) = receive_one(&conn, &opts2, &events2, &active2).await {
                conn.close(1u32.into(), e.kind.id().as_bytes());
                events2(Event::Failed(e));
            }
        };
        if opts.once {
            handle.await;
            break;
        }
        handlers.spawn(handle);
        while handlers.try_join_next().is_some() {}
    }
    Ok(())
}

/// Transfers in progress, by sender and manifest. A sender that reconnects (after a lost
/// connection the receiver may not have noticed yet) takes over from its earlier attempt.
type Active = Arc<Mutex<std::collections::HashMap<(EndpointId, String), Slot>>>;

#[derive(Clone, Default)]
struct Slot {
    cancel: Arc<tokio::sync::Notify>,
    /// Held while an attempt has the transfer's files open.
    files: Arc<tokio::sync::Mutex<()>>,
}

/// Removes partial files of transfers abandoned for a week. Blocking.
fn discard_expired(records: &Records, storage: &dyn ReceiveStorage) {
    let here = storage.describe("");
    for record in records.take_expired() {
        if record.storage != here {
            continue;
        }
        for state in &record.files {
            if let FileState::Partial { token, .. } = state {
                let _ = storage.discard_abandoned(token);
            }
        }
    }
}

async fn receive_one(conn: &Connection, opts: &ReceiveOptions, events: &EventSink, active: &Active) -> Result<(), Error> {
    let sender = conn.remote_id();
    let short = short_id(&sender);
    events(Event::Connected { peer: short.clone() });
    let (mut ctl_send, mut ctl_recv) = conn.accept_bi().await.map_err(lost)?;
    let offer: Offer = proto::read(&mut ctl_recv).await?;
    let manifest = offer.manifest;
    if let Err(e) = manifest.validate() {
        let _ = proto::write(&mut ctl_send, &Answer::Decline).await;
        return Err(e);
    }
    let hash = resume::manifest_hash(&manifest);

    // Take over from an earlier attempt at the same transfer, once it has let go of its files.
    let key = (sender, hash.clone());
    let slot = {
        let mut active = active.lock().unwrap();
        let files = match active.get(&key) {
            Some(earlier) => {
                earlier.cancel.notify_one();
                earlier.files.clone()
            }
            None => Default::default(),
        };
        let slot = Slot { cancel: Default::default(), files };
        active.insert(key.clone(), slot.clone());
        slot
    };
    let held = slot.files.clone().lock_owned().await;
    let result = tokio::select! {
        r = receive_offer(conn, opts, events, (ctl_send, ctl_recv), offer.device_name, manifest, hash, held) => r,
        // Superseded by a newer connection from the same sender, which carries on.
        _ = slot.cancel.notified() => Ok(()),
    };
    let mut active = active.lock().unwrap();
    if active.get(&key).is_some_and(|s| Arc::ptr_eq(&s.cancel, &slot.cancel)) {
        active.remove(&key);
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn receive_offer(
    conn: &Connection,
    opts: &ReceiveOptions,
    events: &EventSink,
    (mut ctl_send, _ctl_recv): (SendStream, RecvStream),
    device_name_offered: String,
    manifest: Manifest,
    hash: String,
    held: tokio::sync::OwnedMutexGuard<()>,
) -> Result<(), Error> {
    let sender = conn.remote_id();
    let label = format!("\"{device_name_offered}\"");
    let totals = manifest.totals();
    let dest = opts.storage.describe("");

    // The same device offering the same files again, after an interruption, continues
    // without asking: the person accepted this transfer already.
    let earlier = opts.records.load(&sender, &hash);
    let earlier = match earlier {
        Some(r) if r.storage == dest && r.files.len() == manifest.files.len() => Some(r),
        Some(r) => {
            opts.records.remove(&r);
            None
        }
        None => None,
    };

    let earlier_resumed = earlier.is_some();
    let live = match earlier {
        Some(record) => {
            let files_done = record.files.iter().filter(|f| **f == FileState::Done).count();
            let bytes_left: u64 = manifest
                .files
                .iter()
                .zip(&record.files)
                .map(|(f, state)| match state {
                    FileState::Done => 0,
                    FileState::Partial { verified, .. } => f.size.saturating_sub(*verified),
                    FileState::Waiting => f.size,
                })
                .sum();
            events(Event::ResumingReceive {
                peer: label.clone(),
                accepted_at: record.accepted_at,
                files_done,
                files: totals.files,
                bytes_left,
            });
            let resumed = Resumed { files_done: files_done as u32, bytes_left };
            proto::write(&mut ctl_send, &Answer::Accept { device_name: device_name(), resumed: Some(resumed) }).await?;
            let (storage, m, renames) = (opts.storage.clone(), manifest.clone(), record.renames.clone());
            blocking(move || make_dirs(&m, &renames, storage.as_ref())).await?;
            Arc::new(opts.records.live(record))
        }
        None => {
            events(Event::OfferReceived { peer: label.clone(), totals, dest: dest.clone() });
            // The person may take their time; if the sender gives up meanwhile, the offer is missed.
            let decision = (opts.decide)(IncomingOffer { id: sender, peer: label.clone(), totals, dest: dest.clone() });
            let accepted = tokio::select! {
                accepted = decision => accepted,
                _ = conn.closed() => {
                    events(Event::Missed { peer: label, totals });
                    return Ok(());
                }
            };
            if !accepted {
                proto::write(&mut ctl_send, &Answer::Decline).await?;
                let _ = ctl_send.finish();
                events(Event::Declined { peer: "You".into() });
                let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
                return Ok(());
            }
            proto::write(&mut ctl_send, &Answer::Accept { device_name: device_name(), resumed: None }).await?;
            let (storage, m, ev) = (opts.storage.clone(), manifest.clone(), events.clone());
            let renames = blocking(move || place_roots(&m, storage.as_ref(), &ev)).await?;
            let record = Record {
                sender: sender.to_string(),
                manifest: hash,
                storage: dest.clone(),
                accepted_at: crate::devices::now(),
                updated_at: 0,
                renames,
                files: vec![FileState::Waiting; manifest.files.len()],
            };
            Arc::new(opts.records.live(record))
        }
    };

    // Timed from here, so the time the person took to decide is not counted.
    let started = Instant::now();
    let renames = live.renames();
    let progress = Progress { peer: label.clone(), done: AtomicU64::new(0), total: totals.bytes, events: events.clone() };
    let job = Arc::new(Job {
        conn: conn.clone(),
        storage: opts.storage.clone(),
        live,
        progress,
        resumed: earlier_resumed,
        _held: held,
    });
    // Several files in flight hide the per-file round trip and fsync. Dropping the set
    // on an error cancels the rest; the record keeps what was verified.
    let fetched = async {
        let mut in_flight = tokio::task::JoinSet::new();
        for (index, file) in manifest.files.iter().enumerate() {
            match job.live.get(index) {
                FileState::Done => {
                    job.progress.add(file.size);
                    continue;
                }
                FileState::Partial { verified, .. } => job.progress.add(verified),
                FileState::Waiting => {}
            }
            if in_flight.len() >= PARALLEL_FILES {
                in_flight.join_next().await.expect("set is not empty").map_err(task_failed)??;
            }
            let (job, file, events) = (job.clone(), file.clone(), events.clone());
            let rel = rebase(&file.path, &renames);
            in_flight.spawn(async move {
                fetch_file(&job, index, &file, rel.clone()).await?;
                events(Event::FileVerified {
                    path: rel.rsplit('/').next().unwrap_or_default().to_string(),
                    saved_to: job.storage.describe(&rel),
                });
                Ok::<(), Error>(())
            });
        }
        while let Some(result) = in_flight.join_next().await {
            result.map_err(task_failed)??;
        }
        Ok::<(), Error>(())
    }
    .await;
    if let Err(e) = fetched {
        if e.kind == ErrorKind::ConnectionLost {
            // Nothing to do on this side: the sender reconnects and continues.
            events(Event::ReceiveInterrupted { peer: label, done: job.progress.done(), total: totals.bytes });
            return Ok(());
        }
        return Err(e);
    }
    job.live.finish();
    proto::write(&mut ctl_send, &Done { files: manifest.files.len() as u32 }).await?;
    // Where each item is now, under the name it got here ("Album (2)").
    let items: Vec<Placed> = manifest
        .roots
        .iter()
        .map(|root| {
            let name = rebase(&root.name, &renames);
            let folder = job.storage.describe(&name);
            Placed {
                place: format!("{}/{name}", folder.trim_end_matches('/')),
                path: job.storage.local_path(&name),
                is_dir: root.is_dir,
                name,
            }
        })
        .collect();
    // A sender from before this message has closed the stream by now; nothing is lost.
    let _ = proto::write(&mut ctl_send, &Saved { places: items.iter().map(|i| i.place.clone()).collect() }).await;
    let _ = ctl_send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
    if let Some(seen) = &opts.seen {
        seen(&sender, &device_name_offered);
    }
    events(Event::Received { peer: label, totals, dest, secs: started.elapsed().as_secs_f64(), items });
    Ok(())
}

/// Bytes verified in the current transfer, reported at checkpoints and file ends.
struct Progress {
    peer: String,
    done: AtomicU64,
    total: u64,
    events: EventSink,
}

impl Progress {
    fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    fn sub(&self, n: u64) {
        self.done.fetch_sub(n, Ordering::Relaxed);
    }

    fn report(&self, n: u64) {
        let done = self.done.fetch_add(n, Ordering::Relaxed) + n;
        (self.events)(Event::Progress { peer: self.peer.clone(), done, total: self.total });
    }
}

/// Picks free names for the offered items and creates the folders. Blocking.
fn place_roots(manifest: &Manifest, storage: &dyn ReceiveStorage, events: &EventSink) -> Result<Vec<(String, String)>, Error> {
    let mut renames = Vec::new();
    for root in &manifest.roots {
        let name = manifest::free_name(&root.name, root.is_dir, |n| storage.exists(n).unwrap_or(true));
        if name != root.name {
            events(Event::Renamed { from: root.name.clone(), to: name.clone() });
        }
        renames.push((root.name.clone(), name));
    }
    make_dirs(manifest, &renames, storage)?;
    Ok(renames)
}

/// Creates every offered folder under its chosen name; folders already there are kept.
fn make_dirs(manifest: &Manifest, renames: &[(String, String)], storage: &dyn ReceiveStorage) -> Result<(), Error> {
    for root in manifest.roots.iter().filter(|r| r.is_dir) {
        let rel = rebase(&root.name, renames);
        storage.make_dir(&rel).map_err(|e| store_err(&rel, e))?;
    }
    for dir in &manifest.dirs {
        let rel = rebase(dir, renames);
        storage.make_dir(&rel).map_err(|e| store_err(&rel, e))?;
    }
    Ok(())
}

/// Replaces a validated path's first segment with the root's chosen name.
fn rebase(path: &str, renames: &[(String, String)]) -> String {
    let (first, rest) = path.split_once('/').unwrap_or((path, ""));
    let root = renames.iter().find(|(from, _)| from == first).map(|(_, to)| to.as_str()).unwrap_or(first);
    if rest.is_empty() { root.to_string() } else { format!("{root}/{rest}") }
}

fn store_err(rel: &str, e: std::io::Error) -> Error {
    Error::io(ErrorKind::ReceiveDirUnwritable, Path::new(rel), e)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, Error> + Send + 'static) -> Result<T, Error> {
    tokio::task::spawn_blocking(f).await.map_err(task_failed)?
}

/// Opens the file's partial: the one from an interrupted attempt, cut back to what was
/// verified and synced, or a new one. Returns it with the offset to continue from.
fn open_partial(storage: &dyn ReceiveStorage, rel: &str, state: FileState) -> Result<(Partial, u64), Error> {
    if let FileState::Partial { token, verified } = state
        && let Some(p) = storage.reopen_partial(rel, &token).map_err(|e| store_err(rel, e))?
    {
        // Anything after the recorded offset may be torn or unsynced; it is fetched again.
        let len = p.file.metadata().map_err(|e| store_err(rel, e))?.len();
        let start = if len >= verified { verified } else { 0 };
        p.file.set_len(start).map_err(|e| store_err(rel, e))?;
        return Ok((p, start));
    }
    Ok((storage.create_partial(rel).map_err(|e| store_err(rel, e))?, 0))
}

/// What every file of one incoming transfer shares.
struct Job {
    conn: Connection,
    storage: Arc<dyn ReceiveStorage>,
    live: Arc<Live>,
    progress: Progress,
    /// Continuing a recorded transfer rather than starting fresh.
    resumed: bool,
    /// Released when the last file task is gone, so a newer attempt never shares files.
    _held: tokio::sync::OwnedMutexGuard<()>,
}

async fn fetch_file(job: &Job, index: usize, file: &FileEntry, rel: String) -> Result<(), Error> {
    let Job { conn, storage, live, progress, resumed, .. } = job;
    let resumed = *resumed;
    let hash = bao_tree::blake3::Hash::from_bytes(file.hash);
    let state = live.get(index);
    let recorded = match &state {
        FileState::Partial { verified, .. } => *verified,
        _ => 0,
    };
    // When resuming, a file already under its name was published by an earlier attempt
    // (whose record was not saved in time), and publishing happens only after verification.
    // A fresh transfer's names were all free, so it skips the check.
    if resumed {
        let (st, r) = (storage.clone(), rel.clone());
        if blocking(move || st.published(&r).map_err(|e| store_err(&r, e))).await? {
            live.set(index, FileState::Done);
            progress.report(file.size - recorded.min(file.size));
            return Ok(());
        }
    }
    let (st, r) = (storage.clone(), rel.clone());
    let (partial, start) = blocking(move || open_partial(st.as_ref(), &r, state)).await?;
    if start < recorded {
        // The partial lost data it was recorded to have; it is fetched again.
        progress.sub(recorded - start);
    }
    let Partial { file: mut target, rel, token } = partial;
    live.set(index, FileState::Partial { token: token.clone(), verified: start });

    if file.size == 0 {
        if hash != bao_tree::blake3::hash(b"") {
            return Err(Error::new(ErrorKind::VerificationFailed, format!("{}: empty but fingerprint differs", file.path)));
        }
    } else if start < file.size {
        let from_chunk = start / 1024;
        let (mut send, recv) = conn.open_bi().await.map_err(lost)?;
        proto::write(&mut send, &FileRequest { index: index as u32, from_chunk }).await?;
        send.finish().map_err(lost)?;
        let tree = BaoTree::new(file.size, manifest::BLOCK_SIZE);
        let ranges = ChunkRanges::from(ChunkNum(from_chunk)..);
        let mut decoder = ResponseDecoder::new(hash, ranges, tree, TokioStreamReader(recv));
        let mut buf = BytesMut::new();
        let mut buf_start = start;
        let mut written = start;
        let mut synced = start;
        loop {
            let item = match decoder.next().await {
                ResponseDecoderNext::Done(_) => break,
                ResponseDecoderNext::More((next, item)) => {
                    decoder = next;
                    item.map_err(|e| decode_error(e, &file.path))?
                }
            };
            if let BaoContentItem::Leaf(leaf) = item {
                if !buf.is_empty() && (buf_start + buf.len() as u64 != leaf.offset || buf.len() >= WRITE_BUFFER) {
                    let len = buf.len() as u64;
                    target = write_at(target, buf_start, buf.split().freeze(), &rel).await?;
                    written = buf_start + len;
                    if written - synced >= CHECKPOINT {
                        target = sync(target, &rel).await?;
                        live.set(index, FileState::Partial { token: token.clone(), verified: written });
                        progress.report(written - synced);
                        synced = written;
                    }
                }
                if buf.is_empty() {
                    buf_start = leaf.offset;
                }
                buf.extend_from_slice(&leaf.data);
            }
        }
        if !buf.is_empty() {
            let len = buf.len() as u64;
            target = write_at(target, buf_start, buf.split().freeze(), &rel).await?;
            written = buf_start + len;
        }
        progress.report(written - synced);
    }
    // Every chunk was checked against the fingerprint on arrival (and the part from an
    // earlier attempt when it arrived then), so the file is complete and verified;
    // publishing it never replaces an existing file.
    let (r, fingerprint, storage) = (rel.clone(), file.hash, storage.clone());
    blocking(move || {
        let partial = Partial { file: target, rel: r.clone(), token };
        storage.finish(partial, &fingerprint).map_err(|e| store_err(&r, e))
    })
    .await?;
    live.set(index, FileState::Done);
    Ok(())
}

async fn write_at(file: std::fs::File, offset: u64, data: bytes::Bytes, rel: &str) -> Result<std::fs::File, Error> {
    let r = rel.to_string();
    blocking(move || {
        use std::os::unix::fs::FileExt;
        file.write_all_at(&data, offset).map_err(|e| store_err(&r, e))?;
        Ok(file)
    })
    .await
}

async fn sync(file: std::fs::File, rel: &str) -> Result<std::fs::File, Error> {
    let r = rel.to_string();
    blocking(move || {
        file.sync_data().map_err(|e| store_err(&r, e))?;
        Ok(file)
    })
    .await
}

fn task_failed(e: tokio::task::JoinError) -> Error {
    Error::new(ErrorKind::ConnectionLost, format!("transfer task ended: {e}"))
}

fn decode_error(e: DecodeError, path: &str) -> Error {
    match e {
        DecodeError::LeafHashMismatch(_) | DecodeError::ParentHashMismatch(_) => {
            Error::new(ErrorKind::VerificationFailed, format!("{path}: {e}"))
        }
        e => Error::new(ErrorKind::ConnectionLost, format!("{path}: {e}")),
    }
}
