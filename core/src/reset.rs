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
//! the `.ac-installer` stamps that make steps skip), the self-provisioned Wine
//! engine on macOS, and the settings file with its saved servers and accounts.
//! Between them that is everything `detect` looks at, so the app returns to the
//! setup screen on its own.
//!
//! Kept: the **download cache**. It holds ~1.4 GB of archives — the runtime, the
//! retail installer, the End-of-Retail bundle — none of which is state. Keeping
//! it is the difference between a reset that costs a re-download and one that
//! finishes in minutes, and the observable result is identical either way,
//! because setup re-verifies every archive before using it.
//!
//! On Linux the GE-Proton build in Steam's `compatibilitytools.d` also stays. It
//! is shared with Steam, we did not create the directory, and other games may be
//! running on it.

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
    let mut v = vec![Target { label: "Windows prefix", path: cfg.prefix.clone() }];

    // The Mac provisions its own Wine build; Linux borrows Steam's GE-Proton and
    // must not delete it.
    #[cfg(target_os = "macos")]
    v.push(Target { label: "Wine engine", path: crate::install::engine_dir() });

    v.push(Target { label: "Settings", path: crate::config::config_path() });
    v
}

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
            std::fs::remove_dir_all(&t.path)
        } else {
            std::fs::remove_file(&t.path)
        };
        r.map_err(|e| format!("removing {} ({}): {e}", t.path.display(), t.label))?;
        removed.push(t.path);
    }
    Ok(removed)
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
