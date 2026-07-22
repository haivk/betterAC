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
use crate::patches;
use crate::prefs::{env_flag, env_resolution};
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

    /// Byte-patch the client for defects that config cannot reach -- see
    /// [`crate::patches`]. Runs after the update bundle, because that is what puts
    /// the client we patch in place. A patch that does not recognise the build is
    /// reported and skipped, never fatal: an unpatched client is still playable.
    fn step_patch_client(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if is_stamped(&self.prefix, SetupStep::PatchClient) {
            on(Progress::skipped(SetupStep::PatchClient, "the client is already patched"));
            return Ok(());
        }
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
/// The screen mode is chosen per display, not fixed — see [`LaunchMode`] for the
/// measurements behind it. Where the Mac driver can give AC a real fullscreen
/// device it gets one; where it cannot (a MacBook's built-in panel offers no 4:3
/// mode, and AC refuses to create a device without one) AC runs fullscreen inside
/// a Wine virtual desktop, which synthesises the modes it wants.
///
/// Escape hatches: `BETTERAC_WINDOWED=1` forces a plain window (AC then pins
/// itself to 800x600); `BETTERAC_DESKTOP=1` forces the virtual desktop;
/// `BETTERAC_FULLSCREEN=1` demands real fullscreen even where we expect it to
/// fail; `BETTERAC_RESOLUTION=WxH` forces a size.
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
    let mode = LaunchMode::choose(
        env_flag("BETTERAC_WINDOWED"),
        env_flag("BETTERAC_DESKTOP"),
        env_flag("BETTERAC_FULLSCREEN"),
        display::exclusive_fullscreen_available,
    );

    if let Some((w, h)) = resolution {
        // AC is told "fullscreen" for both real fullscreen *and* inside a virtual
        // desktop — in the latter it goes fullscreen within the desktop, which is
        // the whole point (it is what makes the device creation succeed there).
        // Enforced every launch, because AC rewrites this file on exit and would
        // otherwise carry its last session's mode into the next one.
        crate::prefs::apply(&install.ac_dir, &install.prefix, (w, h), mode.ac_fullscreen());
    }

    let mut argv: Vec<String> = Vec::new();
    let mut client = "acclient.exe".to_string();
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
    argv.push(client);
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
    /// Real exclusive fullscreen. Only where the display can actually do it.
    Fullscreen,
    /// Fullscreen inside a Wine virtual desktop — the fallback that makes the
    /// built-in panel work, and no worse than a window anywhere else.
    Desktop,
    /// A plain window. Only on request: AC ignores `Resolution` when windowed and
    /// pins itself to 800x600.
    Windowed,
}

impl LaunchMode {
    /// Explicit user intent first, then what the display can do. Falling back to
    /// [`LaunchMode::Desktop`] rather than [`LaunchMode::Windowed`] matters: the
    /// window AC gives us is a fixed 800x600, the virtual desktop fills the screen.
    fn choose(
        force_windowed: bool,
        force_desktop: bool,
        force_fullscreen: bool,
        available: impl Fn() -> bool,
    ) -> Self {
        match (force_windowed, force_desktop, force_fullscreen) {
            (true, _, _) => Self::Windowed,
            (_, true, _) => Self::Desktop,
            (_, _, true) => Self::Fullscreen,
            _ if available() => Self::Fullscreen,
            _ => Self::Desktop,
        }
    }

    /// What to write to AC's `FullScreen` key. True inside a virtual desktop too —
    /// that is what makes its device creation succeed.
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

/// The main display's logical resolution, via CoreGraphics. `CGDisplayPixelsWide`
/// returns points (the "looks like" resolution) rather than raw Retina pixels,
/// which is exactly the size we want AC to render at.
mod display {
    use std::os::raw::c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayPixelsWide(display: u32) -> usize;
        fn CGDisplayPixelsHigh(display: u32) -> usize;
        fn CGDisplayIsBuiltin(display: u32) -> i32;
        fn CGDisplayCopyDisplayMode(display: u32) -> *mut c_void;
        fn CGDisplayModeRelease(mode: *mut c_void);
        fn CGDisplayCopyAllDisplayModes(display: u32, options: *const c_void) -> *const c_void;
        fn CGDisplayModeGetWidth(mode: *const c_void) -> usize;
        fn CGDisplayModeGetHeight(mode: *const c_void) -> usize;
        fn CGDisplayModeGetPixelWidth(mode: *const c_void) -> usize;
        fn CGDisplayModeGetPixelHeight(mode: *const c_void) -> usize;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, idx: isize) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }

    pub fn main_resolution() -> Option<(i32, i32)> {
        // SAFETY: these are pure display queries with no arguments to get wrong.
        unsafe {
            let id = CGMainDisplayID();
            let (w, h) = (CGDisplayPixelsWide(id), CGDisplayPixelsHigh(id));
            (w > 0 && h > 0).then_some((w as i32, h as i32))
        }
    }

    /// Can this display give AC an exclusive-fullscreen D3D9 device?
    ///
    /// Two conditions, both observed rather than derived — see
    /// [`can_go_exclusive_fullscreen`] for the measurements:
    ///
    ///   1. the display is **not** the machine's built-in panel, and
    ///   2. the desktop's current logical size also exists as a 1:1 mode
    ///      (`pixelWidth == width`).
    ///
    /// The answer changes when a monitor is plugged in or unplugged, so this is
    /// asked on every launch and never cached.
    pub fn exclusive_fullscreen_available() -> bool {
        // SAFETY: `CGDisplayCopyDisplayMode`/`CGDisplayCopyAllDisplayModes` return
        // owned references (null on failure), released below; the getters only read
        // a mode we still hold.
        unsafe {
            let id = CGMainDisplayID();
            if CGDisplayIsBuiltin(id) != 0 {
                return false;
            }
            let current = CGDisplayCopyDisplayMode(id);
            if current.is_null() {
                return false;
            }
            let want = (CGDisplayModeGetWidth(current), CGDisplayModeGetHeight(current));
            CGDisplayModeRelease(current);

            let modes = CGDisplayCopyAllDisplayModes(id, std::ptr::null());
            if modes.is_null() {
                return false;
            }
            let mut found = false;
            for i in 0..CFArrayGetCount(modes) {
                let m = CFArrayGetValueAtIndex(modes, i);
                if m.is_null() {
                    continue;
                }
                let (w, h) = (CGDisplayModeGetWidth(m), CGDisplayModeGetHeight(m));
                let one_to_one =
                    CGDisplayModeGetPixelWidth(m) == w && CGDisplayModeGetPixelHeight(m) == h;
                if one_to_one && (w, h) == want {
                    found = true;
                    break;
                }
            }
            CFRelease(modes);
            found
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

    /// The crash this guards: asking for exclusive fullscreen on a display that
    /// offers AC no 4:3 mode kills it with a DirectX dialog before it ever creates
    /// a device, so such a display must get the virtual desktop instead.
    #[test]
    fn the_display_decides_the_mode_unless_the_user_says_otherwise() {
        let (yes, no) = (|| true, || false);

        // The two machines this was measured on.
        assert_eq!(LaunchMode::choose(false, false, false, yes), LaunchMode::Fullscreen);
        assert_eq!(LaunchMode::choose(false, false, false, no), LaunchMode::Desktop);

        // Explicit intent wins, in priority order.
        assert_eq!(LaunchMode::choose(true, false, false, yes), LaunchMode::Windowed);
        assert_eq!(LaunchMode::choose(false, true, false, yes), LaunchMode::Desktop);
        assert_eq!(LaunchMode::choose(false, false, true, no), LaunchMode::Fullscreen);
        assert_eq!(LaunchMode::choose(true, true, true, no), LaunchMode::Windowed);

        // AC must be told "fullscreen" inside a virtual desktop -- writing False
        // there is what the pre-2026-07-21 code did, and it wastes the desktop.
        assert!(LaunchMode::Desktop.ac_fullscreen(), "regression: the whole point of Desktop");
        assert!(LaunchMode::Fullscreen.ac_fullscreen());
        assert!(!LaunchMode::Windowed.ac_fullscreen());
    }

    /// Manual probe — asks the *actual* display whether fullscreen is available,
    /// so the answer depends on what is plugged in and it cannot be a CI
    /// assertion. Run it when a display misbehaves:
    /// `cargo test -p ac-core -- --ignored --nocapture display_reports`
    #[test]
    #[ignore = "depends on the monitors attached right now"]
    fn display_reports_whether_fullscreen_is_available() {
        println!(
            "main display {:?}, exclusive fullscreen available: {}",
            display::main_resolution(),
            display::exclusive_fullscreen_available()
        );
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
