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
//! toolkit, so the frontend detects it and passes it in (see `proton::launch`).

pub mod args;
pub mod config;
pub mod fetch;
pub mod gamefiles;
pub mod install;
pub mod proton;
pub mod servers;
pub mod setup;

/// The macOS Wine runtime. Compiled only on macOS: it self-provisions a
/// CrossOver-lineage Wine engine and runs the 32-bit client under Rosetta 2,
/// none of which means anything on Linux (which uses `proton`).
#[cfg(target_os = "macos")]
pub mod wine;

pub use install::{default_prefix, steam_compat, Install};
