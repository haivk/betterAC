# betterAC — macOS app

Native SwiftUI launcher for Asheron's Call. Thin UI over the shared Rust core
(`ac-core`) through the `ac-ffi` C ABI; all the real logic — server directory,
config, setup steps, launching the client under Wine — lives in Rust and is
shared with the Linux GTK app.

## Build & run

```sh
brew install xcodegen        # once
cd betterAC/macos
xcodegen                     # generates BetterAC.xcodeproj
open BetterAC.xcodeproj      # ⌘R to run
```

Everything is wired by the generated project:

- A **pre-build phase** builds `ac-ffi` once per architecture Xcode asked for and
  `lipo`s the slices into `../target/universal/release/libac_ffi.a`, so the Rust
  staticlib is always current, and always matches the app's architectures.
- The linker pulls it in with `-lac_ffi` (the only flag needed — verified by
  linking a C probe against `libac_ffi.a`).
- `Sources/BetterAC-Bridging-Header.h` imports the cbindgen-generated
  `../ffi/include/ac_ffi.h`, so Swift sees `ac_detect`, `ac_setup_start`, … .
- `BetterAC.entitlements` carries the Wine entitlements (`allow-jit`,
  `allow-unsigned-executable-memory`, `disable-library-validation`). The app is
  **not** sandboxed — a sandboxed app can't drive an external Wine engine.

## Architectures — the app is universal

`ARCHS = arm64 x86_64`. Intel Macs cost almost nothing to support here, because
the Wine engine we self-provision is an **x86_64** build either way (see
`core/src/wine.rs`): on Apple Silicon it is translated by Rosetta 2, on an Intel
Mac it is simply native. The only architecture-conditional code in the project is
`NEEDS_ROSETTA` in `wine.rs`, which turns the Dependencies step into a no-op on
Intel — `softwareupdate --install-rosetta` does not exist there.

- **Debug** sets `ONLY_ACTIVE_ARCH = YES`, so ⌘R builds this Mac's arch only and a
  day-to-day dev never needs a cross-compiling toolchain.
- **Release** builds both slices, which needs a **rustup** toolchain — Homebrew's
  rust ships std for the host alone and is not rustup-managed, so
  `rustup target add` cannot help it:

  ```sh
  rustup update stable                     # >= 1.85: cbindgen pulls clap_lex (edition 2024)
  rustup target add x86_64-apple-darwin
  ```

  The pre-build script picks the first cargo ≥ 1.85 (rustup's first, since it is
  the only one that can cross-compile) and puts its bin directory at the front of
  `PATH`. That last part matters: `cargo` resolves `rustc` from `PATH`, so leaving
  Homebrew's rustc first makes rustup's cargo drive it and fail cross-compiling
  with a misleading *"can't find crate for `core` / target may not be installed"*.

To build a universal binary from the command line, pass a generic destination —
otherwise `xcodebuild` resolves a concrete one (My Mac/arm64) that pins `ARCHS`
and you silently get a single-slice build:

```sh
xcodebuild -project BetterAC.xcodeproj -scheme BetterAC \
           -configuration Release -destination 'generic/platform=macOS' build
lipo -archs "$(...)/Release/BetterAC.app/Contents/MacOS/BetterAC"   # x86_64 arm64
```

Verified on Apple Silicon: universal app + staticlib, `cargo test -p ac-core
--target x86_64-apple-darwin` green (46 tests, under Rosetta), and an x86_64 C
probe against the FFI that detects a real install and reports the Dependencies
step as *"this Mac runs x86 code natively"*. Not yet run on real Intel hardware.

## Configuration (first run needs these)

The runtime is env-driven, same as the Linux side. To run setup end-to-end, set
these in the scheme's environment (Product ▸ Scheme ▸ Edit Scheme ▸ Run ▸
Arguments ▸ Environment Variables), or export them if launching from a terminal:

| Variable | Purpose |
|---|---|
| `AC_WINE_ENGINE` | Path to an **existing** CrossOver-lineage engine (a folder containing `bin/wine`, or the wine binary itself). Reuse the Step-Zero Whisky-fork engine here to test without hosting anything. |
| `AC_WINE_ENGINE_URL` | …or a tarball URL to download + unpack into `~/Library/Application Support/betterac/engine`. |
| `AC_GAMEFILES_URL` | Base URL hosting `ac1install.exe` + `ac-updates.zip`. |
| `AC_SRC` | …or a local folder that already has those two files. |

Nothing is hardcoded: there is no default engine or game-files URL yet — that's a
hosting decision.

## Screens

- **RootView** — calls `ac_detect`; routes to setup or launcher.
- **SetupView** — `ac_setup_start` then polls `ac_setup_poll` (the same Progress
  stream the GTK progress bar renders).
- **LauncherView** — `ac_servers_json` for the directory, per-server credentials
  saved through `ac_config_get`/`ac_config_set`, Play calls `ac_launch`.
