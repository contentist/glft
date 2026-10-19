//! "Pair a phone": the QR code to scan, then "Remember this device?", as `glft pair` asks
//! in the terminal.

use std::{cell::Cell, rc::Rc, time::Instant};

use glft_core::devices::Remember;
use glft_engine::ErrorInfo;
use gtk::{gdk, glib, prelude::*};
use tokio::sync::mpsc::UnboundedSender;

use crate::{bridge::Cmd, dim, hbox, label, vbox};

/// How long a code works; the engine enforces it, the dialog counts it down.
const CODE_LIFETIME_SECS: u64 = 5 * 60;

pub struct PairDialog {
    window: gtk::Window,
    body: gtk::Box,
    cmd: UnboundedSender<Cmd>,
    /// Set once the pairing has finished, so closing the dialog doesn't cancel it.
    done: Rc<Cell<bool>>,
}

impl PairDialog {
    pub fn new(parent: &gtk::ApplicationWindow, cmd: UnboundedSender<Cmd>, on_close: impl Fn() + 'static) -> Self {
        let window = gtk::Window::builder().title("Pair a phone").transient_for(parent).modal(true).default_width(420).resizable(false).build();
        let body = vbox(10);
        body.set_margin_top(16);
        body.set_margin_bottom(16);
        body.set_margin_start(16);
        body.set_margin_end(16);
        body.append(&label("Getting a code…"));
        window.set_child(Some(&body));

        // Esc closes, as dialogs do.
        let keys = gtk::ShortcutController::new();
        keys.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string("Escape"), Some(gtk::NamedAction::new("window.close"))));
        window.add_controller(keys);

        let done = Rc::new(Cell::new(false));
        let (d, c) = (done.clone(), cmd.clone());
        window.connect_close_request(move |_| {
            if !d.get() {
                let _ = c.send(Cmd::PairCancel);
            }
            on_close();
            glib::Propagation::Proceed
        });
        window.present();
        Self { window, body, cmd, done }
    }

    pub fn present(&self) {
        self.window.present();
    }

    fn clear(&self) {
        while let Some(c) = self.body.first_child() {
            self.body.remove(&c);
        }
    }

    pub fn show_code(&self, code: &str) {
        self.clear();
        self.body.append(&label("On the phone, open glft, then under Paired devices tap \"Pair with a computer\" and scan this code."));
        if let Some(texture) = qr_texture(code) {
            let picture = gtk::Picture::for_paintable(&texture);
            picture.set_can_shrink(false);
            picture.set_halign(gtk::Align::Center);
            picture.update_property(&[gtk::accessible::Property::Label("Pairing QR code")]);
            self.body.append(&picture);
        }
        let text = dim(code);
        text.set_selectable(true);
        text.add_css_class("monospace");
        self.body.append(&text);
        let row = hbox(8);
        let copy = gtk::Button::with_mnemonic("_Copy the code");
        let w = self.window.clone();
        let c = code.to_string();
        copy.connect_clicked(move |_| w.clipboard().set_text(&c));
        let countdown = dim("");
        countdown.set_hexpand(true);
        row.append(&countdown);
        row.append(&copy);
        self.body.append(&row);

        let started = Instant::now();
        let left = move || CODE_LIFETIME_SECS.saturating_sub(started.elapsed().as_secs());
        let show = move |l: &gtk::Label| {
            let s = left();
            l.set_text(&if s == 0 { "This code has run out. Close and try again.".to_string() } else { format!("Works once, for {}:{:02} more", s / 60, s % 60) });
            s > 0
        };
        show(&countdown);
        let weak = countdown.downgrade();
        glib::timeout_add_seconds_local(1, move || match weak.upgrade() {
            Some(l) if show(&l) => glib::ControlFlow::Continue,
            _ => glib::ControlFlow::Break,
        });
        copy.grab_focus();
    }

    pub fn ask_remember(&self, name: &str) {
        self.clear();
        let question = gtk::Label::new(None);
        question.set_markup(&format!("<b>Remember \"{}\"?</b>", glib::markup_escape_text(name)));
        question.set_xalign(0.0);
        self.body.append(&question);
        self.body.append(&label(
            "Remember: it can send to this computer again later. Just this time: until you log out (or quit glft). No: don't pair.",
        ));
        let auto = gtk::CheckButton::with_mnemonic(&format!("_Accept files from \"{name}\" without asking"));
        self.body.append(&auto);
        let buttons = hbox(8);
        buttons.set_halign(gtk::Align::End);
        let no = gtk::Button::with_mnemonic("_No");
        let once = gtk::Button::with_mnemonic("_Just this time");
        let yes = gtk::Button::with_mnemonic("_Remember");
        yes.add_css_class("suggested-action");
        for (b, remember) in [(&no, Remember::No), (&once, Remember::JustThisTime), (&yes, Remember::Yes)] {
            let (cmd, auto, body) = (self.cmd.clone(), auto.clone(), self.body.clone());
            b.connect_clicked(move |_| {
                let _ = cmd.send(Cmd::PairAnswer { remember, auto_accept: remember == Remember::Yes && auto.is_active() });
                body.set_sensitive(false);
            });
            buttons.append(b);
        }
        self.body.append(&buttons);
        yes.grab_focus();
    }

    /// Paired (or answered No): the dialog closes; the device list shows the result.
    pub fn finish(self) {
        self.done.set(true);
        self.window.close();
    }

    pub fn show_error(&self, e: &ErrorInfo) {
        self.done.set(true);
        self.clear();
        self.body.set_sensitive(true);
        let head = label(&e.headline);
        head.add_css_class("error");
        self.body.append(&head);
        self.body.append(&label(&format!("What to do: {}", crate::next_step(&e.id, &e.next_step))));
        let detail = dim(&format!("[{}] {}", e.id, e.detail));
        detail.set_selectable(true);
        self.body.append(&detail);
        let close = gtk::Button::with_mnemonic("_Close");
        close.set_halign(gtk::Align::End);
        let w = self.window.clone();
        close.connect_clicked(move |_| w.close());
        self.body.append(&close);
        close.grab_focus();
    }
}

/// The code as black modules on white with the quiet zone scanners need, 6 pixels a module.
fn qr_texture(code: &str) -> Option<gdk::Texture> {
    let qr = qrcode::QrCode::with_error_correction_level(code, qrcode::EcLevel::M).ok()?;
    let (width, colors) = (qr.width(), qr.to_colors());
    const SCALE: usize = 6;
    const QUIET: usize = 4;
    let side = (width + 2 * QUIET) * SCALE;
    let mut pixels = vec![255u8; side * side * 3];
    for y in 0..width {
        for x in 0..width {
            if colors[y * width + x] != qrcode::Color::Dark {
                continue;
            }
            for dy in 0..SCALE {
                let py = (y + QUIET) * SCALE + dy;
                for dx in 0..SCALE {
                    let px = (x + QUIET) * SCALE + dx;
                    let i = (py * side + px) * 3;
                    pixels[i..i + 3].fill(0);
                }
            }
        }
    }
    let bytes = glib::Bytes::from_owned(pixels);
    Some(gdk::MemoryTexture::new(side as i32, side as i32, gdk::MemoryFormat::R8g8b8, &bytes, side * 3).upcast())
}
