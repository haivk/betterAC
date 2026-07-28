//! Build the native helper the macOS "fullscreen Space" launch path injects into
//! Wine. `acspaces.dylib` is the only one — clang is always present on a macOS build
//! host, so it is compiled from source into `OUT_DIR` and embedded by `wine.rs`.
//!
//! It also declares the one environment variable the crate bakes in: the release
//! version (see `ac_core::VERSION`). Cargo does not track variables it was not
//! told about, so without that line a cached build would keep reporting the
//! version it was first compiled with -- and CI caches Rust builds, so that is the
//! normal case rather than an edge one.
//!
//! Everything else here is a no-op unless the *target* is macOS, so Linux builds
//! of the workspace are unaffected.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Before the early return below: this matters on every platform.
    println!("cargo:rerun-if-env-changed=BETTERAC_VERSION");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join("../macos/helpers/acspaces.m");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("acspaces.dylib");
    println!("cargo:rerun-if-changed={}", src.display());

    // x86_64 to match Wine (wine32on64 is an x86_64 build; on Apple Silicon it runs
    // under Rosetta, on Intel it is native — either way the injected dylib is x86_64).
    let status = Command::new("clang")
        .args(["-arch", "x86_64", "-dynamiclib", "-framework", "AppKit", "-O2", "-o"])
        .arg(&out)
        .arg(&src)
        .status()
        .expect("failed to run clang to build acspaces.dylib");
    if !status.success() {
        panic!("clang failed to build acspaces.dylib from {}", src.display());
    }
}
