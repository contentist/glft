//! Two endpoints in one process send a folder tree over 127.0.0.1.

use std::{
    fs,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::{Arc, Mutex},
};

use glft_core::{
    Event, ErrorKind,
    events::EventSink,
    net,
    storage::FsStorage,
    transfer::{self, Authorize, Decide, ReceiveOptions},
};
use iroh::SecretKey;

fn collecting() -> (EventSink, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink_log = log.clone();
    (Arc::new(move |e: Event| sink_log.lock().unwrap().push(e.to_string())), log)
}

fn allow_all() -> Authorize {
    Arc::new(|_| true)
}

fn answer(accept: bool) -> Decide {
    Arc::new(move |_| Box::pin(async move { accept }))
}

/// Deterministic pseudo-random bytes, so the big file is not trivially compressible.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn make_album(root: &Path) -> std::path::PathBuf {
    let album = root.join("Album");
    fs::create_dir_all(album.join("Disc 1")).unwrap();
    fs::create_dir_all(album.join("empty folder")).unwrap();
    fs::write(album.join("Disc 1/track01.flac"), noise(5 * 1024 * 1024 + 123, 1)).unwrap();
    fs::write(album.join("Disc 1/track02.flac"), noise(16 * 1024, 2)).unwrap();
    fs::write(album.join("cover.jpg"), noise(1, 3)).unwrap();
    fs::write(album.join("empty.txt"), b"").unwrap();
    album
}

fn tree_hashes(root: &Path) -> Vec<(String, Option<String>)> {
    let mut out: Vec<_> = walkdir(root)
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let hash = p.is_file().then(|| blake3::hash(&fs::read(&p).unwrap()).to_hex().to_string());
            (rel, hash)
        })
        .collect();
    out.sort();
    out
}

fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            out.extend(walkdir(&p));
        }
        out.push(p);
    }
    out
}

async fn receiver(dest: &Path, decide: Decide) -> (iroh::Endpoint, iroh::EndpointAddr, tokio::task::JoinHandle<()>, Arc<Mutex<Vec<String>>>) {
    receiver_with(dest, decide, allow_all()).await
}

async fn receiver_with(
    dest: &Path,
    decide: Decide,
    authorize: Authorize,
) -> (iroh::Endpoint, iroh::EndpointAddr, tokio::task::JoinHandle<()>, Arc<Mutex<Vec<String>>>) {
    let (events, log) = collecting();
    let ep = net::bind(SecretKey::generate(), Some(0), &events).await.unwrap();
    let port = ep.bound_sockets()[0].port();
    let addr = net::peer_addr(ep.id(), &[SocketAddr::from((Ipv4Addr::LOCALHOST, port))]);
    let storage = Arc::new(FsStorage::new(dest.to_path_buf()).unwrap());
    let opts = ReceiveOptions { storage, decide, authorize, once: true };
    let ep2 = ep.clone();
    let task = tokio::spawn(async move { transfer::serve(&ep2, &opts, &events).await.unwrap() });
    (ep, addr, task, log)
}

#[tokio::test]
async fn folder_arrives_verified_with_structure() {
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let album = make_album(src.path());
    // A clash: the receiver already has an Album.
    fs::create_dir(dest.path().join("Album")).unwrap();
    fs::write(src.path().join("notes.txt"), b"hello").unwrap();

    let (_rx_ep, addr, rx_task, rx_log) = receiver(dest.path(), answer(true)).await;
    let (events, tx_log) = collecting();
    let tx = net::bind_ephemeral(SecretKey::generate()).await.unwrap();
    let totals = transfer::send(&tx, addr, vec![album.clone(), src.path().join("notes.txt")], &events).await.unwrap();
    rx_task.await.unwrap();

    assert_eq!(totals.files, 5);
    assert_eq!(tree_hashes(&album), tree_hashes(&dest.path().join("Album (2)")));
    assert_eq!(fs::read(dest.path().join("notes.txt")).unwrap(), b"hello");
    let leftovers: Vec<_> = walkdir(dest.path()).into_iter().filter(|p| p.to_string_lossy().ends_with(".glft-part")).collect();
    assert!(leftovers.is_empty(), "partials left behind: {leftovers:?}");

    let rx_log = rx_log.lock().unwrap().join("\n");
    assert!(rx_log.contains("offers 1 folder, 5 files"), "{rx_log}");
    assert!(rx_log.contains("\"Album\" already exists here; saving as \"Album (2)\""), "{rx_log}");
    assert!(rx_log.contains("All 5 files verified"), "{rx_log}");
    let tx_log = tx_log.lock().unwrap().join("\n");
    assert!(tx_log.contains("All 5 files verified"), "{tx_log}");
}

#[tokio::test]
async fn declined_offer_saves_nothing() {
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let album = make_album(src.path());
    let (_rx_ep, addr, rx_task, _) = receiver(dest.path(), answer(false)).await;
    let tx = net::bind_ephemeral(SecretKey::generate()).await.unwrap();
    let err = transfer::send(&tx, addr, vec![album], &glft_core::events::discard()).await.unwrap_err();
    rx_task.await.unwrap();
    assert_eq!(err.kind, ErrorKind::Declined);
    assert_eq!(fs::read_dir(dest.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn unreachable_peer_is_explained() {
    let tx = net::bind_ephemeral(SecretKey::generate()).await.unwrap();
    let nobody = SecretKey::generate().public();
    let addr = net::peer_addr(nobody, &[SocketAddr::from((Ipv4Addr::LOCALHOST, 9))]);
    let src = tempfile::tempdir().unwrap();
    fs::write(src.path().join("a"), b"x").unwrap();
    let err = transfer::send(&tx, addr, vec![src.path().join("a")], &glft_core::events::discard()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::PeerUnreachable);
}

#[tokio::test]
async fn unpaired_device_is_refused_before_any_offer() {
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let album = make_album(src.path());
    let asked = Arc::new(Mutex::new(false));
    let asked2 = asked.clone();
    let decide: Decide = Arc::new(move |_| {
        *asked2.lock().unwrap() = true;
        Box::pin(async { true })
    });
    let deny: Authorize = Arc::new(|_| false);
    let (_rx_ep, addr, rx_task, rx_log) = receiver_with(dest.path(), decide, deny).await;
    let tx = net::bind_ephemeral(SecretKey::generate()).await.unwrap();
    let err = transfer::send(&tx, addr, vec![album], &quiet()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotPaired, "{err:?}");
    rx_task.abort();
    assert!(!*asked.lock().unwrap(), "an unpaired device must never reach the accept question");
    assert_eq!(fs::read_dir(dest.path()).unwrap().count(), 0);
    let rx_log = rx_log.lock().unwrap().join("\n");
    assert!(rx_log.contains("not paired with"), "{rx_log}");
}

#[tokio::test]
async fn offer_left_unanswered_becomes_missed_when_sender_gives_up() {
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let album = make_album(src.path());
    let never: Decide = Arc::new(|_| Box::pin(std::future::pending()));
    let (_rx_ep, addr, rx_task, rx_log) = receiver(dest.path(), never).await;
    let tx = net::bind_ephemeral(SecretKey::generate()).await.unwrap();
    let tx2 = tx.clone();
    let sending = tokio::spawn(async move { transfer::send(&tx2, addr, vec![album], &quiet()).await });
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(!sending.is_finished(), "the sender keeps waiting while nobody answers");
    sending.abort();
    tx.close().await;
    tokio::time::timeout(std::time::Duration::from_secs(10), rx_task).await.unwrap().unwrap();
    let rx_log = rx_log.lock().unwrap().join("\n");
    assert!(rx_log.contains("Missed:"), "{rx_log}");
    assert_eq!(fs::read_dir(dest.path()).unwrap().count(), 0);
}

fn quiet() -> EventSink {
    glft_core::events::discard()
}
