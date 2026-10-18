//! The engine as a process: its lifetime (the heartbeat rule), watching, cancelling, and
//! a send between two engines.

use std::{
    path::PathBuf,
    process::{Child, Command},
    time::{Duration, Instant},
};

use glft_core::devices::Remember;
use glft_engine::{Conn, PING_EVERY, Reply, Request, Status};
use tokio::net::UnixStream;

/// A `glftd` with its own runtime and data folders.
struct Engine {
    child: Child,
    dir: tempfile::TempDir,
}

impl Engine {
    fn start(name: &str, idle_secs: f64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_glftd"))
            .env(glft_engine::RUNTIME_DIR_ENV, dir.path().join("run"))
            .env(glft_core::paths::DATA_DIR_ENV, dir.path().join("data"))
            .env(glft_core::paths::DEVICE_NAME_ENV, name)
            .env("GLFT_IDLE_SECS", idle_secs.to_string())
            .env("GLFT_NO_DESKTOP", "1")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        Self { child, dir }
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("run/engine.sock")
    }

    /// Sends `request` and checks the acknowledgement.
    async fn open(&self, request: Request) -> Conn {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match UnixStream::connect(self.socket()).await {
                Ok(s) => break s,
                Err(_) if Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(50)).await,
                Err(e) => panic!("engine socket: {e}"),
            }
        };
        let mut conn = Conn::new(stream);
        conn.send(&request).await.unwrap();
        assert!(matches!(conn.recv::<Reply>().await.unwrap(), Some(Reply::Ack { .. })));
        conn
    }

    /// Runs `request` to the end and returns its replies.
    async fn run(&self, request: Request) -> Vec<Reply> {
        let mut conn = self.open(request).await;
        let mut replies = Vec::new();
        loop {
            match conn.recv::<Reply>().await.unwrap() {
                Some(Reply::Done) | None => return replies,
                Some(Reply::Failed(e)) => panic!("{}: {}", e.headline, e.detail),
                Some(r) => replies.push(r),
            }
        }
    }

    async fn status(&self) -> Status {
        self.run(Request::Status)
            .await
            .into_iter()
            .find_map(|r| if let Reply::Status(s) = r { Some(s) } else { None })
            .unwrap()
    }

    async fn log(&self) -> Vec<String> {
        self.run(Request::Log { follow: false })
            .await
            .into_iter()
            .filter_map(|r| if let Reply::Line(l) = r { Some(l.sentence) } else { None })
            .collect()
    }

    /// Whether the process has exited, waiting up to `limit`.
    fn exits_within(&mut self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn an_engine_nothing_needs_exits() {
    let mut e = Engine::start("Idle", 1.0);
    e.status().await;
    assert!(e.exits_within(Duration::from_secs(5)), "the engine kept running with nothing to do");
}

#[tokio::test]
async fn a_watching_window_keeps_it_running_until_its_heartbeat_stops() {
    let mut e = Engine::start("Watched", 1.0);
    let mut watch = e.open(Request::Watch).await;
    assert!(matches!(watch.recv::<Reply>().await.unwrap(), Some(Reply::Status(_))));
    assert!(matches!(watch.recv::<Reply>().await.unwrap(), Some(Reply::Devices { .. })));

    // Pinging: still running well past the idle limit.
    let until = Instant::now() + Duration::from_secs(3);
    let mut last_ping = Instant::now();
    while Instant::now() < until {
        watch.send(&Request::Ping).await.unwrap();
        last_ping = Instant::now();
        tokio::time::sleep(PING_EVERY).await;
    }
    assert!(!e.exits_within(Duration::ZERO), "the engine exited although a window was watching");

    // The window freezes: connected, but silent. The engine lets go of it and exits.
    let mut ended = false;
    while let Ok(Ok(Some(reply))) = tokio::time::timeout(Duration::from_secs(10), watch.recv::<Reply>()).await {
        if let Reply::Failed(f) = reply {
            assert_eq!(f.id, "engine-not-answering");
            ended = true;
            break;
        }
    }
    assert!(ended, "the watch did not end");
    let took = last_ping.elapsed();
    assert!(took >= glft_engine::PING_LIMIT && took < glft_engine::PING_LIMIT + Duration::from_secs(2), "watch ended {took:?} after the last ping");
    assert!(e.exits_within(Duration::from_secs(5)), "the engine kept running after the window froze");
}

#[tokio::test]
async fn a_send_can_be_cancelled() {
    let mut e = Engine::start("Sender", 1.0);
    let file = e.dir.path().join("a.txt");
    std::fs::write(&file, b"hello").unwrap();
    // A device that isn't there: the send keeps trying until cancelled.
    let nobody = iroh::SecretKey::generate().public().to_string();
    e.run(Request::Send { device: nobody, addrs: vec!["127.0.0.1:9".parse().unwrap()], paths: vec![file], detach: true }).await;
    let status = e.status().await;
    let [send] = status.sending.as_slice() else { panic!("expected one send, got {:?}", status.sending) };
    let id = send.id;

    // While a send runs, the engine stays.
    assert!(!e.exits_within(Duration::from_secs(2)));
    e.run(Request::CancelSend { id }).await;
    assert!(e.status().await.sending.is_empty());
    assert!(e.log().await.iter().any(|l| l.contains("cancelled")));
    assert!(e.exits_within(Duration::from_secs(5)));
}

#[tokio::test]
async fn two_engines_pair_and_send_with_tagged_progress() {
    let a = Engine::start("Laptop A", 0.0);
    let b = Engine::start("Laptop B", 0.0);
    let inbox = b.dir.path().join("inbox");
    b.run(Request::Receive { on: true, dir: Some(inbox.clone()), port: Some(0) }).await;

    // A shows a code, B uses it; both remember, and A's files are accepted without asking.
    pair(&a, Remember::Yes, &b, Remember::Yes, true).await;

    let folder = a.dir.path().join("Album");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("track01.flac"), vec![7u8; 3 << 20]).unwrap();
    let addrs = b.status().await.receiving.unwrap().addrs;
    let replies = a.run(Request::Send { device: "Laptop B".into(), addrs, paths: vec![folder], detach: false }).await;
    let tags: Vec<Option<u64>> = replies.iter().filter_map(|r| if let Reply::Progress { send, .. } = r { Some(*send) } else { None }).collect();
    assert!(!tags.is_empty(), "no progress reported");
    assert!(tags.iter().all(|t| t.is_some()), "progress without a send id: {tags:?}");
    assert_eq!(std::fs::read(inbox.join("Album/track01.flac")).unwrap().len(), 3 << 20);
    assert!(a.status().await.sending.is_empty());
}

#[tokio::test]
async fn a_pairing_for_this_session_keeps_the_engine_until_it_is_forgotten() {
    let mut a = Engine::start("Laptop A", 1.0);
    let b = Engine::start("Phone B", 0.0);
    pair(&a, Remember::JustThisTime, &b, Remember::Yes, false).await;
    // The pairing exists only in A's memory: exiting would lose it.
    assert!(!a.exits_within(Duration::from_secs(3)), "the engine exited and lost a pairing for this session");
    a.run(Request::Forget { device: "Phone B".into() }).await;
    assert!(a.exits_within(Duration::from_secs(5)), "the engine kept running after the pairing was forgotten");
}

/// `host` shows a code and `joiner` uses it; each answers "Remember this device?".
async fn pair(host: &Engine, host_answer: Remember, joiner: &Engine, join_answer: Remember, join_auto_accept: bool) {
    let mut h = host.open(Request::Pair).await;
    let code = loop {
        if let Some(Reply::PairCode { code }) = h.recv::<Reply>().await.unwrap() {
            break code;
        }
    };
    let mut j = joiner.open(Request::Join { code }).await;
    for (conn, remember, auto_accept) in [(&mut h, host_answer, false), (&mut j, join_answer, join_auto_accept)] {
        loop {
            match conn.recv::<Reply>().await.unwrap() {
                Some(Reply::PairPeer { .. }) => break,
                Some(Reply::Failed(f)) => panic!("{}: {}", f.headline, f.detail),
                Some(_) => {}
                None => panic!("pairing connection closed"),
            }
        }
        conn.send(&Request::PairAnswer { remember, auto_accept }).await.unwrap();
    }
    for conn in [&mut h, &mut j] {
        loop {
            match conn.recv::<Reply>().await.unwrap() {
                Some(Reply::Done) => break,
                Some(Reply::Failed(f)) => panic!("{}: {}", f.headline, f.detail),
                Some(_) => {}
                None => panic!("pairing connection closed"),
            }
        }
    }
}
