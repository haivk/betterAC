//! Running acclient.exe under Proton on Linux.
//!
//! The client always goes through umu-run. Proton's bin/wine cannot be invoked
//! directly: it is a new-WoW64 build with no 32-bit GL outside the Steam runtime
//! container, and acclient.exe dies instantly.
//!
//! Display-resolution detection is NOT here -- it needs a toolkit (Mutter over
//! D-Bus, or GDK) and so lives in the GTK frontend, which passes the result in.
//! Keeping it out is what lets this crate build on macOS too.

use crate::args::{gamescope_args_for, invocation, validate};
use crate::fetch::{download, extract_tar_gz, extract_zip, find_in_dir};
use crate::gamefiles::GameSources;
use crate::install::{find_game_dir, find_proton, steam_compat, Install};
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

    let gamescope = gamescope_enabled();
    let inv = invocation(server, account, password, gamescope, &gamescope_args(res));

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

// ------------------------------------------------------------------------ setup
//
// A faithful port of install-ac.sh. Each SetupStep here does exactly what the
// numbered step in that script did, but reports Progress so a GTK progress bar
// (or, later, a SwiftUI view) can render it, and is idempotent so a failure
// halfway never redoes the 1.3 GB client install. curl/unzip/tar/cabextract are
// gone -- ureq, the zip crate and flate2/tar do that in-process -- but umu-run
// and winetricks are still shelled out to, because that is the runtime itself.

const PROTON_SERIES: &str = "GE-Proton10";

// VC++ 2005 SP1, from the Wayback Machine because the original Microsoft URL is
// long dead. Matches the one install-ac.sh fetched.
const VCREDIST_2005_URL: &str = "https://web.archive.org/web/20190419092632/https://download.microsoft.com/download/e/1/c/e1c773de-73ba-494a-a5ba-f24906ecf088/vcredist_x86.exe";

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
    fn umu(&self, proton: &Path, program: &str) -> Command {
        let mut c = Command::new(program);
        c.env("WINEPREFIX", &self.prefix)
            .env("STEAM_COMPAT_DATA_PATH", &self.prefix)
            .env("GAMEID", "umu-default")
            .env("PROTONPATH", proton)
            .env("PROTON_VERB", "waitforexitandrun")
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

    fn step_install_runtime(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if is_stamped(&self.prefix, SetupStep::InstallRuntime) || find_proton().is_some() {
            on(Progress::skipped(SetupStep::InstallRuntime, "already installed"));
            return Ok(());
        }
        // The download step remembers what it fetched; a resumed run that skipped
        // it falls back to whatever GE-Proton tarball is sitting in the cache,
        // which is cheaper (and offline-safe) compared to asking GitHub again.
        let tarball = self
            .tarball
            .get()
            .cloned()
            .or_else(|| find_in_dir(&self.cache, &["ge-proton"], "tar.gz"))
            .ok_or("no GE-Proton tarball was downloaded")?;
        on(Progress::new(SetupStep::InstallRuntime, 0.3, "unpacking GE-Proton…"));
        let compat = steam_compat();
        std::fs::create_dir_all(&compat).map_err(|e| e.to_string())?;
        extract_tar_gz(&tarball, &compat)?;
        find_proton().ok_or("GE-Proton unpacked but no usable build was found")?;
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
        let _ = self.umu(&proton, "umu-run").args(["wineboot", "--init"]).status();
        if !self.prefix.join("drive_c").is_dir() {
            let _ = self.umu(&proton, "umu-run").args(["cmd", "/c", "exit"]).status();
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

    fn step_components(&self, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
        if is_stamped(&self.prefix, SetupStep::Components) {
            on(Progress::skipped(SetupStep::Components, "already installed"));
            return Ok(());
        }
        let proton = find_proton().ok_or("no GE-Proton available")?;
        // Non-zero exit from winetricks is common and usually still installs; the
        // script tolerated it and so do we.
        on(Progress::new(SetupStep::Components, 0.1, "winetricks vcrun2019 (slow, be patient)…"));
        let _ = self.umu(&proton, "umu-run").args(["winetricks", "-q", "vcrun2019"]).status();

        on(Progress::new(SetupStep::Components, 0.7, "VC++ 2005 SP1…"));
        std::fs::create_dir_all(&self.cache).map_err(|e| e.to_string())?;
        let vc = self.cache.join("vcredist_x86.exe");
        if !vc.exists() {
            download(VCREDIST_2005_URL, &vc, SetupStep::Components, on)?;
        }
        let _ = self.umu(&proton, "umu-run").arg(&vc).arg("/Q").status();

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
        let _ = self.umu(&proton, "umu-run").arg(&installer).status();
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
            SetupStep::Finalize => self.step_finalize(on),
        }
    }

    fn discover(&self) -> Result<Install, String> {
        Install::discover(&self.prefix)
    }
}

// ----------------------------------------------------------------- setup helpers

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
