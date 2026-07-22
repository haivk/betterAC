//! AC's `UserPreferences.ini` — the fix for the widescreen stretch.
//!
//! AC reads its render resolution from `UserPreferences.ini`. With no such file it
//! falls back to a resolution that rarely matches the panel, and whatever is
//! scaling the output then stretches the result to fill the screen. On an
//! ultrawide that is glaring.
//!
//! The cause and the cure are identical on both platforms even though the thing
//! doing the stretching is not — the Mac Wine driver on macOS, gamescope on Linux
//! — so this lives here rather than in `wine` or `proton`. Both call
//! [`apply`] with the display resolution their frontend detected, which pins AC's
//! own resolution to the display; from there neither scaler has anything to
//! stretch.
//!
//! Writes are non-destructive: only `Resolution` and `FullScreen` in `[Display]`
//! are managed, and every other line of the user's file survives (see
//! [`merge_display_ini`]).

use std::path::{Path, PathBuf};

/// Pin AC's resolution for the next launch. Writes `UserPreferences.ini` to every
/// location the client might read it from.
///
/// `fullscreen` is what AC's own `FullScreen` key is set to, which is not always
/// "did the user ask for fullscreen": inside a Wine virtual desktop AC has to be
/// windowed so it fills the desktop window instead of fighting it.
///
/// Enforced on every launch rather than written once, so the mode always reflects
/// what betterAC decided rather than whatever AC last saved for itself.
pub fn apply(ac_dir: &Path, prefix: &Path, (w, h): (i32, i32), fullscreen: bool) {
    for path in prefs_paths(ac_dir, prefix) {
        write_display_prefs(&path, (w, h), fullscreen);
    }
}

/// The files AC might read its preferences from: the game directory and
/// `<My Documents>\Asheron's Call` (clients differ on which they use).
pub fn prefs_paths(ac_dir: &Path, prefix: &Path) -> Vec<PathBuf> {
    let mut paths = vec![ac_dir.join("UserPreferences.ini")];
    paths.extend(ac_documents_dirs(prefix).into_iter().map(|d| d.join("UserPreferences.ini")));
    paths
}

/// Every `<prefix>/drive_c/users/<user>/Documents/Asheron's Call` worth writing to.
///
/// Deliberately not a single "best guess" keyed on `$USER`. **Proton always runs
/// the client as `steamuser`, whatever the login name is**, so a prefix built by
/// the Linux path typically has both a `steamuser` profile (the one AC actually
/// uses) and one named after the real user (created by Wine, unused). Guessing
/// `$USER` first picks the empty one and the resolution fix silently does nothing.
///
/// So: once AC has run, it has created exactly one of these, and that is the one
/// we write — no guessing, and no littering the profiles it does not use. Before
/// AC has ever run there is nothing to go on, so seed them all.
pub fn ac_documents_dirs(prefix: &Path) -> Vec<PathBuf> {
    let users = prefix.join("drive_c/users");
    let Ok(entries) = std::fs::read_dir(&users) else { return Vec::new() };

    let all: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n != "Public"))
        .map(|p| p.join("Documents").join("Asheron's Call"))
        .collect();

    let existing: Vec<PathBuf> = all.iter().filter(|p| p.is_dir()).cloned().collect();
    if existing.is_empty() {
        all
    } else {
        existing
    }
}

/// Enforce AC's `[Display]` Resolution + FullScreen at `path`, preserving every
/// other line so the user's other in-game settings survive. Creates the file (and
/// parent dirs) if absent.
pub fn write_display_prefs(path: &Path, res: (i32, i32), fullscreen: bool) {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let merged = merge_display_ini(&existing, res, fullscreen);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, merged);
}

/// `BETTERAC_RESOLUTION=WxH`, parsed. `None` if unset or malformed.
pub fn env_resolution() -> Option<(i32, i32)> {
    let raw = std::env::var("BETTERAC_RESOLUTION").ok()?;
    let (w, h) = raw.trim().split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// True when an env var is set to something other than empty/"0".
pub fn env_flag(key: &str) -> bool {
    std::env::var(key).ok().is_some_and(|v| !v.is_empty() && v != "0")
}

/// Return `existing` with `Resolution` and `FullScreen` in its `[Display]` section
/// set to the given values, every other line untouched. Adds the keys to an
/// existing `[Display]`, or appends a fresh `[Display]` section if there is none.
/// Emits CRLF, as the Windows client expects.
pub fn merge_display_ini(existing: &str, (w, h): (i32, i32), fullscreen: bool) -> String {
    let res_line = format!("Resolution={w}x{h}");
    let fs_line = format!("FullScreen={}", if fullscreen { "True" } else { "False" });
    let key_of =
        |line: &str| line.trim().split('=').next().unwrap_or("").trim().to_ascii_lowercase();

    let mut out: Vec<String> = Vec::new();
    let (mut in_display, mut seen_display, mut set_res, mut set_fs) = (false, false, false, false);

    for raw in existing.lines() {
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_display {
                // Leaving [Display]: append any managed key we didn't overwrite.
                if !set_res { out.push(res_line.clone()); }
                if !set_fs { out.push(fs_line.clone()); }
            }
            in_display = trimmed.eq_ignore_ascii_case("[Display]");
            seen_display |= in_display;
            out.push(line.to_string());
            continue;
        }
        if in_display {
            match key_of(line).as_str() {
                "resolution" => { out.push(res_line.clone()); set_res = true; continue; }
                "fullscreen" => { out.push(fs_line.clone()); set_fs = true; continue; }
                _ => {}
            }
        }
        out.push(line.to_string());
    }
    if in_display {
        if !set_res { out.push(res_line.clone()); }
        if !set_fs { out.push(fs_line.clone()); }
    }
    if !seen_display {
        if out.last().is_some_and(|l| !l.is_empty()) {
            out.push(String::new());
        }
        out.extend([
            "[Display]".into(),
            "RefreshRate=Auto".into(),
            res_line,
            fs_line,
            "SyncToRefresh=False".into(),
        ]);
    }
    let mut joined = out.join("\r\n");
    joined.push_str("\r\n");
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_into_an_empty_file_creates_a_display_section() {
        let out = merge_display_ini("", (3360, 1418), true);
        assert!(out.contains("[Display]"));
        assert!(out.contains("Resolution=3360x1418"));
        assert!(out.contains("FullScreen=True"));
        assert!(out.ends_with("\r\n"), "the Windows client expects CRLF");
    }

    #[test]
    fn merge_updates_existing_keys_and_preserves_the_rest() {
        let existing = "[Display]\r\nRefreshRate=Auto\r\nResolution=1024x768\r\nFullScreen=False\r\n\
                        [Rendering]\r\nTexFilter=2\r\n";
        let out = merge_display_ini(existing, (2560, 1440), true);
        assert!(out.contains("Resolution=2560x1440"));
        assert!(out.contains("FullScreen=True"));
        assert!(!out.contains("1024x768"), "old resolution must be gone");
        assert!(!out.contains("FullScreen=False"), "old fullscreen must be gone");
        // Untouched settings survive.
        assert!(out.contains("[Rendering]"));
        assert!(out.contains("TexFilter=2"));
        assert!(out.contains("RefreshRate=Auto"));
        // Each managed key appears exactly once.
        assert_eq!(out.matches("FullScreen=").count(), 1);
        assert_eq!(out.matches("Resolution=").count(), 1);
    }

    #[test]
    fn merge_adds_missing_keys_to_an_existing_display_section() {
        // A [Display] section that has neither key yet, followed by another section.
        let existing = "[Display]\r\nRefreshRate=Auto\r\n[Sound]\r\nVolume=10\r\n";
        let out = merge_display_ini(existing, (1920, 1080), false);
        assert!(out.contains("Resolution=1920x1080"));
        assert!(out.contains("FullScreen=False"));
        assert!(out.contains("[Sound]") && out.contains("Volume=10"));
        // The keys landed inside [Display], before [Sound].
        let disp = out.find("Resolution=1920x1080").unwrap();
        let sound = out.find("[Sound]").unwrap();
        assert!(disp < sound, "managed keys must stay in the [Display] section");
    }

    /// The bug this guards: on Bazzite the prefix has both a `steamuser` profile
    /// (where Proton actually runs AC, and where the real ini lives) and one named
    /// after the login user. Picking by `$USER` writes to the wrong one and the
    /// widescreen fix does nothing at all.
    #[test]
    fn the_profile_ac_actually_uses_wins_over_the_one_named_after_the_user() {
        let tmp = std::env::temp_dir().join(format!("acprefs-{}", std::process::id()));
        let users = tmp.join("drive_c/users");
        // Both profiles exist, but only steamuser has AC's Documents folder.
        std::fs::create_dir_all(users.join("bazzite")).unwrap();
        std::fs::create_dir_all(users.join("Public")).unwrap();
        std::fs::create_dir_all(users.join("steamuser/Documents/Asheron's Call")).unwrap();

        let dirs = ac_documents_dirs(&tmp);
        assert_eq!(dirs.len(), 1, "only the profile AC has actually used: {dirs:?}");
        assert!(dirs[0].ends_with("steamuser/Documents/Asheron's Call"), "{:?}", dirs[0]);

        // Nothing has run yet: seed every candidate rather than guess wrong.
        let fresh = tmp.join("fresh");
        std::fs::create_dir_all(fresh.join("drive_c/users/steamuser")).unwrap();
        std::fs::create_dir_all(fresh.join("drive_c/users/bazzite")).unwrap();
        std::fs::create_dir_all(fresh.join("drive_c/users/Public")).unwrap();
        let seeded = ac_documents_dirs(&fresh);
        assert_eq!(seeded.len(), 2, "Public is never a profile: {seeded:?}");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_forced_resolution_is_parsed_both_ways_round() {
        // Guards the parser, not the env: BETTERAC_RESOLUTION is process-global and
        // setting it here would race every other test in the binary.
        for (raw, want) in [("3440x1440", (3440, 1440)), ("2560X1080", (2560, 1080))] {
            let (w, h) = raw.trim().split_once(['x', 'X']).unwrap();
            assert_eq!((w.parse::<i32>().unwrap(), h.parse::<i32>().unwrap()), want);
        }
    }
}
