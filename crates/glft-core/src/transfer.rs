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
    Endpoint, EndpointAddr,
    endpoint::{Connection, RecvStream, SendStream},
};
use iroh_io::{AsyncSliceWriter, TokioStreamReader, TokioStreamWriter};

use crate::{
    error::{Error, ErrorKind},
    events::{Event, EventSink, Totals},
    manifest::{self, FileEntry, Manifest, PARTIAL_DIR, Source},
    net::{ALPN, short_id},
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
        .map_err(|e| Error::new(ErrorKind::PeerUnreachable, e.to_string()))?;
    events(Event::Connected { peer: label.clone() });

    let (mut ctl_send, mut ctl_recv) = conn.open_bi().await.map_err(lost)?;
    proto::write(&mut ctl_send, &Offer { device_name: device_name(), manifest: manifest.clone() }).await?;
    events(Event::Offered { peer: label.clone(), totals });

    match proto::read::<Answer>(&mut ctl_recv).await? {
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
    pub dest: PathBuf,
}

/// Decides whether to accept an offer; may ask the user.
pub type Decide = Arc<dyn Fn(IncomingOffer) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

pub struct ReceiveOptions {
    pub dir: PathBuf,
    pub decide: Decide,
    /// Stop after the first offer, accepted or not.
    pub once: bool,
}

/// Accepts connections and receives offers one at a time until the endpoint closes.
pub async fn serve(endpoint: &Endpoint, opts: &ReceiveOptions, events: &EventSink) -> Result<(), Error> {
    std::fs::create_dir_all(&opts.dir).map_err(|e| Error::io(ErrorKind::ReceiveDirUnwritable, &opts.dir, e))?;
    events(Event::ReceiveFolder { dir: opts.dir.clone() });
    while let Some(incoming) = endpoint.accept().await {
        let conn = match incoming.await {
            Ok(conn) => conn,
            Err(e) => {
                events(Event::Failed(Error::new(ErrorKind::ConnectionLost, e.to_string())));
                continue;
            }
        };
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
    events(Event::OfferReceived { peer: label.clone(), totals, dest: opts.dir.clone() });
    let offer = IncomingOffer { peer: label.clone(), totals, dest: opts.dir.clone() };
    if !(opts.decide)(offer).await {
        proto::write(&mut ctl_send, &Answer::Decline).await?;
        let _ = ctl_send.finish();
        events(Event::Declined { peer: "You".into() });
        let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
        return Ok(());
    }
    proto::write(&mut ctl_send, &Answer::Accept { device_name: device_name() }).await?;

    let started = Instant::now();
    let dest = &opts.dir;
    let renames = place_roots(&manifest, dest, events)?;
    let partial_dir = dest.join(PARTIAL_DIR);
    mkdir(&partial_dir)?;
    // Several files in flight hide the per-file round trip and fsync. Dropping the set
    // on an error cancels the rest.
    let mut in_flight = tokio::task::JoinSet::new();
    for (index, file) in manifest.files.iter().enumerate() {
        if in_flight.len() >= PARALLEL_FILES {
            in_flight.join_next().await.expect("set is not empty").map_err(task_failed)??;
        }
        let (conn, file, partial_dir, events) = (conn.clone(), file.clone(), partial_dir.clone(), events.clone());
        let final_path = dest.join(rebase(&file.path, &renames));
        in_flight.spawn(async move {
            fetch_file(&conn, index as u32, &file, &partial_dir, &final_path).await?;
            events(Event::FileVerified {
                path: final_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                saved_to: final_path.parent().map(Path::to_path_buf).unwrap_or_default(),
            });
            Ok::<(), Error>(())
        });
    }
    while let Some(result) = in_flight.join_next().await {
        result.map_err(task_failed)??;
    }
    let _ = std::fs::remove_dir(&partial_dir);
    proto::write(&mut ctl_send, &Done { files: manifest.files.len() as u32 }).await?;
    let _ = ctl_send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
    events(Event::Finished { peer: label, totals, secs: started.elapsed().as_secs_f64() });
    Ok(())
}

/// Picks free names for the offered items and creates the folders.
fn place_roots(manifest: &Manifest, dest: &Path, events: &EventSink) -> Result<Vec<(String, String)>, Error> {
    let mut renames = Vec::new();
    for root in &manifest.roots {
        let name = manifest::free_name(dest, &root.name, root.is_dir);
        if name != root.name {
            events(Event::Renamed { from: root.name.clone(), to: name.clone() });
        }
        if root.is_dir {
            mkdir(&dest.join(&name))?;
        }
        renames.push((root.name.clone(), name));
    }
    for dir in &manifest.dirs {
        mkdir(&dest.join(rebase(dir, &renames)))?;
    }
    Ok(renames)
}

/// Replaces a validated path's first segment with the root's chosen name.
fn rebase(path: &str, renames: &[(String, String)]) -> PathBuf {
    let (first, rest) = path.split_once('/').unwrap_or((path, ""));
    let root = renames.iter().find(|(from, _)| from == first).map(|(_, to)| to.as_str()).unwrap_or(first);
    let mut out = PathBuf::from(root);
    for seg in rest.split('/').filter(|s| !s.is_empty()) {
        out.push(seg);
    }
    out
}

fn mkdir(path: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(path).map_err(|e| Error::io(ErrorKind::ReceiveDirUnwritable, path, e))
}

async fn fetch_file(conn: &Connection, index: u32, file: &FileEntry, partial_dir: &Path, final_path: &Path) -> Result<(), Error> {
    let hash = bao_tree::blake3::Hash::from_bytes(file.hash);
    let partial = partial_dir.join(hash.to_hex().as_str());
    let write_err = |e| Error::io(ErrorKind::ReceiveDirUnwritable, &partial, e);
    let p = partial.clone();
    let mut target = iroh_io::File::create(move || std::fs::File::create(p)).await.map_err(write_err)?;

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
                if buf.is_empty() {
                    buf_start = leaf.offset;
                } else if buf_start + buf.len() as u64 != leaf.offset || buf.len() >= WRITE_BUFFER {
                    target.write_bytes_at(buf_start, buf.split().freeze()).await.map_err(write_err)?;
                    buf_start = leaf.offset;
                }
                buf.extend_from_slice(&leaf.data);
            }
        }
        if !buf.is_empty() {
            target.write_bytes_at(buf_start, buf.split().freeze()).await.map_err(write_err)?;
        }
    }
    target.sync().await.map_err(write_err)?;
    drop(target);
    // Every chunk was checked against the root hash on arrival, so the file is complete
    // and verified. Never overwrite something that appeared meanwhile.
    if std::fs::symlink_metadata(final_path).is_ok() {
        return Err(Error::new(ErrorKind::ReceiveDirUnwritable, format!("{} appeared during the transfer", final_path.display())));
    }
    std::fs::rename(&partial, final_path).map_err(|e| Error::io(ErrorKind::ReceiveDirUnwritable, final_path, e))
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
