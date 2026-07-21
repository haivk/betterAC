//! The main window: pick a server, type your account, play.

use crate::browser;
use crate::launcher::{self, Install};
use ac_core::config::Config;
use ac_core::proton::ProtonRuntime;
use ac_core::servers::Server;
// `Runtime` is in scope for its `discover`, which finish_setup calls on the
// ProtonRuntime once setup succeeds.
use ac_core::setup::{self, Progress, RunState, Runtime, SetupStep, StepState, StepStatus};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// What the background setup thread streams back to the UI: progress lines, then
/// a single terminal Done.
enum SetupEvent {
    Progress(Progress),
    Done(Result<(), String>),
}

/// One row of the setup checklist: an icon for the step's state, its name, a live
/// message, and its own progress bar.
///
/// Setup downloads ~1.4 GB in three separate files and then does five local jobs.
/// Through a single bar that looked like one bar filling and emptying over and
/// over, with no way to tell which pass you were watching -- so every step gets
/// its own row, and the whole list is on screen before you start. This mirrors
/// the macOS SetupView list row for row.
struct SetupRow {
    step: SetupStep,
    /// The whole row, hidden once the step is behind us.
    container: gtk::Box,
    /// The rule above this row, hidden with it so the card has no double lines.
    separator: gtk::Separator,
    icon: gtk::Image,
    title: gtk::Label,
    message: gtk::Label,
    bar: gtk::ProgressBar,
}

/// The icon that marks a step's state. Adwaita symbolics, all core-set.
fn state_icon(state: StepState) -> &'static str {
    match state {
        StepState::Pending => "content-loading-symbolic",
        StepState::Running => "media-playback-start-symbolic",
        StepState::Done => "object-select-symbolic",
        StepState::Skipped => "list-remove-symbolic",
        StepState::Failed => "dialog-warning-symbolic",
    }
}

/// Build the checklist from a fresh `RunState`, so the steps are visible as a
/// plan before setup runs. Returns the widget and the rows to update later, in
/// step order.
fn build_setup_list() -> (gtk::Box, Rc<Vec<SetupRow>>) {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.add_css_class("card");

    let mut rows = Vec::new();
    for (i, status) in RunState::new().steps.iter().enumerate() {
        // Every row carries a rule above it; the first one's is never shown, which
        // keeps "hide the row, hide its rule" a single rule with no special case.
        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        separator.set_visible(i > 0);
        list.append(&separator);

        let icon = gtk::Image::from_icon_name(state_icon(status.state));
        icon.set_valign(gtk::Align::Start);
        icon.add_css_class("dim-label");

        let title = gtk::Label::new(Some(&status.label));
        title.set_xalign(0.0);
        title.add_css_class("dim-label");

        let message = gtk::Label::new(Some(&status.message));
        message.set_xalign(0.0);
        message.set_ellipsize(gtk::pango::EllipsizeMode::End);
        message.add_css_class("caption");
        message.add_css_class("dim-label");

        let bar = gtk::ProgressBar::new();
        bar.set_fraction(0.0);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
        text.set_hexpand(true);
        text.append(&title);
        text.append(&message);
        text.append(&bar);

        let container = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        container.set_margin_top(8);
        container.set_margin_bottom(8);
        container.set_margin_start(12);
        container.set_margin_end(12);
        container.append(&icon);
        container.append(&text);
        list.append(&container);

        rows.push(SetupRow {
            step: status.step,
            container,
            separator,
            icon,
            title,
            message,
            bar,
        });
    }
    (list, Rc::new(rows))
}

/// Redraw one row from its status. Only the row whose step just reported changes,
/// so a download ticking ten times a second touches one bar, not ten.
///
/// `queue` says whether the run has been started. Once it has, the list is a
/// queue: a finished step is hidden so the step being worked on is the top row
/// and the rest slide up behind it -- including after a stop or a failure, where
/// what is left is what a resume will do. Before it starts, every step is shown,
/// because then the list is the plan.
fn update_row(row: &SetupRow, status: &StepStatus, queue: bool) {
    row.icon.set_icon_name(Some(state_icon(status.state)));
    row.message.set_text(&status.message);
    row.bar.set_fraction(status.fraction as f64);
    row.container.set_visible(!(queue && status.state.is_finished()));
    row.separator.set_visible(row.container.is_visible());

    // A step being worked on is the one the eye should land on.
    if status.state == StepState::Running || status.state.is_finished() {
        row.title.remove_css_class("dim-label");
    } else {
        row.title.add_css_class("dim-label");
    }
    if status.state == StepState::Failed {
        row.message.add_css_class("error");
    } else {
        row.message.remove_css_class("error");
    }
}

/// Redraw the whole list. Used at the terminal points, where several rows change
/// at once.
fn update_rows(rows: &[SetupRow], state: &RunState, queue: bool) {
    for (row, status) in rows.iter().zip(state.steps.iter()) {
        update_row(row, status, queue);
    }
}

pub struct App {
    cfg: RefCell<Config>,
    install: RefCell<Result<Install, String>>,
    selected: RefCell<Option<String>>,

    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    list: gtk::ListBox,
    account: adw::EntryRow,
    password: adw::PasswordEntryRow,
    play: gtk::Button,
}

pub fn build(app: &adw::Application) {
    let cfg = Config::load();
    let install = Install::discover(&cfg.prefix);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Asheron's Call")
        .default_width(480)
        .default_height(620)
        .build();

    let header = adw::HeaderBar::new();
    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.set_tooltip_text(Some("Add a server"));
    header.pack_start(&add);

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    list.add_css_class("boxed-list");

    let account = adw::EntryRow::builder().title("Account").build();
    let password = adw::PasswordEntryRow::builder().title("Password").build();
    let creds = adw::PreferencesGroup::new();
    creds.add(&account);
    creds.add(&password);

    let play = gtk::Button::with_label("Play");
    play.add_css_class("suggested-action");
    play.add_css_class("pill");
    play.set_halign(gtk::Align::Center);
    play.set_sensitive(false);

    // The populated view.
    let games = gtk::Box::new(gtk::Orientation::Vertical, 18);
    games.set_margin_top(12);
    games.set_margin_bottom(18);
    games.set_margin_start(12);
    games.set_margin_end(12);
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    games.append(&scroller);
    games.append(&creds);
    games.append(&play);

    // The first-run view.
    let empty = adw::StatusPage::builder()
        .icon_name("network-server-symbolic")
        .title("No servers yet")
        .description("Add a server to get started.")
        .build();
    let empty_add = gtk::Button::with_label("Add a Server");
    empty_add.add_css_class("suggested-action");
    empty_add.add_css_class("pill");
    empty_add.set_halign(gtk::Align::Center);
    empty.set_child(Some(&empty_add));

    // The first-run setup view. Shown when the game/runtime isn't installed, so
    // the launcher is now the entry point -- no separate install script to run.
    // The step list is built up front and shown before setup starts: it doubles as
    // the plan, so you can see the three downloads and the installer coming.
    let (setup_list, setup_rows) = build_setup_list();
    let setup_error = gtk::Label::new(None);
    setup_error.add_css_class("error");
    setup_error.set_wrap(true);
    setup_error.set_justify(gtk::Justification::Center);
    setup_error.set_visible(false);
    let setup_btn = gtk::Button::with_label("Set up Asheron's Call");
    setup_btn.add_css_class("suggested-action");
    setup_btn.add_css_class("pill");
    let setup_cancel = gtk::Button::with_label("Cancel");
    setup_cancel.add_css_class("pill");
    setup_cancel.set_visible(false);
    let setup_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    setup_buttons.set_halign(gtk::Align::Center);
    setup_buttons.append(&setup_btn);
    setup_buttons.append(&setup_cancel);
    let setup_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    setup_box.append(&setup_buttons);
    setup_box.append(&setup_list);
    setup_box.append(&setup_error);
    let setup_inner = adw::StatusPage::builder()
        .icon_name("system-software-install-symbolic")
        .title("Set up Asheron's Call")
        .description(
            "The game and its Windows runtime aren't installed yet. This downloads a \
             couple of gigabytes, runs the Asheron's Call installer, and applies the \
             End-of-Retail update. You only do it once.",
        )
        .child(&setup_box)
        .build();
    // Ten rows plus the header outgrow a 620px window, so the setup page scrolls.
    let setup = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&setup_inner)
        .build();

    let stack = gtk::Stack::new();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&games, Some("games"));
    stack.add_named(&setup, Some("setup"));

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));
    window.set_content(Some(&toolbar));

    let ui = Rc::new(App {
        cfg: RefCell::new(cfg),
        install: RefCell::new(install),
        selected: RefCell::new(None),
        window: window.clone(),
        toasts,
        stack,
        list,
        account,
        password,
        play,
    });

    // The game isn't installed: pressing the button runs setup on a background
    // thread and streams progress here, rather than sending the user off to a
    // shell script. On success we re-discover and drop into the launcher.
    // Stopping is a request, not a kill: the run ends at the next cancellation
    // point, which is immediate mid-download and otherwise once the current
    // external command (wineboot, the installer wizard) returns.
    {
        let cancel = setup_cancel.clone();
        setup_cancel.connect_clicked(move |_| {
            cancel.set_label("Stopping…");
            cancel.set_sensitive(false);
            setup::request_cancel();
        });
    }

    {
        let ui = ui.clone();
        let rows = setup_rows.clone();
        let error = setup_error.clone();
        let btn = setup_btn.clone();
        let cancel = setup_cancel.clone();
        setup_btn.connect_clicked(move |_| {
            btn.set_visible(false);
            cancel.set_label("Cancel");
            cancel.set_sensitive(true);
            cancel.set_visible(true);
            error.set_visible(false);
            setup::clear_cancel();

            let prefix = ui.cfg.borrow().prefix.clone();
            let (tx, rx) = async_channel::unbounded::<SetupEvent>();

            // The work: shells out to umu-run/winetricks, downloads, unzips.
            gio::spawn_blocking(move || {
                let rt = ProtonRuntime::new(prefix);
                let mut on = |p: Progress| {
                    let _ = tx.send_blocking(SetupEvent::Progress(p));
                };
                let result = setup::run_all(&rt, &mut on);
                let _ = tx.send_blocking(SetupEvent::Done(result));
            });

            // The run state lives here, on the main loop: the worker only streams
            // Progress, and folding it into a RunState is what turns that stream
            // into a list a person can read.
            let state = Rc::new(RefCell::new(RunState::new()));
            state.borrow_mut().started = true;

            // Steps that cannot measure themselves (wineboot, the installer
            // wizard) report fraction 0 and then say nothing for minutes. Pulse
            // their bar so the row reads as "working", not "stuck".
            {
                let state = state.clone();
                let rows = rows.clone();
                glib::timeout_add_local(Duration::from_millis(120), move || {
                    let st = state.borrow();
                    if st.done {
                        return glib::ControlFlow::Break;
                    }
                    if let Some(cur) = st.current() {
                        if cur.fraction <= 0.0 {
                            if let Some(row) = rows.iter().find(|r| r.step == cur.step) {
                                row.bar.pulse();
                            }
                        }
                    }
                    glib::ControlFlow::Continue
                });
            }

            // Fold each update back onto the main loop.
            let ui = ui.clone();
            let rows = rows.clone();
            let error = error.clone();
            let btn = btn.clone();
            let cancel = cancel.clone();
            glib::spawn_future_local(async move {
                while let Ok(ev) = rx.recv().await {
                    match ev {
                        SetupEvent::Progress(p) => {
                            let mut st = state.borrow_mut();
                            st.apply(&p);
                            // Only the row that changed is redrawn -- but a step
                            // finishing hides its row, which moves every row below
                            // it, so that case redraws the list.
                            match st.steps.iter().find(|s| s.step == p.step) {
                                Some(status) if status.state.is_finished() => {
                                    update_rows(&rows, &st, true)
                                }
                                Some(status) => {
                                    if let Some(row) = rows.iter().find(|r| r.step == p.step) {
                                        update_row(row, status, true);
                                    }
                                }
                                None => {}
                            }
                        }
                        SetupEvent::Done(result) => {
                            let ok = result.is_ok();
                            {
                                let mut st = state.borrow_mut();
                                st.finish(&result);
                                // Still a queue: after a stop or a failure what is
                                // on screen is what a resume still has to do.
                                update_rows(&rows, &st, true);
                            }
                            cancel.set_visible(false);
                            btn.set_visible(true);
                            btn.set_sensitive(true);
                            if ok {
                                ui.finish_setup();
                            } else if let Err(e) = result {
                                if e == setup::CANCELLED {
                                    btn.set_label("Resume setup");
                                } else {
                                    error.set_text(&e);
                                    error.set_visible(true);
                                    btn.set_label("Try again");
                                }
                            }
                            break;
                        }
                    }
                }
            });
        });
    }

    for b in [&add, &empty_add] {
        let ui = ui.clone();
        b.connect_clicked(move |_| ui.clone().open_browser());
    }

    {
        let ui = ui.clone();
        ui.clone().list.connect_row_selected(move |_, row| {
            let id = row.and_then(|r| unsafe { r.data::<String>("server-id") })
                .map(|p| unsafe { p.as_ref().clone() });
            ui.select(id);
        });
    }

    // Typing credentials for one server must not leak into another, so persist on
    // every keystroke against the selected server rather than on launch.
    {
        let ui = ui.clone();
        ui.clone().account.connect_changed(move |_| ui.remember_credentials());
    }
    {
        let ui = ui.clone();
        ui.clone().password.connect_changed(move |_| ui.remember_credentials());
    }
    {
        let ui = ui.clone();
        ui.clone().play.connect_clicked(move |_| ui.play());
    }
    // Enter in the password field plays, which is what you want after typing it.
    {
        let ui = ui.clone();
        ui.clone().password.connect_entry_activated(move |_| ui.play());
    }

    ui.refresh_list();
    // refresh_list routes to empty/games; if the game isn't installed, the setup
    // page takes precedence until it is.
    if ui.install.borrow().is_err() {
        ui.stack.set_visible_child_name("setup");
    }
    window.present();
}

impl App {
    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    /// Setup finished: re-discover the install and drop into the launcher.
    fn finish_setup(self: &Rc<Self>) {
        let prefix = self.cfg.borrow().prefix.clone();
        *self.install.borrow_mut() = ProtonRuntime::new(prefix).discover();
        self.toast("Setup complete.");
        self.refresh_list();
        // If discovery somehow still failed, stay on setup rather than dropping
        // the user into a launcher that cannot launch anything.
        if self.install.borrow().is_err() {
            self.stack.set_visible_child_name("setup");
        }
    }

    fn open_browser(self: Rc<Self>) {
        let added: Vec<String> = self.cfg.borrow().servers.iter().map(|e| e.id()).collect();
        let ui = self.clone();
        browser::present(&self.window, added, move |s: Server| {
            let name = s.name.clone();
            let id = s.id();
            {
                let mut cfg = ui.cfg.borrow_mut();
                if !cfg.add(&s) {
                    drop(cfg);
                    ui.toast(&format!("{name} is already in your list"));
                    return;
                }
                if let Err(e) = cfg.save() {
                    drop(cfg);
                    ui.toast(&format!("Could not save: {e}"));
                    return;
                }
            }
            ui.refresh_list();
            ui.select_id(&id);
            ui.toast(&format!("Added {name}"));
        });
    }

    /// Rebuild the server list from config and show the right page.
    fn refresh_list(self: &Rc<Self>) {
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }

        let cfg = self.cfg.borrow();
        if cfg.servers.is_empty() {
            self.stack.set_visible_child_name("empty");
            return;
        }
        self.stack.set_visible_child_name("games");

        for e in &cfg.servers {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&e.name))
                .subtitle(glib::markup_escape_text(&format!(
                    "{} · {}",
                    e.address(),
                    e.software.label()
                )))
                .build();

            let remove = gtk::Button::from_icon_name("user-trash-symbolic");
            remove.add_css_class("flat");
            remove.set_valign(gtk::Align::Center);
            remove.set_tooltip_text(Some("Remove this server"));
            {
                let ui = self.clone();
                let id = e.id();
                let name = e.name.clone();
                remove.connect_clicked(move |_| {
                    {
                        let mut cfg = ui.cfg.borrow_mut();
                        cfg.remove(&id);
                        let _ = cfg.save();
                    }
                    if ui.selected.borrow().as_deref() == Some(id.as_str()) {
                        ui.select(None);
                    }
                    ui.refresh_list();
                    ui.toast(&format!("Removed {name}"));
                });
            }
            row.add_suffix(&remove);

            unsafe { row.set_data("server-id", e.id()) };
            self.list.append(&row);
        }
        drop(cfg);

        // Reselect what you played last, so the common case is: open, press Play.
        let last = self.cfg.borrow().last.clone();
        if let Some(id) = last {
            self.select_id(&id);
        }
    }

    fn select_id(self: &Rc<Self>, id: &str) {
        let mut i = 0;
        while let Some(row) = self.list.row_at_index(i) {
            let matches = unsafe { row.data::<String>("server-id") }
                .map(|p| unsafe { p.as_ref() == id })
                .unwrap_or(false);
            if matches {
                self.list.select_row(Some(&row));
                return;
            }
            i += 1;
        }
    }

    /// Load the stored credentials for the newly-selected server into the fields.
    fn select(self: &Rc<Self>, id: Option<String>) {
        *self.selected.borrow_mut() = id.clone();

        let Some(id) = id else {
            self.account.set_text("");
            self.password.set_text("");
            self.play.set_sensitive(false);
            return;
        };

        let cfg = self.cfg.borrow();
        let Some(e) = cfg.find(&id) else { return };
        let (acct, pw) = (e.account.clone(), e.password.clone());
        drop(cfg);

        // Set the fields without the change handler writing them straight back.
        let _guard = Suppress::new(self);
        self.account.set_text(&acct);
        self.password.set_text(&pw);

        self.play.set_sensitive(self.install.borrow().is_ok());

        let mut cfg = self.cfg.borrow_mut();
        cfg.last = Some(id);
        let _ = cfg.save();
    }

    fn remember_credentials(self: &Rc<Self>) {
        if SUPPRESS.with(|s| s.get()) {
            return;
        }
        let Some(id) = self.selected.borrow().clone() else { return };
        let mut cfg = self.cfg.borrow_mut();
        let Some(e) = cfg.find_mut(&id) else { return };
        e.account = self.account.text().to_string();
        e.password = self.password.text().to_string();
        let _ = cfg.save();
    }

    fn play(self: &Rc<Self>) {
        let install = self.install.borrow();
        let install = match &*install {
            Ok(i) => i.clone(),
            Err(e) => {
                self.toast(e);
                return;
            }
        };

        let Some(id) = self.selected.borrow().clone() else {
            self.toast("Pick a server first.");
            return;
        };
        let cfg = self.cfg.borrow();
        let Some(entry) = cfg.find(&id) else { return };
        let server = entry.to_server();
        let (acct, pw) = (entry.account.clone(), entry.password.clone());
        drop(cfg);

        match launcher::launch(&install, &server, &acct, &pw) {
            Ok(_child) => {
                self.toast(&format!("Launching {}…", server.name));
                // The launcher has done its job; get out of the way of the game.
                let w = self.window.clone();
                glib::timeout_add_seconds_local_once(2, move || w.close());
            }
            Err(e) => self.toast(&e),
        }
    }
}

// Setting the entry text programmatically fires `changed`, which would write the
// value straight back into config -- harmless for the row we just read, but it
// also fires while we're mid-swap between two servers. Suppress it for the swap.
thread_local! {
    static SUPPRESS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct Suppress;

impl Suppress {
    fn new(_: &Rc<App>) -> Suppress {
        SUPPRESS.with(|s| s.set(true));
        Suppress
    }
}

impl Drop for Suppress {
    fn drop(&mut self) {
        SUPPRESS.with(|s| s.set(false));
    }
}
