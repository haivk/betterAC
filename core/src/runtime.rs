//! The Windows runtime for this platform, chosen at compile time.
//!
//! `wine` (macOS) and `proton` (Linux) expose deliberately identical surfaces —
//! a `Runtime` that walks the shared [`SetupStep`](crate::setup::SetupStep)
//! sequence, a `launch`, and four ways to reach a Windows program inside an
//! installed prefix. This module is the one place that picks between them, so a
//! caller can say what it wants done rather than which platform it is on.
//!
//! Everything a frontend needs is here. Nothing above this line should be writing
//! `#[cfg(target_os = "macos")]` to choose a runtime — the FFI used to carry five
//! copies of that decision and the GTK frontend named `proton` directly, which is
//! why a Linux-only assumption could reach code that was supposed to be shared.
//!
//! The escape hatch is that both modules stay public: the platform-specific bits
//! that genuinely have no counterpart (macOS's engine provisioning, Linux's
//! gamescope wrapping) are still reached directly, from code that is already
//! platform-specific.

use crate::install::Install;
use crate::servers::Server;
use std::path::PathBuf;
use std::process::Child;

/// This platform's runtime type. Implements [`crate::setup::Runtime`], so setup
/// drives it without knowing which one it got.
#[cfg(target_os = "macos")]
pub type PlatformRuntime = crate::wine::WineRuntime;
#[cfg(not(target_os = "macos"))]
pub type PlatformRuntime = crate::proton::ProtonRuntime;

/// The runtime pointed at `prefix`.
pub fn for_prefix(prefix: PathBuf) -> PlatformRuntime {
    PlatformRuntime::new(prefix)
}

/// The runtime pointed at the configured prefix — what almost every caller wants.
pub fn configured() -> PlatformRuntime {
    for_prefix(crate::config::Config::load().prefix)
}

/// Locate the install in the configured prefix, or explain what is missing.
pub fn discover() -> Result<Install, String> {
    discover_in(crate::config::Config::load().prefix)
}

/// Locate the install in a specific prefix — for a frontend holding a prefix that
/// may not be the saved one yet (mid-setup, or just after a reset).
///
/// Exists so callers do not have to import [`crate::setup::Runtime`] just to reach
/// its `discover`; the point of this module is that a frontend never names a
/// runtime type or trait.
pub fn discover_in(prefix: PathBuf) -> Result<Install, String> {
    use crate::setup::Runtime;
    for_prefix(prefix).discover()
}

/// Launch the client. Returns once the process is spawned, not when it exits.
///
/// `res` is the current display mode in real pixels. Detecting it needs a toolkit,
/// so the frontend does it and passes the result in — except on macOS, where
/// CoreGraphics can answer without one and `None` means "ask the main display".
pub fn launch(
    install: &Install,
    server: &Server,
    account: &str,
    password: &str,
    res: Option<(i32, i32)>,
) -> Result<Child, String> {
    #[cfg(target_os = "macos")]
    return crate::wine::launch(install, server, account, password, res);
    #[cfg(not(target_os = "macos"))]
    return crate::proton::launch(install, server, account, password, res);
}

/// Run a Windows program inside the prefix and wait for it to exit. The
/// [`crate::decal`] operations the settings UI performs (importing a `.reg`,
/// mostly) need one of these.
pub fn run_in_prefix(install: &Install, args: &[&str]) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return crate::wine::run_in_prefix(install, args);
    #[cfg(not(target_os = "macos"))]
    return crate::proton::run_in_prefix(install, args);
}

/// Like [`run_in_prefix`], but returns once the program is *running* — for
/// Windows programs that stay up, such as Decal's agent.
pub fn spawn_in_prefix(install: &Install, args: &[&str]) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return crate::wine::spawn_in_prefix(install, args);
    #[cfg(not(target_os = "macos"))]
    return crate::proton::spawn_in_prefix(install, args);
}

/// Like [`run_in_prefix`], but returns the program's stdout. Used for `reg query`.
pub fn query_in_prefix(install: &Install, args: &[&str]) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    return crate::wine::query_in_prefix(install, args);
    #[cfg(not(target_os = "macos"))]
    return crate::proton::query_in_prefix(install, args);
}

/// End everything running in the prefix, so nothing outlives the app.
///
/// Called when betterAC quits. Without it a Windows program that outlives the app
/// keeps its status icon — and those icons are owned by the prefix's
/// `explorer.exe` rather than by the program, so one that dies abruptly leaves a
/// dead icon nothing later will clear.
///
/// Best-effort and silent: there is usually nothing to kill, and a caller doing
/// this at quit has nobody left to report to.
///
/// **This does not discriminate** — it ends everything in the prefix, the game
/// included. Reset wants exactly that, because it is about to delete the prefix.
/// Quitting does not; use [`shutdown_on_quit`].
pub fn shutdown_prefix(install: &Install) {
    #[cfg(target_os = "macos")]
    crate::wine::shutdown_prefix(install);
    #[cfg(not(target_os = "macos"))]
    crate::proton::shutdown_prefix(install);
}

/// The teardown a frontend should run when the app quits: [`shutdown_prefix`],
/// but only when this session started Decal's agent.
///
/// The unconditional version was a latent bug — quitting the launcher while the
/// game was running took the game down with it, because `wineserver -k` ends the
/// whole session. See [`crate::decal::agent_started`] for why the agent is the
/// only thing that ever needs ending here.
pub fn shutdown_on_quit(install: &Install) {
    if crate::decal::agent_started() {
        shutdown_prefix(install);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the module: the same call compiles on both platforms and
    /// reaches the runtime that platform actually uses.
    #[test]
    fn the_platform_runtime_is_the_one_this_platform_runs_on() {
        let rt = for_prefix(PathBuf::from("/tmp/betterac-runtime-selection-test"));
        assert_eq!(rt.prefix, PathBuf::from("/tmp/betterac-runtime-selection-test"));
        assert_eq!(
            std::any::type_name::<PlatformRuntime>().contains("wine"),
            cfg!(target_os = "macos"),
            "macOS must select the Wine runtime and every other platform Proton"
        );
    }
}
