//! Running acclient.exe under Proton on Linux.
//!
//! The client always goes through umu-run. Proton's bin/wine cannot run *it*
//! directly: outside the Steam runtime container there is no 32-bit GL, and
//! acclient.exe dies instantly. Everything else Windows-side goes through umu-run
//! too, for the same prefix and the same environment.
//!
//! The one exception is deliberate and narrow: provisioning Decal drives
//! winetricks, which wants a `WINE`/`WINESERVER` pair it can run itself, and
//! Microsoft's .NET installer touches no GL at all. `files/bin/wine` is a genuine
//! 32-bit ELF (the classic multilib build) and handles that fine — verified
//! installing real .NET 4.8 into a prefix on Bazzite. See [`wine_bin`].
//!
//! betterAC runs a GE-Proton build it **owns**, copied out of Steam's
//! `compatibilitytools.d` rather than run from it — see
//! [`crate::install::runtime_dir`] for why that matters.
//!
//! Display-resolution detection is NOT here -- it needs a toolkit (Mutter over
//! D-Bus, or GDK) and so lives in the GTK frontend, which passes the result in.
//! Keeping it out is what lets this crate build on macOS too.

use crate::args::{gamescope_args_for, invocation, validate};
use crate::fetch::{download, extract_tar_gz, extract_zip, find_in_dir};
use crate::gamefiles::GameSources;
use crate::install::{
    find_acclient, find_game_dir, find_proton, find_steam_proton, runtime_dir, Install,
};
use crate::patches;
use crate::servers::Server;
use crate::setup::{is_stamped, mark_stamped, Progress, Runtime, SetupStep};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::OnceLock;

/// gamescope unless the user opted out, and only if it is actually installed --
/// wrapping in a binary that is not there would just fail to spawn.
fn gamescope_enabled() -> bool {
    if std::env::var("BETTERAC_GAMESCOPE").is_ok_and(|v| v == "0") {
        return false;
    }
    on_path("gamescope")
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// BETTERAC_GAMESCOPE_ARGS replaces the lot -- including the detected resolution.
/// If you set it, you are driving, and the obvious reason to set it is to render
/// below native and upscale (`-w 2752 -h 1152 -F fsr -f`), which is what makes AC's
/// fixed-size in-engine UI readable on a HiDPI panel now that no Wine DPI knob
/// exists to turn.
fn gamescope_args(res: Option<(i32, i32)>) -> Vec<String> {
    match std::env::var("BETTERAC_GAMESCOPE_ARGS") {
        Ok(s) if !s.trim().is_empty() => s.split_whitespace().map(String::from).collect(),
        _ => gamescope_args_for(res),
    }
}

/// Launch the client. Returns once the process is spawned, not when it exits --
/// the UI stays responsive and the game owns the screen from here.
///
/// `res` is the current display mode in real pixels, detected by the frontend
/// (Mutter/GDK). It drives gamescope's `-W/-H/-w/-h`; pass `None` if it could not
/// be determined and gamescope will pick its own (see `gamescope_args_for`).
pub fn launch(
    install: &Install,
    server: &Server,
    account: &str,
    password: &str,
    res: Option<(i32, i32)>,
) -> Result<Child, String> {
    validate(server, account, password)?;

    // Pin AC's own resolution to the display before launching, or it renders at
    // its 1999 default and gamescope stretches that to fill the panel -- the
    // widescreen stretch. `BETTERAC_RESOLUTION` overrides the detected mode and
    // drives gamescope too, so the two can never disagree about the size.
    let resolution = crate::prefs::env_resolution().or(res);
    if let Some((w, h)) = resolution {
        crate::prefs::apply(&install.ac_dir, &install.prefix, (w, h), true);
    }

    // Keep the client's patches current. They ship with the app, not with the
    // prefix, so an install set up by an older build would otherwise never see a
    // patch added since -- the setup step that applies them does not run again once
    // setup is complete. `apply_all` is idempotent and only writes when something
    // actually changed, and a failure here is not worth refusing to launch over.
    let _ = crate::patches::apply_all(&install.ac_dir.join("acclient.exe"));

    let gamescope = gamescope_enabled();
    // Decal runs in front of the client and starts it itself. Only when it is both
    // switched on and actually provisioned -- a missing install must not stop the
    // game launching.
    let injector = crate::decal::launch_injector(&install.prefix);
    if injector.is_some() {
        // See the matching calls in `wine::launch`: the plugins need both of these
        // to render, and a prefix built before they existed only gets them here.
        let _ = crate::decal::ensure_runtime_config(&install.ac_dir);
        let _ = crate::decal::ensure_msil_assemblies(&install.prefix, &install.ac_dir);
    }
    let inv = invocation(
        server,
        account,
        password,
        gamescope,
        &gamescope_args(resolution),
        injector.as_deref(),
    );

    let mut cmd = Command::new(&inv.program);
    cmd.args(&inv.args)
        .current_dir(&install.ac_dir)
        .env("WINEPREFIX", &install.prefix)
        .env("STEAM_COMPAT_DATA_PATH", &install.prefix)
        .env("GAMEID", "umu-default")
        .env("PROTONPATH", &install.proton)
        .env("PROTON_VERB", "waitforexitandrun")
        .env("WINEDEBUG", "-all");

    if inv.wined3d {
        cmd.env("PROTON_USE_WINED3D", "1");
    }

    cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "{} is not installed. On Bazzite: rpm-ostree install {}",
                inv.program,
                if inv.program == "gamescope" { "gamescope" } else { "umu-launcher" }
            )
        } else {
            format!("Could not start the client: {e}")
        }
    })
}

/// Run a Windows program inside an installed prefix — the [`crate::decal`]
/// operations the settings UI performs (importing a `.reg`, mostly) need one of
/// these. Proton's own wine only works inside the Steam runtime container, so this
/// goes through umu-run exactly like the client does.
pub fn run_in_prefix(install: &Install, args: &[&str]) -> Result<(), String> {
    let status = umu_cmd(install, args).status().map_err(|e| format!("umu-run: {e}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("{} failed ({status})", args.first().unwrap_or(&"umu-run")))
}

/// Like [`run_in_prefix`], but returns as soon as the program is running rather
/// than waiting for it to exit — for Windows programs that stay up, such as Decal's
/// agent. See [`crate::wine::spawn_in_prefix`].
pub fn spawn_in_prefix(install: &Install, args: &[&str]) -> Result<(), String> {
    umu_cmd(install, args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("umu-run: {e}"))
}

/// End everything running in the prefix, so nothing outlives the app. The Proton
/// counterpart of [`crate::wine::shutdown_prefix`].
///
/// **Runs the Proton build's `wineserver` directly, not through umu-run**, and
/// that is the whole content of this function. `umu-run wineserver -k` reports
/// success and kills nothing: umu runs it inside a pressure-vessel container, so
/// the `wineserver` it signals is not the one holding the session. Measured on
/// Bazzite with Decal's agent up — after `umu-run wineserver -k && umu-run
/// wineserver -w`, both `rc=0`, the agent was still running; one direct
/// `files/bin/wineserver -k` ended it.
///
/// Best-effort throughout: a prefix with nothing running is the normal case.
pub fn shutdown_prefix(install: &Install) {
    let server = wine_bin(&install.proton).with_file_name("wineserver");
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
    // wineserver flushes the registry and rewrites `.update-timestamp`, so a reset
    // that deletes the prefix immediately after would race files reappearing
    // underneath it. `-w` waits, and returns at once when there is no server.
    run("-w");
}

/// Like [`run_in_prefix`], but returns the program's stdout. Used for `reg query`.
pub fn query_in_prefix(install: &Install, args: &[&str]) -> Result<String, String> {
    let out = umu_cmd(install, args).output().map_err(|e| format!("umu-run: {e}"))?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
        .ok_or_else(|| format!("{} failed", args.first().unwrap_or(&"umu-run")))
}

/// A umu-run command for a **utility** program in the prefix — a `reg` query, an
/// installer, `wineserver -k`. Not the client; that is built in [`launch`].
///
/// `PROTON_VERB=run` rather than the `waitforexitandrun` the client launch uses,
/// and the difference is not cosmetic: `waitforexitandrun` does not return until
/// the whole wine session has drained, so **one lingering background process hangs
/// it forever**. Measured on Bazzite with Decal's agent up — `run` returned in
/// 1.8 s, `waitforexitandrun` was still blocked at 45 s and would have stayed
/// there.
///
/// A lingering process is the normal case here, not an edge one. Decal's MSI
/// starts `DenAgent.exe` as its last act, and opening Decal's settings leaves that
/// agent running on purpose. With the old verb the settings panel's plugin query
/// would hang the moment the agent it just started was up — and so, circularly,
/// would [`shutdown_prefix`], which exists precisely to kill it.
fn umu_cmd(install: &Install, args: &[&str]) -> Command {
    let mut c = Command::new("umu-run");
    c.args(args)
        .env("WINEPREFIX", &install.prefix)
        .env("STEAM_COMPAT_DATA_PATH", &install.prefix)
        .env("GAMEID", "umu-default")
        .env("PROTONPATH", &install.proton)
        .env("PROTON_VERB", "run")
        .env("UMU_LOG", "warn")
        .env("WINEDEBUG", "-all");
    c
}

// ------------------------------------------------------------------------ setup
//
// A faithful port of install-ac.sh. Each SetupStep here does exactly what the
// numbered step in that script did, but reports Progress so a GTK progress bar
// (or, later, a SwiftUI view) can render it, and is idempotent so a failure
// halfway never redoes the 1.3 GB client install. curl/unzip/tar/cabextract are
// gone -- ureq, the zip crate and flate2/tar do that in-process -- but umu-run
// and winetricks are still shelled out to, because that is the runtime itself.

const PROTON_SERIES: &str = "GE-Proton10";

/// Proton verbs, named so the call sites read as the decision they are making.
///
/// `WAIT` blocks until the whole wine session has drained; `RUN` blocks only on
/// the program itself. Use `RUN` for anything that might leave something running
/// -- see [`umu_cmd`] for what happens otherwise.
const WAIT: &str = "waitforexitandrun";
const RUN: &str = "run";

// The VC++ 2005 SP1 URL that used to live here is gone with the Components step
// that used it; `decal::VCRUN2005_URL` is the surviving copy, fetched only when
// Decal's plugin installers actually need it.

/// The Linux runtime: GE-Proton via umu-run, under gamescope. Owns the paths and
/// sources setup needs; implements `Runtime` so the frontend drives it blind.
pub struct ProtonRuntime {
    /// The Proton prefix to build the game into.
    pub prefix: PathBuf,
    /// Where downloads are cached between runs (Proton tarball, vcredist, and the
    /// game files when fetched rather than found locally).
    pub cache: PathBuf,
    /// Where the two game files come from (local dir or the public archive URLs).
    pub games: GameSources,
    /// The GE-Proton tarball the download step fetched, remembered for the install
    /// step so it doesn't have to ask GitHub for the release URL a second time.
    tarball: OnceLock<PathBuf>,
}

impl ProtonRuntime {
    /// Defaults matching install-ac.sh: cache under XDG cache, game sources from
    /// the environment (falling back to the public archive URLs).
    pub fn new(prefix: PathBuf) -> ProtonRuntime {
        let cache = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("acinstaller");
        ProtonRuntime {
            prefix,
            cache,
            games: GameSources::from_env(),
            tarball: OnceLock::new(),
        }
    }

    /// A umu-run command carrying the same environment install-ac.sh set. Proton's
    /// bin/wine cannot be run directly; everything Windows-side goes through this.
    ///
    /// `verb` picks the Proton verb, and the choice matters — see [`umu_cmd`] for
    /// the measurement. `waitforexitandrun` waits for the whole wine session to
    /// drain, which is what prefix creation and the game installer want (they are
    /// the only thing running, and we want the session settled before the next
    /// step). Anything that may leave a background process behind must use `run`
    /// instead, or it never returns.
    fn umu(&self, proton: &Path, program: &str, verb: &str) -> Command {
        let mut c = Command::new(program);
        c.env("WINEPREFIX", &self.prefix)
            .env("STEAM_COMPAT_DATA_PATH", &self.prefix)
            .env("GAMEID", "umu-default")
            .env("PROTONPATH", proton)
            .env("PROTON_VERB", verb)
            .env("UMU_LOG", "warn")
            .env("WINEDEBUG", "-all");
        c
    }

    fn step_dependencies(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        // Bazzite is atomic: we cannot install host tools, so we can only check and
        // hand back the rpm-ostree line. curl/unzip/cabextract are no longer needed
        // -- we do those in-process now -- so only the true runtime tools remain.
        let mut missing: Vec<&str> = Vec::new();
        for (bin, pkg) in [
            ("umu-run", "umu-launcher"),
            ("gamescope", "gamescope"),
            ("winetricks", "winetricks"),
        ] {
            if !on_path(bin) {
                missing.push(pkg);
            }
        }
        if missing.is_empty() {
            on(Progress::skipped(
                SetupStep::Dependencies,
                "umu-run, gamescope and winetricks are all present",
            ));
            return Ok(());
        }
        Err(format!(
            "Missing host tools: {}.\n\nBazzite is atomic, so these go on the host image:\n  \
             rpm-ostree install {} && systemctl reboot\n\n(umu-run and gamescope normally ship with Bazzite already.)",
            missing.join(", "),
            missing.join(" ")
        ))
    }

    fn step_download_runtime(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if find_proton().is_some() {
            on(Progress::skipped(SetupStep::DownloadRuntime, "GE-Proton is already installed"));
            return Ok(());
        }
        // A build Steam has already downloaded is copied in by the install step,
        // so there is nothing to fetch. Most Bazzite boxes land here.
        if let Some(steam) = find_steam_proton() {
            on(Progress::skipped(
                SetupStep::DownloadRuntime,
                format!("{} is already installed for Steam", name_of(&steam)),
            ));
            return Ok(());
        }
        on(Progress::new(SetupStep::DownloadRuntime, 0.0, "finding the latest GE-Proton10…"));
        let url = latest_ge_proton_url()?;
        let name = url.rsplit('/').next().unwrap_or("ge-proton.tar.gz").to_string();
        let tarball = self.cache.join(&name);
        let _ = self.tarball.set(tarball.clone());
        if tarball.exists() {
            on(Progress::skipped(SetupStep::DownloadRuntime, "already downloaded"));
            return Ok(());
        }
        std::fs::create_dir_all(&self.cache).map_err(|e| e.to_string())?;
        download(&url, &tarball, SetupStep::DownloadRuntime, on)
    }

    fn step_download_client(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        self.games.fetch_installer(&self.cache, on).map(|_| ())
    }

    fn step_download_updates(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        self.games.fetch_updates(&self.cache, on).map(|_| ())
    }

    /// Put a GE-Proton build in [`runtime_dir`], which is betterAC's and nobody
    /// else's — copied from Steam's if there is one there, unpacked from the
    /// download if not.
    ///
    /// Deliberately gated on the build **existing** rather than on the stamp,
    /// unlike its neighbours. betterAC used to run straight out of Steam's
    /// `compatibilitytools.d`; an install from that era is stamped but has nothing
    /// here, and this is what migrates it. The copy is local and takes seconds.
    ///
    /// Why a private copy at all: provisioning Decal hot-patches three no-op
    /// prologues into the runtime's builtin `d3d9`/`kernel32` (see
    /// [`crate::decal`]), and a build Steam shares with every other game is not
    /// ours to modify. Owning the copy also means a GE-Proton update cannot change
    /// the runtime under a working install.
    fn step_install_runtime(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if find_proton().is_some() {
            on(Progress::skipped(SetupStep::InstallRuntime, "already installed"));
            return Ok(());
        }
        let dest = runtime_dir();
        std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;

        if let Some(steam) = find_steam_proton() {
            let name = name_of(&steam);
            on(Progress::new(
                SetupStep::InstallRuntime,
                0.3,
                format!("copying {name} out of Steam's tools (Steam's copy is left alone)…"),
            ));
            // Into a temporary name first, then renamed: an interrupted copy must
            // not leave a half-built GE-Proton that `find_proton` would then pick.
            let staging = dest.join(format!(".{name}.partial"));
            let _ = std::fs::remove_dir_all(&staging);
            copy_tree(&steam, &staging)?;
            std::fs::rename(&staging, dest.join(&name)).map_err(|e| e.to_string())?;
        } else {
            // The download step remembers what it fetched; a resumed run that
            // skipped it falls back to whatever GE-Proton tarball is sitting in the
            // cache, which is cheaper (and offline-safe) than asking GitHub again.
            let tarball = self
                .tarball
                .get()
                .cloned()
                .or_else(|| find_in_dir(&self.cache, &["ge-proton"], "tar.gz"))
                .ok_or("no GE-Proton tarball was downloaded")?;
            on(Progress::new(SetupStep::InstallRuntime, 0.3, "unpacking GE-Proton…"));
            extract_tar_gz(&tarball, &dest)?;
        }

        find_proton().ok_or_else(|| {
            format!("GE-Proton was installed but no usable build was found in {}", dest.display())
        })?;
        mark_stamped(&self.prefix, SetupStep::InstallRuntime).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn step_prefix(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if is_stamped(&self.prefix, SetupStep::Prefix) {
            on(Progress::skipped(SetupStep::Prefix, "the prefix already exists"));
            return Ok(());
        }
        let proton = find_proton().ok_or("no GE-Proton available to create the prefix")?;
        std::fs::create_dir_all(&self.prefix).map_err(|e| e.to_string())?;
        on(Progress::new(SetupStep::Prefix, 0.2, "initialising win64 prefix (first run is slow)…"));
        // wineboot first; fall back to cmd /c exit, exactly as the script did.
        let _ = self.umu(&proton, "umu-run", WAIT).args(["wineboot", "--init"]).status();
        if !self.prefix.join("drive_c").is_dir() {
            let _ = self.umu(&proton, "umu-run", WAIT).args(["cmd", "/c", "exit"]).status();
        }
        if !self.prefix.join("drive_c").is_dir() {
            return Err(format!(
                "prefix creation failed -- no drive_c at {}",
                self.prefix.join("drive_c").display()
            ));
        }
        mark_stamped(&self.prefix, SetupStep::Prefix).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Deliberately empty, exactly like the macOS counterpart.
    ///
    /// This used to run `winetricks -q vcrun2019` (slow — Microsoft's installer
    /// under Wine) and then install VC++ 2005 SP1, both inherited from
    /// install-ac.sh. **The client needs neither**, and that is measured rather
    /// than assumed: the import tables of every `.exe` and `.dll` in the installed
    /// game directory name only `msvcr70`/`msvcp70`/`msvci70`,
    /// `msvcr71`/`msvcp71`, `msvcrt` and `kernel32` — and every one of those
    /// C runtimes **ships inside the game directory itself**, delivered by
    /// `ac-updates.zip`. Nothing anywhere imports `vcruntime140`/`msvcp140`
    /// (vcrun2019) or `msvcr80`/`msvcp80` (VC++ 2005). macOS has run without them
    /// since Step Zero, on the same client, which is the corroborating half.
    ///
    /// VC++ 2005 is still installed where it is genuinely required — by
    /// [`crate::decal::ensure_vcrun2005`], because Decal's *plugin installers*
    /// call `MsiQueryProductState` for it. That is a Decal prerequisite, not a
    /// client one, and it now lives with the rest of Decal's provisioning on both
    /// platforms instead of being paid for by every Linux install.
    ///
    /// Kept as a step (and stamped) so both platforms walk the same sequence and
    /// the setup UI matches. Installs stamped by an older build are unaffected —
    /// they already have the components, and nothing removes them.
    fn step_components(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if is_stamped(&self.prefix, SetupStep::Components) {
            on(Progress::skipped(SetupStep::Components, "already installed"));
            return Ok(());
        }
        on(Progress::skipped(
            SetupStep::Components,
            "not needed — the client's C runtimes ship with the game files",
        ));
        mark_stamped(&self.prefix, SetupStep::Components).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn step_install_client(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if is_stamped(&self.prefix, SetupStep::InstallClient) {
            on(Progress::skipped(SetupStep::InstallClient, "the client is already installed"));
            return Ok(());
        }
        let proton = find_proton().ok_or("no GE-Proton available")?;
        let installer = self.games.fetch_installer(&self.cache, &mut |_| {})?;
        on(Progress::new(
            SetupStep::InstallClient,
            0.1,
            "the Asheron's Call installer is open — click through it and accept the default path",
        ));
        // The real wizard. Non-zero is tolerated; we verify by the data file.
        let _ = self.umu(&proton, "umu-run", WAIT).arg(&installer).status();
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
        // The patched acclient hard-imports these; retail ships them app-local and
        // the update zip is expected to carry them. Warn, do not fail.
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
        let proton = find_proton().ok_or("no GE-Proton available to install Decal")?;
        if crate::decal::is_installed(&self.prefix) {
            // Re-apply the parts that live outside the Decal directory -- the
            // injector and cohook ship with the app, and the runtime hot-patch lives
            // in the Proton build, so it is lost whenever that build is replaced.
            crate::decal::ensure_runtime_hooks(&self.prefix, &wine_bin(&proton))?;
            on(Progress::skipped(SetupStep::InstallDecal, "Decal is already installed"));
            return Ok(());
        }
        let game_dir = find_game_dir(&self.prefix).ok_or("game directory not found for Decal")?;
        // Everything Windows-side still goes through umu-run, exactly as the client
        // does -- `decal::install` only ever reaches the prefix through this.
        let run = |args: &[&str]| -> Result<(), String> {
            let status = self
                .umu(&proton, "umu-run", RUN)
                .args(args)
                .status()
                .map_err(|e| format!("umu-run {}: {e}", args[0]))?;
            status.success().then_some(()).ok_or_else(|| format!("{} failed ({status})", args[0]))
        };
        crate::decal::install(&self.prefix, &game_dir, &self.cache, &wine_bin(&proton), &run, on)
    }

    fn step_finalize(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if is_stamped(&self.prefix, SetupStep::Finalize) {
            on(Progress::skipped(SetupStep::Finalize, "already set up"));
            return Ok(());
        }
        let proton = find_proton().ok_or("no GE-Proton available")?;
        let game_dir = find_game_dir(&self.prefix).ok_or("game directory not found")?;
        on(Progress::new(SetupStep::Finalize, 0.5, "writing the play-ac.sh escape hatch…"));
        write_play_script(&self.prefix, &proton, &game_dir)?;
        mark_stamped(&self.prefix, SetupStep::Finalize).map_err(|e| e.to_string())?;
        Ok(())
    }
}

impl Runtime for ProtonRuntime {
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
        Install::discover(&self.prefix)
    }
}

// ----------------------------------------------------------------- setup helpers

/// A path's last component, for progress messages.
fn name_of(p: &Path) -> String {
    p.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

/// The wine binary inside a Proton build.
///
/// The client is never launched with this — that goes through umu-run, because
/// Proton's wine has no 32-bit GL outside the Steam runtime container and
/// acclient.exe dies instantly. But two things Decal needs are not the client and
/// do not touch GL:
///
///   * **winetricks**, which drives Microsoft's .NET 4.8 installer and wants
///     `WINE`/`WINESERVER` to point at a binary it can run itself. Verified
///     working against GE-Proton10-34 on Bazzite: real `clr.dll` installs.
///   * **the engine hot-patch**, which only needs the path to locate
///     `lib/wine/i386-windows` beside it.
///
/// `files/bin/wine` is the classic multilib build and is a genuine 32-bit ELF,
/// which is what makes the first of those work at all.
fn wine_bin(proton: &Path) -> PathBuf {
    proton.join("files/bin/wine")
}

/// Recursively copy `src` to `dst`.
///
/// A Proton build is not a plain file tree: it is ~1.5 GB containing symlinks
/// (`lib64`, and the DXVK/VKD3D DLLs) and executables whose mode matters.
/// `std::fs::copy` carries permissions but **follows** symlinks, which would both
/// bloat the copy and break links that deliberately point at siblings — so links
/// are recreated as links, verbatim. Directory symlinks are recreated too rather
/// than descended, which is also what stops a link cycle running away.
fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    let fail = |what: &str, p: &Path, e: std::io::Error| format!("{what} {}: {e}", p.display());
    std::fs::create_dir_all(dst).map_err(|e| fail("creating", dst, e))?;
    let entries = std::fs::read_dir(src).map_err(|e| fail("reading", src, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| fail("reading", src, e))?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        // symlink_metadata, not metadata: we are asking about the link itself.
        let meta = std::fs::symlink_metadata(&from).map_err(|e| fail("reading", &from, e))?;
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&from).map_err(|e| fail("reading", &from, e))?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &to).map_err(|e| fail("linking", &to, e))?;
        } else if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| fail("copying", &from, e))?;
        }
    }
    Ok(())
}

/// The newest GE-Proton10 x86_64 .tar.gz on GitHub. Same query the script ran:
/// scan recent releases, take the first matching asset, skip the aarch64 trap.
fn latest_ge_proton_url() -> Result<String, String> {
    let body = ureq::get(
        "https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases?per_page=100",
    )
    .set("User-Agent", "betterac")
    .call()
    .map_err(|e| format!("could not reach the GitHub releases API: {e}"))?
    .into_string()
    .map_err(|e| e.to_string())?;

    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let releases = json.as_array().ok_or("unexpected GitHub API response")?;
    for rel in releases {
        let Some(assets) = rel.get("assets").and_then(|a| a.as_array()) else { continue };
        for a in assets {
            if let Some(url) = a.get("browser_download_url").and_then(|u| u.as_str()) {
                if url.contains(PROTON_SERIES) && url.ends_with(".tar.gz") && !url.contains("aarch64") {
                    return Ok(url.to_string());
                }
            }
        }
    }
    Err(format!("could not find a {PROTON_SERIES} x86_64 release"))
}

/// Write the direct-launch escape hatch. Byte-for-byte the script install-ac.sh
/// produced, with the three install-specific paths filled in.
fn write_play_script(prefix: &Path, proton: &Path, ac_dir: &Path) -> Result<(), String> {
    let script = PLAY_SCRIPT_TEMPLATE
        .replace("@@PREFIX@@", &prefix.display().to_string())
        .replace("@@PROTON@@", &proton.display().to_string())
        .replace("@@ACDIR@@", &ac_dir.display().to_string());
    let path = prefix.join("play-ac.sh");
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
# Launch Asheron's Call directly, bypassing the launcher.
#
#   ACE servers   ./play-ac.sh -a ACCOUNT -v PASSWORD -h HOST:PORT
#   GDLE servers  ./play-ac.sh -h HOST -p PORT -a ACCOUNT:PASSWORD
#
# Goes through umu-run: Proton's wine only works inside the Steam runtime.
#
# Runs inside gamescope, and therefore with DXVK off. Those are one decision, not
# two: wined3d's "Cannot initialize Direct3D" was it failing to enumerate a display
# adapter on a bare Wayland session, and gamescope -- a nested compositor -- gives
# it one. AC is D3D9 from 1999; it does not need Vulkan translation to draw.
#
#   AC_GAMESCOPE=0        no gamescope, and DXVK comes back on (the old setup)
#   AC_GAMESCOPE_ARGS=..  gamescope flags; replaces everything below, including the
#                         detected resolution. Upscale a HiDPI panel by rendering
#                         small: '-w 2752 -h 1152 -F fsr -f'
set -euo pipefail
export WINEPREFIX="@@PREFIX@@"
export STEAM_COMPAT_DATA_PATH="@@PREFIX@@"
export GAMEID=umu-default
export PROTONPATH="@@PROTON@@"
export PROTON_VERB=waitforexitandrun
export WINEDEBUG=-all
cd "@@ACDIR@@"

# The current display mode, in real pixels. Mutter first (GNOME is the host, and
# DisplayConfig reports the real hardware mode); xrandr is the fallback.
detect_res() {
  local res=""
  if command -v gdbus >/dev/null; then
    res="$(gdbus call --session \
              --dest org.gnome.Mutter.DisplayConfig \
              --object-path /org/gnome/Mutter/DisplayConfig \
              --method org.gnome.Mutter.DisplayConfig.GetCurrentState 2>/dev/null \
            | tr '(' '\n' \
            | grep -m1 "'is-current': <true>" \
            | sed -E "s/^'([0-9]+)x([0-9]+).*/\1 \2/")"
  fi
  if [[ ! "$res" =~ ^[0-9]+\ [0-9]+$ ]] && command -v xrandr >/dev/null; then
    res="$(xrandr 2>/dev/null | awk '/\*/ {split($1, a, "x"); print a[1], a[2]; exit}')"
  fi
  if [[ "$res" =~ ^[0-9]+\ [0-9]+$ ]]; then
    printf '%s' "$res"
  fi
  return 0
}

GS="${AC_GAMESCOPE:-1}"
if [[ "$GS" != "0" ]] && command -v gamescope >/dev/null; then
  # DXVK off: only safe because gamescope is providing the display.
  export PROTON_USE_WINED3D=1

  if [[ -n "${AC_GAMESCOPE_ARGS:-}" ]]; then
    read -ra GS_ARGS <<<"$AC_GAMESCOPE_ARGS"
  else
    # Nested gamescope defaults to 1280x720, NOT your screen. -W/-H is the window;
    # -w/-h is the resolution the game is told it has. Both at the current mode =
    # native, 1:1, no scaling.
    GS_ARGS=()
    RES="$(detect_res)"
    if [[ -n "$RES" ]]; then
      read -r RW RH <<<"$RES"
      GS_ARGS+=(-W "$RW" -H "$RH" -w "$RW" -h "$RH")
    else
      echo "play-ac.sh: could not detect the display resolution; gamescope will" >&2
      echo "            pick its own (1280x720). Set AC_GAMESCOPE_ARGS to override." >&2
    fi
    GS_ARGS+=(-f --force-grab-cursor)
  fi

  exec gamescope "${GS_ARGS[@]}" -- umu-run acclient.exe "$@" -rodat off
fi

# No gamescope: leave Proton alone so DXVK stays on.
exec umu-run acclient.exe "$@" -rodat off
"##;
