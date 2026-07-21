//! "Add a server" — the directory browser.
//!
//! Opens on the bundled snapshot so there is never an empty, spinning dialog,
//! then quietly swaps in the live list from treestats when it arrives.

use ac_core::servers::{self, Server};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

/// Builds the row for one server. The population badge is the thing people
/// actually scan for, so it gets the visual weight.
fn server_row(s: &Server, already_added: bool) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&s.name))
        .subtitle(glib::markup_escape_text(&format!(
            "{} · {} · {}",
            s.address(),
            s.ruleset,
            s.software.label()
        )))
        .build();

    if let Some(n) = s.online() {
        let badge = gtk::Label::new(Some(&format!("{n} online")));
        badge.add_css_class("caption");
        badge.add_css_class("accent");
        row.add_suffix(&badge);
    } else if let Some(p) = &s.players {
        // It reports a count, but a stale one. Show it as history, not as live --
        // presenting a day-old number as current is worse than showing nothing.
        let badge = gtk::Label::new(Some(&format!("{} · {}", p.count, p.age)));
        badge.add_css_class("caption");
        badge.add_css_class("dim-label");
        row.add_suffix(&badge);
    }

    if already_added {
        let tick = gtk::Image::from_icon_name("object-select-symbolic");
        tick.add_css_class("dim-label");
        tick.set_tooltip_text(Some("Already added"));
        row.add_suffix(&tick);
        row.set_sensitive(false);
    } else {
        row.add_suffix(&gtk::Image::from_icon_name("list-add-symbolic"));
        row.set_activatable(true);
    }
    row
}

/// Show the browser. `added` is the set of ids already in your list; `on_add` is
/// called with the chosen server.
pub fn present<F>(parent: &impl IsA<gtk::Window>, added: Vec<String>, on_add: F)
where
    F: Fn(Server) + 'static,
{
    let dialog = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("Add a Server")
        .default_width(560)
        .default_height(640)
        .build();

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search servers")
        .hexpand(true)
        .build();

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("boxed-list");

    let status = gtk::Label::new(Some("Loading the server directory…"));
    status.add_css_class("dim-label");
    status.add_css_class("caption");
    status.set_halign(gtk::Align::Start);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&search);
    content.append(&status);

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    content.append(&scroller);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));
    dialog.set_content(Some(&toolbar));

    let on_add = Rc::new(on_add);
    let added = Rc::new(added);
    let all: Rc<RefCell<Vec<Server>>> = Rc::new(RefCell::new(Vec::new()));

    // Rebuild the visible rows for the current filter.
    let rebuild = {
        let list = list.clone();
        let all = all.clone();
        let added = added.clone();
        let on_add = on_add.clone();
        let dialog = dialog.clone();
        Rc::new(move |needle: String| {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            let needle = needle.trim().to_ascii_lowercase();
            let servers = all.borrow();
            let mut shown = 0;
            for s in servers.iter() {
                if !needle.is_empty()
                    && !s.name.to_ascii_lowercase().contains(&needle)
                    && !s.description.to_ascii_lowercase().contains(&needle)
                    && !s.host.to_ascii_lowercase().contains(&needle)
                {
                    continue;
                }
                shown += 1;
                let row = server_row(s, added.contains(&s.id()));
                let s2 = s.clone();
                let on_add = on_add.clone();
                let dialog = dialog.clone();
                row.connect_activated(move |_| {
                    on_add(s2.clone());
                    dialog.close();
                });
                list.append(&row);
            }
            if shown == 0 {
                let empty = adw::StatusPage::builder()
                    .icon_name("system-search-symbolic")
                    .title("No servers match")
                    .build();
                empty.add_css_class("compact");
                list.append(&empty);
            }
        })
    };

    // Open on the bundled list immediately -- a dialog that is instantly useful
    // beats a spinner, and the snapshot is nearly always right.
    *all.borrow_mut() = servers::bundled();
    rebuild(String::new());
    status.set_text("Showing the bundled list — refreshing…");

    {
        let rebuild = rebuild.clone();
        let search = search.clone();
        search.connect_search_changed(move |e| rebuild(e.text().to_string()));
    }

    // Fetch the live directory off the main thread; fold it in when it lands.
    let (tx, rx) = async_channel::bounded(1);
    gio::spawn_blocking(move || {
        let _ = tx.send_blocking(servers::fetch());
    });

    glib::spawn_future_local({
        let all = all.clone();
        let rebuild = rebuild.clone();
        let search = search.clone();
        let status = status.clone();
        async move {
            match rx.recv().await {
                Ok(Ok(live)) => {
                    let n = live.len();
                    *all.borrow_mut() = live;
                    rebuild(search.text().to_string());
                    status.set_text(&format!("{n} servers · live from treestats.net"));
                }
                Ok(Err(e)) => {
                    // Offline is not an error state worth blocking on -- the
                    // bundled list is already on screen and is perfectly usable.
                    status.set_text(&format!("Showing the bundled list — {e}"));
                }
                Err(_) => {}
            }
        }
    });

    dialog.present();
}
