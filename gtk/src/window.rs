//! The main window: pick a server, type your account, play.

use crate::launcher::{self, Install};
use ac_core::config::Config;
use ac_core::proton::ProtonRuntime;
use ac_core::servers::{self, Server};
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
    /// The whole directory, in sidebar order. Selection is an id, so this is what
    /// resolves it back to the `Server` the launcher needs.
    servers: RefCell<Vec<Server>>,
    /// Set when a save changed which section a server belongs in. The resort can't
    /// happen inside the handler that caused it -- rebuilding the list while it is
    /// emitting `row-selected` re-enters -- so it is deferred to an idle.
    resort: std::cell::Cell<bool>,
    /// Set while the sidebar is being rebuilt. Emptying a ListBox emits
    /// `row-selected(None)`, which would otherwise be read as "the user deselected"
    /// and would throw away the selection the rebuild is about to restore.
    rebuilding: std::cell::Cell<bool>,

    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    split: adw::NavigationSplitView,
    list: gtk::ListBox,
    search: gtk::SearchEntry,
    status: gtk::Label,
    /// The detail pane: "none" until a server is picked, then "server".
    detail: gtk::Stack,
    detail_page: adw::NavigationPage,
    address: adw::ActionRow,
    software: adw::ActionRow,
    forget: gtk::Button,
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
        .default_width(900)
        .default_height(640)
        .build();

    // ------------------------------------------------------------- the sidebar
    //
    // The whole directory lives here, which is the shape the Mac app settled on:
    // there is no "add a server" step, you just pick one and type your account.
    // Servers you have credentials for are pinned to a "Saved" section on top.
    let search = gtk::SearchEntry::builder().placeholder_text("Search servers").build();

    let status = gtk::Label::new(None);
    status.add_css_class("dim-label");
    status.add_css_class("caption");
    status.set_halign(gtk::Align::Start);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    list.add_css_class("navigation-sidebar");

    let sidebar_body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    sidebar_body.set_margin_top(6);
    sidebar_body.set_margin_start(6);
    sidebar_body.set_margin_end(6);
    sidebar_body.append(&search);
    sidebar_body.append(&status);
    sidebar_body.append(
        &gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .build(),
    );

    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.set_tooltip_text(Some("Refresh the server list"));
    let sidebar_bar = adw::HeaderBar::new();
    sidebar_bar.pack_end(&refresh);
    let sidebar_view = adw::ToolbarView::new();
    sidebar_view.add_top_bar(&sidebar_bar);
    sidebar_view.set_content(Some(&sidebar_body));
    let sidebar_page =
        adw::NavigationPage::builder().title("Servers").child(&sidebar_view).build();

    // -------------------------------------------------------- the detail pane
    let address = adw::ActionRow::builder().title("Address").build();
    address.add_css_class("property");
    let software = adw::ActionRow::builder().title("Software").build();
    software.add_css_class("property");
    let about = adw::PreferencesGroup::builder().title("Server").build();
    about.add(&address);
    about.add(&software);

    let account = adw::EntryRow::builder().title("Account").build();
    let password = adw::PasswordEntryRow::builder().title("Password").build();
    let creds = adw::PreferencesGroup::builder().title("Account").build();
    creds.add(&account);
    creds.add(&password);

    let play = gtk::Button::with_label("Play");
    play.add_css_class("suggested-action");
    play.add_css_class("pill");
    play.set_halign(gtk::Align::Center);
    play.set_sensitive(false);

    // Only shown for a server you have saved: this is the counterpart of the old
    // per-row trash button, now that every server is in the list whether you use
    // it or not. "Remove" would read as removing it from the directory.
    let forget = gtk::Button::with_label("Forget this server");
    forget.add_css_class("destructive-action");
    forget.add_css_class("flat");
    forget.set_halign(gtk::Align::Center);
    forget.set_visible(false);

    let server_box = gtk::Box::new(gtk::Orientation::Vertical, 18);
    server_box.set_margin_top(18);
    server_box.set_margin_bottom(18);
    server_box.set_margin_start(18);
    server_box.set_margin_end(18);
    server_box.append(&about);
    server_box.append(&creds);
    server_box.append(&play);
    server_box.append(&forget);

    let no_selection = adw::StatusPage::builder()
        .icon_name("network-server-symbolic")
        .title("Choose a server")
        .description("Pick a server on the left to enter your account and play.")
        .build();

    let detail = gtk::Stack::new();
    detail.add_named(&no_selection, Some("none"));
    detail.add_named(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&server_box)
            .build(),
        Some("server"),
    );

    let detail_view = adw::ToolbarView::new();
    detail_view.add_top_bar(&adw::HeaderBar::new());
    detail_view.set_content(Some(&detail));
    let detail_page =
        adw::NavigationPage::builder().title("Asheron's Call").child(&detail_view).build();

    let split = adw::NavigationSplitView::builder()
        .sidebar(&sidebar_page)
        .content(&detail_page)
        .min_sidebar_width(260.0)
        .max_sidebar_width(320.0)
        .build();

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
    // Ten rows plus the header outgrow a 640px window, so the setup page scrolls.
    // It carries its own header bar: now that the launcher is a split view, each
    // page owns its bar rather than sharing one at window level.
    let setup_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&setup_inner)
        .build();
    let setup_view = adw::ToolbarView::new();
    setup_view.add_top_bar(&adw::HeaderBar::new());
    setup_view.set_content(Some(&setup_scroll));

    let stack = gtk::Stack::new();
    stack.add_named(&split, Some("games"));
    stack.add_named(&setup_view, Some("setup"));

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    window.set_content(Some(&toasts));

    // A split view in a narrow window has to collapse or the two panes fight over
    // the width; under this the sidebar becomes a page you navigate into instead.
    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        620.0,
        adw::LengthUnit::Sp,
    ));
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(breakpoint);

    let ui = Rc::new(App {
        cfg: RefCell::new(cfg),
        install: RefCell::new(install),
        selected: RefCell::new(None),
        servers: RefCell::new(Vec::new()),
        resort: std::cell::Cell::new(false),
        rebuilding: std::cell::Cell::new(false),
        window: window.clone(),
        toasts,
        stack,
        split,
        list,
        search,
        status,
        detail,
        detail_page,
        address,
        software,
        forget,
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

    {
        let ui = ui.clone();
        refresh.connect_clicked(move |_| ui.clone().load_directory());
    }
    {
        let ui = ui.clone();
        ui.clone().search.connect_search_changed(move |_| ui.refresh_list());
    }

    {
        let ui = ui.clone();
        ui.clone().list.connect_row_selected(move |_, row| {
            let id = row
                .and_then(|r| unsafe { r.data::<String>("server-id") })
                .map(|p| unsafe { p.as_ref().clone() });
            ui.select(id);
        });
    }

    {
        let ui = ui.clone();
        ui.clone().forget.connect_clicked(move |_| ui.forget_selected());
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
    // Credentials are written when you leave the field rather than on every
    // keystroke: saving is what moves a server into the pinned "Saved" section,
    // and re-sorting the list under a cursor that is still typing in it is worse
    // than waiting for the pause.
    for w in [
        ui.account.clone().upcast::<gtk::Widget>(),
        ui.password.clone().upcast::<gtk::Widget>(),
    ] {
        let ui = ui.clone();
        w.connect_has_focus_notify(move |w| {
            if !w.has_focus() {
                ui.flush_credentials();
            }
        });
    }
    // A close while a field still has focus would otherwise drop what was typed.
    {
        let ui = ui.clone();
        window.connect_close_request(move |_| {
            ui.flush_credentials();
            glib::Propagation::Proceed
        });
    }

    ui.clone().load_directory();
    // The launcher is the default page; if the game isn't installed, setup takes
    // precedence until it is.
    if ui.install.borrow().is_err() {
        ui.stack.set_visible_child_name("setup");
    } else {
        ui.stack.set_visible_child_name("games");
    }
    window.present();
}

impl App {
    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    /// Setup finished: re-discover the install and drop into the launcher.
    ///
    /// The navigation is explicit. `refresh_list` only rebuilds the sidebar -- it
    /// does not decide which page is showing -- so without this the app sits on
    /// the setup screen after a successful run and the button just re-runs a
    /// setup whose every step now skips off its stamp.
    fn finish_setup(self: &Rc<Self>) {
        let prefix = self.cfg.borrow().prefix.clone();
        *self.install.borrow_mut() = ProtonRuntime::new(prefix).discover();
        self.refresh_list();

        // If discovery somehow still failed, stay on setup rather than dropping
        // the user into a launcher that cannot launch anything.
        if let Err(e) = &*self.install.borrow() {
            self.toast(e);
            self.stack.set_visible_child_name("setup");
            return;
        }
        self.toast("Setup complete.");
        // Play was insensitive while there was nothing to launch.
        self.play.set_sensitive(self.selected.borrow().is_some());
        self.stack.set_visible_child_name("games");
    }

    /// Put the directory on screen: the bundled snapshot immediately, then the
    /// live list from treestats when it lands. A sidebar that is instantly usable
    /// beats a spinner, and the snapshot is nearly always right.
    fn load_directory(self: Rc<Self>) {
        if self.servers.borrow().is_empty() {
            *self.servers.borrow_mut() = servers::bundled();
            self.refresh_list();
        }
        self.status.set_text("Showing the bundled list — refreshing…");

        let (tx, rx) = async_channel::bounded(1);
        gio::spawn_blocking(move || {
            let _ = tx.send_blocking(servers::fetch());
        });

        glib::spawn_future_local(async move {
            match rx.recv().await {
                Ok(Ok(live)) => {
                    let n = live.len();
                    *self.servers.borrow_mut() = live;
                    self.refresh_list();
                    self.status.set_text(&format!("{n} servers · live from treestats.net"));
                }
                // Offline is not a state worth blocking on -- the bundled list is
                // already on screen and is perfectly usable.
                Ok(Err(e)) => self.status.set_text(&format!("Showing the bundled list — {e}")),
                Err(_) => {}
            }
        });
    }

    /// The sidebar's contents: saved servers first, then the rest, each filtered
    /// by the search box.
    ///
    /// A saved server that the directory no longer lists (a private server, or one
    /// that dropped off treestats) is rebuilt from its config entry rather than
    /// disappearing -- losing the row would lose the only way back to it.
    fn visible_servers(&self) -> Vec<(Server, bool)> {
        let cfg = self.cfg.borrow();
        let directory = self.servers.borrow();

        let mut saved: Vec<Server> = Vec::new();
        for e in &cfg.servers {
            let id = e.id();
            match directory.iter().find(|s| s.id() == id) {
                Some(s) => saved.push(s.clone()),
                None => saved.push(e.to_server()),
            }
        }
        servers::sort(&mut saved);

        let rest = directory.iter().filter(|s| cfg.find(&s.id()).is_none()).cloned();
        let all = saved.into_iter().map(|s| (s, true)).chain(rest.map(|s| (s, false)));

        let needle = self.search.text().trim().to_ascii_lowercase();
        if needle.is_empty() {
            return all.collect();
        }
        all.filter(|(s, _)| {
            s.name.to_ascii_lowercase().contains(&needle)
                || s.host.to_ascii_lowercase().contains(&needle)
                || s.description.to_ascii_lowercase().contains(&needle)
        })
        .collect()
    }

    /// Rebuild the sidebar. Cheap enough to do wholesale -- the directory is ~44
    /// rows -- but it drops the selection, so the caller's selection is restored
    /// at the end.
    fn refresh_list(self: &Rc<Self>) {
        self.rebuilding.set(true);
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }

        let rows = self.visible_servers();
        if rows.is_empty() {
            let empty = adw::StatusPage::builder()
                .icon_name("system-search-symbolic")
                .title("No servers match")
                .build();
            empty.add_css_class("compact");
            self.list.append(&empty);
            self.rebuilding.set(false);
            return;
        }

        for (s, saved) in &rows {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&s.name))
                .subtitle(glib::markup_escape_text(&if s.ruleset.is_empty() {
                    s.software.label().to_string()
                } else {
                    format!("{} · {}", s.software.label(), s.ruleset)
                }))
                .build();

            // The population badge is the thing people actually scan for.
            if let Some(n) = s.online() {
                let badge = gtk::Label::new(Some(&format!("{n}")));
                badge.add_css_class("caption");
                badge.add_css_class("accent");
                badge.set_tooltip_text(Some("Players online"));
                row.add_suffix(&badge);
            }
            if *saved {
                let tick = gtk::Image::from_icon_name("avatar-default-symbolic");
                tick.add_css_class("accent");
                tick.set_tooltip_text(Some("You have an account saved for this server"));
                row.add_suffix(&tick);
            }

            unsafe { row.set_data("server-id", s.id()) };
            unsafe { row.set_data("saved", *saved) };
            self.list.append(&row);
        }

        // "Saved" and "All Servers" headers, GTK's way: a function that is asked,
        // per row, whether it starts a new group.
        self.list.set_header_func(move |row, before| {
            let saved = |r: &gtk::ListBoxRow| {
                unsafe { r.data::<bool>("saved") }.map(|p| unsafe { *p.as_ref() }).unwrap_or(false)
            };
            let title = match (saved(row), before.map(saved)) {
                (true, None) => "Saved",
                (false, None) | (false, Some(true)) => "Servers",
                _ => {
                    row.set_header(None::<&gtk::Widget>);
                    return;
                }
            };
            let label = gtk::Label::new(Some(title));
            label.add_css_class("heading");
            label.add_css_class("dim-label");
            label.set_halign(gtk::Align::Start);
            label.set_margin_top(12);
            label.set_margin_bottom(4);
            label.set_margin_start(12);
            row.set_header(Some(&label));
        });

        self.rebuilding.set(false);

        // Keep the current selection if it survived the rebuild, else fall back to
        // what you played last: open, press Play is the common case.
        let want = self.selected.borrow().clone().or_else(|| self.cfg.borrow().last.clone());
        if let Some(id) = want {
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

    /// Resolve a selection back to the server it names.
    fn server(&self, id: &str) -> Option<Server> {
        if let Some(s) = self.servers.borrow().iter().find(|s| s.id() == id) {
            return Some(s.clone());
        }
        // Saved but no longer in the directory -- same fallback the sidebar makes.
        self.cfg.borrow().find(id).map(|e| e.to_server())
    }

    /// Switch the detail pane to a newly-selected server: its details, and its
    /// saved credentials if it has any.
    fn select(self: &Rc<Self>, id: Option<String>) {
        if self.rebuilding.get() {
            return;
        }
        // Whatever was typed for the previous server is written before the fields
        // are reused for this one, or it would leak across.
        self.flush_credentials();
        *self.selected.borrow_mut() = id.clone();

        let Some(id) = id else {
            self.detail.set_visible_child_name("none");
            self.detail_page.set_title("Asheron's Call");
            self.account.set_text("");
            self.password.set_text("");
            self.play.set_sensitive(false);
            self.forget.set_visible(false);
            return;
        };
        let Some(s) = self.server(&id) else { return };

        self.detail_page.set_title(&s.name);
        self.address.set_subtitle(&s.address());
        self.software.set_subtitle(s.software.label());

        let cfg = self.cfg.borrow();
        let entry = cfg.find(&id);
        let (acct, pw) = match entry {
            Some(e) => (e.account.clone(), e.password.clone()),
            None => (String::new(), String::new()),
        };
        let saved = entry.is_some();
        drop(cfg);

        // Set the fields without the focus handler writing them straight back.
        {
            let _guard = Suppress::new(self);
            self.account.set_text(&acct);
            self.password.set_text(&pw);
        }
        self.forget.set_visible(saved);
        self.play.set_sensitive(self.install.borrow().is_ok());
        self.detail.set_visible_child_name("server");
        // Collapsed (narrow window): picking a server should walk you to it.
        if self.split.is_collapsed() {
            self.split.set_show_content(true);
        }
    }

    /// Write the fields into the selected server's config entry, creating the
    /// entry if this is the first time you have typed an account for it. Creating
    /// or emptying one changes which section the server belongs in, so that case
    /// asks for a resort.
    fn flush_credentials(self: &Rc<Self>) {
        if SUPPRESS.with(|s| s.get()) {
            return;
        }
        let Some(id) = self.selected.borrow().clone() else { return };
        let (acct, pw) = (self.account.text().to_string(), self.password.text().to_string());

        let mut cfg = self.cfg.borrow_mut();
        let was_saved = cfg.find(&id).is_some();
        // Nothing typed and nothing stored: don't create an empty entry just
        // because the field was focused.
        if !was_saved && acct.is_empty() && pw.is_empty() {
            return;
        }
        if !was_saved {
            let Some(s) = self.servers.borrow().iter().find(|s| s.id() == id).cloned() else {
                return;
            };
            cfg.add(&s);
        }
        let Some(e) = cfg.find_mut(&id) else { return };
        if e.account == acct && e.password == pw {
            return;
        }
        e.account = acct;
        e.password = pw;
        if let Err(err) = cfg.save() {
            drop(cfg);
            self.toast(&format!("Could not save: {err}"));
            return;
        }
        drop(cfg);

        if !was_saved {
            self.forget.set_visible(true);
            self.queue_resort();
        }
    }

    /// Rebuild the sidebar once the current signal handler has returned. Doing it
    /// inline would re-enter: the rebuild drops rows, which emits `row-selected`,
    /// which is often what we are already inside.
    fn queue_resort(self: &Rc<Self>) {
        if self.resort.replace(true) {
            return;
        }
        let ui = self.clone();
        glib::idle_add_local_once(move || {
            ui.resort.set(false);
            ui.refresh_list();
        });
    }

    /// Drop the saved account for the selected server. The server stays in the
    /// directory list; it just moves back out of "Saved".
    fn forget_selected(self: &Rc<Self>) {
        let Some(id) = self.selected.borrow().clone() else { return };
        let name = self.server(&id).map(|s| s.name).unwrap_or_else(|| id.clone());
        {
            let mut cfg = self.cfg.borrow_mut();
            if cfg.find(&id).is_none() {
                return;
            }
            cfg.remove(&id);
            let _ = cfg.save();
        }
        {
            let _guard = Suppress::new(self);
            self.account.set_text("");
            self.password.set_text("");
        }
        self.forget.set_visible(false);
        self.queue_resort();
        self.toast(&format!("Forgot the account for {name}"));
    }

    fn play(self: &Rc<Self>) {
        let install = match &*self.install.borrow() {
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
        let Some(server) = self.server(&id) else { return };
        // Persist before launching, which is also what moves a first-time server
        // into "Saved" -- the same moment the Mac app saves.
        self.flush_credentials();
        let (acct, pw) = (self.account.text().to_string(), self.password.text().to_string());
        if acct.is_empty() || pw.is_empty() {
            self.toast("Enter your account and password first.");
            return;
        }
        {
            let mut cfg = self.cfg.borrow_mut();
            cfg.last = Some(id);
            let _ = cfg.save();
        }

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
