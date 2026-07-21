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

#[cfg(not(target_os = "macos"))]
pub fn default_prefix() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Games/asherons-call")
}

pub fn steam_compat() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share/Steam/compatibilitytools.d")
}

// --- macOS ------------------------------------------------------------------
//
// The Mac keeps everything the app owns under one Application Support folder,
// the same way the Linux side leans on XDG. `support_dir()/prefix` is the Wine
// prefix; `support_dir()/engine` is the self-provisioned CrossOver-lineage Wine
// build (the analogue of GE-Proton under Steam's compatibilitytools.d). The
// heavy runtime living here, outside the .app bundle, is what keeps the notarised
// bundle small and lets first launch download it.

/// `~/Library/Application Support/betterac` — the root of everything the Mac app
/// provisions for itself.
#[cfg(target_os = "macos")]
pub fn support_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("betterac")
}

/// The Wine prefix on macOS. Mirrors Linux's `default_prefix()` role.
#[cfg(target_os = "macos")]
pub fn default_prefix() -> PathBuf {
    support_dir().join("prefix")
}

/// Where the downloaded (or symlinked) Wine engine lives. The wine binary is
/// expected at `engine_dir()/bin/wine`.
#[cfg(target_os = "macos")]
pub fn engine_dir() -> PathBuf {
    support_dir().join("engine")
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

/// Newest GE-Proton 10. Deliberately not 11: it wants the steamrt4 runtime, which
/// umu-run does not provision -- it fetches steamrt3 and then dies looking for
/// steamrt4/toolmanifest.vdf. The aarch64 tarball is skipped; it is a real trap.
pub(crate) fn find_proton() -> Option<PathBuf> {
    let mut best: Option<PathBuf> = None;
    for e in std::fs::read_dir(steam_compat()).ok()?.flatten() {
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
            format!("No GE-Proton10 build in {}. Run setup first.", steam_compat().display())
        })?;
        Ok(Install { prefix: prefix.to_path_buf(), ac_dir, proton })
    }
}
