//! A help page in a dialog: the built-in Markdown (`docs/help/<id>.md`), shown as text with
//! headings.

use gtk::{glib, prelude::*};

/// Shows the page for `id` over `parent`; nothing if there is none.
pub fn show(parent: &impl IsA<gtk::Window>, id: &str) {
    let Some(page) = glft_core::help::page(id) else { return };
    let title = page.lines().next().unwrap_or_default().trim_start_matches("# ");
    let window = gtk::Window::builder().title(title).transient_for(parent).modal(true).default_width(560).default_height(520).build();

    let text = gtk::Label::new(None);
    text.set_markup(&to_pango(page));
    text.set_xalign(0.0);
    text.set_yalign(0.0);
    text.set_wrap(true);
    text.set_selectable(true);
    text.set_margin_top(16);
    text.set_margin_bottom(16);
    text.set_margin_start(16);
    text.set_margin_end(16);
    let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&text).build();

    let close = gtk::Button::with_mnemonic("_Close");
    close.set_halign(gtk::Align::End);
    close.set_margin_bottom(12);
    close.set_margin_end(16);
    let w = window.clone();
    close.connect_clicked(move |_| w.close());

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&scroll);
    body.append(&close);
    window.set_child(Some(&body));
    let keys = gtk::ShortcutController::new();
    keys.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string("Escape"), Some(gtk::NamedAction::new("window.close"))));
    window.add_controller(keys);
    window.present();
    close.grab_focus();
    // Not selected on open: a selectable label would otherwise start fully highlighted.
    text.select_region(0, 0);
}

/// The little Markdown the help pages use: headings, **bold**, `code`, lists and code blocks.
fn to_pango(md: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in md.lines() {
        if line.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        let esc = glib::markup_escape_text(line).to_string();
        if in_code {
            out.push_str(&format!("    <tt>{esc}</tt>\n"));
        } else if let Some(h) = esc.strip_prefix("# ") {
            out.push_str(&format!("<span size=\"x-large\" weight=\"bold\">{h}</span>\n"));
        } else if let Some(h) = esc.strip_prefix("## ") {
            out.push_str(&format!("\n<b>{h}</b>\n"));
        } else {
            out.push_str(&inline(&esc));
            out.push('\n');
        }
    }
    out
}

/// `**bold**` and `code` within a line (already escaped).
fn inline(line: &str) -> String {
    let mut s = String::new();
    for (i, part) in line.split("**").enumerate() {
        let part: String = part.split('`').enumerate().map(|(j, p)| if j % 2 == 1 { format!("<tt>{p}</tt>") } else { p.to_string() }).collect();
        if i % 2 == 1 { s.push_str(&format!("<b>{part}</b>")) } else { s.push_str(&part) }
    }
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_page_is_valid_markup() {
        for (id, _) in glft_core::help::topics() {
            let markup = super::to_pango(glft_core::help::page(id).unwrap());
            assert!(gtk::pango::parse_markup(&markup, '\0').is_ok(), "{id}: bad markup");
        }
    }
}
