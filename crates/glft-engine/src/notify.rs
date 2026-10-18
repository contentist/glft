//! Desktop notifications: the visible sign that receiving is on, and offers to answer.
//!
//! Without a notification server (no desktop session), the engine carries on and the
//! terminal commands do the same jobs.

use std::sync::Arc;

use notify_rust::{Hint, Notification, NotificationResponse, Timeout};
use tokio::sync::oneshot;

use crate::Engine;
use glft_engine::OfferInfo;

/// A shown notification; dropping it takes it off the screen.
pub struct Notice {
    cancel: Option<oneshot::Sender<()>>,
}

impl Drop for Notice {
    fn drop(&mut self) {
        if let Some(c) = self.cancel.take() {
            let _ = c.send(());
        }
    }
}

/// What the person did with a notification.
enum Outcome {
    Action(String),
    Dismissed,
}

/// Set (to anything) to show no notifications or panel icon, e.g. in tests.
pub const NO_DESKTOP_ENV: &str = "GLFT_NO_DESKTOP";

pub fn disabled() -> bool {
    std::env::var_os(NO_DESKTOP_ENV).is_some()
}

/// Shows `n` and calls `on_outcome` with what the person did, until the `Notice` is dropped.
fn show(n: Notification, on_outcome: impl Fn(Outcome) -> bool + Send + 'static) -> Notice {
    let (cancel, mut cancelled) = oneshot::channel();
    if disabled() {
        return Notice { cancel: Some(cancel) };
    }
    tokio::spawn(async move {
        loop {
            let Ok(handle) = n.show_async().await else { return };
            let (tx, rx) = oneshot::channel();
            let wait = handle.wait_for_action_async(move |r: &NotificationResponse| {
                let _ = tx.send(match r {
                    NotificationResponse::Action(a) => Outcome::Action(a.clone()),
                    NotificationResponse::Default => Outcome::Action("default".into()),
                    _ => Outcome::Dismissed,
                });
            });
            tokio::select! {
                _ = wait => {}
                _ = &mut cancelled => {
                    handle.close_async().await;
                    return;
                }
            }
            // `on_outcome` returns whether the notification should come back.
            let again = rx.await.map(&on_outcome).unwrap_or(false);
            if !again {
                return;
            }
        }
    });
    Notice { cancel: Some(cancel) }
}

/// "Receiving into ~/Downloads/glft", with Stop. It comes back if dismissed while
/// receiving is still on, since nothing should listen unseen.
pub fn receiving(engine: Arc<Engine>, folder: String) -> Option<Notice> {
    let mut n = Notification::new();
    n.appname("glft")
        .summary("glft is receiving")
        .body(&format!("Paired devices can send files into {folder}. Stop closes the network port."))
        .icon("folder-download")
        .hint(Hint::Resident(true))
        .timeout(Timeout::Never)
        .action("stop", "Stop receiving");
    Some(show(n, move |outcome| match outcome {
        Outcome::Action(a) if a == "stop" => {
            let engine = engine.clone();
            tokio::spawn(async move { engine.stop_receiving().await });
            false
        }
        _ => true,
    }))
}

/// "Pixel 8 Pro offers 1 folder, 23 files, 232 MB", with Accept and Decline.
pub fn offer(engine: Arc<Engine>, offer: OfferInfo) -> Option<Notice> {
    let mut n = Notification::new();
    n.appname("glft")
        .summary(&format!("{} offers {}", offer.peer, offer.summary))
        .body(&format!("To be saved to {}", offer.dest))
        .icon("document-save")
        .hint(Hint::Resident(true))
        .timeout(Timeout::Never)
        .action("accept", "Accept")
        .action("decline", "Decline");
    let id = offer.id;
    Some(show(n, move |outcome| match outcome {
        Outcome::Action(a) if a == "accept" => {
            engine.answer(id, true);
            false
        }
        Outcome::Action(a) if a == "decline" => {
            engine.answer(id, false);
            false
        }
        // Dismissed: the offer stays answerable with `glft accept`.
        _ => false,
    }))
}

/// "Received from Pixel 8 Pro": shown once when a transfer arrives complete, so files
/// accepted without asking don't arrive unnoticed.
pub fn received(peer: &str, summary: &str, dest: &str) {
    let mut n = Notification::new();
    n.appname("glft")
        .summary(&format!("Received from {peer}"))
        .body(&format!("{summary}, every file verified, in {dest}"))
        .icon("document-save")
        .timeout(Timeout::Default);
    if disabled() {
        return;
    }
    tokio::spawn(async move {
        let _ = n.show_async().await;
    });
}
