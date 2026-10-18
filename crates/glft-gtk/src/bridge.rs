//! The window's line to the engine. Engine I/O runs on a tokio thread; the GTK main loop
//! sends it [`Cmd`]s and receives [`Msg`]s.
//!
//! The heartbeat comes from the main loop ([`Cmd::Ping`] every [`PING_EVERY`]), so a
//! frozen window stops it and the engine lets go (design rule 1). A lost watch is opened
//! again only by the next ping, never by this thread alone, so a frozen window cannot keep
//! the engine alive by reconnecting.

use glft_core::devices::Remember;
use glft_engine::{ErrorInfo, Reply, Request, client::Session};
use tokio::sync::mpsc;

pub use glft_engine::PING_EVERY;

/// From the window to the engine thread.
pub enum Cmd {
    /// The heartbeat; opens the watch if it isn't open.
    Ping,
    /// A one-off request (receive on/off, answer, forget, send...). Failures come back as
    /// [`Msg::Failed`].
    Request(Request),
    /// Show a pairing code, or use one when `code` is given.
    Pair { code: Option<String> },
    PairAnswer { remember: Remember, auto_accept: bool },
    PairCancel,
}

/// From the engine thread to the window.
pub enum Msg {
    /// A reply on the watch: status, devices, log lines, progress, offers.
    Watch(Reply),
    /// The watch closed (engine stopped, restarted or unreachable).
    Lost(ErrorInfo),
    Failed(ErrorInfo),
    PairCode(String),
    PairPeer { name: String },
    /// Pairing finished (`None`) or failed.
    PairDone(Option<ErrorInfo>),
}

pub fn start() -> (mpsc::UnboundedSender<Cmd>, async_channel::Receiver<Msg>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (msg_tx, msg_rx) = async_channel::unbounded();
    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime");
            rt.block_on(run(cmd_rx, msg_tx));
        })
        .expect("engine thread");
    (cmd_tx, msg_rx)
}

async fn run(mut cmds: mpsc::UnboundedReceiver<Cmd>, msgs: async_channel::Sender<Msg>) {
    let mut watch: Option<mpsc::UnboundedSender<Request>> = None;
    let mut pairing: Option<mpsc::UnboundedSender<Request>> = None;
    while let Some(cmd) = cmds.recv().await {
        match cmd {
            Cmd::Ping => match &watch {
                Some(w) if !w.is_closed() => {
                    let _ = w.send(Request::Ping);
                }
                _ => watch = Some(open_watch(msgs.clone())),
            },
            Cmd::Request(request) => {
                let msgs = msgs.clone();
                tokio::spawn(async move {
                    if let Err(e) = one_off(request).await {
                        let _ = msgs.send(Msg::Failed(e)).await;
                    }
                });
            }
            Cmd::Pair { code } => {
                let request = match code {
                    Some(code) => Request::Join { code },
                    None => Request::Pair,
                };
                pairing = Some(open_pairing(request, msgs.clone()));
            }
            Cmd::PairAnswer { remember, auto_accept } => {
                if let Some(p) = &pairing {
                    let _ = p.send(Request::PairAnswer { remember, auto_accept });
                }
            }
            // Dropping the sender ends the session, which cancels the pairing.
            Cmd::PairCancel => pairing = None,
        }
    }
}

async fn one_off(request: Request) -> Result<(), ErrorInfo> {
    let mut s = Session::open(request, |_| {}).await?;
    while s.next().await?.is_some() {}
    Ok(())
}

/// Opens a [`Request::Watch`] and forwards its replies; requests sent to the returned
/// channel (pings) go to the engine. The channel closes when the watch ends.
fn open_watch(msgs: async_channel::Sender<Msg>) -> mpsc::UnboundedSender<Request> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Request>();
    tokio::spawn(async move {
        let mut s = match Session::open(Request::Watch, |_| {}).await {
            Ok(s) => s,
            Err(e) => {
                let _ = msgs.send(Msg::Lost(e)).await;
                return;
            }
        };
        let end = loop {
            tokio::select! {
                reply = s.next() => match reply {
                    Ok(Some(r)) => { let _ = msgs.send(Msg::Watch(r)).await; }
                    Ok(None) => break glft_engine::client::lost_engine("the engine ended the watch"),
                    Err(e) => break e,
                },
                request = rx.recv() => match request {
                    Some(r) => if let Err(e) = s.send(&r).await { break e },
                    None => return,
                },
            }
        };
        rx.close();
        let _ = msgs.send(Msg::Lost(end)).await;
    });
    tx
}

fn open_pairing(request: Request, msgs: async_channel::Sender<Msg>) -> mpsc::UnboundedSender<Request> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Request>();
    tokio::spawn(async move {
        // Ok(true): paired; Ok(false): cancelled by the window.
        let result: Result<bool, ErrorInfo> = async {
            let mut s = Session::open(request, |_| {}).await?;
            loop {
                tokio::select! {
                    reply = s.next() => match reply? {
                        Some(Reply::PairCode { code }) => { let _ = msgs.send(Msg::PairCode(code)).await; }
                        Some(Reply::PairPeer { name, .. }) => { let _ = msgs.send(Msg::PairPeer { name }).await; }
                        Some(_) => {}
                        None => return Ok(true),
                    },
                    answer = rx.recv() => match answer {
                        Some(a) => s.send(&a).await?,
                        // The window cancelled: closing the session tells the engine.
                        None => return Ok(false),
                    },
                }
            }
        }
        .await;
        match result {
            Ok(true) => { let _ = msgs.send(Msg::PairDone(None)).await; }
            Ok(false) => {}
            Err(e) => { let _ = msgs.send(Msg::PairDone(Some(e))).await; }
        }
    });
    tx
}
