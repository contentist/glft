//! `glft-window`: the window on the computer. It shows what the engine is doing and asks it
//! to do things; it never touches the network itself. Files or folders dropped on it, or
//! passed to it (`glft-window PATHS`, the Nemo entry), are offered to a paired device
//! after one more click, so a mis-click sends nothing.

mod bridge;
mod pair;

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
};

use bridge::{Cmd, Msg};
use glft_core::events::{human_bytes, local_time};
use glft_engine::{DeviceInfo, ErrorInfo, LogLine, Reply, Request, Status};
use gtk::{gdk, gio, glib, prelude::*};
use tokio::sync::mpsc::UnboundedSender;

const APP_ID: &str = "io.github.contentist.glft";

/// Log lines kept for "Copy all"; the window shows the last few.
const LOG_KEEP: usize = 500;
const LOG_SHOWN: usize = 8;

const CSS: &str = "
.drop { border: 2px dashed alpha(currentColor, 0.35); border-radius: 8px; padding: 18px; }
.drop.hover { background-color: alpha(@theme_selected_bg_color, 0.2); border-color: @theme_selected_bg_color; }
.section { margin-top: 6px; }
.banner { border-radius: 6px; padding: 10px; background-color: alpha(@error_color, 0.12); }
.log-time { font-feature-settings: 'tnum'; }
";

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();
    let ui: Rc<RefCell<Option<Rc<Ui>>>> = Rc::default();

    app.connect_startup(|app| {
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        let quit = gio::SimpleAction::new("quit", None);
        let a = app.clone();
        quit.connect_activate(move |_, _| a.quit());
        app.add_action(&quit);
        app.set_accels_for_action("app.quit", &["<Ctrl>q"]);
        app.set_accels_for_action("window.close", &["<Ctrl>w"]);
        app.set_accels_for_action("win.choose-files", &["<Ctrl>o"]);
    });

    let u = ui.clone();
    app.connect_activate(move |app| window(app, &u).window.present());
    let u = ui.clone();
    app.connect_open(move |app, files, _| {
        let ui = window(app, &u);
        ui.window.present();
        ui.offer(files);
    });
    app.run()
}

/// The one window, built on first use.
fn window(app: &gtk::Application, slot: &Rc<RefCell<Option<Rc<Ui>>>>) -> Rc<Ui> {
    if let Some(ui) = slot.borrow().as_ref() {
        return ui.clone();
    }
    let ui = Ui::build(app);
    *slot.borrow_mut() = Some(ui.clone());
    ui
}

/// Which bar a progress reply moves.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Send(u64),
    Incoming(String),
}

pub struct Ui {
    window: gtk::ApplicationWindow,
    cmd: UnboundedSender<Cmd>,
    connected: Cell<bool>,

    banner: gtk::Box,
    banner_head: gtk::Label,
    banner_next: gtk::Label,
    banner_detail: gtk::Label,

    receive_switch: gtk::Switch,
    receive_label: gtk::Label,
    receive_addrs: gtk::Label,
    /// Set while the window itself moves the switch, so that isn't taken as a click.
    updating: Cell<bool>,

    offers_section: gtk::Box,
    offers: gtk::ListBox,

    drop_area: gtk::Box,
    confirm: gtk::Box,
    confirm_label: gtk::Label,
    confirm_devices: gtk::Box,
    pending: RefCell<Vec<PathBuf>>,

    transfers_section: gtk::Box,
    transfers: gtk::ListBox,
    bars: RefCell<HashMap<Key, gtk::ProgressBar>>,

    devices_list: gtk::ListBox,
    devices: RefCell<Vec<DeviceInfo>>,

    log_list: gtk::ListBox,
    log: RefCell<Vec<LogLine>>,

    pair: RefCell<Option<pair::PairDialog>>,
}

fn heading(text: &str) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_markup(&format!("<b>{}</b>", glib::markup_escape_text(text)));
    l.set_xalign(0.0);
    l.add_css_class("section");
    l
}

fn label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    l
}

fn dim(text: &str) -> gtk::Label {
    let l = label(text);
    l.add_css_class("dim-label");
    l
}

fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

fn list() -> gtk::ListBox {
    let l = gtk::ListBox::new();
    l.set_selection_mode(gtk::SelectionMode::None);
    l.add_css_class("boxed-list");
    l.add_css_class("frame");
    l
}

fn row(child: &impl IsA<gtk::Widget>) -> gtk::ListBoxRow {
    let r = gtk::ListBoxRow::new();
    r.set_activatable(false);
    child.set_margin_top(8);
    child.set_margin_bottom(8);
    child.set_margin_start(10);
    child.set_margin_end(10);
    r.set_child(Some(child));
    r
}

impl Ui {
    fn build(app: &gtk::Application) -> Rc<Self> {
        // No custom title bar: the window manager draws it (Muffin on Cinnamon), which is
        // what LocalSend's window got wrong.
        let window = gtk::ApplicationWindow::builder().application(app).title("glft").default_width(560).default_height(780).build();
        let (cmd, msgs) = bridge::start();

        let content = vbox(10);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);

        // The error banner: headline, what to do, details on request.
        let banner = vbox(4);
        banner.add_css_class("banner");
        let banner_head = label("");
        banner_head.add_css_class("error");
        let banner_next = label("");
        let banner_detail = dim("");
        banner_detail.set_selectable(true);
        let details = gtk::Expander::new(Some("Details"));
        details.set_child(Some(&banner_detail));
        let dismiss = gtk::Button::with_label("Dismiss");
        dismiss.set_halign(gtk::Align::End);
        banner.append(&banner_head);
        banner.append(&banner_next);
        banner.append(&details);
        banner.append(&dismiss);
        banner.set_visible(false);
        content.append(&banner);

        // Receiving.
        let receive_row = hbox(12);
        let receive_text = vbox(2);
        let receive_title = heading("Receiving");
        let receive_label = label("Off: nothing on this computer listens for other devices.");
        let receive_addrs = dim("");
        receive_addrs.set_selectable(true);
        receive_addrs.set_visible(false);
        receive_text.append(&receive_title);
        receive_text.append(&receive_label);
        receive_text.append(&receive_addrs);
        receive_text.set_hexpand(true);
        let receive_switch = gtk::Switch::new();
        receive_switch.set_valign(gtk::Align::Center);
        receive_switch.update_property(&[gtk::accessible::Property::Label("Receiving")]);
        receive_row.append(&receive_text);
        receive_row.append(&receive_switch);
        content.append(&receive_row);

        // Offers waiting.
        let offers_section = vbox(6);
        offers_section.append(&heading("Waiting for your answer"));
        let offers = list();
        offers_section.append(&offers);
        offers_section.set_visible(false);
        content.append(&offers_section);

        // Send.
        content.append(&heading("Send"));
        let drop_area = vbox(8);
        drop_area.add_css_class("drop");
        let drop_text = label("Drop files or folders here");
        drop_text.set_xalign(0.5);
        let choose = hbox(8);
        choose.set_halign(gtk::Align::Center);
        let choose_files = gtk::Button::with_mnemonic("Choose _files…");
        let choose_folder = gtk::Button::with_mnemonic("Choose a f_older…");
        choose.append(&choose_files);
        choose.append(&choose_folder);
        drop_area.append(&drop_text);
        drop_area.append(&choose);
        content.append(&drop_area);

        let confirm = vbox(6);
        let confirm_label = label("");
        let confirm_devices = vbox(6);
        let confirm_cancel = gtk::Button::with_mnemonic("_Cancel");
        confirm_cancel.set_halign(gtk::Align::Start);
        confirm.append(&confirm_label);
        confirm.append(&confirm_devices);
        confirm.append(&confirm_cancel);
        confirm.set_visible(false);
        content.append(&confirm);

        // Transfers.
        let transfers_section = vbox(6);
        transfers_section.append(&heading("Transfers"));
        let transfers = list();
        transfers_section.append(&transfers);
        transfers_section.set_visible(false);
        content.append(&transfers_section);

        // Paired devices.
        let devices_head = hbox(8);
        let devices_title = heading("Paired devices");
        devices_title.set_hexpand(true);
        let pair_button = gtk::Button::with_mnemonic("_Pair a phone…");
        devices_head.append(&devices_title);
        devices_head.append(&pair_button);
        content.append(&devices_head);
        let devices_list = list();
        content.append(&devices_list);

        // What's happening.
        let log_head = hbox(8);
        let log_title = heading("What's happening");
        log_title.set_hexpand(true);
        let copy_log = gtk::Button::with_label("Copy all");
        copy_log.set_tooltip_text(Some("Copies the whole log as text, e.g. for a help request"));
        log_head.append(&log_title);
        log_head.append(&copy_log);
        content.append(&log_head);
        let log_list = list();
        content.append(&log_list);

        let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&content).build();
        window.set_child(Some(&scroll));

        let ui = Rc::new(Self {
            window,
            cmd,
            connected: Cell::new(false),
            banner,
            banner_head,
            banner_next,
            banner_detail,
            receive_switch,
            receive_label,
            receive_addrs,
            updating: Cell::new(false),
            offers_section,
            offers,
            drop_area,
            confirm,
            confirm_label,
            confirm_devices,
            pending: RefCell::default(),
            transfers_section,
            transfers,
            bars: RefCell::default(),
            devices_list,
            devices: RefCell::default(),
            log_list,
            log: RefCell::default(),
            pair: RefCell::default(),
        });

        // ---- controls
        let u = ui.clone();
        dismiss.connect_clicked(move |_| u.banner.set_visible(false));

        let u = ui.clone();
        ui.receive_switch.connect_state_set(move |_, on| {
            if !u.updating.get() {
                u.request(Request::Receive { on, dir: None, port: None });
            }
            // The switch follows the engine's status, not the click.
            glib::Propagation::Stop
        });

        let u = ui.clone();
        choose_files.connect_clicked(move |_| u.choose(false));
        let u = ui.clone();
        choose_folder.connect_clicked(move |_| u.choose(true));
        let action = gio::SimpleAction::new("choose-files", None);
        let u = ui.clone();
        action.connect_activate(move |_, _| u.choose(false));
        ui.window.add_action(&action);

        let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let area = ui.drop_area.clone();
        target.connect_enter(move |_, _, _| {
            area.add_css_class("hover");
            gdk::DragAction::COPY
        });
        let area = ui.drop_area.clone();
        target.connect_leave(move |_| area.remove_css_class("hover"));
        let u = ui.clone();
        target.connect_drop(move |_, value, _, _| {
            u.drop_area.remove_css_class("hover");
            match value.get::<gdk::FileList>() {
                Ok(list) => {
                    u.offer(&list.files());
                    true
                }
                Err(_) => false,
            }
        });
        // The whole window takes drops, not only the marked area.
        ui.window.add_controller(target);

        let u = ui.clone();
        confirm_cancel.connect_clicked(move |_| u.clear_pending());

        let u = ui.clone();
        pair_button.connect_clicked(move |_| u.start_pairing());

        let u = ui.clone();
        copy_log.connect_clicked(move |_| {
            let text: String = u.log.borrow().iter().map(log_text).collect();
            u.window.clipboard().set_text(&text);
        });

        // ---- the engine
        // The heartbeat runs on the GTK main loop: if the window freezes, it stops.
        let u = ui.clone();
        let _ = u.cmd.send(Cmd::Ping);
        glib::timeout_add_local(bridge::PING_EVERY, move || {
            let _ = u.cmd.send(Cmd::Ping);
            glib::ControlFlow::Continue
        });
        let u = ui.clone();
        glib::spawn_future_local(async move {
            while let Ok(msg) = msgs.recv().await {
                u.handle(msg);
            }
        });

        ui
    }

    fn request(&self, request: Request) {
        let _ = self.cmd.send(Cmd::Request(request));
    }

    fn show_error(&self, e: &ErrorInfo) {
        self.banner_head.set_text(&e.headline);
        self.banner_next.set_text(&format!("What to do: {}", e.next_step));
        self.banner_detail.set_text(&format!("[{}] {}", e.id, e.detail));
        self.banner.set_visible(true);
    }

    fn handle(self: &Rc<Self>, msg: Msg) {
        match msg {
            Msg::Watch(reply) => self.on_reply(reply),
            Msg::Lost(e) => {
                if self.connected.replace(false) {
                    self.bars.borrow_mut().clear();
                }
                self.show_error(&e);
            }
            Msg::Failed(e) => self.show_error(&e),
            Msg::PairCode(code) => {
                if let Some(p) = self.pair.borrow().as_ref() {
                    p.show_code(&code);
                }
            }
            Msg::PairPeer { name } => {
                if let Some(p) = self.pair.borrow().as_ref() {
                    p.ask_remember(&name);
                }
            }
            Msg::PairDone(result) => {
                let dialog = self.pair.borrow_mut().take();
                match result {
                    None => {
                        if let Some(p) = dialog {
                            p.finish();
                        }
                    }
                    Some(e) => match dialog {
                        Some(p) => {
                            p.show_error(&e);
                            *self.pair.borrow_mut() = Some(p);
                        }
                        None => self.show_error(&e),
                    },
                }
            }
        }
    }

    fn on_reply(self: &Rc<Self>, reply: Reply) {
        match reply {
            Reply::Status(s) => {
                if !self.connected.replace(true) {
                    // A new watch sends the recent log again.
                    self.log.borrow_mut().clear();
                    self.banner.set_visible(false);
                }
                self.show_status(&s);
            }
            Reply::Devices { devices } => {
                *self.devices.borrow_mut() = devices;
                self.show_devices();
                if !self.pending.borrow().is_empty() {
                    self.show_confirm_devices();
                }
            }
            Reply::Line(l) => {
                let mut log = self.log.borrow_mut();
                log.push(l);
                let extra = log.len().saturating_sub(LOG_KEEP);
                log.drain(..extra);
                drop(log);
                self.show_log();
            }
            Reply::Progress { done, total, send, peer, .. } => {
                let key = match (send, peer) {
                    (Some(id), _) => Key::Send(id),
                    (None, Some(peer)) => Key::Incoming(peer),
                    (None, None) => return,
                };
                if let Some(bar) = self.bars.borrow().get(&key) {
                    set_progress(bar, done, total);
                }
            }
            _ => {}
        }
    }

    fn show_status(self: &Rc<Self>, s: &Status) {
        self.updating.set(true);
        self.receive_switch.set_active(s.receiving.is_some());
        self.receive_switch.set_state(s.receiving.is_some());
        self.updating.set(false);
        match &s.receiving {
            Some(r) => {
                self.receive_label.set_text(&format!("On: paired devices can send files into {}.", r.folder));
                let addrs: Vec<String> = r.addrs.iter().filter(|a| a.is_ipv4() || glft_core::net::mesh_name(a).is_some()).map(|a| a.to_string()).collect();
                self.receive_addrs.set_text(&format!("Reachable at {}", addrs.join(", ")));
                self.receive_addrs.set_visible(!addrs.is_empty());
            }
            None => {
                self.receive_label.set_text("Off: nothing on this computer listens for other devices.");
                self.receive_addrs.set_visible(false);
            }
        }

        // Offers.
        self.offers.remove_all();
        for o in &s.waiting {
            let line = hbox(8);
            let text = vbox(2);
            text.append(&label(&format!("{} offers {}", o.peer, o.summary)));
            text.append(&dim(&format!("To be saved to {}", o.dest)));
            text.set_hexpand(true);
            let accept = gtk::Button::with_label("Accept");
            accept.add_css_class("suggested-action");
            let decline = gtk::Button::with_label("Decline");
            for (b, yes) in [(&accept, true), (&decline, false)] {
                b.set_valign(gtk::Align::Center);
                let (u, offer) = (self.clone(), o.id);
                b.connect_clicked(move |_| u.request(Request::Answer { offer, accept: yes }));
            }
            line.append(&text);
            line.append(&accept);
            line.append(&decline);
            self.offers.append(&row(&line));
        }
        self.offers_section.set_visible(!s.waiting.is_empty());

        // Transfers: rebuilt, keeping each bar's place in the map for progress replies.
        self.transfers.remove_all();
        let mut bars = HashMap::new();
        for t in &s.sending {
            let bar = self.transfer_row(&format!("To {}: {}", t.peer, t.summary), t.done, t.total, Some(t.id));
            bars.insert(Key::Send(t.id), bar);
        }
        for t in &s.incoming {
            let bar = self.transfer_row(&format!("From {}", t.peer), t.done, t.total, None);
            bars.insert(Key::Incoming(t.peer.clone()), bar);
        }
        *self.bars.borrow_mut() = bars;
        self.transfers_section.set_visible(!s.sending.is_empty() || !s.incoming.is_empty());
    }

    fn transfer_row(self: &Rc<Self>, what: &str, done: u64, total: u64, send: Option<u64>) -> gtk::ProgressBar {
        let line = hbox(8);
        let text = vbox(4);
        text.append(&label(what));
        let bar = gtk::ProgressBar::new();
        bar.set_show_text(true);
        set_progress(&bar, done, total);
        text.append(&bar);
        text.set_hexpand(true);
        line.append(&text);
        if let Some(id) = send {
            let cancel = gtk::Button::with_label("Cancel");
            cancel.set_valign(gtk::Align::Center);
            let u = self.clone();
            cancel.connect_clicked(move |_| u.request(Request::CancelSend { id }));
            line.append(&cancel);
        }
        self.transfers.append(&row(&line));
        bar
    }

    fn show_devices(self: &Rc<Self>) {
        self.devices_list.remove_all();
        let devices = self.devices.borrow();
        if devices.is_empty() {
            self.devices_list.append(&row(&dim("No paired devices yet. \"Pair a phone…\" shows a code to scan with glft on the phone.")));
        }
        for d in devices.iter() {
            let line = hbox(8);
            let text = vbox(2);
            text.append(&label(&d.device.name));
            let mut about = vec![if d.remembered { "remembered" } else { "until you log out" }.to_string()];
            if let Some(t) = d.device.last_seen {
                about.push(format!("last transfer {}", local_time(t)));
            }
            if let Some(mesh) = d.device.mesh_addrs.first().and_then(glft_core::net::mesh_name) {
                about.push(format!("also on {mesh}"));
            }
            text.append(&dim(&about.join(", ")));
            text.set_hexpand(true);
            line.append(&text);

            let auto = gtk::CheckButton::with_label("Accept without asking");
            auto.set_active(d.device.auto_accept);
            auto.set_valign(gtk::Align::Center);
            let (u, id) = (self.clone(), d.device.id.clone());
            auto.connect_toggled(move |b| u.request(Request::AutoAccept { device: id.clone(), on: b.is_active() }));
            line.append(&auto);

            let forget = gtk::Button::with_label("Forget…");
            forget.set_valign(gtk::Align::Center);
            let (u, id, name) = (self.clone(), d.device.id.clone(), d.device.name.clone());
            forget.connect_clicked(move |_| u.confirm_forget(&id, &name));
            line.append(&forget);
            self.devices_list.append(&row(&line));
        }
    }

    fn confirm_forget(self: &Rc<Self>, id: &str, name: &str) {
        let dialog = gtk::AlertDialog::builder()
            .message(format!("Forget \"{name}\"?"))
            .detail("It can no longer send files here, and you can't send to it until you pair again.")
            .buttons(["Cancel", "Forget"])
            .cancel_button(0)
            .default_button(0)
            .modal(true)
            .build();
        let (u, id) = (self.clone(), id.to_string());
        dialog.choose(Some(&self.window), gio::Cancellable::NONE, move |answer| {
            if answer == Ok(1) {
                u.request(Request::Forget { device: id });
            }
        });
    }

    fn show_log(&self) {
        self.log_list.remove_all();
        let log = self.log.borrow();
        if log.is_empty() {
            self.log_list.append(&row(&dim("Nothing yet.")));
        }
        for l in log.iter().skip(log.len().saturating_sub(LOG_SHOWN)) {
            let line = hbox(10);
            let time = dim(&time_of(l));
            time.add_css_class("log-time");
            time.set_valign(gtk::Align::Start);
            time.set_wrap(false);
            let text = vbox(2);
            let sentence = label(&l.sentence);
            sentence.set_selectable(true);
            text.append(&sentence);
            if l.is_error {
                sentence.add_css_class("error");
                if let Some(n) = &l.next_step {
                    text.append(&label(&format!("What to do: {n}")));
                }
                if let Some(d) = &l.detail {
                    let more = gtk::Expander::new(Some("Details"));
                    let detail = dim(d);
                    detail.set_selectable(true);
                    more.set_child(Some(&detail));
                    text.append(&more);
                }
            }
            text.set_hexpand(true);
            line.append(&time);
            line.append(&text);
            self.log_list.append(&row(&line));
        }
    }

    // ---- sending

    fn choose(self: &Rc<Self>, folders: bool) {
        let dialog = gtk::FileDialog::builder().title(if folders { "Choose folders to send" } else { "Choose files to send" }).modal(true).build();
        let u = self.clone();
        let done = move |result: Result<gio::ListModel, glib::Error>| {
            if let Ok(model) = result {
                let files: Vec<gio::File> = (0..model.n_items()).filter_map(|i| model.item(i).and_downcast::<gio::File>()).collect();
                u.offer(&files);
            }
        };
        if folders {
            dialog.select_multiple_folders(Some(&self.window), gio::Cancellable::NONE, done);
        } else {
            dialog.open_multiple(Some(&self.window), gio::Cancellable::NONE, done);
        }
    }

    /// Shows "Send … to:" for these files; nothing is sent until a device is clicked.
    fn offer(self: &Rc<Self>, files: &[gio::File]) {
        let mut paths = Vec::new();
        let mut not_local = Vec::new();
        for f in files {
            match f.path() {
                Some(p) => paths.push(p),
                None => not_local.push(f.uri().to_string()),
            }
        }
        if !not_local.is_empty() {
            self.show_error(&ErrorInfo {
                id: "not-a-local-file".into(),
                headline: "Some of these aren't files on this computer, so glft can't send them.".into(),
                next_step: "Copy them to a folder on this computer first, then send them from there.".into(),
                detail: not_local.join(", "),
            });
        }
        if paths.is_empty() {
            return;
        }
        *self.pending.borrow_mut() = paths.clone();
        self.confirm_label.set_text("Reading what you chose…");
        self.confirm.set_visible(true);
        self.drop_area.set_visible(false);
        self.show_confirm_devices();
        let u = self.clone();
        glib::spawn_future_local(async move {
            let summary = gio::spawn_blocking(move || summarize(&paths)).await.unwrap_or_default();
            if !u.pending.borrow().is_empty() {
                u.confirm_label.set_markup(&format!("Send <b>{}</b> to:", glib::markup_escape_text(&summary)));
            }
        });
    }

    fn show_confirm_devices(self: &Rc<Self>) {
        while let Some(c) = self.confirm_devices.first_child() {
            self.confirm_devices.remove(&c);
        }
        let devices = self.devices.borrow();
        if devices.is_empty() {
            self.confirm_devices.append(&dim("No paired device yet: pair one first, with \"Pair a phone…\" below."));
        }
        for d in devices.iter() {
            let b = gtk::Button::with_label(&d.device.name);
            b.add_css_class("suggested-action");
            let (u, id) = (self.clone(), d.device.id.clone());
            b.connect_clicked(move |_| {
                let paths = std::mem::take(&mut *u.pending.borrow_mut());
                if !paths.is_empty() {
                    // Detached: closing the window doesn't stop the send.
                    u.request(Request::Send { device: id.clone(), addrs: Vec::new(), paths, detach: true });
                }
                u.clear_pending();
            });
            self.confirm_devices.append(&b);
        }
        if let Some(first) = self.confirm_devices.first_child() {
            first.grab_focus();
        }
    }

    fn clear_pending(&self) {
        self.pending.borrow_mut().clear();
        self.confirm.set_visible(false);
        self.drop_area.set_visible(true);
    }

    // ---- pairing

    fn start_pairing(self: &Rc<Self>) {
        if let Some(p) = self.pair.borrow().as_ref() {
            p.present();
            return;
        }
        let u = self.clone();
        let dialog = pair::PairDialog::new(&self.window, self.cmd.clone(), move || {
            u.pair.borrow_mut().take();
        });
        *self.pair.borrow_mut() = Some(dialog);
        let _ = self.cmd.send(Cmd::Pair { code: None });
    }
}

fn set_progress(bar: &gtk::ProgressBar, done: u64, total: u64) {
    let fraction = if total == 0 { 0.0 } else { (done as f64 / total as f64).clamp(0.0, 1.0) };
    bar.set_fraction(fraction);
    bar.set_text(Some(&format!("{} of {}", human_bytes(done), human_bytes(total))));
}

fn time_of(l: &LogLine) -> String {
    chrono::DateTime::from_timestamp_millis(l.at_unix_ms)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// The log as the terminal prints it.
fn log_text(l: &LogLine) -> String {
    let mut s = format!("{}    {}\n", time_of(l), l.sentence);
    if let Some(n) = &l.next_step {
        s.push_str(&format!("          What to do: {n}\n"));
    }
    if let Some(d) = &l.detail {
        s.push_str(&format!("          details: {d}\n"));
    }
    s
}

/// "3 files, 12 MB" or "Album (folder), 23 files, 230 MB", as the phone says it.
fn summarize(paths: &[PathBuf]) -> String {
    let (mut files, mut bytes) = (0u64, 0u64);
    for p in paths {
        for entry in walkdir::WalkDir::new(p).into_iter().flatten() {
            if entry.file_type().is_file() {
                files += 1;
                bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    let count = if files == 1 { "1 file".to_string() } else { format!("{files} files") };
    match paths {
        [one] if one.is_dir() => {
            let name = one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            format!("{name} (folder), {count}, {}", human_bytes(bytes))
        }
        [one] => format!("{}, {}", one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), human_bytes(bytes)),
        _ => format!("{count}, {}", human_bytes(bytes)),
    }
}
