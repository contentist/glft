//! The panel icon while receiving is on (StatusNotifierItem, which Cinnamon's xapp
//! applet, KDE and most panels show). The engine owns it, so it cannot outlive or
//! disappear before the listening it stands for. Where no panel shows such icons, the
//! "Receiving" notification takes its place.

use std::sync::{Arc, Mutex};

use glft_engine::OfferInfo;
use ksni::{
    MenuItem, TrayMethods,
    menu::{StandardItem, SubMenu},
};
use tokio::task::JoinHandle;

use crate::{Engine, notify};

struct Icon {
    engine: Arc<Engine>,
    folder: String,
    offers: Vec<OfferInfo>,
    /// The notification shown while the panel's icon host is gone.
    fallback: Mutex<Option<notify::Notice>>,
}

/// The icon on the panel; dropping it takes it off.
pub struct Shown {
    handle: ksni::Handle<Icon>,
    updates: JoinHandle<()>,
}

impl Drop for Shown {
    fn drop(&mut self) {
        self.updates.abort();
        // The shutdown is sent at once; awaiting would only wait for it to finish.
        drop(self.handle.shutdown());
    }
}

/// Shows the icon, or returns `None` if nothing on this desktop shows panel icons.
pub async fn show(engine: Arc<Engine>, folder: String) -> Option<Shown> {
    if notify::disabled() {
        return None;
    }
    let icon = Icon { engine: engine.clone(), folder, offers: Vec::new(), fallback: Mutex::new(None) };
    let handle = match icon.spawn().await {
        Ok(h) => h,
        Err(e) => {
            println!("No panel icon ({e}); a notification shows that receiving is on");
            return None;
        }
    };
    // Offers come and go; the menu follows.
    let mut changes = engine.changed.subscribe();
    let h = handle.clone();
    let updates = tokio::spawn(async move {
        while changes.changed().await.is_ok() {
            let offers: Vec<OfferInfo> = engine.offers.lock().unwrap().values().map(|o| o.info.clone()).collect();
            if h.update(|icon: &mut Icon| icon.offers = offers).await.is_none() {
                return;
            }
        }
    });
    Some(Shown { handle, updates })
}

/// Starts the window (`glft-window`, installed next to `glftd`).
pub fn open_window(engine: &Engine) {
    let window = glft_engine::client::sibling("glft-window");
    if let Err(e) = std::process::Command::new(&window).stdin(std::process::Stdio::null()).spawn() {
        engine.log_note(&format!("The glft window could not be opened ({}: {e})", window.display()));
    }
}

impl ksni::Tray for Icon {
    fn id(&self) -> String {
        "io.github.contentist.glft".into()
    }

    fn title(&self) -> String {
        "glft".into()
    }

    fn icon_name(&self) -> String {
        // The app's own icon once `glft setup` has installed it.
        let installed = std::env::var_os("HOME")
            .map(|h| std::path::PathBuf::from(h).join(".local/share/icons/hicolor/scalable/apps/io.github.contentist.glft.svg"))
            .is_some_and(|p| p.exists())
            || std::path::Path::new("/usr/share/icons/hicolor/scalable/apps/io.github.contentist.glft.svg").exists();
        if installed { "io.github.contentist.glft" } else { "folder-download" }.into()
    }

    fn status(&self) -> ksni::Status {
        if self.offers.is_empty() { ksni::Status::Active } else { ksni::Status::NeedsAttention }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let description = match self.offers.len() {
            0 => format!("Receiving into {}", self.folder),
            1 => format!("{} offers {}", self.offers[0].peer, self.offers[0].summary),
            n => format!("{n} offers waiting for an answer"),
        };
        ksni::ToolTip { title: "glft is receiving".into(), description, ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        open_window(&self.engine);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem { label: format!("Receiving into {}", self.folder), enabled: false, ..Default::default() }.into(),
            StandardItem { label: "Open glft".into(), activate: Box::new(|icon: &mut Self| open_window(&icon.engine)), ..Default::default() }.into(),
        ];
        if !self.offers.is_empty() {
            items.push(MenuItem::Separator);
        }
        for offer in &self.offers {
            let (accept, decline) = (offer.id, offer.id);
            items.push(
                SubMenu {
                    label: format!("{} offers {}", offer.peer, offer.summary),
                    submenu: vec![
                        StandardItem { label: "Accept".into(), activate: Box::new(move |icon: &mut Self| icon.engine.answer(accept, true)), ..Default::default() }
                            .into(),
                        StandardItem { label: "Decline".into(), activate: Box::new(move |icon: &mut Self| icon.engine.answer(decline, false)), ..Default::default() }
                            .into(),
                    ],
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Stop receiving".into(),
                icon_name: "process-stop".into(),
                activate: Box::new(|icon: &mut Self| {
                    let engine = icon.engine.clone();
                    tokio::spawn(async move { engine.stop_receiving().await });
                }),
                ..Default::default()
            }
            .into(),
        );
        items
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        // The panel applet went away: receiving must still be visible.
        *self.fallback.lock().unwrap() = notify::receiving(self.engine.clone(), self.folder.clone());
        true
    }

    fn watcher_online(&self) {
        self.fallback.lock().unwrap().take();
    }
}
