//! Interrupted transfers continue where they stopped; pairing by code.

use std::{
    fs,
    io::Write,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use glft_core::{
    ErrorKind, Event,
    devices::Remember,
    events::EventSink,
    net,
    pairing::{self, Host, PairingCode},
    resume::Records,
    storage::{FsStorage, Partial, ReceiveStorage},
    transfer::{self, Decide, ReceiveOptions},
};
use iroh::{Endpoint, SecretKey};
use tokio::sync::Notify;

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
    fs::create_dir_all(&album).unwrap();
    fs::write(album.join("big.bin"), noise(60 * 1024 * 1024 + 77, 9)).unwrap();
    for i in 0..5 {
        fs::write(album.join(format!("small{i}.txt")), noise(3000 + i, i as u64)).unwrap();
    }
    album
}

fn same_tree(a: &Path, b: &Path) {
    let mut names: Vec<_> = fs::read_dir(a).unwrap().map(|e| e.unwrap().file_name()).collect();
    names.sort();
    let mut other: Vec<_> = fs::read_dir(b).unwrap().map(|e| e.unwrap().file_name()).collect();
    other.sort();
    assert_eq!(names, other, "same files, and no partials left");
    for n in names {
        assert!(fs::read(a.join(&n)).unwrap() == fs::read(b.join(&n)).unwrap(), "{n:?} differs");
    }
}

/// A log, plus a signal fired the first time `trigger` matches an event.
fn watched(trigger: impl Fn(&Event) -> bool + Send + Sync + 'static) -> (EventSink, Arc<Mutex<Vec<String>>>, Arc<Notify>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let fired = Arc::new(Notify::new());
    let (l, f) = (log.clone(), fired.clone());
    let once = std::sync::atomic::AtomicBool::new(false);
    let sink: EventSink = Arc::new(move |e: Event| {
        if trigger(&e) && !once.swap(true, Ordering::SeqCst) {
            f.notify_one();
        }
        if !matches!(e, Event::Progress { .. }) {
            l.lock().unwrap().push(e.to_string());
        }
    });
    (sink, log, fired)
}

/// Plain files, except that a file still arriving also "exists" under its final name, as
/// with Android's media store, whose pending entries carry their final name.
struct LikeMediaStore {
    fs: FsStorage,
    root: std::path::PathBuf,
}

impl LikeMediaStore {
    fn arriving(&self, rel: &str) -> bool {
        let path = self.root.join(rel);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        path.with_file_name(format!(".{name}.glft-part")).exists()
    }
}

impl ReceiveStorage for LikeMediaStore {
    fn exists(&self, rel: &str) -> std::io::Result<bool> {
        Ok(self.fs.exists(rel)? || self.arriving(rel))
    }
    fn published(&self, rel: &str) -> std::io::Result<bool> {
        self.fs.published(rel)
    }
    fn make_dir(&self, rel: &str) -> std::io::Result<()> {
        self.fs.make_dir(rel)
    }
    fn create_partial(&self, rel: &str) -> std::io::Result<Partial> {
        self.fs.create_partial(rel)
    }
    fn reopen_partial(&self, rel: &str, token: &str) -> std::io::Result<Option<Partial>> {
        self.fs.reopen_partial(rel, token)
    }
    fn finish(&self, partial: Partial, hash: &[u8; 32]) -> std::io::Result<()> {
        self.fs.finish(partial, hash)
    }
    fn discard(&self, partial: Partial) -> std::io::Result<()> {
        self.fs.discard(partial)
    }
    fn discard_abandoned(&self, token: &str) -> std::io::Result<()> {
        self.fs.discard_abandoned(token)
    }
    fn describe(&self, rel: &str) -> String {
        self.fs.describe(rel)
    }
}

struct Receiver {
    endpoint: Endpoint,
    task: tokio::task::JoinHandle<()>,
}

async fn start_receiver(key: &SecretKey, port: u16, dest: &Path, state: &Path, decide: Decide, events: EventSink, once: bool) -> Receiver {
    // The port of a just-closed endpoint can take a moment to come free.
    let mut tries = 0;
    let endpoint = loop {
        match net::bind(key.clone(), Some(port), &events).await {
            Ok(ep) => break ep,
            Err(e) if tries < 50 => {
                tries += 1;
                let _ = e;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => panic!("{e:?}"),
        }
    };
    let opts = Arc::new(ReceiveOptions {
        storage: Arc::new(LikeMediaStore { fs: FsStorage::new(dest.to_path_buf()).unwrap(), root: dest.to_path_buf() }),
        decide,
        authorize: Arc::new(|_| true),
        records: Records::new(state),
        seen: None,
        once,
    });
    let ep = endpoint.clone();
    let task = tokio::spawn(async move {
        let _ = transfer::serve(&ep, opts, &events).await;
    });
    Receiver { endpoint, task }
}

fn counting_yes(count: Arc<AtomicUsize>) -> Decide {
    Arc::new(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { true })
    })
}

fn free_port() -> u16 {
    std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port()
}

#[tokio::test(flavor = "multi_thread")]
async fn receiver_restart_mid_file_resumes_without_asking() {
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let album = make_album(src.path());
    let rx_key = SecretKey::generate();
    let port = free_port();
    let asked = Arc::new(AtomicUsize::new(0));

    // First receiver: stopped as soon as 8 MB of the big file are verified and recorded.
    let (rx_events, _, cut) = watched(|e| matches!(e, Event::Progress { done, total, .. } if *done >= 8 << 20 && done < total));
    let rx = start_receiver(&rx_key, port, dest.path(), state.path(), counting_yes(asked.clone()), rx_events, false).await;

    let (tx_events, tx_log, _) = watched(|_| false);
    let tx = net::bind_ephemeral(SecretKey::generate()).await.unwrap();
    let peer = net::peer_addr(rx_key.public(), &[SocketAddr::from((Ipv4Addr::LOCALHOST, port))]);
    let sending = tokio::spawn(async move { transfer::send(&tx, peer, vec![album], &tx_events).await });

    tokio::time::timeout(Duration::from_secs(60), cut.notified()).await.expect("8 MB arrive");
    rx.task.abort();
    rx.endpoint.close().await;
    let _ = rx.task.await;
    drop(rx.endpoint);

    // What a crash can leave: the partial with unsynced garbage after the recorded part.
    let partial = dest.path().join("Album/.big.bin.glft-part");
    let before = fs::metadata(&partial).expect("the partial is kept").len();
    assert!(before >= 8 << 20, "partial has {before} bytes");
    fs::OpenOptions::new().append(true).open(&partial).unwrap().write_all(&noise(100_000, 42)).unwrap();

    // Second receiver, same key, port and records, as after a restart.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (rx_events, rx_log, _) = watched(|_| false);
    let rx = start_receiver(&rx_key, port, dest.path(), state.path(), counting_yes(asked.clone()), rx_events, true).await;
    let totals = tokio::time::timeout(Duration::from_secs(90), sending).await.expect("finishes").unwrap().unwrap().totals;
    tokio::time::timeout(Duration::from_secs(10), rx.task).await.unwrap().unwrap();

    assert_eq!(totals.files, 6);
    same_tree(&src.path().join("Album"), &dest.path().join("Album"));
    assert_eq!(asked.load(Ordering::SeqCst), 1, "the person is asked once, not again on resume");
    let rx_log = rx_log.lock().unwrap().join("\n");
    assert!(rx_log.contains("Resuming the transfer"), "{rx_log}");
    let tx_log = tx_log.lock().unwrap().join("\n");
    assert!(tx_log.contains("waiting to resume"), "{tx_log}");
    assert!(tx_log.contains("Resuming:"), "{tx_log}");
    assert!(fs::read_dir(state.path().join("transfers")).unwrap().next().is_none(), "the record is removed when done");
}

/// A phone back on Wi-Fi usually has a new address. The sender knows only the old one, so
/// it has to pick up the new one from the local network lookup, and promptly.
#[tokio::test(flavor = "multi_thread")]
async fn receiver_back_on_a_new_address_is_found_quickly() {
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let album = make_album(src.path());
    let rx_key = SecretKey::generate();
    let port = free_port();
    let asked = Arc::new(AtomicUsize::new(0));

    let (rx_events, _, cut) = watched(|e| matches!(e, Event::Progress { done, total, .. } if *done >= 8 << 20 && done < total));
    let rx = start_receiver(&rx_key, port, dest.path(), state.path(), counting_yes(asked.clone()), rx_events, false).await;
    let (tx_events, tx_log, resumed) = watched(|e| matches!(e, Event::ResumingSend { .. }));
    let tx = net::bind_ephemeral(SecretKey::generate()).await.unwrap();
    let peer = net::peer_addr(rx_key.public(), &[SocketAddr::from((Ipv4Addr::LOCALHOST, port))]);
    let sending = tokio::spawn(async move { transfer::send(&tx, peer, vec![album], &tx_events).await });

    tokio::time::timeout(Duration::from_secs(60), cut.notified()).await.expect("8 MB arrive");
    // Gone without a word, as when Wi-Fi drops: no close is sent, so the sender only
    // notices when the connection times out.
    rx.task.abort();
    let _ = rx.task.await;
    drop(rx.endpoint);
    // Packets to the old address now vanish instead of being refused, like on Wi-Fi.
    let _black_hole = loop {
        if let Ok(s) = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, port)) {
            break s;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    // Gone long enough for the sender to be well into retrying the old address.
    tokio::time::sleep(Duration::from_secs(20)).await;
    let back = Instant::now();
    let (rx_events, _, _) = watched(|_| false);
    let rx = start_receiver(&rx_key, free_port(), dest.path(), state.path(), counting_yes(asked.clone()), rx_events, true).await;
    tokio::time::timeout(Duration::from_secs(30), resumed.notified()).await.expect("resumes on the new address");
    let took = back.elapsed();
    let delivered = tokio::time::timeout(Duration::from_secs(90), sending).await.expect("finishes").unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(10), rx.task).await.unwrap().unwrap();

    assert!(took < Duration::from_secs(3), "resumed {took:?} after the receiver came back");
    assert_eq!(delivered.totals.files, 6);
    assert_ne!(delivered.addr.map(|a| a.port()), Some(port), "finished on the new address");
    same_tree(&src.path().join("Album"), &dest.path().join("Album"));
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    let tx_log = tx_log.lock().unwrap().join("\n");
    assert!(tx_log.contains("Resuming:"), "{tx_log}");
}

/// Storage that hangs every file request during its first 10 seconds, so the sender sees an
/// accepted transfer that makes no progress at all.
struct HangsOnce {
    inner: LikeMediaStore,
    first: Mutex<Option<std::time::Instant>>,
}

impl ReceiveStorage for HangsOnce {
    fn exists(&self, rel: &str) -> std::io::Result<bool> {
        self.inner.exists(rel)
    }
    fn published(&self, rel: &str) -> std::io::Result<bool> {
        self.inner.published(rel)
    }
    fn make_dir(&self, rel: &str) -> std::io::Result<()> {
        self.inner.make_dir(rel)
    }
    fn create_partial(&self, rel: &str) -> std::io::Result<Partial> {
        let first = *self.first.lock().unwrap().get_or_insert_with(std::time::Instant::now);
        if let Some(left) = Duration::from_secs(10).checked_sub(first.elapsed()) {
            std::thread::sleep(left);
        }
        self.inner.create_partial(rel)
    }
    fn reopen_partial(&self, rel: &str, token: &str) -> std::io::Result<Option<Partial>> {
        self.inner.reopen_partial(rel, token)
    }
    fn finish(&self, partial: Partial, hash: &[u8; 32]) -> std::io::Result<()> {
        self.inner.finish(partial, hash)
    }
    fn discard(&self, partial: Partial) -> std::io::Result<()> {
        self.inner.discard(partial)
    }
    fn discard_abandoned(&self, token: &str) -> std::io::Result<()> {
        self.inner.discard_abandoned(token)
    }
    fn describe(&self, rel: &str) -> String {
        self.inner.describe(rel)
    }
}

/// An accepted transfer that stops moving reconnects after a few seconds rather than waiting
/// for the connection to time out, and still arrives complete.
#[tokio::test(flavor = "multi_thread")]
async fn a_transfer_that_stops_moving_reconnects_and_finishes() {
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let album = make_album(src.path());
    let rx_key = SecretKey::generate();
    let (rx_events, _, _) = watched(|_| false);
    let endpoint = net::bind(rx_key.clone(), Some(0), &rx_events).await.unwrap();
    let port = endpoint.bound_sockets()[0].port();
    let storage = HangsOnce {
        inner: LikeMediaStore { fs: FsStorage::new(dest.path().to_path_buf()).unwrap(), root: dest.path().to_path_buf() },
        first: Mutex::new(None),
    };
    let opts = Arc::new(ReceiveOptions {
        storage: Arc::new(storage),
        decide: counting_yes(Arc::new(AtomicUsize::new(0))),
        authorize: Arc::new(|_| true),
        records: Records::new(state.path()),
        seen: None,
        once: false,
    });
    let ep = endpoint.clone();
    let rx = tokio::spawn(async move {
        let _ = transfer::serve(&ep, opts, &rx_events).await;
    });

    let (tx_events, tx_log, _) = watched(|_| false);
    let tx = net::bind_ephemeral(SecretKey::generate()).await.unwrap();
    let peer = net::peer_addr(rx_key.public(), &[SocketAddr::from((Ipv4Addr::LOCALHOST, port))]);
    let delivered = tokio::time::timeout(Duration::from_secs(60), transfer::send(&tx, peer, vec![album], &tx_events))
        .await
        .expect("finishes")
        .unwrap();
    rx.abort();

    assert_eq!(delivered.totals.files, 6);
    same_tree(&src.path().join("Album"), &dest.path().join("Album"));
    let tx_log = tx_log.lock().unwrap().join("\n");
    assert!(tx_log.contains("waiting to resume"), "the stalled connection was given up: {tx_log}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sender_restart_resumes_the_same_files() {
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let album = make_album(src.path());
    let rx_key = SecretKey::generate();
    let port = free_port();
    let asked = Arc::new(AtomicUsize::new(0));
    let (rx_events, rx_log, cut) = watched(|e| matches!(e, Event::Progress { done, total, .. } if *done >= 8 << 20 && done < total));
    let rx = start_receiver(&rx_key, port, dest.path(), state.path(), counting_yes(asked.clone()), rx_events, false).await;
    let peer = net::peer_addr(rx_key.public(), &[SocketAddr::from((Ipv4Addr::LOCALHOST, port))]);
    let tx_key = SecretKey::generate();

    // The sending program is killed part-way.
    let tx = net::bind_ephemeral(tx_key.clone()).await.unwrap();
    let (a, p) = (album.clone(), peer.clone());
    let first = tokio::spawn(async move {
        let _ = transfer::send(&tx, p, vec![a], &glft_core::events::discard()).await;
    });
    tokio::time::timeout(Duration::from_secs(60), cut.notified()).await.expect("8 MB arrive");
    first.abort();
    let _ = first.await;

    // Started again with the same files.
    let tx = net::bind_ephemeral(tx_key).await.unwrap();
    let totals = tokio::time::timeout(Duration::from_secs(90), transfer::send(&tx, peer, vec![album], &glft_core::events::discard()))
        .await
        .expect("finishes")
        .unwrap()
        .totals;
    assert_eq!(totals.files, 6);
    same_tree(&src.path().join("Album"), &dest.path().join("Album"));
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    let rx_log = rx_log.lock().unwrap().join("\n");
    assert!(rx_log.contains("Resuming the transfer"), "{rx_log}");
    rx.task.abort();
}

async fn pair(host_choice: Remember, join_choice: Remember) -> (Remember, Remember) {
    let host = Host::start(SecretKey::generate(), vec![]).await.unwrap();
    let code = PairingCode::parse(&host.code().encode()).unwrap();
    let hosting = tokio::spawn(async move {
        let pending = host.accept(&glft_core::events::discard()).await.unwrap();
        pending.finish(host_choice, &glft_core::events::discard()).await.unwrap()
    });
    let joined = pairing::join(SecretKey::generate(), &code, vec![], &glft_core::events::discard()).await.unwrap();
    let host_saw = joined.finish(join_choice, &glft_core::events::discard()).await.unwrap();
    let join_saw = hosting.await.unwrap();
    (host_saw, join_saw)
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_exchanges_both_answers() {
    assert_eq!(pair(Remember::Yes, Remember::JustThisTime).await, (Remember::Yes, Remember::JustThisTime));
    assert_eq!(pair(Remember::No, Remember::Yes).await, (Remember::No, Remember::Yes));
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_secret_is_refused_and_the_code_still_works() {
    let host_key = SecretKey::generate();
    let host = Host::start(host_key.clone(), vec![]).await.unwrap();
    let code = host.code().clone();
    let (events, log, _) = watched(|_| false);
    let hosting = tokio::spawn(async move {
        let pending = host.accept(&events).await.unwrap();
        let name = pending.peer.name.clone();
        pending.finish(Remember::Yes, &glft_core::events::discard()).await.unwrap();
        name
    });
    let mut wrong = code.clone();
    wrong.secret[0] ^= 1;
    let err = pairing::join(SecretKey::generate(), &wrong, vec![], &glft_core::events::discard()).await.err().unwrap();
    assert_eq!(err.kind, ErrorKind::PairingCodeWrong);

    let joined = pairing::join(SecretKey::generate(), &code, vec![], &glft_core::events::discard()).await.unwrap();
    assert_eq!(joined.peer.id, host_key.public().to_string(), "the joiner learns the host's key from the code");
    joined.finish(Remember::Yes, &glft_core::events::discard()).await.unwrap();
    hosting.await.unwrap();
    let log = log.lock().unwrap().join("\n");
    assert!(log.contains("wrong code"), "{log}");
}

#[tokio::test]
async fn nobody_showing_the_code_is_explained() {
    let code = PairingCode {
        id: SecretKey::generate().public(),
        secret: [1; 16],
        addrs: vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 9))],
    };
    let err = pairing::join(SecretKey::generate(), &code, vec![], &glft_core::events::discard()).await.err().unwrap();
    assert_eq!(err.kind, ErrorKind::PairingNoAnswer);
}

/// Storage whose disk is full for the big file: the receiver fails with "disk full".
struct FullForBigFile {
    inner: LikeMediaStore,
}

impl ReceiveStorage for FullForBigFile {
    fn exists(&self, rel: &str) -> std::io::Result<bool> {
        self.inner.exists(rel)
    }
    fn published(&self, rel: &str) -> std::io::Result<bool> {
        self.inner.published(rel)
    }
    fn make_dir(&self, rel: &str) -> std::io::Result<()> {
        self.inner.make_dir(rel)
    }
    fn create_partial(&self, rel: &str) -> std::io::Result<Partial> {
        if rel.ends_with("big.bin") {
            return Err(std::io::Error::from(std::io::ErrorKind::StorageFull));
        }
        self.inner.create_partial(rel)
    }
    fn reopen_partial(&self, rel: &str, token: &str) -> std::io::Result<Option<Partial>> {
        self.inner.reopen_partial(rel, token)
    }
    fn finish(&self, partial: Partial, hash: &[u8; 32]) -> std::io::Result<()> {
        self.inner.finish(partial, hash)
    }
    fn discard(&self, partial: Partial) -> std::io::Result<()> {
        self.inner.discard(partial)
    }
    fn discard_abandoned(&self, token: &str) -> std::io::Result<()> {
        self.inner.discard_abandoned(token)
    }
    fn describe(&self, rel: &str) -> String {
        self.inner.describe(rel)
    }
}

/// A receiver whose disk is full says so, and the sender reports it instead of resending
/// the same bytes again and again.
#[tokio::test(flavor = "multi_thread")]
async fn a_full_disk_on_the_receiver_stops_the_sender_with_a_sentence() {
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let album = make_album(src.path());
    let rx_key = SecretKey::generate();
    let (rx_events, rx_log, _) = watched(|_| false);
    let endpoint = net::bind(rx_key.clone(), Some(0), &rx_events).await.unwrap();
    let port = endpoint.bound_sockets()[0].port();
    let storage = FullForBigFile { inner: LikeMediaStore { fs: FsStorage::new(dest.path().to_path_buf()).unwrap(), root: dest.path().to_path_buf() } };
    let opts = Arc::new(ReceiveOptions {
        storage: Arc::new(storage),
        decide: counting_yes(Arc::new(AtomicUsize::new(0))),
        authorize: Arc::new(|_| true),
        records: Records::new(state.path()),
        seen: None,
        once: false,
    });
    let ep = endpoint.clone();
    let rx = tokio::spawn(async move {
        let _ = transfer::serve(&ep, opts, &rx_events).await;
    });

    let (tx_events, _, _) = watched(|_| false);
    let tx = net::bind_ephemeral(SecretKey::generate()).await.unwrap();
    let peer = net::peer_addr(rx_key.public(), &[SocketAddr::from((Ipv4Addr::LOCALHOST, port))]);
    let err = tokio::time::timeout(Duration::from_secs(30), transfer::send(&tx, peer, vec![album], &tx_events))
        .await
        .expect("the sender gives up rather than retrying")
        .unwrap_err();
    rx.abort();
    assert_eq!(err.kind, glft_core::ErrorKind::PeerDiskFull, "{err:?}");
    assert!(rx_log.lock().unwrap().join("\n").contains("The disk is full"));
}

/// A link so poor that every attempt drops before the 8 MiB checkpoint still moves forward:
/// what arrived verified is kept each time, so resuming never starts over (the "0 bytes
/// here" loop seen when a fast second sender starved a phone).
#[tokio::test(flavor = "multi_thread")]
async fn short_attempts_each_keep_what_arrived() {
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let file = src.path().join("big.bin");
    fs::write(&file, noise(40 * 1024 * 1024 + 5, 3)).unwrap();
    let part = dest.path().join(".big.bin.glft-part");
    let rx_key = SecretKey::generate();
    let port = free_port();
    let kept = Arc::new(Mutex::new(Vec::new()));
    let k = kept.clone();
    let rx_events: EventSink = Arc::new(move |e: Event| {
        if let Event::ReceiveInterrupted { done, .. } = e {
            k.lock().unwrap().push(done);
        }
    });
    let rx = start_receiver(&rx_key, port, dest.path(), state.path(), counting_yes(Arc::new(AtomicUsize::new(0))), rx_events, false).await;
    let peer = net::peer_addr(rx_key.public(), &[SocketAddr::from((Ipv4Addr::LOCALHOST, port))]);
    let tx_key = SecretKey::generate();

    let mut base = 0;
    for round in 0..3 {
        let tx = net::bind_ephemeral(tx_key.clone()).await.unwrap();
        let (f, p, ep) = (file.clone(), peer.clone(), tx.clone());
        let attempt = tokio::spawn(async move {
            let _ = transfer::send(&tx, p, vec![f], &glft_core::events::discard()).await;
        });
        // Cut as soon as one more 4 MiB write has landed, well short of a checkpoint.
        let deadline = Instant::now() + Duration::from_secs(60);
        while fs::metadata(&part).map(|m| m.len()).unwrap_or(0) < base + (4 << 20) {
            assert!(Instant::now() < deadline, "round {round}: nothing arrived");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        attempt.abort();
        let _ = attempt.await;
        ep.close().await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while kept.lock().unwrap().len() <= round {
            assert!(Instant::now() < deadline, "round {round}: the receiver never noticed");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let done = kept.lock().unwrap()[round];
        assert!(done >= base + (4 << 20), "round {round}: kept {done} bytes, had {base} before");
        assert_eq!(done, fs::metadata(&part).unwrap().len(), "round {round}: all that was written is kept");
        base = done;
    }

    let tx = net::bind_ephemeral(tx_key).await.unwrap();
    tokio::time::timeout(Duration::from_secs(90), transfer::send(&tx, peer, vec![file.clone()], &glft_core::events::discard()))
        .await
        .expect("finishes")
        .unwrap();
    assert!(fs::read(&file).unwrap() == fs::read(dest.path().join("big.bin")).unwrap(), "arrived intact");
    rx.task.abort();
}
