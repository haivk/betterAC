//! Where the game and its Proton build actually live on disk.
//!
//! Discovered, not assumed. The AC installer's wizard picks its own path, and
//! people move prefixes around, so we walk for the files rather than hardcoding
//! C:\Turbine\Asheron's Call or a fixed Proton directory.

use std::path::{Path, PathBuf};

/// A located install: the prefix, the directory the client lives in, and the
/// Proton build that runs it.
#[derive(Debug, Clone)]
pub struct Install {
    pub prefix: PathBuf,
    pub ac_dir: PathBuf,
    pub proton: PathBuf,
}

/// The root of everything betterAC provisions for itself: the Windows runtime it
/// runs the client on, and (on macOS) the prefix and download cache too.
///
/// `dirs::data_dir()` is the right XDG-ish answer on both platforms —
/// `~/Library/Application Support/betterac` on macOS, `~/.local/share/betterac`
/// on Linux. The heavy runtime living here rather than inside the installed
/// application is what keeps the notarised Mac bundle small and lets first launch
/// provision it.
pub fn support_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("betterac")
}

/// The prefix the game is installed into.
///
/// Linux keeps it beside the user's other games rather than under `support_dir()`
/// — it is the biggest thing setup creates and people expect to find it in
/// `~/Games`. macOS has no such convention, so it goes under the app's own folder.
#[cfg(not(target_os = "macos"))]
pub fn default_prefix() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Games/asherons-call")
}

/// Steam's own compatibility-tool directory. betterAC **reads** this — a
/// GE-Proton build already sitting here is copied rather than re-downloaded — but
/// never writes to it or runs out of it. See [`runtime_dir`].
pub fn steam_compat() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share/Steam/compatibilitytools.d")
}

/// The Windows runtime betterAC owns and runs the client on: the
/// CrossOver-lineage Wine engine on macOS, betterAC's private GE-Proton copy on
/// Linux.
///
/// It matters that this is *ours*. Provisioning Decal hot-patches three no-op
/// prologues into the runtime's builtin `d3d9`/`kernel32` (see
/// [`crate::decal`]), and doing that to a build Steam shares with every other
/// game is not ours to do. So Linux copies GE-Proton in here and patches the copy;
/// Steam's stays byte-identical.
///
/// The two names differ because the mac path predates this and moving it would
/// orphan an 800 MB engine that is already on disk.
#[cfg(target_os = "macos")]
pub fn runtime_dir() -> PathBuf {
    support_dir().join("engine")
}

#[cfg(not(target_os = "macos"))]
pub fn runtime_dir() -> PathBuf {
    support_dir().join("proton")
}

/// The macOS Wine prefix. Mirrors Linux's `default_prefix()` role.
#[cfg(target_os = "macos")]
pub fn default_prefix() -> PathBuf {
    support_dir().join("prefix")
}

/// Find a file by name (case-insensitive) somewhere under `root`. Shallow-walks
/// rather than assuming a fixed install path -- the wizard picks its own.
fn find_named(root: &Path, filename: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, want: &str, depth: usize, out: &mut Option<PathBuf>) {
        if out.is_some() || depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            // Never follow symlinks. Wine points the profile folders (Desktop,
            // Documents, Downloads, My Music) at the real macOS home; descending
            // them escapes the prefix and trips macOS privacy prompts, and the
            // files we look for never live behind them.
            if e.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                walk(&p, want, depth - 1, out);
            } else if p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(want))
            {
                *out = Some(p);
                return;
            }
        }
    }
    let mut found = None;
    walk(root, filename, 6, &mut found);
    found
}

pub(crate) fn find_acclient(drive_c: &Path) -> Option<PathBuf> {
    find_named(drive_c, "acclient.exe")
}

/// The directory the game installed into, located by its data file. Used during
/// setup: `client_portal.dat` exists right after the retail wizard (before the
/// End-of-Retail updates land), which is exactly when we need to know where to
/// unzip them.
pub fn find_game_dir(prefix: &Path) -> Option<PathBuf> {
    let f = find_named(&prefix.join("drive_c"), "client_portal.dat")?;
    f.parent().map(|p| p.to_path_buf())
}

/// The GE-Proton build betterAC runs on: the newest one in [`runtime_dir`], which
/// is betterAC's own copy and nobody else's.
///
/// This is what `PROTONPATH` is pointed at and what Decal's engine hot-patch is
/// allowed to touch. A build that exists only in Steam's directory does **not**
/// count — [`find_steam_proton`] finds those, and setup copies one in here.
pub(crate) fn find_proton() -> Option<PathBuf> {
    newest_ge_proton(&runtime_dir())
}

/// A GE-Proton build already installed for Steam, if there is one. Setup uses it
/// as a copy source so a box that has already downloaded GE-Proton for its games
/// does not fetch another 500 MB of the same thing. Never run from directly.
pub(crate) fn find_steam_proton() -> Option<PathBuf> {
    newest_ge_proton(&steam_compat())
}

/// Newest GE-Proton 10 under `dir`. Deliberately not 11: it wants the steamrt4
/// runtime, which umu-run does not provision -- it fetches steamrt3 and then dies
/// looking for steamrt4/toolmanifest.vdf. The aarch64 tarball is skipped; it is a
/// real trap.
fn newest_ge_proton(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<PathBuf> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("GE-Proton10") || name.contains("aarch64") {
            continue;
        }
        if !p.join("proton").exists() {
            continue;
        }
        if best.as_ref().is_none_or(|b| {
            b.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() < name
        }) {
            best = Some(p);
        }
    }
    best
}

impl Install {
    /// Locate a usable install, or explain precisely what is missing. The error
    /// strings are shown to the user, so they name the thing to go fix.
    pub fn discover(prefix: &Path) -> Result<Install, String> {
        if !prefix.is_dir() {
            return Err(format!("No Proton prefix at {}. Run setup first.", prefix.display()));
        }
        let drive_c = prefix.join("drive_c");
        if !drive_c.is_dir() {
            return Err(format!("{} is not a Proton prefix -- it has no drive_c.", prefix.display()));
        }
        let acclient = find_acclient(&drive_c)
            .ok_or_else(|| format!("No acclient.exe under {}. Is the client installed?", drive_c.display()))?;
        let ac_dir = acclient
            .parent()
            .ok_or("acclient.exe has no parent directory")?
            .to_path_buf();
        let proton = find_proton().ok_or_else(|| {
            format!("No GE-Proton10 build in {}. Run setup first.", runtime_dir().display())
        })?;
        Ok(Install { prefix: prefix.to_path_buf(), ac_dir, proton })
    }
}
