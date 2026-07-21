//! Locating the two Asheron's Call game files — shared by both platform runtimes.
//!
//! The retail installer and the End-of-Retail update bundle are the same files
//! whether we run them under Proton (Linux) or Wine (macOS), and they come from
//! the same places, so the resolution logic lives here once rather than being
//! copied into each runtime.
//!
//! There is no single canonical host for these, so the defaults point at public
//! archives: the retail `ac1install.exe` off the Wayback Machine (Turbine's
//! original CDN is long gone) and `ac-updates.zip` off the Internet Archive. Both
//! can be overridden by environment for testing or self-hosting.

use crate::fetch::{download, find_in_dir};
use crate::setup::{Progress, SetupStep};
use std::path::{Path, PathBuf};

/// Retail client installer (~571 MB), from the Wayback Machine.
pub const DEFAULT_INSTALLER_URL: &str =
    "https://web.archive.org/web/20201121104423/http://content.turbine.com/sites/clientdl/ac1/ac1install.exe";

/// End-of-Retail data + patched client (~484 MB), from the Internet Archive.
pub const DEFAULT_UPDATES_URL: &str = "https://archive.org/download/ac-updates/ac-updates.zip";

/// Where the two files come from, with sensible public defaults.
pub struct GameSources {
    /// A local folder that already holds both files (`AC_SRC`). Checked first, so
    /// a dev never re-downloads a gigabyte to test.
    pub src: Option<PathBuf>,
    /// Full URL to the retail installer (`AC_INSTALLER_URL`, else the default).
    pub installer_url: String,
    /// Full URL to the update bundle (`AC_UPDATES_URL`, else the default).
    pub updates_url: String,
}

impl GameSources {
    pub fn from_env() -> GameSources {
        GameSources {
            src: std::env::var_os("AC_SRC").map(PathBuf::from),
            installer_url: env_or("AC_INSTALLER_URL", DEFAULT_INSTALLER_URL),
            updates_url: env_or("AC_UPDATES_URL", DEFAULT_UPDATES_URL),
        }
    }

    /// Where the retail installer is (or will be), without fetching anything: the
    /// `AC_SRC` copy if there is one, else its place in the cache.
    pub fn installer_path(&self, cache: &Path) -> PathBuf {
        self.local(&["ac1install"], "exe").unwrap_or_else(|| cache.join("ac1install.exe"))
    }

    /// Where the update bundle is (or will be). See [`GameSources::installer_path`].
    pub fn updates_path(&self, cache: &Path) -> PathBuf {
        self.local(&["ac-updates", "ac_data", "acupdate"], "zip")
            .unwrap_or_else(|| cache.join("ac-updates.zip"))
    }

    /// The retail installer, downloaded into `cache` if it is neither in `src` nor
    /// already there. Its own setup step, because it is 571 MB and deserves its own
    /// progress bar rather than sharing one with the update bundle.
    pub fn fetch_installer(
        &self,
        cache: &Path,
        on: &mut dyn FnMut(Progress),
    ) -> Result<PathBuf, String> {
        self.fetch(self.installer_path(cache), &self.installer_url, SetupStep::DownloadClient, on)
    }

    /// The End-of-Retail bundle, same deal — 484 MB, its own step.
    pub fn fetch_updates(
        &self,
        cache: &Path,
        on: &mut dyn FnMut(Progress),
    ) -> Result<PathBuf, String> {
        self.fetch(self.updates_path(cache), &self.updates_url, SetupStep::DownloadUpdates, on)
    }

    /// A file in the local `src` folder, by the name prefixes the shell installer
    /// matched.
    fn local(&self, prefixes: &[&str], ext: &str) -> Option<PathBuf> {
        find_in_dir(self.src.as_deref()?, prefixes, ext)
    }

    /// Fetch one file, or report why we didn't have to. Idempotent by the presence
    /// of `path`, so a setup that fails later resumes without re-downloading a
    /// gigabyte.
    fn fetch(
        &self,
        path: PathBuf,
        url: &str,
        step: SetupStep,
        on: &mut dyn FnMut(Progress),
    ) -> Result<PathBuf, String> {
        if path.exists() {
            on(Progress::skipped(step, format!("already have {}", path.display())));
            return Ok(path);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        download(url, &path, step, on)?;
        Ok(path)
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}
