//! Finding and installing a newer betterAC.
//!
//! ## Finding one costs a single unauthenticated GET
//!
//! Every commit to `main` is released, and each release publishes `SHA256SUMS`
//! whose entries name the artifacts — and the artifact names carry the version:
//!
//! ```text
//! 155ff7…  betterac-2026.07.27.42-x86_64.tar.gz
//! f38dcc…  BetterAC-2026.07.27.42-universal.dmg
//! ```
//!
//! So fetching `releases/latest/download/SHA256SUMS` answers "what is the newest
//! version, what is it called, and what should it hash to" in one request. No
//! GitHub API, which means no rate limit, no token, and nothing to break when an
//! artifact is renamed. It is the same trick `install.sh` uses, for the same
//! reasons.
//!
//! ## Installing one depends on how this copy got here
//!
//! Replacing a Homebrew-managed install behind Homebrew's back leaves its metadata
//! describing a version that is no longer on disk, so [`Source`] is checked first
//! and a brew install is told to use `brew upgrade` instead. See [`install`] for
//! the two self-update paths and why each is shaped the way it is.

use crate::fetch;
use std::path::Path;

const REPO: &str = "haivk/betterAC";

/// Where `SHA256SUMS` and the artifacts hang off. Overridable so the whole path
/// can be exercised against a local directory (`file://…`) before trusting it
/// against the real thing — the same knob `install.sh` takes.
fn base_url() -> String {
    std::env::var("BETTERAC_BASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("https://github.com/{REPO}/releases/latest/download"))
}

/// A release newer than this build.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Release {
    pub version: String,
    /// The artifact for *this* platform, as named in `SHA256SUMS`.
    pub asset: String,
    pub sha256: String,
}

impl Release {
    pub fn url(&self) -> String {
        format!("{}/{}", base_url(), self.asset)
    }
}

/// How this copy of betterAC was installed, which decides whether it may replace
/// itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Installed by `install.sh` — ours to replace.
    SelfManaged,
    /// Installed by Homebrew. Updating means `brew upgrade`, not a self-replace:
    /// swapping the files underneath would leave brew's receipt describing a
    /// version that is no longer there.
    Homebrew,
}

/// Work out which. On macOS the app sits in `/Applications` either way, so the
/// path cannot answer it — Homebrew's Caskroom entry is what distinguishes a cask
/// install from a copied bundle.
pub fn source() -> Source {
    #[cfg(target_os = "macos")]
    {
        let prefixes = ["/opt/homebrew", "/usr/local"];
        if prefixes.iter().any(|p| Path::new(p).join("Caskroom/betterac").is_dir()) {
            return Source::Homebrew;
        }
    }
    Source::SelfManaged
}

/// Everything a settings panel needs to describe the update situation, in one
/// call — so the frontends do not each assemble it from three.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Status {
    /// The version running right now.
    pub current: String,
    /// The newer release, or `None` when this build is already the newest.
    pub available: Option<Release>,
    /// `"homebrew"` when this copy must be updated with `brew upgrade` instead of
    /// by us — the UI should say so rather than offering a button that refuses.
    pub source: String,
}

/// One round trip: what is running, what is available, and who owns updating it.
pub fn status() -> Result<Status, String> {
    Ok(Status {
        current: crate::VERSION.to_string(),
        available: check()?,
        source: match source() {
            Source::Homebrew => "homebrew".into(),
            Source::SelfManaged => "self".into(),
        },
    })
}

/// Check, and install if there is anything to install. `Ok(None)` means there was
/// not — already current.
///
/// The check is repeated here rather than trusting a `Release` the UI is holding,
/// so what gets installed is whatever is newest at the moment the button is
/// pressed, not whatever was newest when the panel opened.
pub fn update_now(on: &mut dyn FnMut(crate::setup::Progress)) -> Result<Option<Applied>, String> {
    match check()? {
        None => Ok(None),
        Some(release) => install(&release, on).map(Some),
    }
}

/// The newest release, or `None` when this build is already it.
///
/// Errors are the caller's to show or swallow: a launcher that cannot reach
/// GitHub should still launch the game.
pub fn check() -> Result<Option<Release>, String> {
    let sums = fetch::get_string(&format!("{}/SHA256SUMS", base_url()))?;
    let release = parse_sums(&sums).ok_or("no artifact for this platform in SHA256SUMS")?;
    Ok((is_newer(&release.version, crate::VERSION)).then_some(release))
}

/// The artifact this platform installs, pulled out of a `SHA256SUMS` body.
fn parse_sums(sums: &str) -> Option<Release> {
    // The tarball is Linux's, the disk image is the Mac's. Nothing else ships.
    let want_ext = if cfg!(target_os = "macos") { ".dmg" } else { ".tar.gz" };
    for line in sums.lines() {
        let mut parts = line.split_whitespace();
        let (Some(sha), Some(name)) = (parts.next(), parts.next()) else { continue };
        if !name.ends_with(want_ext) {
            continue;
        }
        return Some(Release {
            version: version_of(name)?,
            asset: name.to_string(),
            sha256: sha.to_string(),
        });
    }
    None
}

/// The version embedded in an artifact name: `betterac-2026.07.27.42-x86_64.tar.gz`
/// and `BetterAC-2026.07.27.42-universal.dmg` both yield `2026.07.27.42`.
///
/// Taken as everything between the first and last hyphen, which survives both
/// shapes without knowing either arch suffix.
fn version_of(asset: &str) -> Option<String> {
    let start = asset.find('-')? + 1;
    let end = asset.rfind('-')?;
    (start < end).then(|| asset[start..end].to_string())
}

/// Is `candidate` a newer version than `current`?
///
/// Compared component-wise and **numerically**, which string comparison cannot do:
/// released versions are `YYYY.MM.DD.<build>` and the build number is not padded,
/// so `2026.07.27.10` sorts *before* `2026.07.27.9` as text. A local build reads as
/// the workspace version (`0.1.0`) and so is older than every release, which is the
/// right answer — a dev build should be told a real release exists.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0)).collect()
    };
    let (a, b) = (parts(candidate), parts(current));
    for i in 0..a.len().max(b.len()) {
        // A missing component is zero: 2026.07.27 is older than 2026.07.27.1.
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// What the caller must do once an update has landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The new version is on disk; the running process is still the old one.
    /// Restarting picks it up whenever the user is ready.
    RestartWhenReady,
    /// A helper is waiting for this process to exit before it can swap the app.
    /// The caller must quit *now*, or nothing happens (macOS).
    QuitNow,
}

/// Download `release`, verify it, and put it in place.
///
/// Refuses a Homebrew install rather than fighting it. `on` reports download
/// progress; it is the same shape setup uses, so a frontend can render it with
/// what it already has.
pub fn install(
    release: &Release,
    on: &mut dyn FnMut(crate::setup::Progress),
) -> Result<Applied, String> {
    if source() == Source::Homebrew {
        return Err("This copy was installed by Homebrew. Update it with:\n    \
                    brew upgrade --cask betterac"
            .into());
    }

    let dir = std::env::temp_dir().join(format!("betterac-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let archive = dir.join(&release.asset);

    fetch::download(&release.url(), &archive, crate::setup::SetupStep::DownloadRuntime, on)?;
    // Refuse to install anything that is not bit-for-bit what the release says.
    fetch::verify_sha256(&archive, &release.sha256)?;

    let applied = apply(&archive, &dir);
    // On macOS the swap happens after we exit, so the staged copy has to outlive
    // this function; everything else is ours to clean up.
    if !matches!(applied, Ok(Applied::QuitNow)) {
        let _ = std::fs::remove_dir_all(&dir);
    }
    applied
}

// ------------------------------------------------------------------------ Linux

/// Replace the installed binary and its desktop files from the tarball.
///
/// The replacement is a **rename over the old path**, never a write into it. That
/// is what makes updating a *running* launcher safe: the rename drops the old
/// directory entry but the running process keeps the inode it is executing, so it
/// carries on untouched and simply runs the old code until it is restarted.
/// Writing into the file in place would corrupt it under itself.
#[cfg(not(target_os = "macos"))]
fn apply(archive: &Path, dir: &Path) -> Result<Applied, String> {
    let unpacked = dir.join("unpacked");
    fetch::extract_tar_gz(archive, &unpacked)?;

    // The tarball holds one top-level directory, named for the release.
    let root = std::fs::read_dir(&unpacked)
        .map_err(|e| format!("{}: {e}", unpacked.display()))?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .ok_or("the update archive is empty")?;

    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this binary: {e}"))?;
    let new_exe = root.join("betterac");
    if !new_exe.is_file() {
        return Err("the update archive has no betterac binary".into());
    }
    replace(&new_exe, &exe)?;

    // Best-effort: a failed icon refresh is not worth failing an update over, and
    // the binary -- the part that matters -- is already in place.
    if let Some(home) = dirs::home_dir() {
        let share = home.join(".local/share");
        let _ = replace(
            &root.join("data/betterac.desktop"),
            &share.join("applications/ac.betterac.BetterAC.desktop"),
        );
        let _ = replace(
            &root.join("data/betterac.svg"),
            &share.join("icons/hicolor/scalable/apps/ac.betterac.BetterAC.svg"),
        );
    }
    Ok(Applied::RestartWhenReady)
}

/// Copy `src` over `dst` atomically, preserving `src`'s mode.
///
/// Staged as a sibling first because [`std::fs::rename`] is only atomic within one
/// filesystem, and the download lives in `/tmp`, which frequently is not the one
/// `~/.local` is on.
#[cfg(not(target_os = "macos"))]
fn replace(src: &Path, dst: &Path) -> Result<(), String> {
    let parent = dst.parent().ok_or("no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;

    let mut staged = dst.as_os_str().to_os_string();
    staged.push(".new");
    let staged = std::path::PathBuf::from(staged);

    std::fs::copy(src, &staged).map_err(|e| format!("staging {}: {e}", dst.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(src) {
            let _ = std::fs::set_permissions(&staged, PermissionsExt::from_mode(meta.permissions().mode()));
        }
    }
    std::fs::rename(&staged, dst).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!("replacing {}: {e}", dst.display())
    })
}

// ------------------------------------------------------------------------ macOS

/// Stage the new `.app` and leave a detached helper to swap it in once this
/// process exits.
///
/// An application cannot replace its own bundle while it is running, so the swap
/// has to outlive us. The helper waits for this pid to disappear, moves the old
/// bundle aside, moves the new one in, deletes the old, and relaunches — moving
/// aside rather than deleting first so an interruption leaves a recoverable
/// `.app.old` instead of no application at all.
#[cfg(target_os = "macos")]
fn apply(archive: &Path, dir: &Path) -> Result<Applied, String> {
    use std::process::Command;

    let mount = dir.join("mnt");
    std::fs::create_dir_all(&mount).map_err(|e| e.to_string())?;
    let status = Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-quiet", "-mountpoint"])
        .arg(&mount)
        .arg(archive)
        .status()
        .map_err(|e| format!("hdiutil: {e}"))?;
    if !status.success() {
        return Err("could not mount the downloaded disk image".into());
    }

    // Copied off the image, because the image is unmounted before we quit.
    let staged = dir.join("BetterAC.app");
    let copy = Command::new("ditto").arg(mount.join("BetterAC.app")).arg(&staged).status();
    let _ = Command::new("hdiutil").arg("detach").arg(&mount).arg("-quiet").status();
    if !copy.map_err(|e| format!("ditto: {e}"))?.success() {
        return Err("could not copy the new app out of the disk image".into());
    }

    // The bundle this process is running from: <app>/Contents/MacOS/BetterAC.
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this app: {e}"))?;
    let dest = exe
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|p| p.extension().is_some_and(|e| e == "app"))
        .ok_or("this does not look like it is running from an .app bundle")?
        .to_path_buf();

    let script = dir.join("swap.sh");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/bash
# Wait for betterAC to quit, then swap the bundle and relaunch.
while kill -0 {pid} 2>/dev/null; do sleep 0.2; done
sleep 0.5
rm -rf {dest}.old
mv {dest} {dest}.old || exit 1
if mv {staged} {dest}; then
  rm -rf {dest}.old
else
  # Put it back rather than leaving the user with no application.
  mv {dest}.old {dest}
  exit 1
fi
rm -rf {dir}
open {dest}
"#,
            pid = std::process::id(),
            dest = shell_quote(&dest),
            staged = shell_quote(&staged),
            dir = shell_quote(dir),
        ),
    )
    .map_err(|e| format!("{}: {e}", script.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, PermissionsExt::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }

    Command::new("/bin/bash")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start the updater: {e}"))?;

    Ok(Applied::QuitNow)
}

/// Single-quote a path for the helper script. Paths here are ours, but
/// `/Applications` is not the only place an app can live and a space in the path
/// would otherwise split into two arguments.
#[cfg(target_os = "macos")]
fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_is_compared_numerically_not_as_text() {
        // The reason this function exists: the build number is not zero-padded, so
        // text comparison puts .10 before .9 and the tenth build of a day would
        // never be offered as an update.
        assert!(is_newer("2026.07.27.10", "2026.07.27.9"));
        assert!(!is_newer("2026.07.27.9", "2026.07.27.10"));

        assert!(is_newer("2026.07.28.1", "2026.07.27.99"));
        assert!(is_newer("2027.01.01.1", "2026.12.31.9"));
        assert!(!is_newer("2026.07.27.1", "2026.07.27.1"), "same version is not an update");
        assert!(!is_newer("2026.07.26.1", "2026.07.27.1"));

        // A dev build is older than any release, so a developer still gets told.
        assert!(is_newer("2026.07.27.1", "0.1.0"));
        // ...and a release is never "older" than a dev build in the other direction.
        assert!(!is_newer("0.1.0", "2026.07.27.1"));

        // A missing trailing component reads as zero.
        assert!(is_newer("2026.07.27.1", "2026.07.27"));
        assert!(!is_newer("2026.07.27", "2026.07.27.1"));
    }

    #[test]
    fn the_version_comes_out_of_the_artifact_name() {
        assert_eq!(version_of("betterac-2026.07.27.42-x86_64.tar.gz").unwrap(), "2026.07.27.42");
        assert_eq!(version_of("BetterAC-2026.07.27.42-universal.dmg").unwrap(), "2026.07.27.42");
        assert_eq!(version_of("betterac-0.1.0-x86_64.tar.gz").unwrap(), "0.1.0");
        assert_eq!(version_of("nohyphens.tar.gz"), None);
    }

    #[test]
    fn the_right_artifact_is_picked_for_this_platform() {
        let sums = "\
aaa  BetterAC-2026.07.27.42-universal.dmg
bbb  betterac-2026.07.27.42-x86_64.tar.gz
";
        let r = parse_sums(sums).expect("an artifact for this platform");
        assert_eq!(r.version, "2026.07.27.42");
        if cfg!(target_os = "macos") {
            assert_eq!(r.asset, "BetterAC-2026.07.27.42-universal.dmg");
            assert_eq!(r.sha256, "aaa");
        } else {
            assert_eq!(r.asset, "betterac-2026.07.27.42-x86_64.tar.gz");
            assert_eq!(r.sha256, "bbb");
        }
    }

    #[test]
    fn a_release_without_this_platforms_artifact_is_not_an_update() {
        // Exactly what an unsigned build publishes: a tarball and no DMG. The Mac
        // must report "nothing to install", not offer a Linux tarball.
        let linux_only = "bbb  betterac-2026.07.27.42-x86_64.tar.gz\n";
        assert_eq!(parse_sums(linux_only).is_some(), !cfg!(target_os = "macos"));
    }

    #[test]
    fn rubbish_sums_do_not_panic() {
        assert!(parse_sums("").is_none());
        assert!(parse_sums("garbage\n\n   \n").is_none());
        assert!(parse_sums("onlyonefield\n").is_none());
    }
}
