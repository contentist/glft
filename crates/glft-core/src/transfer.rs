//! Sending and receiving, over iroh connections with BLAKE3 verified streaming.
//!
//! See `docs/decisions/0001-transfer-layer.md` for why this is our own small layer
//! on `bao-tree` rather than iroh-blobs. The receiver pulls each file and checks
//! every 16 KiB chunk group against the sender's fingerprint as it lands; data
//! goes straight into a partial file inside the receive folder, which is moved into
//! place only once all of it has been verified.

use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
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
    Endpoint, EndpointAddr, EndpointId,
    endpoint::{Connection, RecvStream, SendStream},
};
use iroh_io::{TokioStreamReader, TokioStreamWriter};

use crate::{
    error::{Error, ErrorKind},
    events::{Event, EventSink, Totals},
    manifest::{self, FileEntry, Manifest, Source},
    storage::{Partial, ReceiveStorage},
    net::{self, ALPN, short_id},
    paths::device_name,
    proto::{self, Answer, Done, FileRequest, Offer, lost},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Verified data is gathered into writes of this size.
const WRITE_BUFFER: usize = 4 * 1024 * 1024;
/// Files fetched at the same time.
const PARALLEL_FILES: usize = 8;

// ---------------------------------------------------------------- sending

/// Sends `paths` to `peer` and returns what was delivered.
pub async fn send(endpoint: &Endpoint, peer: EndpointAddr, paths: Vec<PathBuf>, events: &EventSink) -> Result<Totals, Error> {
    events(Event::Hashing { items: paths.len() });
    let (manifest, sources) = tokio::task::spawn_blocking(move || manifest::build(&paths))
        .await
        .map_err(|e| Error::new(ErrorKind::SourceUnreadable, e.to_string()))??;
    let totals = manifest.totals();

    let mut label = short_id(&peer.id);
    events(Event::Connecting { peer: label.clone() });
    let conn = tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(peer, ALPN))
        .await
        .map_err(|_| Error::new(ErrorKind::PeerUnreachable, "no answer within 15 seconds"))?
        .map_err(|e| Error::new(net::classify_unreachable(&e.to_string()), e.to_string()))?;
    events(Event::Connected { peer: label.clone() });

    // A receiver that does not know this device closes the connection straight away.
    let answer = async {
        let (mut ctl_send, mut ctl_recv) = conn.open_bi().await.map_err(lost)?;
        proto::write(&mut ctl_send, &Offer { device_name: device_name(), manifest: manifest.clone() }).await?;
        events(Event::Offered { peer: label.clone(), totals });
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

    match answer {
        Answer::Decline => {
            events(Event::Declined { peer: label.clone() });
            conn.close(0u32.into(), b"declined");
            return Err(Error::new(ErrorKind::Declined, format!("{label} declined")));
        }
        Answer::Accept { device_name } => {
            label = format!("\"{device_name}\"");
            events(Event::Accepted { peer: label.clone() });
        }
    }

    let started = Instant::now();
    let shared = Arc::new((manifest, sources));
    let server = tokio::spawn(serve_requests(conn.clone(), shared, events.clone()));
    let done = proto::read::<Done>(&mut ctl_recv).await;
    server.abort();
    conn.close(0u32.into(), b"done");
    let done = done?;
    if done.files as usize != totals.files {
        return Err(Error::new(
            ErrorKind::ProtocolError,
            format!("receiver confirmed {} of {} files", done.files, totals.files),
        ));
    }
    events(Event::Finished { peer: label, totals, secs: started.elapsed().as_secs_f64() });
    Ok(totals)
}

/// Answers file requests until the connection closes.
async fn serve_requests(conn: Connection, shared: Arc<(Manifest, Vec<Source>)>, events: EventSink) {
    while let Ok((send, recv)) = conn.accept_bi().await {
        let shared = shared.clone();
        let events = events.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_file(send, recv, &shared, &events).await {
                events(Event::Failed(e));
            }
        });
    }
}

async fn serve_file(mut send: SendStream, mut recv: RecvStream, shared: &(Manifest, Vec<Source>), events: &EventSink) -> Result<(), Error> {
    let (manifest, sources) = shared;
    let req: FileRequest = proto::read(&mut recv).await?;
    let (entry, source) = manifest
        .files
        .get(req.index as usize)
        .zip(sources.get(req.index as usize))
        .ok_or_else(|| Error::new(ErrorKind::ProtocolError, format!("request for file {}", req.index)))?;
    let path = source.path.clone();
    let file = iroh_io::File::open(path.clone())
        .await
        .map_err(|e| Error::io(ErrorKind::SourceUnreadable, &path, e))?;
    let outboard = PreOrderMemOutboard { root: source.outboard.root, tree: source.outboard.tree, data: &source.outboard.data[..] };
    let ranges = ChunkRanges::from(ChunkNum(req.from_chunk)..);
    encode_ranges_validated(file, outboard, &ranges, TokioStreamWriter(&mut send))
        .await
        .map_err(|e| match e {
            bao_tree::io::EncodeError::Io(e) if e.kind() != std::io::ErrorKind::InvalidData => lost(e),
            e => Error::new(ErrorKind::VerificationFailed, format!("{}: changed while being sent ({e})", path.display())),
        })?;
    send.finish().map_err(lost)?;
    // Wait until the receiver has everything before reporting it sent.
    let _ = send.stopped().await;
    events(Event::FileSent { path: entry.path.clone() });
    Ok(())
}

// ---------------------------------------------------------------- receiving

/// An offer as shown to the person deciding.
#[derive(Debug, Clone)]
pub struct IncomingOffer {
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

pub struct ReceiveOptions {
    pub storage: Arc<dyn ReceiveStorage>,
    pub decide: Decide,
    pub authorize: Authorize,
    /// Stop after the first offer, accepted or not.
    pub once: bool,
}

/// Reason sent when closing a connection from a key that is not allowed.
const NOT_PAIRED: &[u8] = b"not-paired";

/// Accepts connections and receives offers one at a time until the endpoint closes.
pub async fn serve(endpoint: &Endpoint, opts: &ReceiveOptions, events: &EventSink) -> Result<(), Error> {
    events(Event::ReceiveFolder { dir: opts.storage.describe("") });
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
        if let Err(e) = receive_one(&conn, opts, events).await {
            conn.close(1u32.into(), e.kind.id().as_bytes());
            events(Event::Failed(e));
        }
        if opts.once {
            break;
        }
    }
    Ok(())
}

async fn receive_one(conn: &Connection, opts: &ReceiveOptions, events: &EventSink) -> Result<(), Error> {
    let short = short_id(&conn.remote_id());
    events(Event::Connected { peer: short.clone() });
    let (mut ctl_send, mut ctl_recv) = conn.accept_bi().await.map_err(lost)?;
    let offer: Offer = proto::read(&mut ctl_recv).await?;
    let label = format!("\"{}\"", offer.device_name);
    let manifest = offer.manifest;
    if let Err(e) = manifest.validate() {
        let _ = proto::write(&mut ctl_send, &Answer::Decline).await;
        return Err(e);
    }
    let totals = manifest.totals();
    let dest = opts.storage.describe("");
    events(Event::OfferReceived { peer: label.clone(), totals, dest: dest.clone() });

    // The person may take their time; if the sender gives up meanwhile, the offer is missed.
    let decision = (opts.decide)(IncomingOffer { peer: label.clone(), totals, dest });
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
    proto::write(&mut ctl_send, &Answer::Accept { device_name: device_name() }).await?;

    let started = Instant::now();
    let storage = opts.storage.clone();
    let (m, ev) = (manifest.clone(), events.clone());
    let renames = blocking(move || place_roots(&m, storage.as_ref(), &ev)).await?;
    // Several files in flight hide the per-file round trip and fsync. Dropping the set
    // on an error cancels the rest.
    let mut in_flight = tokio::task::JoinSet::new();
    for (index, file) in manifest.files.iter().enumerate() {
        if in_flight.len() >= PARALLEL_FILES {
            in_flight.join_next().await.expect("set is not empty").map_err(task_failed)??;
        }
        let (conn, file, storage, events) = (conn.clone(), file.clone(), opts.storage.clone(), events.clone());
        let rel = rebase(&file.path, &renames);
        in_flight.spawn(async move {
            fetch_file(&conn, index as u32, &file, rel.clone(), storage.clone()).await?;
            events(Event::FileVerified {
                path: rel.rsplit('/').next().unwrap_or_default().to_string(),
                saved_to: storage.describe(&rel),
            });
            Ok::<(), Error>(())
        });
    }
    while let Some(result) = in_flight.join_next().await {
        result.map_err(task_failed)??;
    }
    proto::write(&mut ctl_send, &Done { files: manifest.files.len() as u32 }).await?;
    let _ = ctl_send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
    events(Event::Finished { peer: label, totals, secs: started.elapsed().as_secs_f64() });
    Ok(())
}

/// Picks free names for the offered items and creates the folders. Blocking.
fn place_roots(manifest: &Manifest, storage: &dyn ReceiveStorage, events: &EventSink) -> Result<Vec<(String, String)>, Error> {
    let mut renames = Vec::new();
    for root in &manifest.roots {
        let name = manifest::free_name(&root.name, root.is_dir, |n| storage.exists(n).unwrap_or(true));
        if name != root.name {
            events(Event::Renamed { from: root.name.clone(), to: name.clone() });
        }
        if root.is_dir {
            storage.make_dir(&name).map_err(|e| store_err(&name, e))?;
        }
        renames.push((root.name.clone(), name));
    }
    for dir in &manifest.dirs {
        let rel = rebase(dir, &renames);
        storage.make_dir(&rel).map_err(|e| store_err(&rel, e))?;
    }
    Ok(renames)
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

async fn fetch_file(conn: &Connection, index: u32, file: &FileEntry, rel: String, storage: Arc<dyn ReceiveStorage>) -> Result<(), Error> {
    let hash = bao_tree::blake3::Hash::from_bytes(file.hash);
    let st = storage.clone();
    let r = rel.clone();
    let partial = blocking(move || st.create_partial(&r).map_err(|e| store_err(&r, e))).await?;
    let Partial { file: mut target, rel, token } = partial;

    if file.size == 0 {
        if hash != bao_tree::blake3::hash(b"") {
            return Err(Error::new(ErrorKind::VerificationFailed, format!("{}: empty but fingerprint differs", file.path)));
        }
    } else {
        let (mut send, recv) = conn.open_bi().await.map_err(lost)?;
        proto::write(&mut send, &FileRequest { index, from_chunk: 0 }).await?;
        send.finish().map_err(lost)?;
        let tree = BaoTree::new(file.size, manifest::BLOCK_SIZE);
        let ranges = ChunkRanges::from(ChunkNum(0)..);
        let mut decoder = ResponseDecoder::new(hash, ranges, tree, TokioStreamReader(recv));
        let mut buf = BytesMut::new();
        let mut buf_start = 0u64;
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
                    target = write_at(target, buf_start, buf.split().freeze(), &rel).await?;
                }
                if buf.is_empty() {
                    buf_start = leaf.offset;
                }
                buf.extend_from_slice(&leaf.data);
            }
        }
        if !buf.is_empty() {
            target = write_at(target, buf_start, buf.split().freeze(), &rel).await?;
        }
    }
    // Every chunk was checked against the fingerprint on arrival, so the file is complete
    // and verified; publishing it never replaces an existing file.
    blocking(move || {
        let partial = Partial { file: target, rel: rel.clone(), token };
        storage.finish(partial).map_err(|e| store_err(&rel, e))
    })
    .await
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
