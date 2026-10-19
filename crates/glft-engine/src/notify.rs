//! Desktop notifications: the visible sign that receiving is on, and offers to answer.
//!
//! Without a notification server (no desktop session), the engine carries on and the
//! terminal commands do the same jobs.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use glft_core::events::{Placed, Totals};

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

/// "Received from Pixel 8 Pro: screenshot.png is in ~/Downloads/glft", with Show in folder
/// and Open: shown when a transfer arrives complete, so nobody has to know where glft
/// saves things, and files accepted without asking don't arrive unnoticed.
pub fn received(peer: &str, totals: &Totals, items: &[Placed]) {
    let landed = glft_core::events::landed(items, totals);
    let mut n = Notification::new();
    n.appname("glft")
        .summary(&format!("Received from {peer}"))
        .body(&format!("{landed}. Every file verified."))
        .icon("document-save")
        .timeout(Timeout::Never);
    let paths: Vec<PathBuf> = items.iter().filter_map(|i| i.path.clone()).collect();
    if !paths.is_empty() {
        n.action("show", "Show in folder");
        // One file or folder opens in its usual app; several are best seen in the folder.
        if paths.len() == 1 {
            n.action("open", "Open");
        }
    }
    if disabled() {
        return;
    }
    tokio::spawn(async move {
        let Ok(handle) = n.show_async().await else { return };
        handle
            .wait_for_action_async(move |r: &NotificationResponse| match r {
                NotificationResponse::Action(a) if a == "show" => show_in_folder(&paths),
                NotificationResponse::Action(a) if a == "open" => open(&paths[0]),
                // Clicking the notification itself: the most useful of the two.
                NotificationResponse::Default if paths.len() == 1 => open(&paths[0]),
                NotificationResponse::Default if !paths.is_empty() => show_in_folder(&paths),
                _ => {}
            })
            .await;
    });
}

/// Opens the file manager with `paths` selected (the FileManager1 interface Nemo, Nautilus,
/// Dolphin and others offer), or else just the folder of the first.
pub fn show_in_folder(paths: &[PathBuf]) {
    let uris: Vec<String> = paths.iter().map(|p| file_uri(p)).collect();
    let list = format!("[{}]", uris.iter().map(|u| format!("'{u}'")).collect::<Vec<_>>().join(", "));
    let shown = std::process::Command::new("gdbus")
        .args(["call", "--session", "--dest", "org.freedesktop.FileManager1", "--object-path", "/org/freedesktop/FileManager1"])
        .args(["--method", "org.freedesktop.FileManager1.ShowItems", &list, ""])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !shown && let Some(folder) = paths.first().and_then(|p| p.parent()) {
        open(folder);
    }
}

/// Opens a file in its usual app, or a folder in the file manager.
pub fn open(path: &Path) {
    let _ = std::process::Command::new("xdg-open").arg(path).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

/// `file:///home/me/Downloads/glft/my%20photo.jpg`
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => uri.push(b as char),
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri
}
