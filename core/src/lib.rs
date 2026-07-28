//! ac-core — the platform-agnostic heart of the Asheron's Call launcher.
//!
//! Everything here builds on both Linux and macOS. The split from the old single
//! crate is along exactly one line: code that needs a GUI toolkit (GTK on Linux,
//! Cocoa/SwiftUI on macOS) or a compositor to ask about the display lives in the
//! frontends; everything else — the server directory, persisted config, the two
//! client argument shapes, install discovery, and launching — lives here so it is
//! written and tested once.
//!
//! The one deliberate exception is display-resolution detection: it needs a
//! toolkit, so the frontend detects it and passes it in (see `runtime::launch`).
//!
//! Which of the two Windows runtimes a build uses is decided once, in `runtime` —
//! frontends go through that rather than naming `wine` or `proton` themselves.

pub mod args;
pub mod clrmeta;
pub mod config;
pub mod decal;
pub mod deps;
pub mod fetch;
pub mod gamefiles;
pub mod install;
pub mod patches;
pub mod prefs;
pub mod proton;
pub mod reset;
pub mod runtime;
pub mod servers;
pub mod setup;
pub mod update;

/// The macOS Wine runtime. Compiled only on macOS: it self-provisions a
/// CrossOver-lineage Wine engine and runs the 32-bit client under Rosetta 2,
/// none of which means anything on Linux (which uses `proton`).
#[cfg(target_os = "macos")]
pub mod wine;

pub use install::{default_prefix, runtime_dir, steam_compat, support_dir, Install};

/// The version of this build.
///
/// Released builds are dated -- `2026.07.27.42` -- which is four components and so
/// cannot be a crate version, since Cargo requires semver `X.Y.Z`. CI passes the
/// real one in at compile time as `BETTERAC_VERSION`; a local build falls back to
/// the workspace version, which is why a dev build reads as `0.1.0` and always
/// looks older than any release.
///
/// `build.rs` tells Cargo to watch that variable, or a cached build would keep
/// reporting a stale version.
pub const VERSION: &str = match option_env!("BETTERAC_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
