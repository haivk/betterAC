//! Running acclient.exe under Wine on macOS (Apple Silicon).
//!
//! The Linux sibling of this file is `proton.rs`; the shape is deliberately the
//! same — a `Runtime` that walks the shared `SetupStep` sequence and a `launch`
//! that spawns the client — but the runtime underneath is different and so are a
//! handful of hard-won specifics, every one of which comes straight out of the
//! Step Zero smoke test (see STEP-ZERO-whisky-smoketest.md):
//!
//!   * **The engine is a CrossOver-lineage Wine build, not Proton.** Unlike
//!     Proton's bin/wine (which only works inside the Steam runtime container and
//!     so must go through umu-run), this wine binary is invoked **directly, by
//!     full path**. We self-provision it under `~/Library/Application Support/
//!     betterac/engine`, the macOS analogue of GE-Proton under compatibilitytools.d.
//!   * **AC is a 32-bit x86 D3D9 game from 1999**, and the engine that runs it is
//!     an x86_64 build (wine32on64: 32-bit Windows code inside a 64-bit Mac
//!     process). On Apple Silicon that whole stack runs under Rosetta 2, so the
//!     Dependencies step ensures Rosetta; on an Intel Mac it is native and that
//!     step is a no-op. See [`NEEDS_ROSETTA`] -- it is the only architecture
//!     difference in this file.
//!   * **Graphics backend is builtin d3d9 (wined3d), NOT DXVK.** Step Zero proved
//!     AC renders on wined3d and that DXVK never engaged; so there is no DXVK
//!     download, no MoltenVK, no Vulkan. `WINEDLLOVERRIDES=d3d9=b` just picks the
//!     engine's builtin d3d9. This is the macOS mirror of Linux's
//!     `PROTON_USE_WINED3D=1`.
//!   * **No winetricks.** Step Zero ran without vcrun2019 / VC++2005: the
//!     msvcr70/msvcp70/zlib1 the patched client needs ship inside ac-updates.zip.
//!     So the Components step is a no-op here, kept only for UI parity.
//!   * **The prefix is set to Windows 7**, which is the version the smoke test bottle
//!     ran as.
//!
//! Display-resolution handling (the widescreen-stretch fix) is a known follow-up:
//! `launch` takes a `res` for signature parity with Proton but does not yet use it.

use crate::args::{client_args, validate};
use crate::fetch::{download, extract_tar_gz, extract_zip, verify_sha256};
use crate::gamefiles::GameSources;
use crate::install::{engine_dir, find_acclient, find_game_dir, Install};
use crate::servers::Server;
use crate::setup::{is_stamped, mark_stamped, Progress, Runtime, SetupStep};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// The Wine engine we self-provision: the CrossOver-lineage build from the active
/// Whisky fork (`frankea/Whisky`), the exact lineage Step Zero validated AC on.
/// Pinned to a known version + hash for a deterministic, verifiable install;
/// override with `AC_WINE_ENGINE_URL` (which skips the hash check, since the hash
/// is only known for this build). The tarball unpacks to `Libraries/Wine/bin/…`.
const DEFAULT_ENGINE_URL: &str =
    "https://github.com/frankea/Whisky/releases/download/v3.1.1/Libraries.tar.gz";
const DEFAULT_ENGINE_SHA256: &str =
    "01f3a1b43b98065fe20c529c1023b61dd79a6d2ad93bba6040865f646481ccf3";

/// The macOS runtime: a self-provisioned CrossOver-lineage Wine engine running
/// the 32-bit client under Rosetta 2. Owns the paths and sources setup needs and
/// implements `Runtime` so the SwiftUI frontend drives it blind, exactly as the
/// GTK frontend drives `ProtonRuntime`.
pub struct WineRuntime {
    /// The Wine prefix to build the game into.
    pub prefix: PathBuf,
    /// Where downloads are cached between runs (the engine tarball, and the game
    /// files when fetched rather than found locally).
    pub cache: PathBuf,
    /// An existing engine to use in place instead of downloading, from
    /// `AC_WINE_ENGINE`. May be an engine root (containing `bin/wine`) or the wine
    /// binary itself. This is how a dev reuses an already-installed engine (e.g.
    /// the frankea Whisky fork's `Libraries/Wine`) without any download.
    pub engine_override: Option<PathBuf>,
    /// Tarball URL the engine is downloaded from when not overridden. Defaults to
    /// [`DEFAULT_ENGINE_URL`]; override with `AC_WINE_ENGINE_URL`.
    pub engine_url: String,
    /// Expected SHA-256 of the engine tarball, verified before unpacking. `Some`
    /// only for the pinned default (we don't know the hash of a custom URL).
    pub engine_sha256: Option<String>,
    /// Where the two game files come from (local dir or the public archive URLs).
    pub games: GameSources,
}

impl WineRuntime {
    /// Defaults: cache under the app's Application Support folder; the engine and
    /// game files self-provision from public sources unless the environment points
    /// them elsewhere, so a fresh install needs no configuration at all.
    pub fn new(prefix: PathBuf) -> WineRuntime {
        let cache = crate::install::support_dir().join("cache");
        // A custom engine URL skips the hash check (its hash is unknown); the
        // pinned default is verified.
        let (engine_url, engine_sha256) =
            match std::env::var("AC_WINE_ENGINE_URL").ok().filter(|s| !s.trim().is_empty()) {
                Some(url) => (url, None),
                None => (DEFAULT_ENGINE_URL.to_string(), Some(DEFAULT_ENGINE_SHA256.to_string())),
            };
        WineRuntime {
            prefix,
            cache,
            engine_override: std::env::var_os("AC_WINE_ENGINE").map(PathBuf::from),
            engine_url,
            engine_sha256,
            games: GameSources::from_env(),
        }
    }

    /// The engine root to look inside for `bin/wine`. The `AC_WINE_ENGINE` override
    /// wins; otherwise the self-provisioned `engine_dir()`.
    fn engine_root(&self) -> PathBuf {
        self.engine_override.clone().unwrap_or_else(engine_dir)
    }

    /// Locate the wine binary, or `None` if the engine isn't present yet. If the
    /// override points straight at an executable file, that file is used as-is;
    /// otherwise we search the engine root for a `bin/wine`, tolerating nesting
    /// (the WhiskyWine tarball unpacks to `Libraries/Wine/bin/wine`).
    fn wine_bin(&self) -> Option<PathBuf> {
        if let Some(over) = &self.engine_override {
            if over.is_file() {
                return Some(over.clone());
            }
        }
        find_wine_bin(&self.engine_root())
    }

    /// A wine command carrying the prefix env every Windows-side call needs. The
    /// binary is always the full path — the engine is never assumed to be on PATH
    /// (Step Zero found the Whisky fork's shell even aliases `wine` to something
    /// broken), so we never rely on it.
    fn wine(&self, wine_bin: &Path) -> Command {
        let mut c = Command::new(wine_bin);
        c.env("WINEPREFIX", &self.prefix).env("WINEDEBUG", "-all");
        c
    }

    fn step_dependencies(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        // On an Intel Mac the engine is already native code, so there is no
        // translation layer to install and nothing to check.
        if !NEEDS_ROSETTA {
            on(Progress::skipped(SetupStep::Dependencies, "this Mac runs x86 code natively"));
            return Ok(());
        }
        // AC is 32-bit x86; on Apple Silicon that needs Rosetta 2. Unlike Bazzite's
        // atomic host tools, this is one we *can* install.
        if rosetta_present() {
            on(Progress::skipped(SetupStep::Dependencies, "Rosetta 2 is already installed"));
            return Ok(());
        }
        on(Progress::new(SetupStep::Dependencies, 0.3, "installing Rosetta 2 (x86 translation)…"));
        let status = Command::new("softwareupdate")
            .args(["--install-rosetta", "--agree-to-license"])
            .status();
        match status {
            Ok(s) if s.success() && rosetta_present() => Ok(()),
            _ => Err("Rosetta 2 is required to run the 32-bit x86 client but could not be \
                      installed automatically. Install it by hand with:\n  \
                      softwareupdate --install-rosetta --agree-to-license"
                .into()),
        }
    }

    /// Where the engine tarball lands. Named from the URL so a custom
    /// `AC_WINE_ENGINE_URL` doesn't collide with the pinned default in the cache.
    fn engine_tarball(&self) -> PathBuf {
        let name = self.engine_url.rsplit('/').next().unwrap_or("wine-engine.tar.gz");
        self.cache.join(name)
    }

    fn step_download_runtime(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        // Already have a usable engine (unpacked before, or pointed at via the
        // override)? Then there is nothing to fetch.
        if self.wine_bin().is_some() {
            on(Progress::skipped(
                SetupStep::DownloadRuntime,
                "the Wine engine is already installed",
            ));
            return Ok(());
        }
        let tarball = self.engine_tarball();
        if tarball.exists() {
            on(Progress::skipped(SetupStep::DownloadRuntime, "already downloaded"));
            return Ok(());
        }
        std::fs::create_dir_all(&self.cache).map_err(|e| e.to_string())?;
        download(&self.engine_url, &tarball, SetupStep::DownloadRuntime, on)
    }

    fn step_download_client(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        self.games.fetch_installer(&self.cache, on).map(|_| ())
    }

    fn step_download_updates(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        self.games.fetch_updates(&self.cache, on).map(|_| ())
    }

    fn step_install_runtime(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if self.wine_bin().is_some() {
            on(Progress::skipped(SetupStep::InstallRuntime, "already installed"));
            return Ok(());
        }
        let tarball = self.engine_tarball();
        if !tarball.exists() {
            return Err(format!("the Wine engine was not downloaded to {}", tarball.display()));
        }
        // Verify integrity before unpacking a third of a gigabyte. On mismatch,
        // drop the bad file so the next run re-downloads rather than looping on it.
        if let Some(expected) = &self.engine_sha256 {
            on(Progress::new(SetupStep::InstallRuntime, 0.1, "verifying the download…"));
            if let Err(e) = verify_sha256(&tarball, expected) {
                let _ = std::fs::remove_file(&tarball);
                return Err(e);
            }
        }
        on(Progress::new(SetupStep::InstallRuntime, 0.4, "unpacking the Wine engine…"));
        let dest = engine_dir();
        std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        extract_tar_gz(&tarball, &dest)?;
        // Gatekeeper quarantines everything downloaded; strip it so the engine's
        // binaries and dylibs are allowed to run without a per-file prompt.
        on(Progress::new(SetupStep::InstallRuntime, 0.9, "clearing the quarantine flag…"));
        let _ = Command::new("xattr").args(["-dr", "com.apple.quarantine"]).arg(&dest).status();
        self.wine_bin().ok_or("engine unpacked but no bin/wine was found inside it")?;
        mark_stamped(&self.prefix, SetupStep::InstallRuntime).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn step_prefix(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if is_stamped(&self.prefix, SetupStep::Prefix) {
            on(Progress::skipped(SetupStep::Prefix, "the prefix already exists"));
            return Ok(());
        }
        let wine = self.wine_bin().ok_or("no Wine engine available to create the prefix")?;
        std::fs::create_dir_all(&self.prefix).map_err(|e| e.to_string())?;
        on(Progress::new(SetupStep::Prefix, 0.2, "initialising the Wine prefix (first run is slow)…"));
        let _ = self.wine(&wine).args(["wineboot", "--init"]).status();
        if !self.prefix.join("drive_c").is_dir() {
            return Err(format!(
                "prefix creation failed -- no drive_c at {}",
                self.prefix.join("drive_c").display()
            ));
        }
        // Pin the prefix to Windows 7, the version the Step Zero bottle ran as.
        // Setting HKCU\Software\Wine\Version is exactly what winecfg's global
        // version dropdown writes.
        on(Progress::new(SetupStep::Prefix, 0.8, "setting the Windows version to 7…"));
        let _ = self
            .wine(&wine)
            .args(["reg", "add", r"HKCU\Software\Wine", "/v", "Version", "/t", "REG_SZ", "/d", "win7", "/f"])
            .status();
        // Keep the profile inside the prefix before anything (the installer, the
        // client, us) writes to Documents/Desktop and reaches the real home.
        on(Progress::new(SetupStep::Prefix, 0.9, "containing the Wine user profile…"));
        contain_user_profile(&self.prefix);
        mark_stamped(&self.prefix, SetupStep::Prefix).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn step_components(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        // Deliberately empty on macOS. Step Zero confirmed AC runs without
        // vcrun2019 / VC++2005: the msvcr70/msvcp70/zlib1 the patched client
        // imports ship inside ac-updates.zip and land in the game dir at the
        // ApplyUpdates step. Kept as a step (and stamped) purely so both platforms
        // walk the same sequence and the frontend UI matches.
        on(Progress::skipped(SetupStep::Components, "not needed on macOS"));
        mark_stamped(&self.prefix, SetupStep::Components).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn step_install_client(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if is_stamped(&self.prefix, SetupStep::InstallClient) {
            on(Progress::skipped(SetupStep::InstallClient, "the client is already installed"));
            return Ok(());
        }
        let wine = self.wine_bin().ok_or("no Wine engine available")?;
        let installer = self.games.fetch_installer(&self.cache, &mut |_| {})?;
        on(Progress::new(
            SetupStep::InstallClient,
            0.1,
            "the Asheron's Call installer is open — click through it and accept the default path",
        ));
        // The real wizard, drawn by Wine's Mac driver. Non-zero exit is tolerated;
        // we verify success by the data file it drops.
        let _ = self.wine(&wine).arg(&installer).status();
        find_game_dir(&self.prefix)
            .ok_or("no client_portal.dat found under the prefix -- did the install finish?")?;
        mark_stamped(&self.prefix, SetupStep::InstallClient).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn step_apply_updates(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if is_stamped(&self.prefix, SetupStep::ApplyUpdates) {
            on(Progress::skipped(SetupStep::ApplyUpdates, "the update is already applied"));
            return Ok(());
        }
        let updates = self.games.fetch_updates(&self.cache, &mut |_| {})?;
        let game_dir =
            find_game_dir(&self.prefix).ok_or("game directory not found for applying updates")?;
        on(Progress::new(
            SetupStep::ApplyUpdates,
            0.3,
            "unpacking the End-of-Retail dats + patched acclient…",
        ));
        extract_zip(&updates, &game_dir)?;
        // The patched acclient hard-imports these; on macOS they come only from the
        // update zip (there is no winetricks fallback). Warn, do not fail.
        for dll in ["msvcr70.dll", "msvcp70.dll", "zlib1.dll"] {
            if !game_dir.join(dll).exists() {
                on(Progress::new(
                    SetupStep::ApplyUpdates,
                    0.9,
                    format!("warning: {dll} missing from the game dir"),
                ));
            }
        }
        mark_stamped(&self.prefix, SetupStep::ApplyUpdates).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn step_finalize(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if is_stamped(&self.prefix, SetupStep::Finalize) {
            on(Progress::skipped(SetupStep::Finalize, "already set up"));
            return Ok(());
        }
        let wine = self.wine_bin().ok_or("no Wine engine available")?;
        let game_dir = find_game_dir(&self.prefix).ok_or("game directory not found")?;
        on(Progress::new(SetupStep::Finalize, 0.5, "writing the play-ac.command escape hatch…"));
        write_play_script(&self.prefix, &wine, &game_dir)?;
        mark_stamped(&self.prefix, SetupStep::Finalize).map_err(|e| e.to_string())?;
        Ok(())
    }
}

impl Runtime for WineRuntime {
    fn run_step(&self, step: SetupStep, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        match step {
            SetupStep::Dependencies => self.step_dependencies(on),
            SetupStep::DownloadRuntime => self.step_download_runtime(on),
            SetupStep::DownloadClient => self.step_download_client(on),
            SetupStep::DownloadUpdates => self.step_download_updates(on),
            SetupStep::InstallRuntime => self.step_install_runtime(on),
            SetupStep::Prefix => self.step_prefix(on),
            SetupStep::Components => self.step_components(on),
            SetupStep::InstallClient => self.step_install_client(on),
            SetupStep::ApplyUpdates => self.step_apply_updates(on),
            SetupStep::Finalize => self.step_finalize(on),
        }
    }

    fn discover(&self) -> Result<Install, String> {
        if !self.prefix.is_dir() {
            return Err(format!("No Wine prefix at {}. Run setup first.", self.prefix.display()));
        }
        let drive_c = self.prefix.join("drive_c");
        if !drive_c.is_dir() {
            return Err(format!("{} is not a Wine prefix -- it has no drive_c.", self.prefix.display()));
        }
        let acclient = find_acclient(&drive_c)
            .ok_or_else(|| format!("No acclient.exe under {}. Is the client installed?", drive_c.display()))?;
        let ac_dir = acclient.parent().ok_or("acclient.exe has no parent directory")?.to_path_buf();
        let wine = self
            .wine_bin()
            .ok_or("No Wine engine is installed. Run setup first.")?;
        // `Install::proton` names the runtime that runs the client; on macOS that
        // is the wine binary rather than a Proton build.
        Ok(Install { prefix: self.prefix.clone(), ac_dir, proton: wine })
    }
}

/// Launch the client. Returns once spawned, not when it exits — the UI stays
/// responsive and the game owns the screen from here.
///
/// ## Avoiding the widescreen stretch
///
/// AC reads its resolution from `UserPreferences.ini`; with no such file it
/// defaults to a resolution that rarely matches the panel, and the Mac Wine
/// driver stretches the result to fill the screen (very visible on an ultrawide).
/// The fix is two coupled parts, both keyed on the display's real resolution:
///
///   1. **Write `UserPreferences.ini`** so AC renders at the display resolution
///      and correct aspect (see [`ensure_display_prefs`]). Never overwrites an
///      existing file unless the user forced a size via `BETTERAC_RESOLUTION`.
///   2. **Run inside a Wine virtual desktop** of the same size. Exclusive
///      fullscreen mode-switching is unreliable on Retina/scaled displays; a
///      virtual desktop renders into a window of exactly that size instead, so
///      there is no mode switch to stretch. AC fills it because its resolution
///      (from step 1) matches.
///
/// Resolution is taken from `BETTERAC_RESOLUTION` (WxH), else the `res` argument,
/// else the main display (CoreGraphics).
///
/// Fullscreen is the default: the Mac driver drops a fullscreen-requesting game
/// into a real macOS fullscreen space, and because we pin AC's resolution to the
/// display it fills correctly rather than stretching. Escape hatches:
/// `BETTERAC_DESKTOP=1` runs inside a Wine virtual desktop instead (always a
/// window on the Mac driver, but it cannot stretch — a fallback for displays that
/// mis-scale fullscreen); `BETTERAC_WINDOWED=1` runs AC in a plain window; and
/// `BETTERAC_RESOLUTION=WxH` forces a size.
pub fn launch(
    install: &Install,
    server: &Server,
    account: &str,
    password: &str,
    res: Option<(i32, i32)>,
) -> Result<Child, String> {
    validate(server, account, password)?;

    // Existing prefixes were created before we contained the profile; do it here
    // too so the game never reaches the real ~/Documents, ~/Desktop, ~/Music.
    contain_user_profile(&install.prefix);

    let resolution = env_resolution().or(res).or_else(display::main_resolution);
    let use_desktop = env_flag("BETTERAC_DESKTOP");
    let fullscreen = !use_desktop && !env_flag("BETTERAC_WINDOWED");

    if let Some((w, h)) = resolution {
        // Inside a virtual desktop AC must be windowed (it fills the desktop
        // window); otherwise it takes the mode the ini requests. We enforce these
        // two keys every launch (preserving AC's other settings) so the window
        // mode always reflects betterAC's choice.
        let ac_fullscreen = if use_desktop { false } else { fullscreen };
        for path in prefs_paths(install) {
            write_display_prefs(&path, (w, h), ac_fullscreen);
        }
    }

    let mut argv: Vec<String> = Vec::new();
    if let (true, Some((w, h))) = (use_desktop, resolution) {
        // wine explorer /desktop=NAME,WxH  acclient.exe <args>
        argv.push("explorer".into());
        argv.push(format!("/desktop=betterac,{w}x{h}"));
    }
    argv.push("acclient.exe".into());
    argv.extend(client_args(server, account, password));

    // `install.proton` holds the wine binary on macOS (see `discover`).
    let mut cmd = Command::new(&install.proton);
    cmd.args(&argv)
        .current_dir(&install.ac_dir)
        .env("WINEPREFIX", &install.prefix)
        // Builtin d3d9 = wined3d. Step Zero proved AC renders this way and DXVK
        // never engaged, so there is no native d3d9 to prefer and no Vulkan layer.
        .env("WINEDLLOVERRIDES", "d3d9=b")
        .env("WINEDEBUG", "-all");

    cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("The Wine engine is missing at {}. Run setup again.", install.proton.display())
        } else {
            format!("Could not start the client: {e}")
        }
    })
}

/// `BETTERAC_RESOLUTION=WxH`, parsed. `None` if unset or malformed.
fn env_resolution() -> Option<(i32, i32)> {
    let raw = std::env::var("BETTERAC_RESOLUTION").ok()?;
    let (w, h) = raw.trim().split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// True when an env var is set to something other than empty/"0".
fn env_flag(key: &str) -> bool {
    std::env::var(key).ok().is_some_and(|v| !v.is_empty() && v != "0")
}

/// The files AC might read its preferences from: the game directory and
/// `<My Documents>\Asheron's Call` (clients differ on which they use).
fn prefs_paths(install: &Install) -> Vec<PathBuf> {
    let mut paths = vec![install.ac_dir.join("UserPreferences.ini")];
    if let Some(docs) = ac_documents_dir(&install.prefix) {
        paths.push(docs.join("UserPreferences.ini"));
    }
    paths
}

/// Enforce AC's `[Display]` Resolution + FullScreen at `path`, preserving every
/// other line so the user's other in-game settings survive. Creates the file (and
/// parent dirs) if absent.
fn write_display_prefs(path: &Path, res: (i32, i32), fullscreen: bool) {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let merged = merge_display_ini(&existing, res, fullscreen);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, merged);
}

/// Return `existing` with `Resolution` and `FullScreen` in its `[Display]` section
/// set to the given values, every other line untouched. Adds the keys to an
/// existing `[Display]`, or appends a fresh `[Display]` section if there is none.
/// Emits CRLF, as the Windows client expects.
fn merge_display_ini(existing: &str, (w, h): (i32, i32), fullscreen: bool) -> String {
    let res_line = format!("Resolution={w}x{h}");
    let fs_line = format!("FullScreen={}", if fullscreen { "True" } else { "False" });
    let key_of = |line: &str| line.trim().split('=').next().unwrap_or("").trim().to_ascii_lowercase();

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

/// Keep the Wine user profile inside the prefix. Wine points the profile folders
/// (Desktop, Documents, Downloads, My Music, …) at the real macOS home, so
/// anything the installer or client writes there lands in the user's actual files
/// and trips macOS privacy (TCC) prompts for Documents/Desktop/Downloads/Music.
///
/// This replaces any *escaping* symlink under each user profile with a real,
/// empty in-prefix directory. `remove_file` on a symlink drops only the link, so
/// the real home contents are never touched. Idempotent — an already-contained
/// (non-symlink) folder is left alone, and a link that stays inside the prefix is
/// kept.
fn contain_user_profile(prefix: &Path) {
    let users = prefix.join("drive_c/users");
    let canon_prefix = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    let Ok(user_dirs) = std::fs::read_dir(&users) else { return };
    for user in user_dirs.flatten() {
        let Ok(items) = std::fs::read_dir(user.path()) else { continue };
        for item in items.flatten() {
            let p = item.path();
            let is_symlink =
                std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink());
            if !is_symlink {
                continue;
            }
            let escapes = match std::fs::read_link(&p) {
                Ok(target) => {
                    let abs = if target.is_absolute() {
                        target
                    } else {
                        p.parent().map(|d| d.join(&target)).unwrap_or(target)
                    };
                    let canon = std::fs::canonicalize(&abs).unwrap_or(abs);
                    !canon.starts_with(&canon_prefix)
                }
                Err(_) => true,
            };
            if escapes {
                let _ = std::fs::remove_file(&p);
                let _ = std::fs::create_dir_all(&p);
            }
        }
    }
}

/// `<prefix>/drive_c/users/<user>/Documents/Asheron's Call`, the retail client's
/// preferences home. Uses `$USER`, falling back to the first non-Public user dir.
fn ac_documents_dir(prefix: &Path) -> Option<PathBuf> {
    let users = prefix.join("drive_c/users");
    if let Ok(user) = std::env::var("USER") {
        let p = users.join(&user);
        if p.is_dir() {
            return Some(p.join("Documents").join("Asheron's Call"));
        }
    }
    for e in std::fs::read_dir(&users).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() && p.file_name().is_some_and(|n| n != "Public") {
            return Some(p.join("Documents").join("Asheron's Call"));
        }
    }
    None
}

/// The main display's logical resolution, via CoreGraphics. `CGDisplayPixelsWide`
/// returns points (the "looks like" resolution) rather than raw Retina pixels,
/// which is exactly the size we want AC to render at.
mod display {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayPixelsWide(display: u32) -> usize;
        fn CGDisplayPixelsHigh(display: u32) -> usize;
    }

    pub fn main_resolution() -> Option<(i32, i32)> {
        // SAFETY: these are pure display queries with no arguments to get wrong.
        unsafe {
            let id = CGMainDisplayID();
            let (w, h) = (CGDisplayPixelsWide(id), CGDisplayPixelsHigh(id));
            (w > 0 && h > 0).then_some((w as i32, h as i32))
        }
    }
}

/// Find a `bin/wine` (preferring the 32-bit-capable `wine` over `wine64`) under
/// `root`, tolerating nesting. The WhiskyWine tarball unpacks to
/// `Libraries/Wine/bin/wine`, so a plain `root/bin/wine` check is not enough; we
/// walk a few levels for a directory named `bin` that holds a wine binary.
fn find_wine_bin(root: &Path) -> Option<PathBuf> {
    // Fast path: the binary sits directly under root/bin.
    for cand in ["bin/wine", "bin/wine64"] {
        let p = root.join(cand);
        if p.is_file() {
            return Some(p);
        }
    }
    // Bounded search for a nested .../bin/{wine,wine64}.
    fn walk(dir: &Path, depth: usize, out: &mut Option<PathBuf>) {
        if out.is_some() || depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            if p.file_name().is_some_and(|n| n == "bin") {
                for w in ["wine", "wine64"] {
                    let wp = p.join(w);
                    if wp.is_file() {
                        *out = Some(wp);
                        return;
                    }
                }
            }
            walk(&p, depth - 1, out);
        }
    }
    let mut out = None;
    walk(root, 4, &mut out);
    out
}

/// Does this Mac need Rosetta 2 to run the engine?
///
/// The Wine engine we self-provision is an **x86_64** build (the whole Whisky /
/// CrossOver lineage is), and AC itself is 32-bit x86 running inside it via
/// wine32on64. On Apple Silicon the engine's x86_64 code is translated by Rosetta
/// 2; on an Intel Mac it is simply native, Rosetta does not exist, and
/// `softwareupdate --install-rosetta` fails. So the whole dependency is
/// architecture-conditional, and this is the only place the two Macs differ.
const NEEDS_ROSETTA: bool = cfg!(target_arch = "aarch64");

/// Is x86 translation (Rosetta 2) available? The runtime installs it here when
/// AC needs it; this file's presence is what `softwareupdate --install-rosetta`
/// drops, and it is the check Apple's own tooling uses. Only meaningful when
/// [`NEEDS_ROSETTA`].
fn rosetta_present() -> bool {
    Path::new("/Library/Apple/usr/libexec/oah/libRosettaRuntime").exists()
}

/// Write the direct-launch escape hatch — the macOS analogue of Linux's
/// play-ac.sh. Double-clickable (`.command`), goes straight through the engine's
/// wine with builtin d3d9, and bypasses the launcher entirely.
fn write_play_script(prefix: &Path, wine_bin: &Path, ac_dir: &Path) -> Result<(), String> {
    let script = PLAY_SCRIPT_TEMPLATE
        .replace("@@PREFIX@@", &prefix.display().to_string())
        .replace("@@WINE@@", &wine_bin.display().to_string())
        .replace("@@ACDIR@@", &ac_dir.display().to_string());
    let path = prefix.join("play-ac.command");
    std::fs::write(&path, script).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

const PLAY_SCRIPT_TEMPLATE: &str = r##"#!/usr/bin/env bash
# Launch Asheron's Call directly on macOS, bypassing the launcher.
#
#   ACE servers   ./play-ac.command -a ACCOUNT -v PASSWORD -h HOST:PORT
#   GDLE servers  ./play-ac.command -h HOST -p PORT -a ACCOUNT:PASSWORD
#
# Goes straight through the self-provisioned Wine engine by full path (it is not
# on PATH by design). AC is D3D9 from 1999; the engine's builtin d3d9 (wined3d)
# draws it -- no DXVK, no Vulkan -- which is what Step Zero confirmed works.
set -euo pipefail
export WINEPREFIX="@@PREFIX@@"
export WINEDLLOVERRIDES="d3d9=b"
export WINEDEBUG=-all
cd "@@ACDIR@@"
exec "@@WINE@@" acclient.exe "$@" -rodat off
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers::Software;

    fn rt() -> WineRuntime {
        WineRuntime {
            prefix: PathBuf::from("/tmp/does-not-exist-betterac-test"),
            cache: PathBuf::from("/tmp/does-not-exist-betterac-test/cache"),
            // A guaranteed-empty engine root, so the tests don't depend on whether
            // this dev machine happens to have a real engine in engine_dir().
            engine_override: Some(PathBuf::from("/tmp/does-not-exist-betterac-engine")),
            engine_url: String::new(),
            engine_sha256: None,
            games: GameSources { src: None, installer_url: String::new(), updates_url: String::new() },
        }
    }

    #[test]
    fn an_override_pointing_at_an_executable_is_used_directly() {
        // A file override is taken as the wine binary itself, dir override as a root.
        let mut r = rt();
        r.engine_override = Some(PathBuf::from("/bin/sh")); // a real file, stands in for wine
        assert_eq!(r.wine_bin(), Some(PathBuf::from("/bin/sh")));
    }

    #[test]
    fn a_missing_engine_reads_back_as_none() {
        assert!(rt().wine_bin().is_none());
    }

    #[test]
    fn discover_on_a_bare_path_names_the_missing_prefix() {
        let e = rt().discover().unwrap_err();
        assert!(e.contains("No Wine prefix"), "unexpected error: {e}");
    }

    fn srv(software: Software) -> Server {
        Server {
            name: "Coldeve".into(),
            description: String::new(),
            ruleset: "PvE".into(),
            software,
            host: "play.coldeve.ac".into(),
            port: "9000".into(),
            players: None,
            website_url: None,
            discord_url: None,
        }
    }

    #[test]
    fn merge_into_an_empty_file_creates_a_display_section() {
        let out = merge_display_ini("", (3360, 1418), true);
        assert!(out.contains("[Display]"));
        assert!(out.contains("Resolution=3360x1418"));
        assert!(out.contains("FullScreen=True"));
        assert!(out.ends_with("\r\n"));
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

    #[test]
    fn launch_refuses_bad_credentials_before_spawning() {
        // validate() runs first, so a colon password on GDLE is rejected here and
        // we never try to exec a nonexistent wine binary.
        let install = Install {
            prefix: PathBuf::from("/tmp/x"),
            ac_dir: PathBuf::from("/tmp/x"),
            proton: PathBuf::from("/tmp/x/bin/wine"),
        };
        let err = launch(&install, &srv(Software::Gdle), "hank", "hun:ter2", None).unwrap_err();
        assert!(err.contains("colon"), "unexpected error: {err}");
    }
}
