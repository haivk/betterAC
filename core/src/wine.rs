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
//! How AC gets the screen — the display resolution, the fullscreen Space, and the
//! escape hatches around both — is documented on [`launch`] and [`LaunchMode`].

use crate::args::{client_args, validate};
use crate::fetch::{download, extract_tar_gz, extract_zip, verify_sha256};
use crate::gamefiles::GameSources;
use crate::install::{find_acclient, find_game_dir, runtime_dir, Install};
use crate::patches;
use crate::prefs::{env_flag, env_resolution};
use crate::servers::Server;
use crate::setup::{is_stamped, mark_stamped, Progress, Runtime, SetupStep};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// The macOS "fullscreen Space" helper, embedded so a launch needs nothing on disk
/// to provision. Built from `macos/helpers/acspaces.m` by `build.rs`, and written to
/// the support dir on first use by [`ensure_spaces_helper`].
const ACSPACES_DYLIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/acspaces.dylib"));

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
    /// wins; otherwise the self-provisioned `runtime_dir()`.
    fn engine_root(&self) -> PathBuf {
        self.engine_override.clone().unwrap_or_else(runtime_dir)
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
        // Give Wine a working directory it can map to a DOS path. We drop the drives
        // that reach the host filesystem (no Z:), so inheriting the launcher's cwd
        // leaves Wine unable to map it — it warns and starts in the Windows dir, and
        // msiexec /a fails outright. `drive_c` always maps to C:. Skipped before it
        // exists (the wineboot that creates it), where the default cwd is fine.
        let drive_c = self.prefix.join("drive_c");
        if drive_c.is_dir() {
            c.current_dir(drive_c);
        }
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
        let dest = runtime_dir();
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
        // Seal the prefix off from the host before anything (the installer, the
        // client, us) can write out to the real home or reach the host filesystem:
        // contain the profile folders and drop the drives that map outside.
        on(Progress::new(SetupStep::Prefix, 0.9, "sealing the prefix off from the host…"));
        harden_prefix(&self.prefix);
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
        // The installer lives in the download cache, outside the prefix, and the
        // prefix no longer maps the host filesystem (harden_prefix drops Z:). So
        // stage a copy inside C: and run that; remove it afterwards.
        let staged = self.prefix.join("drive_c").join(
            installer.file_name().ok_or("the installer has no file name")?,
        );
        std::fs::copy(&installer, &staged)
            .map_err(|e| format!("staging the installer into the prefix: {e}"))?;
        // The real wizard, drawn by Wine's Mac driver. Non-zero exit is tolerated;
        // we verify success by the data file it drops.
        let _ = self.wine(&wine).arg(windows_path(&self.prefix, &staged)).status();
        let _ = std::fs::remove_file(&staged);
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

    /// Byte-patch the client for defects that config cannot reach -- see
    /// [`crate::patches`]. Runs after the update bundle, because that is what puts
    /// the client we patch in place. A patch that does not recognise the build is
    /// reported and skipped, never fatal: an unpatched client is still playable.
    ///
    /// Deliberately *not* short-circuited on its stamp, unlike every other step:
    /// [`patches::apply_all`] is idempotent and only rewrites the file when a byte
    /// actually changed, so re-running it costs one read of a 4.8 MB file and means
    /// a patch added in a later release lands on installs that are already set up.
    fn step_patch_client(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        let game_dir =
            find_game_dir(&self.prefix).ok_or("game directory not found for patching the client")?;
        let client = find_acclient(&game_dir)
            .ok_or("no acclient.exe in the game directory -- did the update apply?")?;
        on(Progress::new(SetupStep::PatchClient, 0.5, "applying client patches…"));
        for (name, outcome) in patches::apply_all(&client)? {
            let msg = match outcome {
                patches::Outcome::Applied => format!("applied {name}"),
                patches::Outcome::AlreadyApplied => format!("{name}: already applied"),
                patches::Outcome::Skipped => {
                    format!("warning: skipped {name} -- not the client build it targets")
                }
            };
            on(Progress::new(SetupStep::PatchClient, 0.9, msg));
        }
        mark_stamped(&self.prefix, SetupStep::PatchClient).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Provision Decal, but only if the user opted in on the setup screen. Off is
    /// the default, so for most installs this downloads nothing and does nothing.
    ///
    /// Deliberately stampless (see [`SetupStep::stamp`]): the opt-in is read from
    /// config, and a resumed run must re-evaluate it rather than remember an earlier
    /// "no".
    fn step_install_decal(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if !crate::config::Config::load().decal.enabled {
            on(Progress::skipped(SetupStep::InstallDecal, "Decal was not selected"));
            return Ok(());
        }
        let wine = self.wine_bin().ok_or("no Wine engine available to install Decal")?;
        if crate::decal::is_installed(&self.prefix) {
            // Re-apply the parts that live outside the Decal directory -- the
            // injector and cohook ship with the app, and the engine hot-patch lives
            // in the engine, so an app update or a re-provisioned engine loses them.
            crate::decal::ensure_runtime_hooks(&self.prefix, &wine)?;
            on(Progress::skipped(SetupStep::InstallDecal, "Decal is already installed"));
            return Ok(());
        }
        let game_dir = find_game_dir(&self.prefix).ok_or("game directory not found for Decal")?;
        let run = |args: &[&str]| -> Result<(), String> {
            let status =
                self.wine(&wine).args(args).status().map_err(|e| format!("{}: {e}", args[0]))?;
            status.success().then_some(()).ok_or_else(|| format!("{} failed ({status})", args[0]))
        };
        crate::decal::install(&self.prefix, &game_dir, &self.cache, &wine, &run, on)
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
            SetupStep::PatchClient => self.step_patch_client(on),
            SetupStep::InstallDecal => self.step_install_decal(on),
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
/// AC reads its resolution from `UserPreferences.ini`; with no such file it defaults
/// to a resolution that rarely matches the panel, and the Mac Wine driver stretches
/// the result to fill the screen (very visible on an ultrawide). So every launch
/// writes that file with the display's real resolution — [`crate::prefs::apply`],
/// enforced each time because AC rewrites the file on a clean exit and would
/// otherwise carry its last session's mode into the next one. With AC's own
/// resolution pinned to the display there is nothing left for anything to stretch.
///
/// Resolution is taken from `BETTERAC_RESOLUTION` (WxH), else the `res` argument,
/// else the main display (CoreGraphics).
///
/// ## The default: a native macOS fullscreen Space
///
/// By default AC runs in [`LaunchMode::Spaces`]: windowed at the display resolution
/// (step 1 above, with `FullScreen=False`), in its own fullscreen Space that slides
/// aside on alt-tab rather than the borderless overlay exclusive fullscreen
/// produces. Three pieces make that work, and two of them are byte patches applied
/// at setup (see [`crate::patches`]):
///
///   * **`window-style-create` / `window-style-restyle`** put `WS_THICKFRAME` in the
///     style AC gives its own window. winemac only grants a window the native
///     fullscreen capability (`NSWindowCollectionBehaviorFullScreenPrimary`) once it
///     has a resizable frame, and AC never set one. They also drop `WS_MINIMIZEBOX`,
///     so the game cannot be minimised out from under the player.
///   * **`login-resolution`** removes the client's hardcoded 800x600 for the splash,
///     login and character-select screens, so the whole session is one window at one
///     size, fullscreen from the first frame. (Those screens still *draw* their UI at
///     800x600 in the top-left — that is fixed-pixel artwork with no scaling hook,
///     and the attempt to scale it by resizing the window was measured and reverted;
///     see the patch's notes.)
///   * **`acspaces.dylib`** is injected into the Wine process via
///     `DYLD_INSERT_LIBRARIES` and, from *inside* that process (so no Accessibility
///     permission), calls the window's own `-toggleFullScreen:` once it is capable.
///
/// The dylib is provisioned by [`ensure_spaces_helper`]; if it can't be written the
/// launch still proceeds, just as a plain window the user can fullscreen by hand.
///
/// Escape hatches (all skip the dylib, so none of them auto-fullscreen):
/// `BETTERAC_WINDOWED=1` forces a plain window; `BETTERAC_DESKTOP=1` forces the Wine
/// virtual desktop (the fallback for a MacBook's built-in panel, which offers no 4:3
/// mode for an exclusive-fullscreen device); `BETTERAC_FULLSCREEN=1` demands the old
/// exclusive-fullscreen overlay; `BETTERAC_RESOLUTION=WxH` forces a size.
pub fn launch(
    install: &Install,
    server: &Server,
    account: &str,
    password: &str,
    res: Option<(i32, i32)>,
) -> Result<Child, String> {
    validate(server, account, password)?;

    // Keep the game inside its prefix — profile and drives — every launch, so a
    // prefix built before this existed is hardened on its next run too.
    harden_prefix(&install.prefix);

    let resolution = env_resolution().or(res).or_else(display::main_resolution);
    let mode = LaunchMode::choose(
        env_flag("BETTERAC_WINDOWED"),
        env_flag("BETTERAC_DESKTOP"),
        env_flag("BETTERAC_FULLSCREEN"),
    );

    if let Some((w, h)) = resolution {
        // AC is told "fullscreen" for both real fullscreen *and* inside a virtual
        // desktop — in the latter it goes fullscreen within the desktop, which is
        // the whole point (it is what makes the device creation succeed there).
        // Enforced every launch, because AC rewrites this file on exit and would
        // otherwise carry its last session's mode into the next one.
        crate::prefs::apply(&install.ac_dir, &install.prefix, (w, h), mode.ac_fullscreen());
    }

    // Keep the client's patches current. They ship with the app, not with the
    // prefix, so an install set up by an older build would otherwise never see a
    // patch added since -- the setup step that applies them does not run again once
    // setup is complete. `apply_all` is idempotent and only writes when something
    // actually changed, and a failure here is not worth refusing to launch over.
    let _ = crate::patches::apply_all(&install.ac_dir.join("acclient.exe"));

    let mut argv: Vec<String> = Vec::new();
    let mut client = "acclient.exe".to_string();
    // Decal, when switched on and provisioned: the injector runs in front of the
    // client and starts it itself. Inside a virtual desktop it slots between
    // explorer and the client, which is why this is resolved before the mode check.
    let injector = crate::decal::launch_injector(&install.prefix);
    if injector.is_some() {
        // Best-effort, and on every launch so a prefix built before these existed
        // gets them: without the config the plugins load but draw nothing, and
        // without the MSIL flip their binds against Decal are refused outright. A
        // failure here must not stop the game starting — losing plugins beats losing
        // the game, the same rule the injector itself follows.
        let _ = crate::decal::ensure_runtime_config(&install.ac_dir);
        let _ = crate::decal::ensure_msil_assemblies(&install.prefix, &install.ac_dir);
    }
    if let (LaunchMode::Desktop, Some((w, h))) = (mode, resolution) {
        // wine explorer /desktop=NAME,WxH  C:\...\acclient.exe <args>
        //
        // explorer.exe does NOT resolve a bare "acclient.exe" against our working
        // directory the way `wine acclient.exe` does — it silently starts nothing
        // at all, so this path has to name the client outright.
        argv.push("explorer".into());
        argv.push(format!("/desktop=betterac,{w}x{h}"));
        client = windows_path(&install.prefix, &install.ac_dir.join("acclient.exe"));
    }
    if let Some(injector) = &injector {
        argv.push(injector.clone());
    }
    argv.push(client);
    argv.extend(client_args(server, account, password));

    // In Spaces mode, provision the dylib that puts AC's window in a native macOS
    // fullscreen Space (see [`launch`] docs and [`ensure_spaces_helper`]). If it
    // can't be written we still launch — AC just comes up as a plain window.
    let spaces_dylib = matches!(mode, LaunchMode::Spaces).then(ensure_spaces_helper).flatten();

    // `install.proton` holds the wine binary on macOS (see `discover`).
    // `BETTERAC_WINEDEBUG` overrides the silent default so a launch can be traced
    // (e.g. to see what Decal loads); unset, it stays quiet as normal play wants.
    let winedebug = std::env::var("BETTERAC_WINEDEBUG").unwrap_or_else(|_| "-all".into());
    let mut cmd = Command::new(&install.proton);
    cmd.args(&argv)
        .current_dir(&install.ac_dir)
        .env("WINEPREFIX", &install.prefix)
        // Builtin d3d9 = wined3d. Step Zero proved AC renders this way and DXVK
        // never engaged, so there is no native d3d9 to prefer and no Vulkan layer.
        .env("WINEDLLOVERRIDES", "d3d9=b")
        .env("WINEDEBUG", &winedebug);
    if let Some(dylib) = &spaces_dylib {
        // Inject the auto-fullscreen dylib into the Wine process. It calls AC's own
        // window's -toggleFullScreen: from *inside* the process, so no Accessibility
        // permission is needed. Must exec wine directly (Command does) — going via a
        // SIP-restricted binary like `nohup` would make dyld strip this.
        cmd.env("DYLD_INSERT_LIBRARIES", dylib);
    }

    let child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("The Wine engine is missing at {}. Run setup again.", install.proton.display())
        } else {
            format!("Could not start the client: {e}")
        }
    })?;

    Ok(child)
}

/// Run a Windows program inside an installed prefix — the [`crate::decal`]
/// operations the settings UI performs (importing a `.reg`, mostly) need one of
/// these, and how you get to a Windows program differs per platform.
pub fn run_in_prefix(install: &Install, args: &[&str]) -> Result<(), String> {
    let status = wine_cmd(install, args)
        .status()
        .map_err(|e| format!("{}: {e}", args.first().unwrap_or(&"wine")))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("{} failed ({status})", args.first().unwrap_or(&"wine")))
}

/// Like [`run_in_prefix`], but returns as soon as the program is running instead of
/// waiting for it to exit. For Windows programs that stay up — Decal's agent, which
/// lives in the menu bar until it is asked to quit.
///
/// The `Child` is dropped, so the process is never reaped by us; it is the prefix's
/// wineserver that owns its lifetime, and [`shutdown_prefix`] is how it ends.
pub fn spawn_in_prefix(install: &Install, args: &[&str]) -> Result<(), String> {
    wine_cmd(install, args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", args.first().unwrap_or(&"wine")))
}

/// End everything running in the prefix, via `wineserver -k`.
///
/// Called when betterAC quits. Without it, a Windows program that outlives the app
/// keeps its menu-bar status item — and, worse, those items are owned by the
/// prefix's `explorer.exe` rather than by the program itself, so an agent that dies
/// abruptly leaves a **dead icon** behind that nothing later will clear. Killing
/// the session is the only thing that reliably removes them.
///
/// Best-effort and non-blocking in spirit: any failure just means there was nothing
/// to kill, which is the normal case.
pub fn shutdown_prefix(install: &Install) {
    let server = install.proton.with_file_name("wineserver");
    if !server.is_file() {
        return;
    }
    let run = |arg: &str| {
        let _ = Command::new(&server)
            .arg(arg)
            .env("WINEPREFIX", &install.prefix)
            .env("WINEDEBUG", "-all")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    };
    run("-k");
    // `-k` only asks; it returns before the server has gone. On its way out
    // wineserver flushes the registry and rewrites `.update-timestamp`, so anything
    // that deletes the prefix immediately after racks up files reappearing
    // underneath it — a reset would empty the prefix and then fail to remove the
    // directory itself. `-w` waits for the server to actually exit, and returns at
    // once when there is none.
    run("-w");
}

/// Like [`run_in_prefix`], but returns the program's stdout. Used for `reg query`.
pub fn query_in_prefix(install: &Install, args: &[&str]) -> Result<String, String> {
    let out = wine_cmd(install, args)
        .output()
        .map_err(|e| format!("{}: {e}", args.first().unwrap_or(&"wine")))?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
        .ok_or_else(|| format!("{} failed", args.first().unwrap_or(&"wine")))
}

fn wine_cmd(install: &Install, args: &[&str]) -> Command {
    let mut c = Command::new(&install.proton);
    c.args(args).env("WINEPREFIX", &install.prefix).env("WINEDEBUG", "-all");
    // A working directory Wine can map to a DOS path — the drives that reach the
    // host are gone, so an inherited cwd would leave Wine warning and rootless.
    let drive_c = install.prefix.join("drive_c");
    if drive_c.is_dir() {
        c.current_dir(drive_c);
    }
    c
}

/// Write the embedded macOS fullscreen-Space dylib to the support dir and return its
/// path. Idempotent and cheap: the file is rewritten only when its bytes differ from
/// what is already there, so relaunches don't churn the disk. Returns `None` if it
/// can't be written — the caller then launches without the auto-Space rather than
/// failing.
fn ensure_spaces_helper() -> Option<PathBuf> {
    let dir = crate::install::support_dir().join("helpers");
    std::fs::create_dir_all(&dir).ok()?;
    let dylib = dir.join("acspaces.dylib");
    write_if_changed(&dylib, ACSPACES_DYLIB)?;
    // The Win32 style helper that used to live here is gone -- AC now applies the
    // resizable frame itself (the `resizable-window` patch). Clean up after upgrades.
    let _ = std::fs::remove_file(dir.join("acwindow.exe"));
    Some(dylib)
}

/// Write `bytes` to `path` only if the file is absent or its contents differ.
fn write_if_changed(path: &Path, bytes: &[u8]) -> Option<()> {
    let up_to_date = std::fs::read(path).is_ok_and(|cur| cur == bytes);
    if !up_to_date {
        std::fs::write(path, bytes).ok()?;
    }
    Some(())
}

/// How AC gets its screen.
///
/// ## The bug this exists for
///
/// AC dies on startup with *"The game encountered a fatal DirectX issue while
/// attempting to start. Try a different screen resolution or bit depth."* when it
/// asks for a fullscreen D3D9 device the Mac driver cannot satisfy. On a MacBook's
/// built-in Retina panel that is **every** resolution: measured on a 14" M4 Pro
/// (2026-07-21), 1024x768, 1512x945, 1512x982, 1920x1200, 2560x1600 and the native
/// 3024x1964 all produce the dialog, at both HiDPI and true 1:1 desktop modes, at
/// 60 and 120 Hz. The same client on an external ultrawide goes fullscreen fine.
///
/// ## Why
///
/// A `+d3d` trace shows AC enumerating all 132 modes the driver offers and then
/// giving up without ever calling `CreateDevice`. The offered list is the tell —
/// winemac.drv passes through what macOS reports for the attached display:
///
///   - built-in panel: `960x600 … 3024x1964`, every one of them 16:10 or the
///     notch-inclusive variant. **No 4:3 mode exists.**
///   - the ultrawide: includes `640x480`, `800x600`, `1024x768`, `1280x960`,
///     `1344x1008`, `1600x1200` — the classic 4:3 modes a 1999 client expects.
///
/// A Wine **virtual desktop** does not pass the host's list through; it synthesises
/// its own, which contains exactly those 4:3 modes. So AC finds what it needs,
/// `wined3d_device_create` succeeds, and it renders fullscreen inside the desktop
/// — verified end to end on the built-in panel, all the way into the world.
///
/// Ruled out along the way: the requested resolution, bit depth (every mode on
/// both displays is 32-bit), Wine's `RetinaMode`, `CaptureDisplaysForFullscreen`,
/// `AspectRatio=Widescreen`, the desktop being HiDPI (the ultrawide's is too), the
/// refresh rate, and an active macOS fullscreen Space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchMode {
    /// The default: AC runs windowed at the display resolution, in a native macOS
    /// fullscreen **Space** it puts itself into (see [`launch`]). This is the
    /// modern-Mac-game behaviour — its own Space that slides aside on alt-tab —
    /// instead of the overlay [`Fullscreen`](Self::Fullscreen) produces. Works on
    /// every display: windowed needs no exclusive adapter mode, so it also sidesteps
    /// the built-in-panel problem that [`Desktop`](Self::Desktop) exists for.
    Spaces,
    /// Real exclusive fullscreen — a borderless window that *overlays* the current
    /// Space. Only on request now (`BETTERAC_FULLSCREEN`); superseded by
    /// [`Spaces`](Self::Spaces) as the default.
    Fullscreen,
    /// Fullscreen inside a Wine virtual desktop — the old fallback that makes the
    /// built-in panel work, and no worse than a window anywhere else.
    Desktop,
    /// A plain window at the display resolution — [`Spaces`](Self::Spaces) minus the
    /// automatic Space. Only on request, for when the game should stay on the
    /// current desktop; the window is resizable, so it can still be fullscreened by
    /// hand from the green button.
    Windowed,
}

impl LaunchMode {
    /// Explicit user intent first, otherwise the [`Spaces`](Self::Spaces) default.
    /// The default used to be decided by the display — exclusive `Fullscreen` where
    /// the adapter mode list allowed it, else `Desktop`. `Spaces` is windowed
    /// underneath, so it needs no exclusive mode and works on every display; the
    /// probe that answered that question is gone with it.
    fn choose(force_windowed: bool, force_desktop: bool, force_fullscreen: bool) -> Self {
        match (force_windowed, force_desktop, force_fullscreen) {
            (true, _, _) => Self::Windowed,
            (_, true, _) => Self::Desktop,
            (_, _, true) => Self::Fullscreen,
            _ => Self::Spaces,
        }
    }

    /// What to write to AC's `FullScreen` key. `Spaces` is windowed (False): AC must
    /// be a normal resizable window for macOS to give it a Space; only exclusive
    /// `Fullscreen` and the virtual `Desktop` (where True makes device creation
    /// succeed) get True.
    fn ac_fullscreen(self) -> bool {
        matches!(self, Self::Fullscreen | Self::Desktop)
    }
}

/// A path inside the prefix as Wine sees it: `C:\Turbine\Asheron's Call\…` for
/// anything under `drive_c`, else the `Z:` drive that maps the real filesystem.
/// Needed because `explorer /desktop=` starts nothing when handed a bare
/// executable name.
fn windows_path(prefix: &Path, path: &Path) -> String {
    let win = |rest: &Path, drive: &str| {
        let joined: Vec<String> =
            rest.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        format!("{drive}\\{}", joined.join("\\"))
    };
    match path.strip_prefix(prefix.join("drive_c")) {
        Ok(rest) => win(rest, "C:"),
        Err(_) => win(path.strip_prefix("/").unwrap_or(path), "Z:"),
    }
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
    let Ok(user_dirs) = std::fs::read_dir(&users) else { return };
    for user in user_dirs.flatten() {
        let Ok(items) = std::fs::read_dir(user.path()) else { continue };
        for item in items.flatten() {
            let p = item.path();
            if symlink_escapes(prefix, &p) {
                // Replace the escaping link with a real, empty in-prefix folder.
                let _ = std::fs::remove_file(&p);
                let _ = std::fs::create_dir_all(&p);
            }
        }
    }
}

/// Is `p` a symlink whose target resolves outside the prefix? `remove_file` on a
/// symlink drops only the link, so callers can sever an escape without touching
/// whatever it pointed at.
fn symlink_escapes(prefix: &Path, p: &Path) -> bool {
    let canon_prefix = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    if !std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
        return false;
    }
    match std::fs::read_link(p) {
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
    }
}

/// Lock the game inside its prefix, on every launch as well as at prefix creation
/// (so a prefix built before this existed is hardened on its next launch — the
/// operations are cheap and idempotent):
///
///   * **profile** — the Wine profile folders (Documents, Desktop, …) are pointed
///     at the real Mac home by default, so anything the client writes there lands
///     in the user's actual files; [`contain_user_profile`] replaces those links
///     with contained folders.
///   * **drives** — `Z:` maps the whole host filesystem and `D:` the engine's
///     source volume, so a Windows-side program (the client, a plugin) could reach
///     anywhere on the Mac. [`contain_drives`] removes every drive that leaves the
///     prefix, keeping only `C:`. Nothing we run needs the others — setup stages
///     the files it installs into `C:` first.
///
/// ## Why this is macOS-only
///
/// Measured on a live Proton prefix (Bazzite, 2026-07-27) rather than assumed,
/// because "harden both platforms" is the obvious-looking call and it is wrong:
///
///   * **Profile containment is already true on Linux.** Proton creates
///     `drive_c/users/steamuser/{Documents,Desktop,Music,…}` as *real
///     directories*, not as symlinks into `$HOME` the way plain Wine does. There
///     is nothing to sever, so running this there would be a no-op — and the
///     macOS-specific half of the motivation (TCC privacy prompts for Documents
///     and Desktop) does not exist on Linux either.
///   * **Drive containment would need setup rewritten to match.** The Linux
///     prefix genuinely does escape (`z:`→`/`, `x:`→`$HOME`, `s:`, `u:`, `v:`),
///     but Linux setup reaches its downloads *through* those drives: the game
///     installer and winetricks both run from host cache paths. macOS could drop
///     them only because [`step_install_client`](WineRuntime::step_install_client)
///     and Decal's MSI stage their files into `C:` first. On top of that Proton
///     re-runs `wineboot` on every launch, which recreates them.
///
/// So Linux keeps its drives. Doing it properly there is a real piece of work
/// with a real payoff, not a comment away.
///
/// Nothing here touches the systray. There used to be a step that set
/// `HKCU\Software\Wine\Explorer` `ShowSystray` and the `NoTrayItemsDisplay` policy
/// to stop Decal's agent becoming a macOS menu-bar item; it never worked. Both
/// values are in this engine's `explorer.exe`, and both were set correctly in the
/// prefix, and the status item appeared regardless — gcenx's build does not honour
/// them. It is also no longer wanted: the agent has no window, so that icon *is*
/// Decal's settings UI (see [`crate::decal::open_settings`]). Stray icons are dealt
/// with at the other end instead, by [`shutdown_prefix`] on quit.
fn harden_prefix(prefix: &Path) {
    contain_user_profile(prefix);
    contain_drives(prefix);
}

/// Remove every `dosdevices` drive that points outside the prefix. `C:` maps
/// `../drive_c` (inside) and stays; `Z:`→`/`, `D:`→a host volume and the like are
/// severed. Runs before every launch because some wineboot paths recreate `Z:`.
fn contain_drives(prefix: &Path) {
    let dosdevices = prefix.join("dosdevices");
    let Ok(entries) = std::fs::read_dir(&dosdevices) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if symlink_escapes(prefix, &p) {
            let _ = std::fs::remove_file(&p);
        }
    }
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
            // this dev machine happens to have a real engine in runtime_dir().
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

    #[test]
    fn the_default_is_a_space_and_explicit_intent_wins() {
        // Spaces regardless of the display: it is windowed underneath, so unlike the
        // old default it needs no exclusive adapter mode and cannot hit the
        // built-in-panel DirectX failure that Desktop exists for.
        assert_eq!(LaunchMode::choose(false, false, false), LaunchMode::Spaces);

        // Explicit intent wins, in priority order.
        assert_eq!(LaunchMode::choose(true, false, false), LaunchMode::Windowed);
        assert_eq!(LaunchMode::choose(false, true, false), LaunchMode::Desktop);
        assert_eq!(LaunchMode::choose(false, false, true), LaunchMode::Fullscreen);
        assert_eq!(LaunchMode::choose(true, true, true), LaunchMode::Windowed);

        // Spaces is windowed (False) so macOS will give it a Space; AC must be told
        // "fullscreen" inside a virtual desktop -- writing False there is what the
        // pre-2026-07-21 code did, and it wastes the desktop.
        assert!(!LaunchMode::Spaces.ac_fullscreen(), "Spaces must be windowed to get a Space");
        assert!(LaunchMode::Desktop.ac_fullscreen(), "regression: the whole point of Desktop");
        assert!(LaunchMode::Fullscreen.ac_fullscreen());
        assert!(!LaunchMode::Windowed.ac_fullscreen());
    }

    /// Manual probe — the answer depends on what is plugged in right now, so it
    /// cannot be a CI assertion. Run it when a display misbehaves:
    /// `cargo test -p ac-core -- --ignored --nocapture display_reports`
    #[test]
    #[ignore = "depends on the monitors attached right now"]
    fn display_reports_the_resolution_ac_will_be_pinned_to() {
        println!("main display {:?}", display::main_resolution());
    }

    #[test]
    fn a_prefix_path_becomes_the_windows_path_explorer_needs() {
        let prefix = PathBuf::from("/Users/h/Library/Application Support/betterac/prefix");
        let client = prefix.join("drive_c/Turbine/Asheron's Call/acclient.exe");
        assert_eq!(windows_path(&prefix, &client), r"C:\Turbine\Asheron's Call\acclient.exe");

        // Anything outside drive_c still has to be nameable, via Wine's Z: mapping.
        assert_eq!(windows_path(&prefix, Path::new("/opt/ac/acclient.exe")), r"Z:\opt\ac\acclient.exe");
    }
}
