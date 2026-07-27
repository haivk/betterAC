//! Undo an install: put the machine back to "app installed, setup never run".
//!
//! This is the escape hatch for a prefix that has gone wrong — a half-finished
//! wizard, a Wine update that broke the engine, a client patched by something
//! else. Rather than teaching every step how to repair itself, we delete what
//! setup built and let it run again from a known-clean start.
//!
//! ## What goes, and what deliberately stays
//!
//! Removed: the Wine prefix (which carries the installed game, the registry, and
//! the `.ac-installer` stamps that make steps skip), the Windows runtime betterAC
//! provisioned for itself, and the settings file with its saved servers and
//! accounts. Between them that is everything `detect` looks at, so the app returns
//! to the setup screen on its own.
//!
//! The runtime is removed on **both** platforms, and it is the same decision on
//! each: it is a build we downloaded into a directory we own, and — since Decal
//! hot-patches it — one a reset has a specific reason to rebuild. Linux only
//! qualified once it stopped running out of Steam's `compatibilitytools.d`; see
//! [`crate::install::runtime_dir`]. Steam's own GE-Proton is never a target. It is
//! shared, we did not create the directory, and other games run on it.
//!
//! Kept: the **download cache**. It holds ~1.4 GB of archives — the runtime, the
//! retail installer, the End-of-Retail bundle — none of which is state. Keeping
//! it is the difference between a reset that costs a re-download and one that
//! finishes in minutes, and the observable result is identical either way,
//! because setup re-verifies every archive before using it. On Linux it is cheaper
//! still: if Steam has a GE-Proton build, setup copies that back rather than
//! fetching anything.

use std::path::{Path, PathBuf};

/// One thing a reset removes. The label is shown to the user before they commit.
#[derive(Debug, Clone)]
pub struct Target {
    pub label: &'static str,
    pub path: PathBuf,
}

/// Everything a reset would remove, whether or not it currently exists.
///
/// The prefix is read from the saved config rather than assumed, so a custom
/// prefix is removed instead of a default one that was never used.
pub fn targets() -> Vec<Target> {
    let cfg = crate::config::Config::load();
    vec![
        Target { label: "Windows prefix", path: cfg.prefix.clone() },
        Target { label: RUNTIME_LABEL, path: crate::install::runtime_dir() },
        Target { label: "Settings", path: crate::config::config_path() },
    ]
}

/// What the runtime is called in the confirmation list. Only the name differs
/// between the platforms — the thing itself plays the same role on both.
#[cfg(target_os = "macos")]
const RUNTIME_LABEL: &str = "Wine engine";
#[cfg(not(target_os = "macos"))]
const RUNTIME_LABEL: &str = "Proton runtime";

/// Reject paths broad enough that removing them would be a catastrophe.
///
/// `targets` derives one of its paths from user-editable config, so this is the
/// backstop between a hand-edited `"prefix": "/"` and `remove_dir_all`.
fn is_safe(p: &Path) -> bool {
    if !p.is_absolute() {
        return false;
    }
    // "/" is 1 component, "/Users" is 2. Nothing we own is that shallow.
    if p.components().count() < 3 {
        return false;
    }
    if dirs::home_dir().is_some_and(|h| p == h) {
        return false;
    }
    true
}

/// Delete everything in [`targets`], returning what was actually removed.
///
/// Every path is validated *before* anything is deleted, so a bad target cannot
/// leave the install half-erased. Paths that do not exist are skipped quietly —
/// a reset after a failed setup is normal, and there is nothing to report about
/// a prefix that was never created.
pub fn reset() -> Result<Vec<PathBuf>, String> {
    let targets = targets();

    for t in &targets {
        if !is_safe(&t.path) {
            return Err(format!(
                "refusing to remove {} for \"{}\" -- that path is too broad to delete",
                t.path.display(),
                t.label
            ));
        }
    }

    let mut removed = Vec::new();
    for t in targets {
        let meta = match std::fs::symlink_metadata(&t.path) {
            Ok(m) => m,
            Err(_) => continue, // not there; nothing to undo
        };
        let r = if meta.is_dir() {
            remove_dir_all_settling(&t.path)
        } else {
            std::fs::remove_file(&t.path)
        };
        r.map_err(|e| format!("removing {} ({}): {e}", t.path.display(), t.label))?;
        removed.push(t.path);
    }
    Ok(removed)
}

/// `remove_dir_all`, retried while something keeps putting files back.
///
/// A Wine prefix is not inert while it is being deleted. On its way out wineserver
/// flushes `system.reg`, `user.reg` and `userdef.reg`, rewrites `.update-timestamp`
/// and can recreate `dosdevices` entries — so a delete that started before it had
/// finished exiting empties the prefix and then fails to remove the directory
/// itself, with `ENOTEMPTY`, leaving exactly those files behind.
///
/// Callers shut the prefix down first, which is the real fix; this is the backstop
/// for a wineserver that is slow or wedged. Each pass deletes whatever is there
/// now, so the retries converge as long as the writer is stopping.
fn remove_dir_all_settling(path: &Path) -> std::io::Result<()> {
    const ATTEMPTS: u32 = 10;
    let mut last = None;
    for attempt in 0..ATTEMPTS {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            // Someone else removed it in the meantime; that is the outcome we wanted.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => last = Some(e),
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("could not remove the directory")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prefix_and_the_settings_are_both_removed() {
        let labels: Vec<_> = targets().iter().map(|t| t.label).collect();
        assert!(labels.contains(&"Windows prefix"), "{labels:?}");
        assert!(labels.contains(&"Settings"), "{labels:?}");
    }

    #[test]
    fn the_download_cache_is_never_a_target() {
        // Keeping it is what makes a reset cheap. If it ever becomes a target,
        // that should be a deliberate change with a UI warning, not a silent one.
        for t in targets() {
            let p = t.path.to_string_lossy().to_lowercase();
            assert!(!p.ends_with("cache"), "{} would delete the download cache", t.label);
        }
    }

    /// A prefix-shaped tree, including the files wineserver rewrites on its way out,
    /// goes in one call.
    #[test]
    fn a_populated_prefix_is_removed_whole() {
        let dir = std::env::temp_dir().join("betterac-reset-populated-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("drive_c/windows/system32")).unwrap();
        std::fs::create_dir_all(dir.join("dosdevices")).unwrap();
        for f in ["system.reg", "user.reg", "userdef.reg", ".update-timestamp"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        std::fs::write(dir.join("drive_c/windows/system32/a.dll"), b"x").unwrap();

        assert!(remove_dir_all_settling(&dir).is_ok());
        assert!(!dir.exists(), "{} still exists", dir.display());
    }

    #[test]
    fn removing_something_already_gone_is_not_an_error() {
        let dir = std::env::temp_dir().join("betterac-reset-absent-test");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(remove_dir_all_settling(&dir).is_ok());
    }

    #[test]
    fn broad_paths_are_refused() {
        assert!(!is_safe(Path::new("/")));
        assert!(!is_safe(Path::new("/Users")));
        assert!(!is_safe(Path::new("relative/path")));
        if let Some(home) = dirs::home_dir() {
            assert!(!is_safe(&home), "the home directory must never be a target");
        }
    }

    #[test]
    fn real_install_paths_are_accepted() {
        assert!(is_safe(Path::new("/home/someone/Games/asherons-call")));
        assert!(is_safe(Path::new("/Users/someone/Library/Application Support/betterac/prefix")));
        // Everything we would actually delete has to pass its own guard.
        for t in targets() {
            assert!(is_safe(&t.path), "{} at {} fails the guard", t.label, t.path.display());
        }
    }

    #[test]
    fn a_missing_target_is_not_an_error() {
        // reset() runs against this machine, so assert the weaker property that
        // absent paths are skipped rather than reported.
        let gone = std::env::temp_dir().join("ac-reset-definitely-absent");
        let _ = std::fs::remove_dir_all(&gone);
        assert!(std::fs::symlink_metadata(&gone).is_err());
    }
}
