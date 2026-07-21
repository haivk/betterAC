//! The GTK frontend's glue to the launch path.
//!
//! Almost all of launching lives in `ac_core::proton`. The one thing that has to
//! stay here is detecting the current display mode, because that needs a toolkit
//! (Mutter over D-Bus, or GDK) and `ac-core` is deliberately toolkit-free so it
//! can also build on macOS. We detect the resolution and hand it to the core.

use ac_core::servers::Server;

pub use ac_core::Install;

/// Launch the client at the current display resolution. Thin wrapper over
/// `ac_core::proton::launch`; see it for the gamescope/DXVK coupling.
pub fn launch(
    install: &Install,
    server: &Server,
    account: &str,
    password: &str,
) -> Result<std::process::Child, String> {
    ac_core::proton::launch(install, server, account, password, current_resolution())
}

/// The current display mode, in real pixels.
///
/// Mutter first, and deliberately: GNOME is the stated host, and DisplayConfig
/// reports the actual hardware mode. GDK is the fallback, and it is only a
/// fallback because `Monitor::geometry` is in logical pixels and `scale_factor` is
/// an integer -- on a fractionally scaled desktop (GNOME's 125%, 150%) that pair
/// cannot reconstruct the true mode and will overshoot. Right answer first, honest
/// approximation second.
fn current_resolution() -> Option<(i32, i32)> {
    mutter_resolution().or_else(gdk_resolution)
}

/// Ask Mutter. The reply is
///   (u, a((ssss)a(siiddada{sv})a{sv}), a(iiduba(ssss)a{sv}), a{sv})
/// and the mode we want is the one flagged `is-current` on the first monitor that
/// has one.
fn mutter_resolution() -> Option<(i32, i32)> {
    use gtk::gio;
    use gtk::glib;

    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    let reply = conn
        .call_sync(
            Some("org.gnome.Mutter.DisplayConfig"),
            "/org/gnome/Mutter/DisplayConfig",
            "org.gnome.Mutter.DisplayConfig",
            "GetCurrentState",
            None,
            None,
            gio::DBusCallFlags::NONE,
            1000,
            gio::Cancellable::NONE,
        )
        .ok()?;

    let monitors = reply.child_value(1);
    for monitor in monitors.iter() {
        for mode in monitor.child_value(1).iter() {
            // The mode's properties are an a{sv}; VariantDict is what reads one.
            let props = glib::VariantDict::new(Some(&mode.child_value(6)));
            let is_current = props
                .lookup_value("is-current", Some(glib::VariantTy::BOOLEAN))
                .and_then(|v| v.get::<bool>())
                .unwrap_or(false);
            if is_current {
                let w = mode.child_value(1).get::<i32>()?;
                let h = mode.child_value(2).get::<i32>()?;
                return Some((w, h));
            }
        }
    }
    None
}

/// Not GNOME, or Mutter would not answer. Logical size times the integer scale is
/// the best we can do without a compositor to ask.
fn gdk_resolution() -> Option<(i32, i32)> {
    use gtk::prelude::*;

    let display = gtk::gdk::Display::default()?;
    let monitor = display.monitors().item(0)?.downcast::<gtk::gdk::Monitor>().ok()?;
    let g = monitor.geometry();
    let scale = monitor.scale_factor().max(1);
    Some((g.width() * scale, g.height() * scale))
}
