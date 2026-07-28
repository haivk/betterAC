//! Decal — the Asheron's Call plugin framework — provisioned without its installer.
//!
//! Decal ships as an MSI that wants an administrative install, drops assemblies in
//! the GAC, and leaves you configuring plugins through DenAgent, its MFC config GUI.
//! None of that fits a self-contained prefix, and DenAgent is the only piece that
//! needs MFC at all. So betterAC provisions Decal itself and replaces the agent:
//!
//!   * **Files** come out of the MSI with `msiexec /a` (an administrative install
//!     unpacks the payload without running the install sequence).
//!   * **Registration** is a plain `.reg` import. Decal's COM registration is not
//!     self-registration — the MSI carries all ~1900 rows in its `Registry` table —
//!     so there is no `regsvr32`, no `RegAsm`, no GAC and **no .NET** involved in
//!     getting the native framework working. See `helpers/decal/genreg.py`.
//!   * **Plugins** are registry keys under `Software\Decal\Plugins\{CLSID}`, one
//!     `Enabled` DWORD each. That is the entire "manage plugins" feature, and it is
//!     why this module can offer it without shipping Decal's GUI.
//!
//! Two things bite here and are worth knowing before touching this file:
//!
//! **The prefix is 64-bit and Decal is 32-bit.** Wine applies WoW64 registry
//! redirection in the *server*, so a 32-bit COM client reads
//! `Software\Classes\Wow6432Node\CLSID\…` while a 64-bit tool reads the plain path.
//! Everything written here goes to both views, and everything *read* names the
//! `Wow6432Node` path explicitly — a bare `HKLM\Software\Decal` query runs through
//! the 64-bit `reg.exe` and cheerfully reports keys the client cannot see.
//!
//! **Reads go through Wine, not through `system.reg`.** wineserver keeps the
//! registry in memory and flushes it on its own schedule, so the file can lag a
//! write by an unbounded amount — long enough that a toggle-then-refresh in the
//! settings UI read back the old value. Asking wineserver is authoritative and
//! immediate; it costs one process spawn, which a settings panel can afford.
//!
//! **Decal validates the client against data files it normally downloads.** Without
//! them it refuses to start with *"Your AC Client is out of date … xml
//! version=0.0.0.0"*, so [`install`] seeds them.
//!
//! **Plugin installers are older than the runtime we install.** Several are CLR 2
//! executables, and CLR 4 does not satisfy a CLR 2 request — see
//! [`allow_old_clr_apps`], which is why they run at all.

use crate::setup::{Progress, SetupStep};
use std::path::{Path, PathBuf};

/// The registry template, generated from the pinned MSI by
/// `helpers/decal/genreg.py`. Held as UTF-8; [`apply_reg`] re-encodes it.
const REG_TEMPLATE: &str = include_str!("../../helpers/decal/decal.reg.in");

/// The injector that starts the client with Decal loaded (`helpers/decinject.c`).
/// Committed prebuilt because mingw-w64 is not a standard build dependency.
const DECINJECT_EXE: &[u8] = include_bytes!("../../helpers/decinject.exe");

/// `cohook.dll` (`helpers/cohook.c`): gives `ole32!CoInitialize` an MSVC-hotpatch
/// façade so Decal's native COM-init hook installs under Wine. Loaded by the
/// injector before Decal's own startup. See [`deploy_cohook`].
const COHOOK_DLL: &[u8] = include_bytes!("../../helpers/cohook.dll");

/// The `winetricks` script, pinned to the version verified to install .NET 4.8 on
/// this engine. Bundled so the user needs no separate install; run only when the
/// user opts into Decal. See [`ensure_dotnet`].
const WINETRICKS: &[u8] = include_bytes!("../../helpers/dotnet/winetricks");

/// The real Microsoft `mscoree.dll` CLR shim (x86 → syswow64, x64 → system32),
/// carved from the .NET 4.0 payload. winetricks' `remove_mono` deletes the copy
/// the .NET installer lays down and the 4.8 update doesn't re-ship it (it's an
/// OS-inbox file on real Windows), so we restore it. These are Microsoft
/// redistributables — review their terms before shipping betterAC publicly.
const MSCOREE_X86: &[u8] = include_bytes!("../../helpers/dotnet/mscoree32.dll");
const MSCOREE_X64: &[u8] = include_bytes!("../../helpers/dotnet/mscoree64.dll");

/// 32-bit `mfc140u.dll` (14.44.35211.0), the one import [`DenAgent.exe`][agent_exe]
/// has that the prefix does not otherwise get: nothing else we install ships MFC,
/// so without this the agent dies at load with `c0000135`. Carved from Microsoft's
/// `vc_redist.x86.exe` (VC++ 2015–2022, SHA-256
/// `0c09f2611660441084ce0df425c51c11e147e6447963c3690f97e0b25c55ed64`) with
/// `cabextract -F a11`, which is what winetricks' `mfc140` verb does — embedded
/// rather than downloaded so no host-side cab tool is needed, and because a stock
/// Mac has none. Same licensing caveat as the mscoree shims above.
///
/// Only the Unicode build is here: `DenAgent.exe` imports `mfc140u.dll` alone, and
/// it starts with the ANSI/managed siblings absent (verified on this engine).
const MFC140U_X86: &[u8] = include_bytes!("../../helpers/decal/mfc140u.dll");

/// Decal 3.0 (2.9.8.3). Pinned like the Wine engine: a fixed URL plus the hash of
/// the exact file the registry template was generated from, so a different build
/// can never be silently paired with a template that does not describe it.
const DEFAULT_MSI_URL: &str = "https://www.decaldev.com/releases/2983/Decal.msi";
const DEFAULT_MSI_SHA256: &str =
    "101365ba4378be20d9ab57ba9f1c1deda5f93bb1b7bdb511da836c9a69a31f26";

/// The Decal.Adapter surrogate — the COM object that hosts managed plugins. A
/// plugin declared with `Assembly`/`Path`/`Object` names this as its `Surrogate`.
const ADAPTER_SURROGATE: &str = "{71A69713-6593-47EC-0002-0000000DECA1}";

/// Microsoft's DirectX June 2010 redistributable — ~100 MB, and the only place
/// Managed DirectX is distributed. Pinned by hash like everything else we fetch;
/// this is the same URL and digest winetricks' `mdx` verb uses, that project's
/// mirror having outlived Microsoft's own download page.
const DIRECTX_URL: &str =
    "https://files.holarse-linuxgaming.de/mirrors/microsoft/directx_Jun2010_redist.exe";
const DIRECTX_SHA256: &str =
    "8746ee1a84a083a90e37899d71d50d5c7c015e69688a466aa80447f011780c0d";

/// The public-key token every Managed DirectX assembly is strong-named with. Part
/// of the GAC directory name, so it has to match exactly.
const MDX_TOKEN: &str = "31bf3856ad364e35";

/// Microsoft's Visual C++ 2005 SP1 redistributable, MFC security update
/// (KB2538242) — the newest servicing of the VC80 runtime, and the same package
/// and digest winetricks' `vcrun2005` verb installs. 2.6 MB, straight from
/// Microsoft.
const VCRUN2005_URL: &str =
    "https://download.microsoft.com/download/8/B/4/8B42259F-5D70-43F4-AC2E-4B208FD8D66A/vcredist_x86.EXE";
const VCRUN2005_SHA256: &str =
    "8648c5fc29c44b9112fe52f9a33f80e7fc42d10f3b5b42b2121542a13e44adfd";

/// The product code the package above registers itself under: "Microsoft Visual
/// C++ 2005 Redistributable" 8.0.61001.
const VC80_PRODUCT_CODE: &str = "{710f4c1c-cc18-4c49-8cbf-51240c89a1a2}";

/// The product code Virindi's installer — and other plugin installers of that
/// vintage — actually look for. See [`ensure_vcrun2005`] for why the two differ
/// and why we bridge them.
const VC80_LEGACY_PRODUCT_CODE: &str = "{7299052B-02A4-4627-81F2-1818DA5D550D}";

/// The DLLs the VC80 runtime provides, which Wine also has builtins for. `native,
/// builtin` prefers Microsoft's without breaking anything that needs the builtin
/// as a fallback — the ordering winetricks settled on after several bug reports.
const VC80_OVERRIDES: [&str; 5] = ["atl80", "msvcm80", "msvcp80", "msvcr80", "vcomp"];

/// Microsoft's `d3dx9_30.dll`, carved from the same redistributable as Managed
/// DirectX. Pinned by hash so what lands in the prefix is the build this was
/// verified against.
///
/// **This is what makes plugin windows draw.** Both Managed DirectX's `Direct3DX`
/// and Decal's own `D3DService.dll` import `d3dx9_30.dll` by name, and Wine's
/// builtin reimplementation of it is not sufficient for the way they use it:
/// plugin windows appeared with correct layout and correct geometry but no
/// content — every texture and glyph blank — until this was in place. Nothing
/// reports an error along the way, which is what made it expensive to find.
const D3DX9_30_SHA256: &str =
    "5edeed79f2359527a55b8189cfa8b9b121cd608d44eead905a0f3436938ad532";
/// The cabinet inside the redistributable that carries it, and the DLL's name.
const D3DX9_30_CAB: &str = "d3dx9_30_x86.cab";
const D3DX9_30_DLL: &str = "d3dx9_30.dll";

/// The MDX versions shipped as per-version cabinets in the redistributable, plus
/// the two the base cabinet carries. `1.0.2902.0` is what `Microsoft.DirectX` and
/// `Microsoft.DirectX.Direct3D` bind as, and `1.0.2911.0` is
/// `Microsoft.DirectX.Direct3DX` — the exact pair Virindi's installer probes for.
const MDX_BASE_VERSION: &str = "1.0.2902.0";
const MDX_D3DX_VERSION: &str = "1.0.2911.0";

/// Decal's overlay (the switchbar) and its plugins are driven by a managed .NET
/// adapter that talks to the native core over **COM connection-point events**.
/// wine-mono cannot host that — it asserts and aborts (`cominterop.c`,
/// `MONO_CLASS_IS_INTERFACE_INTERNAL`) the moment the adapter subscribes, in every
/// wine-mono 8→11. So Decal needs the **real Microsoft .NET Framework 4.8**, which
/// we install with the bundled winetricks (`dotnet48`) — it removes wine-mono, sets
/// the prefix to Windows 7, and runs Microsoft's own installers. Four more fixes on
/// top make the adapter actually load; see [`ensure_dotnet`] and
/// [`ensure_msil_assemblies`].
const DOTNET_VERB: &str = "dotnet48";

/// Run a Windows program inside the prefix.
///
/// Supplied by the caller because the two runtimes get there differently: macOS
/// invokes the engine's `wine` by full path, while Proton's wine only works inside
/// the Steam runtime container and has to go through `umu-run`.
pub type RunInPrefix<'a> = &'a dyn Fn(&[&str]) -> Result<(), String>;

/// Like [`RunInPrefix`], but hands back the program's stdout. Used for `reg query`,
/// which is how plugin state is read.
pub type QueryInPrefix<'a> = &'a dyn Fn(&[&str]) -> Result<String, String>;

/// Where the installer comes from. Mirrors [`crate::gamefiles::GameSources`]: a
/// local file short-circuits the download so a dev never re-fetches.
pub struct MsiSource {
    /// A local Decal.msi (`AC_DECAL_MSI`), used as-is and not hash-checked.
    pub local: Option<PathBuf>,
    /// Where to fetch it from (`AC_DECAL_MSI_URL`, else the pinned default).
    pub url: String,
    /// Expected SHA-256. `None` when the URL was overridden, since we only know
    /// the hash of the pinned build.
    pub sha256: Option<String>,
}

impl MsiSource {
    pub fn from_env() -> MsiSource {
        match std::env::var("AC_DECAL_MSI_URL").ok().filter(|s| !s.trim().is_empty()) {
            Some(url) => MsiSource {
                local: std::env::var_os("AC_DECAL_MSI").map(PathBuf::from),
                url,
                sha256: None,
            },
            None => MsiSource {
                local: std::env::var_os("AC_DECAL_MSI").map(PathBuf::from),
                url: DEFAULT_MSI_URL.to_string(),
                sha256: Some(DEFAULT_MSI_SHA256.to_string()),
            },
        }
    }

    /// The MSI on disk, downloading and verifying it if we don't have it yet.
    fn fetch(&self, cache: &Path, on: &mut dyn FnMut(Progress)) -> Result<PathBuf, String> {
        if let Some(local) = &self.local {
            if local.is_file() {
                return Ok(local.clone());
            }
            return Err(format!("AC_DECAL_MSI points at {}, which is not a file", local.display()));
        }
        let dest = cache.join("Decal.msi");
        if dest.exists() {
            return Ok(dest);
        }
        std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
        // Through fetch_small, not the streaming downloader: decaldev.com only
        // speaks CBC TLS, which rustls (the downloader's backend) cannot negotiate,
        // so this needs the curl fallback. The MSI is ~2 MB, so buffering it in
        // memory costs nothing and a progress bar would barely flicker.
        on(Progress::new(SetupStep::InstallDecal, 0.2, "downloading Decal…"));
        let bytes = crate::fetch::get_bytes(&self.url).map_err(|e| format!("downloading Decal: {e}"))?;
        std::fs::write(&dest, &bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
        if let Some(expected) = &self.sha256 {
            if let Err(e) = crate::fetch::verify_sha256(&dest, expected) {
                let _ = std::fs::remove_file(&dest);
                return Err(e);
            }
        }
        Ok(dest)
    }
}

/// One plugin as Decal sees it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DecalPlugin {
    /// Braced, upper-case CLSID — the key name under `Software\Decal\Plugins`.
    pub clsid: String,
    /// Friendly name (the key's default value).
    pub name: String,
    pub enabled: bool,
}

/// Where Decal's files live inside the prefix. A 32-bit MSI on a 64-bit Windows
/// installs to the x86 Program Files, and the registry template hardcodes the
/// same path, so the two must not drift.
pub fn install_dir(prefix: &Path) -> PathBuf {
    prefix.join("drive_c/Program Files (x86)/Decal 3.0")
}

/// Has Decal been provisioned into this prefix? Checks a native core DLL rather
/// than the directory, so a half-extracted install does not read as done.
pub fn is_installed(prefix: &Path) -> bool {
    install_dir(prefix).join("Decal.dll").is_file()
}

/// Where the injector is staged, beside Decal's own files.
pub fn injector_path(prefix: &Path) -> PathBuf {
    install_dir(prefix).join("decinject.exe")
}

/// `DenAgent.exe` — Decal's own configuration UI, laid down by the MSI.
///
/// betterAC does not use it to *manage* plugins (that is registry work this module
/// does directly), but it is the only interface to everything else Decal exposes,
/// so settings offers a way to open it. See [`open_settings`].
pub fn agent_exe(prefix: &Path) -> PathBuf {
    install_dir(prefix).join("DenAgent.exe")
}

/// Launch Decal's agent, after making sure the MFC runtime it needs is present.
///
/// **The agent has no main window.** It registers a `Shell_NotifyIcon` tray icon
/// and shows its dialog only when that is clicked — under Wine on macOS the icon
/// becomes a menu-bar status item, which is the entire UI. So this returns as soon
/// as the process is spawned rather than waiting for it to exit, and the caller is
/// expected to tell the user where to look.
///
/// `spawn` must start the program **without waiting**: the agent runs until it is
/// asked to quit, so a blocking run would hang the caller forever.
pub fn open_settings(prefix: &Path, spawn: RunInPrefix) -> Result<(), String> {
    let exe = agent_exe(prefix);
    if !exe.is_file() {
        return Err("Decal's agent (DenAgent.exe) is not in the prefix".into());
    }
    install_mfc140(prefix)?;
    spawn(&[&windows_path(prefix, &exe)])?;
    AGENT_STARTED.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// Did this session start Decal's agent? See [`agent_started`].
static AGENT_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether [`open_settings`] has started Decal's agent during this run of the app.
///
/// This is what makes tearing the prefix down at quit safe. The teardown exists
/// for exactly one thing — the agent is deliberately left running so its dialog
/// stays reachable, and its tray icon is owned by the prefix's `explorer.exe`
/// rather than by the agent, so quitting without ending the session strands an
/// icon nothing will ever clear.
///
/// But `wineserver -k` does not discriminate: it ends **everything** in the
/// prefix, and that includes the game. Quitting the launcher while playing would
/// have killed the session. Since the leak can only exist if the agent was
/// started, and the agent can only be started from the settings panel, this flag
/// is a precise answer to "is there anything that needs ending?" — and it is false
/// for every user who never opens Decal's settings, which is almost all of them.
pub fn agent_started() -> bool {
    AGENT_STARTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Install a Decal plugin by running its own installer, picked out of the user's
/// Mac filesystem. Handles both forms plugins ship in: an `.msi`, or a plain
/// `.exe` (NSIS and Inno Setup are both common).
///
/// Running the author's installer is the only thing that reliably gets a plugin
/// registered the way it intended — it writes its own COM registration, drops its
/// files where its config expects them, and handles versions of itself already
/// present. betterAC's job is just to put the file somewhere Wine can see and start
/// it.
///
/// Three things make that less trivial than it sounds:
///
///   * **The prefix maps no host drives.** `contain_drives` strips `Z:` and
///     everything else that reaches outside, so a `/Users/...` path is unreachable
///     from inside Wine no matter how it is spelled. The installer is copied into
///     `C:` first and the Windows path of *that* is what gets run.
///   * **The installer runs with its own UI.** No `/qn` and no silent switches:
///     these are third-party packages that may want a licence accepted or a
///     directory chosen, and silencing them turns a question into a failure. So
///     this blocks until the user has finished — call it off the main thread.
///   * **An `.exe` is launched directly, an `.msi` through `msiexec`.** An MSI is
///     data, not a program; Wine will not execute one.
///
/// The staged copy is removed afterwards whether or not the install worked, so a
/// cancelled run leaves nothing behind.
pub fn install_plugin(prefix: &Path, installer: &Path, run: RunInPrefix) -> Result<(), String> {
    if !installer.is_file() {
        return Err(format!("{} is not a file", installer.display()));
    }
    let ext = installer
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(ext.as_str(), "msi" | "exe" | "zip") {
        return Err("a plugin installer must be a .zip, .msi or .exe".into());
    }

    // A directory of our own rather than the root of C:, so the staged copy can
    // keep its original name (the installer's UI shows it) without any chance of
    // landing on something already there.
    let dir = prefix.join("drive_c/betterac-plugin");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let staged = if ext == "zip" {
        unpack_installer_archive(installer, &dir)?
    } else {
        stage_installer(installer, &dir)?
    };
    let ext = staged
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    // Cheap next to the installer that follows, and done here rather than only at
    // setup so prefixes built before this existed get it too.
    allow_old_clr_apps(prefix, run)?;

    let win = windows_path(prefix, &staged);
    let result =
        if ext == "msi" { run(&["msiexec", "/i", &win]) } else { run(&[&win]) };
    let _ = std::fs::remove_dir_all(&dir);

    result.map_err(cancelled_or)
}

/// Is the VC80 runtime in this prefix? Checks the side-by-side assembly store,
/// which is where a manifest-dependent binary actually resolves it from, rather
/// than a registry key that only records that an installer ran.
pub fn is_vcrun2005_installed(prefix: &Path) -> bool {
    let sxs = prefix.join("drive_c/windows/winsxs");
    std::fs::read_dir(sxs).into_iter().flatten().flatten().any(|e| {
        let n = e.file_name().to_string_lossy().to_ascii_lowercase();
        n.starts_with("x86_") && n.contains("microsoft.vc80.crt")
    })
}

/// Install the **Visual C++ 2005 runtime**, which plugin installers of this era
/// require. Virindi's refuses to continue without it.
///
/// The runtime is installed by running Microsoft's own redistributable, because
/// VC80 binaries find their CRT through **side-by-side assembly manifests** — a
/// loose `msvcr80.dll` dropped in `system32` does not satisfy that, so the real
/// installer, which populates `winsxs`, is the only thing that works.
///
/// # The product-code mismatch
///
/// The redistributable registers itself as `8.0.61001` under
/// [`VC80_PRODUCT_CODE`]. Installers of Virindi's vintage instead probe for
/// [`VC80_LEGACY_PRODUCT_CODE`], an earlier product code for the same
/// redistributable — so a *serviced* install legitimately fails their check while
/// providing everything they need, and more recently than they asked for.
///
/// The alternative was to install the pre-update package that carries the expected
/// code, which would mean downgrading the CRT past the MFC security fix
/// (KB2538242) that the newer one exists to deliver. So instead the legacy code is
/// registered as an alias alongside the real entry, describing the runtime that is
/// genuinely present. Nothing is faked about the runtime itself — only the
/// identifier an old check looks under.
pub fn ensure_vcrun2005(
    prefix: &Path,
    cache: &Path,
    run: RunInPrefix,
    on: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    if !is_vcrun2005_installed(prefix) {
        std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
        let redist = cache.join("vcredist_x86_2005sp1.exe");
        if !redist.is_file() {
            on(Progress::new(
                SetupStep::InstallDecal,
                0.98,
                "downloading the Visual C++ 2005 runtime…",
            ));
            crate::fetch::download(VCRUN2005_URL, &redist, SetupStep::InstallDecal, on)?;
        }
        if let Err(e) = crate::fetch::verify_sha256(&redist, VCRUN2005_SHA256) {
            let _ = std::fs::remove_file(&redist);
            return Err(e);
        }

        // Prefer Microsoft's DLLs over Wine's builtins before the installer runs,
        // so what it lays down is what actually gets loaded.
        let mut reg = String::from("Windows Registry Editor Version 5.00\r\n\r\n\
             [HKEY_CURRENT_USER\\Software\\Wine\\DllOverrides]\r\n");
        for dll in VC80_OVERRIDES {
            reg.push_str(&format!("\"{dll}\"=\"native,builtin\"\r\n"));
        }
        reg.push_str("\r\n");
        apply_reg(prefix, &reg, run)?;

        on(Progress::new(
            SetupStep::InstallDecal,
            0.99,
            "installing the Visual C++ 2005 runtime…",
        ));
        let dir = prefix.join("drive_c/betterac-vc");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let staged = dir.join("vcredist_x86.exe");
        std::fs::copy(&redist, &staged)
            .map_err(|e| format!("staging the VC++ runtime installer: {e}"))?;
        let result = run(&[&windows_path(prefix, &staged), "/q"]);
        let _ = std::fs::remove_dir_all(&dir);
        result?;

        if !is_vcrun2005_installed(prefix) {
            return Err("the Visual C++ 2005 runtime did not install (nothing in winsxs)".into());
        }
    }
    register_vc80_legacy_code(prefix, run)
}

/// Record the older product code plugin installers check for, against the runtime
/// that is actually installed. See [`ensure_vcrun2005`].
///
/// These installers ask `msi.dll!MsiQueryProductState`, **not** the Add/Remove
/// Programs list — so an `Uninstall\{code}` key, which is the obvious place to put
/// this, has no effect at all. What that call reads is the Windows Installer
/// product database, under a *packed* form of the GUID: a product key, and an
/// `InstallProperties` whose `WindowsInstaller` value is what turns the answer from
/// "advertised" into "installed". Both are written here, mirroring exactly what the
/// real redistributable's own registration looks like.
///
/// The Add/Remove entry is written too, since it costs nothing and is where a
/// human, or a differently-written installer, would look.
fn register_vc80_legacy_code(prefix: &Path, run: RunInPrefix) -> Result<(), String> {
    let packed = pack_product_code(VC80_LEGACY_PRODUCT_CODE)
        .ok_or("could not pack the VC++ 2005 product code")?;
    let name = "Microsoft Visual C++ 2005 Redistributable";
    let note = format!(
        "Registered by betterAC against {VC80_PRODUCT_CODE}, which is the runtime actually installed."
    );

    let mut reg = String::from("Windows Registry Editor Version 5.00\r\n\r\n");
    // Both views for Add/Remove: these installers are 32-bit, but a managed one
    // built AnyCPU runs 64-bit in this prefix and would read the unredirected key.
    for base in REG_VIEWS {
        reg.push_str(&format!(
            "[HKEY_LOCAL_MACHINE\\{base}\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{VC80_LEGACY_PRODUCT_CODE}]\r\n\
             @=\"{name}\"\r\n\
             \"DisplayName\"=\"{name}\"\r\n\
             \"DisplayVersion\"=\"8.0.61001\"\r\n\
             \"Comments\"=\"{note}\"\r\n\r\n"
        ));
    }
    // The product key and its install properties, at the unredirected paths — which
    // is where Wine's msi puts a 32-bit installer's own registration, so it is where
    // it looks for one too.
    reg.push_str(&format!(
        "[HKEY_LOCAL_MACHINE\\Software\\Classes\\Installer\\Products\\{packed}]\r\n\
         \"ProductName\"=\"{name}\"\r\n\
         \"Language\"=dword:00000000\r\n\
         \"Version\"=dword:0800ee49\r\n\
         \"Assignment\"=dword:00000001\r\n\
         \"AdvertiseFlags\"=dword:00000184\r\n\
         \"InstanceType\"=dword:00000000\r\n\r\n\
         [HKEY_LOCAL_MACHINE\\Software\\Microsoft\\Windows\\CurrentVersion\\Installer\\UserData\\S-1-5-18\\Products\\{packed}\\InstallProperties]\r\n\
         \"DisplayName\"=\"{name}\"\r\n\
         \"DisplayVersion\"=\"8.0.61001\"\r\n\
         \"Comments\"=\"{note}\"\r\n\
         \"Version\"=dword:0800ee49\r\n\
         \"VersionMajor\"=dword:00000008\r\n\
         \"VersionMinor\"=dword:00000000\r\n\
         \"NoModify\"=dword:00000001\r\n\
         \"NoRepair\"=dword:00000001\r\n\
         \"WindowsInstaller\"=dword:00000001\r\n\r\n"
    ));
    apply_reg(prefix, &reg, run)
}

/// Squash a product code into the form Windows Installer keys its registry by.
///
/// The transform is not a hash: each of the GUID's first three groups is reversed
/// wholesale, and the remaining bytes have their hex pairs swapped. `None` if the
/// input is not a braced 36-character GUID.
fn pack_product_code(guid: &str) -> Option<String> {
    let inner = guid.strip_prefix('{')?.strip_suffix('}')?;
    let parts: Vec<&str> = inner.split('-').collect();
    let [a, b, c, d, e] = parts[..] else { return None };
    if (a.len(), b.len(), c.len(), d.len(), e.len()) != (8, 4, 4, 4, 12) {
        return None;
    }
    if !inner.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-') {
        return None;
    }
    let reversed = |s: &str| s.chars().rev().collect::<String>();
    // The tail is byte-swapped rather than reversed: "8cbf" becomes "c8fb", not
    // "fbc8".
    let swapped = |s: &str| {
        s.as_bytes()
            .chunks(2)
            .map(|p| format!("{}{}", p[1] as char, p[0] as char))
            .collect::<String>()
    };
    Some(format!("{}{}{}{}{}", reversed(a), reversed(b), reversed(c), swapped(d), swapped(e)))
}

/// Where the prefix's GAC lives. Not a real GAC — under Wine it is a plain
/// directory tree that fusion probes, which is why assemblies can be put there by
/// copying rather than by running gacutil.
fn gac_dir(prefix: &Path) -> PathBuf {
    prefix.join("drive_c/windows/assembly/GAC")
}

/// Is Managed DirectX already in this prefix?
pub fn is_mdx_installed(prefix: &Path) -> bool {
    gac_dir(prefix)
        .join("microsoft.directx")
        .join(format!("{MDX_BASE_VERSION}__{MDX_TOKEN}"))
        .join("microsoft.directx.dll")
        .is_file()
}

/// Install the DirectX pieces the plugin ecosystem needs: **Managed DirectX 1.1**
/// into the GAC, and Microsoft's native **d3dx9_30** alongside Wine's builtins.
///
/// Both come out of one download, which is why they share a function.
///
/// Plugin installers in this ecosystem check for it — Virindi's, the most widely
/// used package, refuses to run without it ("The latest DX9 update is not
/// installed"). Despite that wording it is not asking for the native DirectX
/// runtime: it probes for the `Microsoft.DirectX*` **managed** assemblies that
/// shipped with the DirectX 9.0c end-user runtime, which Wine does not provide and
/// nothing else installs.
///
/// Getting them is a three-layer unwrap, and each layer is chosen the way it is for
/// a reason:
///
///   * The redistributable is a **self-extracting archive**, unpacked by running it
///     inside the prefix. Wine does this correctly and it saves having to locate a
///     cabinet header inside a PE.
///   * What that yields is a **cabinet of cabinets**: a base `MDX1_x86.cab` plus an
///     `_Archive.cab` holding one cabinet per MDX point release. Those are unpacked
///     in-process — Wine's `expand.exe` opens them and then fails the extraction,
///     and winetricks shells out to `cabextract`, which a stock Mac has not got.
///   * Installing to the GAC is a **directory copy**. No gacutil is involved, and
///     none is available; fusion finds an assembly by the path shape
///     `GAC\<name>\<version>__<token>\<name>.dll`, so building that shape is the
///     whole job.
///
/// Idempotent, and the expensive parts are skipped once the GAC entry exists, so
/// it is safe to call again to recover a prefix where this step failed.
pub fn ensure_directx_runtime(
    prefix: &Path,
    cache: &Path,
    run: RunInPrefix,
    on: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    if is_mdx_installed(prefix) && is_native_d3dx9_installed(prefix) {
        return Ok(());
    }
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let redist = cache.join("directx_Jun2010_redist.exe");
    if !redist.is_file() {
        on(Progress::new(
            SetupStep::InstallDecal,
            0.94,
            "downloading Managed DirectX (~100 MB; plugin installers require it)…",
        ));
        crate::fetch::download(DIRECTX_URL, &redist, SetupStep::InstallDecal, on)?;
    }
    // Verify every time, not just after a fresh download: a half-written file from
    // an interrupted run would otherwise be trusted forever.
    if let Err(e) = crate::fetch::verify_sha256(&redist, DIRECTX_SHA256) {
        let _ = std::fs::remove_file(&redist);
        return Err(e);
    }

    on(Progress::new(SetupStep::InstallDecal, 0.97, "unpacking Managed DirectX…"));
    let work = prefix.join("drive_c/betterac-dx");
    let _ = std::fs::remove_dir_all(&work);
    let out = work.join("out");
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let staged = work.join("directx_redist.exe");
    std::fs::copy(&redist, &staged)
        .map_err(|e| format!("staging the DirectX redistributable: {e}"))?;

    let result = unpack_mdx(prefix, &staged, &out, run);
    let _ = std::fs::remove_dir_all(&work);
    result?;

    if !is_mdx_installed(prefix) {
        return Err("Managed DirectX did not install (no assembly in the GAC)".into());
    }
    if !is_native_d3dx9_installed(prefix) {
        return Err("d3dx9_30.dll did not install".into());
    }
    Ok(())
}

/// Is Microsoft's `d3dx9_30.dll` — not Wine's builtin — in the prefix?
///
/// By content, because the builtin sits at the same path under the same name and
/// only the bytes tell them apart.
fn is_native_d3dx9_installed(prefix: &Path) -> bool {
    let path = prefix.join("drive_c/windows/syswow64").join(D3DX9_30_DLL);
    crate::fetch::verify_sha256(&path, D3DX9_30_SHA256).is_ok()
}

/// Put Microsoft's `d3dx9_30.dll` in the prefix and tell Wine to prefer it.
///
/// See [`D3DX9_30_SHA256`] for why this matters. The override is `native,builtin`
/// rather than `native`: if the file is ever missing, falling back to Wine's own
/// leaves plugins drawing badly rather than the game failing to start.
fn install_native_d3dx9(
    prefix: &Path,
    out: &Path,
    run: RunInPrefix,
) -> Result<(), String> {
    let cab = std::fs::read_dir(out)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase().ends_with(D3DX9_30_CAB))
                .unwrap_or(false)
        })
        .ok_or("the DirectX redistributable carried no d3dx9_30 cabinet")?;

    let files = cab_extract(&cab, &|n| n.eq_ignore_ascii_case(D3DX9_30_DLL))?;
    let (_, bytes) = files.into_iter().next().ok_or("no d3dx9_30.dll in its cabinet")?;
    let dest = prefix.join("drive_c/windows/syswow64").join(D3DX9_30_DLL);
    std::fs::write(&dest, &bytes).map_err(|e| format!("{}: {e}", dest.display()))?;

    apply_reg(
        prefix,
        "Windows Registry Editor Version 5.00\r\n\r\n\
         [HKEY_CURRENT_USER\\Software\\Wine\\DllOverrides]\r\n\
         \"d3dx9_30\"=\"native,builtin\"\r\n\r\n",
        run,
    )
}

/// The unwrapping half of [`ensure_directx_runtime`], split out so its temporary
/// directory is cleaned up on the failure paths too.
fn unpack_mdx(
    prefix: &Path,
    staged: &Path,
    out: &Path,
    run: RunInPrefix,
) -> Result<(), String> {
    // `/Q` quiet, `/C` extract-only, `/T:` target. Without `/C` this would try to
    // *install* DirectX, which is neither wanted nor possible here.
    run(&[
        &windows_path(prefix, staged),
        "/Q",
        "/C",
        &format!("/T:{}", windows_path(prefix, out)),
    ])?;

    // The native D3DX the plugin renderers need, out of the same unpack.
    install_native_d3dx9(prefix, out, run)?;

    let named = |needle: &str, exclude: &str| -> Option<PathBuf> {
        std::fs::read_dir(out).ok()?.flatten().map(|e| e.path()).find(|p| {
            let n = p.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
            n.contains(needle) && (exclude.is_empty() || !n.contains(exclude))
        })
    };
    let base = named("mdx1_x86.cab", "archive")
        .ok_or("the DirectX redistributable carried no MDX cabinet")?;
    let archive = named("mdx1_x86_archive.cab", "")
        .ok_or("the DirectX redistributable carried no MDX archive cabinet")?;

    let is_assembly = |n: &str| {
        let n = n.to_ascii_lowercase();
        n.starts_with("microsoft.directx") && n.ends_with(".dll")
    };

    // The base cabinet holds the 2902 assemblies, and Direct3DX, which binds as
    // 2911 — it is deliberately published under both, exactly as the official
    // installer leaves it.
    let mut versions = std::collections::BTreeSet::new();

    let assemblies = cab_extract(&base, &is_assembly)?;
    gac_install(prefix, MDX_BASE_VERSION, &assemblies)?;
    versions.insert(MDX_BASE_VERSION.to_string());
    let d3dx: Vec<_> = assemblies
        .into_iter()
        .filter(|(n, _)| n.to_ascii_lowercase().contains("direct3dx"))
        .collect();
    gac_install(prefix, MDX_D3DX_VERSION, &d3dx)?;
    versions.insert(MDX_D3DX_VERSION.to_string());

    // …and the archive holds one nested cabinet per point release, named for the
    // version it carries. Plugins built against any of them then resolve.
    for (name, bytes) in cab_extract(&archive, &|n| n.to_ascii_lowercase().ends_with(".cab"))? {
        let lower = name.to_ascii_lowercase();
        let Some(version) = lower.strip_prefix("mdx_").and_then(|s| s.split("_x86").next())
        else {
            continue;
        };
        let inner = cab_extract_reader(std::io::Cursor::new(bytes), &is_assembly)?;
        gac_install(prefix, version, &inner)?;
        versions.insert(version.to_string());
    }

    register_assembly_folders(prefix, &versions, run)
}

/// Every file in a cabinet whose name `want` accepts, as `(name, bytes)`.
fn cab_extract(
    path: &Path,
    want: &dyn Fn(&str) -> bool,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    cab_extract_reader(file, want)
}

/// As [`cab_extract`], from anything readable — which is what makes the nested
/// cabinets work, since those are only ever in memory.
fn cab_extract_reader<R: std::io::Read + std::io::Seek>(
    reader: R,
    want: &dyn Fn(&str) -> bool,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    use std::io::Read;
    let mut cabinet = cab::Cabinet::new(reader).map_err(|e| format!("reading a cabinet: {e}"))?;
    // Collected first because reading a file borrows the cabinet mutably.
    let names: Vec<String> = cabinet
        .folder_entries()
        .flat_map(|f| f.file_entries().map(|e| e.name().to_string()))
        .filter(|n| want(n))
        .collect();
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let mut reader =
            cabinet.read_file(&name).map_err(|e| format!("extracting {name}: {e}"))?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).map_err(|e| format!("extracting {name}: {e}"))?;
        out.push((name, bytes));
    }
    Ok(out)
}

/// Lay assemblies out where fusion looks for a given version.
fn gac_install(
    prefix: &Path,
    version: &str,
    files: &[(String, Vec<u8>)],
) -> Result<(), String> {
    for (name, bytes) in files {
        let file = name.to_ascii_lowercase();
        let Some(stem) = file.strip_suffix(".dll") else { continue };
        let dir = gac_dir(prefix).join(stem).join(format!("{version}__{MDX_TOKEN}"));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let dest = dir.join(&file);
        std::fs::write(&dest, bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    Ok(())
}

/// Point `AssemblyFolders` at each MDX version, the way the official installer
/// does. Not needed to *load* an assembly — the GAC covers that — but it is where
/// an installer checking "is Managed DirectX here?" may look rather than trying a
/// load, and it costs one registry import.
fn register_assembly_folders(
    prefix: &Path,
    versions: &std::collections::BTreeSet<String>,
    run: RunInPrefix,
) -> Result<(), String> {
    let mut reg = String::from("Windows Registry Editor Version 5.00\r\n\r\n");
    for version in versions {
        for base in REG_VIEWS {
            reg.push_str(&format!(
                "[HKEY_LOCAL_MACHINE\\{base}\\Microsoft\\.NETFramework\\AssemblyFolders\\DX_{version}]\r\n\
                 @=\"C:\\\\windows\\\\Microsoft.NET\\\\DirectX for Managed Code\\\\{version}\\\\\"\r\n\r\n"
            ));
        }
    }
    apply_reg(prefix, &reg, run)
}

/// The marker that says an `acclient.exe.config` is ours to rewrite.
const RUNTIME_CONFIG_MARKER: &str = "written by betterAC";

/// Let the client's CLR load the **mixed-mode assemblies** the plugin ecosystem is
/// built on, by giving `acclient.exe` an application config.
///
/// Managed DirectX — which Virindi's view service, and so most of its UI, renders
/// through — is a C++/CLI assembly built against **CLR v1.1**. A 4.0 runtime
/// refuses to load such an assembly outright:
///
/// > Mixed mode assembly is built against version 'v1.1.4322' of the runtime and
/// > cannot be loaded in the 4.0 runtime without additional configuration
/// > information.
///
/// `useLegacyV2RuntimeActivationPolicy` is that configuration information. Without
/// it the failure is quiet and confusing rather than loud: the plugin loads, its
/// windows appear, and everything drawn through DirectX inside them — text, icons,
/// backgrounds — silently does not, leaving empty panels.
///
/// The config belongs to the **host process**, not the plugin, which is why it goes
/// beside `acclient.exe` even though nothing about the client itself needs it. The
/// CLR is loaded into the client by COM activation, and this is the file the shim
/// reads when that happens.
///
/// A config we did not write is left alone: it would belong to something else, and
/// silently replacing it is worse than the plugins not rendering.
pub fn ensure_runtime_config(ac_dir: &Path) -> Result<(), String> {
    let path = ac_dir.join("acclient.exe.config");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if !existing.contains(RUNTIME_CONFIG_MARKER) {
            return Ok(());
        }
        if existing.contains("useLegacyV2RuntimeActivationPolicy") {
            return Ok(());
        }
    }
    let config = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n\
         <!-- {RUNTIME_CONFIG_MARKER}: lets the client's .NET 4 runtime load the\r\n\
         \x20    CLR 1.1/2.0 mixed-mode assemblies Decal plugins use, Managed\r\n\
         \x20    DirectX above all. Delete this file to undo it. -->\r\n\
         <configuration>\r\n\
         \x20 <startup useLegacyV2RuntimeActivationPolicy=\"true\">\r\n\
         \x20   <supportedRuntime version=\"v4.0\" sku=\".NETFramework,Version=v4.0\" />\r\n\
         \x20 </startup>\r\n\
         </configuration>\r\n"
    );
    std::fs::write(&path, config).map_err(|e| format!("{}: {e}", path.display()))
}

/// Let executables built for CLR 2 run on the CLR 4 this prefix actually has.
///
/// .NET 4 is a **side-by-side** runtime, not an in-place upgrade for .NET 2/3.5:
/// an EXE whose metadata asks for `v2.0.50727` gets refused by the activation shim
/// with *"You must enable the .NET Framework from the Windows Features dialog"* —
/// on real Windows the fix is ticking .NET 3.5 in Windows Features, which is not a
/// thing here. `OnlyUseLatestCLR` is the documented switch that makes the shim fall
/// back to the newest installed runtime instead of giving up.
///
/// This matters because plugin installers are old: Virindi's, the most common one,
/// is a CLR 2 executable with no `app.config` to redirect itself. The alternative
/// was winetricks `dotnet35`, which wants a host-side cabextract and does
/// `remove_mono` plus `w_override_dlls native mscoree` — it would trample the
/// mscoree shim that makes Decal's managed adapter load at all.
///
/// Written to both registry views: the shim reads the 32-bit one for the 32-bit
/// processes these installers are.
fn allow_old_clr_apps(prefix: &Path, run: RunInPrefix) -> Result<(), String> {
    let mut reg = String::from("Windows Registry Editor Version 5.00\r\n\r\n");
    for base in REG_VIEWS {
        reg.push_str(&format!(
            "[HKEY_LOCAL_MACHINE\\{base}\\Microsoft\\.NETFramework]\r\n\
             \"OnlyUseLatestCLR\"=dword:00000001\r\n\r\n"
        ));
    }
    apply_reg(prefix, &reg, run)
}

/// Unpack a downloaded plugin archive into the prefix and find the installer in it.
///
/// This is the *preferred* route, and the one to point people at: plugins are
/// distributed as zips, and an archive already says exactly which files belong
/// together. Extracting it wholesale means the installer runs with its libraries
/// beside it as its author intended, with nothing inferred — compare
/// [`stage_installer`], which has to guess when handed a loose executable.
fn unpack_installer_archive(archive: &Path, dir: &Path) -> Result<PathBuf, String> {
    crate::fetch::extract_zip(archive, dir)?;
    let stem = archive
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    let mut found = Vec::new();
    collect_installers(dir, dir, 0, &mut found);
    let names: Vec<String> = found.iter().map(|(rel, _)| rel.clone()).collect();
    let pick = choose_installer(&names, &stem).ok_or_else(|| {
        if names.is_empty() {
            format!("{} contains no .exe or .msi installer", archive.display())
        } else {
            format!(
                "{} contains several installers and it is not clear which to run ({}). \
                 Unpack it and choose one.",
                archive.display(),
                names.join(", ")
            )
        }
    })?;
    found
        .into_iter()
        .find(|(rel, _)| *rel == pick)
        .map(|(_, path)| path)
        .ok_or_else(|| "the chosen installer vanished".to_string())
}

/// Every `.exe`/`.msi` under `dir`, as (path relative to the root, full path).
fn collect_installers(root: &Path, dir: &Path, depth: u32, out: &mut Vec<(String, PathBuf)>) {
    // Archives are nested a level or two at most; anything deeper is a payload
    // tree, not a place to look for the thing to run.
    if depth > MAX_ARCHIVE_DEPTH {
        return;
    }
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_installers(root, &path, depth + 1, out);
        } else if matches!(
            path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).as_deref(),
            Some("exe") | Some("msi")
        ) {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
            out.push((rel, path));
        }
    }
}

const MAX_ARCHIVE_DEPTH: u32 = 3;

/// Decide which of an archive's executables is the one to run.
///
/// Returns `None` rather than picking arbitrarily when the archive holds several
/// equally plausible candidates — running the wrong executable out of somebody's
/// download is worse than saying so and letting them choose.
fn choose_installer(names: &[String], archive_stem: &str) -> Option<String> {
    if names.len() == 1 {
        return names.first().cloned();
    }
    let depth = |n: &String| n.matches(['/', '\\']).count();
    let shallowest = names.iter().map(depth).min()?;
    let top: Vec<&String> = names.iter().filter(|n| depth(n) == shallowest).collect();
    if top.len() == 1 {
        return top.first().map(|n| (*n).clone());
    }
    // The archive is usually named for the thing inside it.
    let stem_of = |n: &str| {
        n.rsplit(['/', '\\']).next().unwrap_or(n).rsplit_once('.').map(|(s, _)| s.to_ascii_lowercase()).unwrap_or_default()
    };
    if !archive_stem.is_empty() {
        let named: Vec<&&String> = top
            .iter()
            .filter(|n| {
                let s = stem_of(n);
                archive_stem.starts_with(&s) || s.starts_with(archive_stem)
            })
            .collect();
        if named.len() == 1 {
            return named.first().map(|n| (**n).clone());
        }
    }
    // Failing that, the conventional names.
    let obvious: Vec<&&String> = top
        .iter()
        .filter(|n| {
            let s = stem_of(n);
            s.contains("install") || s.contains("setup")
        })
        .collect();
    if obvious.len() == 1 {
        return obvious.first().map(|n| (**n).clone());
    }
    None
}

/// Copy an installer into the prefix, together with the files beside it that it
/// needs to run. Returns the staged installer's path.
///
/// Copying just the chosen file is not enough: installers are commonly shipped as a
/// small executable plus its libraries, and moving the executable away from them
/// breaks it. Virindi's is exactly this — it fails every plugin with *"Could not
/// load file or assembly 'ICSharpCode.SharpZipLib'"* if that DLL is left behind.
///
/// The obvious fix, copying the whole containing folder, is not viable: people
/// download installers to Downloads, which here is 65 GB.
///
/// So dependencies are *identified* instead — a sibling is taken along when the
/// installer references it by name (see [`referenced_siblings`]) — and the search
/// repeats over what it copies, so a dependency's own dependencies follow. Bounded
/// at every step, because the input is a directory chosen by the user.
fn stage_installer(installer: &Path, dir: &Path) -> Result<PathBuf, String> {
    let name = installer.file_name().ok_or("the installer path has no file name")?;
    let staged = dir.join(name);
    std::fs::copy(installer, &staged)
        .map_err(|e| format!("staging the installer into the prefix: {e}"))?;

    let Some(parent) = installer.parent() else { return Ok(staged) };
    // Candidates: the ordinary files beside the installer, small enough to be a
    // library rather than a payload.
    let mut candidates: Vec<(String, PathBuf)> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let meta = e.metadata().ok()?;
            if !meta.is_file() || meta.len() > MAX_DEPENDENCY_BYTES || path == installer {
                return None;
            }
            Some((path.file_name()?.to_string_lossy().into_owned(), path))
        })
        .collect();
    candidates.sort();

    let mut scan = vec![staged.clone()];
    let mut copied = 0u64;
    while let Some(current) = scan.pop() {
        let Ok(bytes) = std::fs::read(&current) else { continue };
        if bytes.len() as u64 > MAX_SCAN_BYTES {
            continue;
        }
        let names: Vec<&str> = candidates.iter().map(|(n, _)| n.as_str()).collect();
        for wanted in referenced_siblings(&bytes, &names) {
            let Some(i) = candidates.iter().position(|(n, _)| *n == wanted) else { continue };
            let (name, from) = candidates.remove(i);
            let size = from.metadata().map(|m| m.len()).unwrap_or(0);
            if copied + size > MAX_STAGED_BYTES {
                continue;
            }
            let to = dir.join(&name);
            if std::fs::copy(&from, &to).is_ok() {
                copied += size;
                scan.push(to);
            }
        }
    }
    Ok(staged)
}

/// A dependency has to be smaller than this to be taken along, and everything taken
/// along has to fit in the total. Generous for a library, mean for a payload — the
/// point is that pointing this at a directory full of disc images cannot turn into
/// copying them.
const MAX_DEPENDENCY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STAGED_BYTES: u64 = 128 * 1024 * 1024;
/// Files bigger than this are not searched for references to their neighbours.
const MAX_SCAN_BYTES: u64 = 32 * 1024 * 1024;

/// Which of `names` does `bytes` mention?
///
/// A managed assembly names its dependencies in the metadata string heap, and a
/// native one names its imports in the import table; either way the name is in the
/// file as plain text, so a substring search finds it without having to model
/// either format. Matching is case-insensitive, and covers UTF-16 as well as ASCII
/// because a name can come from a resource string rather than a table.
///
/// Both the assembly name (`Foo`) and the file name (`Foo.dll`) are accepted, since
/// an assembly reference carries the former and the file on disk is the latter.
///
/// Deliberately loose in one direction only: matching something the installer never
/// loads costs a copy into a directory that is deleted afterwards, whereas missing
/// a real dependency breaks the install.
/// Searching the file once per candidate name is what the obvious implementation
/// does, and it is far too slow: a few hundred neighbours against a multi-megabyte
/// binary is a substring search per pair. Instead the file is tokenised in one
/// pass and each token looked up — the names being sought are whole tokens, since
/// whatever separates them (a quote, a comma, a path separator, a NUL) is not part
/// of a file name.
fn referenced_siblings(bytes: &[u8], names: &[&str]) -> Vec<String> {
    let mut keys: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, name) in names.iter().enumerate() {
        let full = name.to_ascii_lowercase();
        let Some((stem, ext)) = full.rsplit_once('.') else { continue };
        // Only things an installer can actually load. Without this the name match
        // alone is far too generous: stems like "icon", "more" or "index" occur in
        // any binary by chance, and a folder of ordinary downloads gets dragged in
        // alongside the dependency — measured at 32 MB of PDFs and images on a real
        // Downloads folder, against 240 KB of actual dependency.
        if !DEPENDENCY_EXTENSIONS.contains(&ext) {
            continue;
        }
        // Short stems ("a", "x64") match by accident; a real library name is longer
        // than this, and the cost of skipping one is only a dependency not copied.
        if stem.len() < MIN_DEPENDENCY_STEM {
            continue;
        }
        keys.entry(stem.to_string()).or_insert(i);
        keys.entry(full.clone()).or_insert(i);
    }

    let ascii = bytes.to_ascii_lowercase();
    // Every other byte of a UTF-16 run, so an ASCII name matches it too — enough
    // for the ASCII-range names these files use. Both parities, since a run need
    // not begin on an even offset.
    let wide: Vec<u8> = ascii.iter().copied().step_by(2).collect();
    let wide_odd: Vec<u8> = ascii.iter().copied().skip(1).step_by(2).collect();

    let mut hit = vec![false; names.len()];
    for buffer in [&ascii, &wide, &wide_odd] {
        for token in buffer.split(|b| !is_name_byte(*b)) {
            if token.len() < MIN_DEPENDENCY_STEM {
                continue;
            }
            let Ok(token) = std::str::from_utf8(token) else { continue };
            // Try the token whole, then each of its dot-separated tails: a name can
            // end up glued to whatever preceded it when that was itself name-shaped,
            // and both `Foo.Bar` (assembly) and `Foo.Bar.dll` (file) are sought.
            let mut rest = Some(token);
            while let Some(part) = rest {
                if part.len() >= MIN_DEPENDENCY_STEM {
                    if let Some(&i) = keys.get(part) {
                        hit[i] = true;
                    }
                }
                rest = part.split_once('.').map(|(_, tail)| tail);
            }
        }
    }
    names
        .iter()
        .zip(&hit)
        .filter(|(_, &h)| h)
        .map(|(n, _)| n.to_string())
        .collect()
}

/// The shortest file stem worth matching on. See [`referenced_siblings`].
const MIN_DEPENDENCY_STEM: usize = 4;

/// What a sibling has to be for the installer to plausibly load it. `.exe` is
/// deliberately absent: another installer sitting in the same folder is exactly the
/// kind of thing whose name matches by chance, and copying one is worse than
/// useless.
const DEPENDENCY_EXTENSIONS: [&str; 3] = ["dll", "config", "manifest"];

/// Bytes that can appear inside a file or assembly name. Anything else ends a
/// token, which is what makes a name findable without knowing the file format.
fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+')
}

/// Recast an installer's exit code as a plain-English cancellation where that is
/// what it means. Closing an installer is a normal thing to do, and reporting it
/// as a failure reads like something broke.
///
/// `1602` is `ERROR_INSTALL_USEREXIT` (msiexec) and `1223` is `ERROR_CANCELLED`,
/// which is what Inno Setup returns; NSIS uses plain `1`, too ambiguous to claim.
fn cancelled_or(e: String) -> String {
    if e.contains("1602") || e.contains("1223") {
        "the installer was cancelled".to_string()
    } else {
        e
    }
}

/// Put the 32-bit MFC runtime beside Wine's own system DLLs, if it isn't already
/// there. Idempotent, and cheap on the common path — the bytes are only written
/// when the file is missing or differs, so repeat launches don't churn ~5 MB.
///
/// Called at Decal-install time *and* from [`open_settings`], because prefixes
/// provisioned before this existed would otherwise never get it.
fn install_mfc140(prefix: &Path) -> Result<(), String> {
    let dest = prefix.join("drive_c/windows/syswow64/mfc140u.dll");
    if std::fs::read(&dest).is_ok_and(|cur| cur == MFC140U_X86) {
        return Ok(());
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&dest, MFC140U_X86).map_err(|e| format!("{}: {e}", dest.display()))
}

/// The injector's Windows path when Decal should actually be used on this launch:
/// enabled (chosen at setup, toggleable in settings) **and** provisioned **and**
/// staged. `None` otherwise,
/// which leaves the command line exactly as it was before Decal existed.
///
/// Every condition is checked rather than assumed, because a half-provisioned
/// prefix must not stop the game launching — losing plugins beats losing the game.
pub fn launch_injector(prefix: &Path) -> Option<String> {
    if !crate::config::Config::load().decal.enabled || !is_installed(prefix) {
        return None;
    }
    let exe = injector_path(prefix);
    exe.is_file().then(|| windows_path(prefix, &exe))
}

/// Write the embedded injector into the prefix, if it isn't already current.
pub fn ensure_injector(prefix: &Path) -> Result<PathBuf, String> {
    let path = injector_path(prefix);
    let dir = path.parent().ok_or("no parent for the injector")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let current = std::fs::read(&path).is_ok_and(|b| b == DECINJECT_EXE);
    if !current {
        std::fs::write(&path, DECINJECT_EXE).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(path)
}

/// Re-apply the pieces of Decal's provisioning that live **outside** the prefix's
/// Decal directory, for a prefix where [`is_installed`] already says yes.
///
/// [`is_installed`] only asks whether `Decal.dll` is on disk, which the MSI puts
/// there early — so an install interrupted after that point reads as complete
/// forever and the rest of [`install`] would never run again. These three are the
/// parts that are cheap, idempotent, and not covered anywhere else:
///
///   * the **injector**, which ships with the app and so can be newer than the
///     prefix after an update;
///   * the **runtime hot-patch**, which lives in the Wine/Proton build rather than
///     the prefix and is therefore lost whenever the runtime is replaced;
///   * **cohook**, for the same reason the injector needs refreshing.
///
/// The two other launch-critical pieces — [`ensure_runtime_config`] and
/// [`ensure_msil_assemblies`] — are already re-applied on every launch by both
/// runtimes, so they are deliberately not repeated here.
pub fn ensure_runtime_hooks(prefix: &Path, wine_bin: &Path) -> Result<(), String> {
    ensure_injector(prefix)?;
    let runtime_root = wine_bin
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot locate the runtime root from the wine binary")?;
    patch_engine(runtime_root)?;
    deploy_cohook(prefix)
}

/// Provision Decal into `prefix`: unpack the MSI, seed the data files Decal
/// validates the client against, write the registration, and leave every plugin
/// switched **off**.
///
/// `ac_dir` is the game directory, which Decal records as its `PortalPath`.
pub fn install(
    prefix: &Path,
    ac_dir: &Path,
    cache: &Path,
    wine_bin: &Path,
    run: RunInPrefix,
    on: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    let source = MsiSource::from_env();
    let msi = source.fetch(cache, on)?;

    // Real .NET Framework 4.8 must be in place *before* the MSI's GAC publish runs
    // (so fusion, not mono, publishes) and before the managed adapter is ever
    // activated. This is the slow part — Microsoft's installers under Wine.
    ensure_dotnet(prefix, wine_bin, cache, run, on)?;

    // Full install (not an admin `/a` extract): it lays the files down, writes the
    // COM registration, AND publishes the `Decal.Interop.*` assemblies into the GAC
    // — which the managed adapter binds against. The reg template below then pins
    // the two paths the MSI can't know (install dir + PortalPath).
    on(Progress::new(SetupStep::InstallDecal, 0.6, "installing Decal…"));
    msi_install(prefix, &msi, run)?;
    if !is_installed(prefix) {
        return Err("Decal did not install (no Decal.dll after msiexec)".into());
    }

    on(Progress::new(SetupStep::InstallDecal, 0.75, "fetching Decal's client data…"));
    seed_data_files(prefix)?;

    // The template ships every plugin disabled (genreg.py forces Plugins\*\Enabled
    // to 0) and carries the PortalPath, so nothing runs until the user opts in.
    on(Progress::new(SetupStep::InstallDecal, 0.85, "registering Decal…"));
    apply_reg(prefix, &render_template(prefix, ac_dir), run)?;

    // As shipped, Decal's managed assemblies won't load under real .NET: they are
    // x86 images that get bound by-name as MSIL (a fatal arch mismatch), and they
    // live outside the GAC so fusion probes the app base.
    // [`ensure_msil_assemblies`] flips them and drops the adapter beside the client,
    // both of which the earlier strong-name-skip makes acceptable.
    on(Progress::new(SetupStep::InstallDecal, 0.92, "wiring up Decal's assemblies…"));
    ensure_msil_assemblies(prefix, ac_dir)?;

    // Decal's *native* hooks, orthogonal to .NET: the runtime's builtin
    // d3d9/kernel32 need the MSVC hot-patch signature Decal's installer looks for,
    // and CoInitialize needs cohook's façade (the injector loads it).
    //
    // `bin/wine` sits directly under the root that holds `lib/wine`, on both
    // platforms: `<engine>/Libraries/Wine/bin/wine` on macOS and
    // `<GE-Proton>/files/bin/wine` on Linux.
    let runtime_root = wine_bin
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot locate the runtime root from the wine binary")?;
    patch_engine(runtime_root)?;
    deploy_cohook(prefix)?;

    // The MSI installs DenAgent.exe but not the MFC runtime it imports, so stage
    // that now rather than at first use — settings should not be the thing that
    // discovers Decal's own UI can't start.
    install_mfc140(prefix)?;
    allow_old_clr_apps(prefix, run)?;

    // Last, because it is the biggest download and the least essential to Decal
    // itself: nothing here is needed to *run* Decal, only to install the plugins
    // people actually want.
    ensure_directx_runtime(prefix, cache, run, on)?;
    ensure_vcrun2005(prefix, cache, run, on)?;
    ensure_runtime_config(ac_dir)?;

    ensure_injector(prefix)?;

    // The MSI starts `DenAgent.exe` as the last thing it does, and it never exits
    // -- it is a tray agent. betterAC deliberately replaces it (plugins are managed
    // from settings) and starts it only on request, so an agent nobody asked for
    // should not be left sitting in the prefix after setup.
    //
    // Best-effort: `taskkill` failing means it was not running, which is the
    // outcome we wanted anyway.
    let _ = run(&["taskkill", "/f", "/im", "DenAgent.exe"]);
    Ok(())
}

/// Full MSI install into the prefix's default location. Unlike the old `/a` admin
/// extract, this runs the install sequence — crucially `MsiPublishAssemblies`,
/// which GACs the managed interop assemblies the adapter needs.
fn msi_install(prefix: &Path, msi: &Path, run: RunInPrefix) -> Result<(), String> {
    // The MSI is in the cache outside the prefix, which maps no host drives; stage
    // a copy inside C: for msiexec to reach.
    let staged = prefix.join("drive_c/betterac-decal.msi");
    std::fs::copy(msi, &staged)
        .map_err(|e| format!("staging the Decal MSI into the prefix: {e}"))?;
    let result = run(&["msiexec", "/i", &windows_path(prefix, &staged), "/qn"]);
    let _ = std::fs::remove_file(&staged);
    result
}

/// Where real .NET's CLR lands. Its presence (plus the mscoree shim) is how we know
/// [`ensure_dotnet`] already ran in this prefix.
fn is_dotnet_installed(prefix: &Path) -> bool {
    prefix.join("drive_c/windows/Microsoft.NET/Framework/v4.0.30319/clr.dll").is_file()
        && prefix.join("drive_c/windows/syswow64/mscoree.dll").is_file()
}

/// Install real Microsoft .NET Framework 4.8 into the prefix and apply the fixes
/// that make Decal's managed adapter loadable under it. Idempotent. See the module
/// note above [`DOTNET_VERB`] for *why* real .NET rather than wine-mono.
fn ensure_dotnet(
    prefix: &Path,
    wine_bin: &Path,
    cache: &Path,
    run: RunInPrefix,
    on: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    if is_dotnet_installed(prefix) {
        return Ok(());
    }
    on(Progress::new(
        SetupStep::InstallDecal,
        0.3,
        ".NET Framework (a few minutes; runs Microsoft's installer)…",
    ));
    // Wine pre-registers a fake `NDP\v4\Full` (Install=1, Version 4.7) to advertise
    // wine-mono's .NET compatibility. Microsoft's installer reads it, decides
    // ".NET 4 is already part of this OS", and silently installs nothing. Clear it
    // in both views first — `remove_mono` won't, since these are Wine's own keys.
    // (`run` treats the key being absent as an error, which is fine here.)
    for view in ["32", "64"] {
        let _ = run(&[
            "reg",
            "delete",
            r"HKLM\Software\Microsoft\NET Framework Setup\NDP\v4",
            "/f",
            &format!("/reg:{view}"),
        ]);
    }
    run_winetricks(wine_bin, prefix, cache)?;

    // winetricks' `remove_mono` deletes the mscoree.dll the .NET installer laid
    // down, and the 4.8 update doesn't re-ship it, so restore the real one.
    install_mscoree(prefix)?;
    // Fusion can't verify the (about-to-be-modified) adapter's strong name under
    // Wine; tell it to skip verification so the identity is trusted from the
    // manifest. Also pin the .NET shims to native so Wine uses the real ones.
    for view in ["32", "64"] {
        run(&[
            "reg",
            "add",
            r"HKLM\Software\Microsoft\StrongName\Verification\*,*",
            "/f",
            &format!("/reg:{view}"),
        ])?;
        for dll in ["mscoree", "mscoreei"] {
            run(&[
                "reg", "add", r"HKCU\Software\Wine\DllOverrides", "/v", dll, "/t", "REG_SZ",
                "/d", "native", "/f", &format!("/reg:{view}"),
            ])?;
        }
    }

    if !is_dotnet_installed(prefix) {
        return Err("real .NET Framework did not install (no clr.dll/mscoree)".into());
    }
    Ok(())
}

/// Run the bundled winetricks `dotnet48` verb against the engine. Long-running
/// (Microsoft's installers under Wine). Its whole (voluminous) output is captured
/// to `cache/winetricks-dotnet.log` — winetricks' own exit code is unreliable, so
/// that log is the only way to diagnose a silent .NET install failure, and this one
/// took a round-trip to find the first time.
///
/// No dialog-dismissing watchdog: on a fresh prefix (no wine-mono) `remove_mono`
/// finds nothing to uninstall and pops no blocking box, so the unattended run
/// completes on its own.
fn run_winetricks(wine_bin: &Path, prefix: &Path, cache: &Path) -> Result<(), String> {
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let script = cache.join("winetricks");
    std::fs::write(&script, WINETRICKS).map_err(|e| format!("{}: {e}", script.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    let log = cache.join("winetricks-dotnet.log");
    let out = std::fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;

    std::process::Command::new(&script)
        .args(["-q", DOTNET_VERB])
        .env("WINE", wine_bin)
        .env("WINESERVER", wine_bin.with_file_name("wineserver"))
        .env("WINEPREFIX", prefix)
        .env("WINEDEBUG", "-all")
        .env("W_OPT_UNATTENDED", "1")
        .stdout(out)
        .stderr(err)
        .status()
        .map_err(|e| format!("running winetricks (see {}): {e}", log.display()))?;
    // winetricks' exit code is unreliable (its own note); the caller checks whether
    // the CLR actually landed.
    Ok(())
}

/// Restore the real Microsoft `mscoree.dll` shims removed by winetricks.
fn install_mscoree(prefix: &Path) -> Result<(), String> {
    let write = |rel: &str, bytes: &[u8]| -> Result<(), String> {
        let dest = prefix.join(rel);
        std::fs::write(&dest, bytes).map_err(|e| format!("{}: {e}", dest.display()))
    };
    write("drive_c/windows/syswow64/mscoree.dll", MSCOREE_X86)?;
    write("drive_c/windows/system32/mscoree.dll", MSCOREE_X64)?;
    Ok(())
}

/// Make the managed adapter loadable under real .NET: flip `Decal.Adapter.dll` from
/// x86 to MSIL (so its arch matches the by-name reference) and drop a copy in the
/// game dir, which is the app base fusion probes for a non-GAC assembly.
pub fn ensure_msil_assemblies(prefix: &Path, ac_dir: &Path) -> Result<(), String> {
    // The adapter: flipped, and copied to the app base fusion probes for it.
    let adapter = install_dir(prefix).join("Decal.Adapter.dll");
    let mut bytes =
        std::fs::read(&adapter).map_err(|e| format!("{}: {e}", adapter.display()))?;
    if flip_to_msil(&mut bytes).map_err(|e| format!("{}: {e}", adapter.display()))? {
        std::fs::write(&adapter, &bytes).map_err(|e| format!("{}: {e}", adapter.display()))?;
    }
    let beside = ac_dir.join("Decal.Adapter.dll");
    if std::fs::read(&beside).ok().as_deref() != Some(&bytes[..]) {
        std::fs::write(&beside, &bytes).map_err(|e| format!("{}: {e}", beside.display()))?;
    }

    // The file service needs the same flip and for the same reason, but no app-base
    // copy: Decal loads it itself, out of its own directory.
    //
    // Missing this is what left plugin windows empty. `Decal.FileService` is how a
    // plugin reads the game's own art out of the DAT files, and the publisher policy
    // Decal installs locks references to it to MSIL. With the file still marked x86,
    // fusion refused every plugin's bind — *"An assembly with different processor
    // architecture is already loaded"* — so Virindi's views drew their frames and
    // nothing inside them.
    let service = install_dir(prefix).join("Decal.FileService.dll");
    if service.is_file() {
        let mut bytes =
            std::fs::read(&service).map_err(|e| format!("{}: {e}", service.display()))?;
        if flip_to_msil(&mut bytes).map_err(|e| format!("{}: {e}", service.display()))? {
            std::fs::write(&service, &bytes)
                .map_err(|e| format!("{}: {e}", service.display()))?;
        }
    }
    Ok(())
}

/// Clear `COMIMAGE_FLAGS_32BITREQUIRED` in a managed PE's CLI header, turning an
/// x86 assembly into MSIL/AnyCPU. It still runs 32-bit in the 32-bit client; the
/// flag only governs how fusion compares processor architecture. Breaks the strong
/// name — harmless because we tell .NET to skip verification.
fn flip_to_msil(b: &mut [u8]) -> Result<bool, String> {
    let pe = u32le(b, 0x3c) as usize;
    if b.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err("not a PE image".into());
    }
    let magic = u16le(b, pe + 24);
    // Data directory 14 (COM descriptor) sits after the optional header's fixed
    // part: 96 bytes for PE32, 112 for PE32+.
    let dd = pe + 24 + if magic == 0x20b { 112 } else { 96 };
    let com_rva = u32le(b, dd + 14 * 8);
    if com_rva == 0 {
        return Err("has no CLI header (not managed?)".into());
    }
    let com = rva_to_offset(b, pe, com_rva).ok_or("bad CLI header address")?;
    // COR20 header: Flags is a u32 at offset 16.
    let flags_off = com + 16;
    let before = u32le(b, flags_off);
    let flags = before & !0x2;
    b.get_mut(flags_off..flags_off + 4)
        .ok_or("CLI header past end of file")?
        .copy_from_slice(&flags.to_le_bytes());
    // Whether anything actually changed, so a caller can skip rewriting a file that
    // is already MSIL — this runs on every launch.
    Ok(flags != before)
}

fn u16le(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]) as u32)
}

fn u32le(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Map a virtual address to a file offset via the PE section table.
fn rva_to_offset(b: &[u8], pe: usize, rva: u32) -> Option<usize> {
    let nsec = u16le(b, pe + 6) as usize;
    let optsz = u16le(b, pe + 20) as usize;
    let sections = pe + 24 + optsz;
    for i in 0..nsec {
        let s = sections + 40 * i;
        let va = u32le(b, s + 12);
        let vsz = u32le(b, s + 8);
        let raw = u32le(b, s + 20);
        if rva >= va && rva < va + vsz {
            return Some((rva - va + raw) as usize);
        }
    }
    None
}

/// The three functions Decal hot-patches in the runtime's builtin DLLs, named
/// rather than located.
///
/// Decal installs its hooks by overwriting the first two bytes of a function with
/// a jump, and it only accepts a target whose prologue is the MSVC hot-patch
/// signature `90 90` (`nop; nop`). Wine's builtins open with `8b ff`
/// (`mov edi,edi`) instead — a different encoding of the same two-byte no-op — so
/// Decal refuses them and its native hooks never install.
///
/// See [`patch_engine`] for the swap. These three are the whole list: `d3d9` is
/// how Decal draws its overlay, and the two `CreateFile` variants are how it
/// intercepts the client's data-file reads.
const ENGINE_HOTPATCH_TARGETS: [(&str, &[&str]); 2] =
    [("d3d9.dll", &["Direct3DCreate9"]), ("kernel32.dll", &["CreateFileA", "CreateFileW"])];

/// Normalise those prologues from `8b ff` to `90 90`. Both encode a two-byte
/// no-op, so this is invisible to everything except Decal's installer, which is
/// looking for the signature. Idempotent, and it only writes when a byte actually
/// changed.
///
/// `runtime_root` is the directory holding `lib/wine/i386-windows` — the Wine
/// engine root on macOS, `<GE-Proton>/files` on Linux. Both are builds betterAC
/// owns (see [`crate::install::runtime_dir`]); this must never be handed a Proton
/// build that Steam shares.
///
/// **Targets are resolved through the PE export table, not by fixed offset.**
/// They used to be hardcoded file offsets read off one Whisky build, which made
/// this both fragile (an engine bump turned it into a hard error telling you to
/// regenerate them) and unportable — the same three functions sit at completely
/// different offsets in GE-Proton, whose sections are mapped 1:1 while the Whisky
/// engine's are not. By name they are the same three functions in any build.
fn patch_engine(runtime_root: &Path) -> Result<(), String> {
    let dir = runtime_root.join("lib/wine/i386-windows");
    for (file, exports) in ENGINE_HOTPATCH_TARGETS {
        let path = dir.join(file);
        let mut bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut dirty = false;
        for name in exports {
            let off = export_offset(&bytes, name)
                .ok_or_else(|| format!("{}: no `{name}` export", path.display()))?;
            match bytes.get(off..off + 2) {
                Some([0x90, 0x90]) => {} // already normalised
                Some([0x8b, 0xff]) => {
                    bytes[off] = 0x90;
                    bytes[off + 1] = 0x90;
                    dirty = true;
                }
                other => {
                    return Err(format!(
                        "{} {name}@{off:#x}: expected the 8b ff prologue, found {other:02x?} \
                         -- this runtime build is not one Decal's hooks can patch",
                        path.display()
                    ));
                }
            }
        }
        if dirty {
            std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// File offset of an exported function, by name, from a PE's export directory.
///
/// Walks the parallel name/ordinal arrays: `AddressOfNames[i]` is the name and
/// `AddressOfNameOrdinals[i]` indexes `AddressOfFunctions` for it. Forwarded
/// exports (whose address falls inside the export directory itself) are rejected
/// — there is no code there to patch.
fn export_offset(b: &[u8], want: &str) -> Option<usize> {
    let pe = u32le(b, 0x3c) as usize;
    if b.get(pe..pe + 4) != Some(b"PE\0\0") {
        return None;
    }
    // Data directory 0 is the export table; it follows the optional header's
    // fixed part, 96 bytes for PE32 and 112 for PE32+.
    let dd = pe + 24 + if u16le(b, pe + 24) == 0x20b { 112 } else { 96 };
    let (dir_rva, dir_size) = (u32le(b, dd), u32le(b, dd + 4));
    let dir = rva_to_offset(b, pe, dir_rva)?;

    let count = u32le(b, dir + 24) as usize;
    let functions = rva_to_offset(b, pe, u32le(b, dir + 28))?;
    let names = rva_to_offset(b, pe, u32le(b, dir + 32))?;
    let ordinals = rva_to_offset(b, pe, u32le(b, dir + 36))?;

    for i in 0..count {
        let name_at = rva_to_offset(b, pe, u32le(b, names + 4 * i))?;
        let end = b[name_at..].iter().position(|&c| c == 0)? + name_at;
        if b.get(name_at..end)? != want.as_bytes() {
            continue;
        }
        let ordinal = u16le(b, ordinals + 2 * i) as usize;
        let rva = u32le(b, functions + 4 * ordinal);
        // A forwarder points at a "DLL.Function" string inside the directory.
        if rva >= dir_rva && rva < dir_rva + dir_size {
            return None;
        }
        return rva_to_offset(b, pe, rva);
    }
    None
}

/// Stage cohook.dll beside Decal's files, where the injector loads it from.
fn deploy_cohook(prefix: &Path) -> Result<(), String> {
    let dest = install_dir(prefix).join("cohook.dll");
    std::fs::write(&dest, COHOOK_DLL).map_err(|e| format!("{}: {e}", dest.display()))
}

/// The XML Decal downloads on first run and validates the client against. Without
/// these it reports the client as out of date and refuses to load.
const DATA_FILES: [&str; 5] =
    ["memlocs", "clientpatches", "decalplugins", "messages", "killbit"];

fn seed_data_files(prefix: &Path) -> Result<(), String> {
    let dir = install_dir(prefix);
    for name in DATA_FILES {
        let url = format!("https://decaldev.com/updatelist/{name}.xml?a");
        let bytes = crate::fetch::get_bytes(&url).map_err(|e| format!("fetching {name}.xml: {e}"))?;
        let dest = dir.join(format!("{name}.xml"));
        std::fs::write(&dest, &bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    Ok(())
}



/// Fill the two paths into the generated template. They arrive already
/// `.reg`-escaped, because every other backslash in the template is escaped too.
fn render_template(prefix: &Path, ac_dir: &Path) -> String {
    let esc = |p: String| p.replace('\\', "\\\\");
    let install = esc(format!("{}\\", windows_path(prefix, &install_dir(prefix))));
    let portal = esc(format!("{}\\", windows_path(prefix, ac_dir)));
    REG_TEMPLATE.replace("@@INSTALLDIR@@", &install).replace("@@PORTALPATH@@", &portal)
}

/// Import a `.reg` through the prefix's own regedit.
///
/// Written as UTF-16LE with a BOM: that is what a "Version 5.00" file is supposed
/// to be, and regedit decides how to read the file from it.
fn apply_reg(prefix: &Path, contents: &str, run: RunInPrefix) -> Result<(), String> {
    let path = prefix.join("drive_c/betterac-decal.reg");
    let mut bytes = vec![0xFF, 0xFE];
    for unit in contents.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let result = run(&["regedit", "/S", &windows_path(prefix, &path)]);
    let _ = std::fs::remove_file(&path);
    result
    // No wait for a disk flush afterwards: reads go through Wine (see [`plugins`]),
    // which sees wineserver's in-memory registry immediately, so the file lagging
    // behind does not matter.
}

/// Every plugin registered in this prefix, read from Wine's live registry.
///
/// Goes through `reg query` rather than the on-disk `system.reg`, because
/// wineserver flushes that file lazily and a toggle-then-refresh (what the settings
/// UI does) would otherwise read back the value it just replaced. `query` is the
/// prefix-scoped runner the caller supplies, made to return stdout.
pub fn plugins(prefix: &Path, query: QueryInPrefix) -> Vec<DecalPlugin> {
    let _ = prefix;
    // The 32-bit view is the one a 32-bit client reads, so name it explicitly;
    // `reg query /s` walks every plugin subkey under it in one call.
    let Ok(registered) = query(&["reg", "query", &format!("HKLM\\{PLUGINS_KEY}"), "/s"]) else {
        return Vec::new();
    };
    // Whichever plugins the user has actually switched. Missing entirely on a
    // prefix nobody has configured yet, which is not an error — it just means
    // every plugin still stands at its installed default.
    let chosen = query(&["reg", "query", &format!("HKCU\\{USER_PLUGINS_KEY}"), "/s"])
        .unwrap_or_default();
    let overrides: std::collections::HashMap<String, bool> = parse_plugin_query(&chosen)
        .into_iter()
        .filter_map(|p| p.enabled.map(|on| (p.clsid, on)))
        .collect();

    parse_plugin_query(&registered)
        .into_iter()
        .map(|p| DecalPlugin {
            enabled: overrides.get(&p.clsid).copied().or(p.enabled).unwrap_or(false),
            clsid: p.clsid,
            name: p.name,
        })
        .collect()
}

/// The plugins root, in the 32-bit registry view Decal reads. This is where a
/// plugin is *registered*: its name, its class, and the `Enabled` its installer
/// shipped it with.
const PLUGINS_KEY: &str = "Software\\Wow6432Node\\Decal\\Plugins";

/// Where a plugin is *switched*, per user — and the value that actually decides
/// whether Decal loads it.
///
/// This split is not documented anywhere; it was found by diffing the registry
/// across a toggle in Decal's own agent. `DenAgent` writes `Enabled` here and
/// leaves the [`PLUGINS_KEY`] copy alone, so reading only HKLM reports the state a
/// plugin was *installed* with and never the one the user chose — a plugin running
/// happily in-game shows as off. Both hives use the same `Enabled` value name,
/// which is what makes the mistake so easy to miss.
///
/// Unlike HKLM, `HKCU\Software` is not subject to WoW64 redirection, so there is
/// one copy of this key rather than a 32- and a 64-bit view.
const USER_PLUGINS_KEY: &str = "Software\\Decal\\Plugins";

/// One plugin exactly as reg.exe reported it, before the two hives are reconciled.
///
/// `enabled` is `None` when the key carries no `Enabled` value at all. That is the
/// distinction the merge in [`plugins`] turns on: absent means "this hive has no
/// opinion", which is not the same as "off".
#[derive(Debug)]
struct ParsedPlugin {
    clsid: String,
    name: String,
    enabled: Option<bool>,
}

/// Parse `reg query …\\Decal\\Plugins /s` output into plugins.
///
/// reg.exe prints a blank-line-separated block per key: the key path, then its
/// values as `    Name    TYPE    data` (three or more spaces between columns).
fn parse_plugin_query(text: &str) -> Vec<ParsedPlugin> {
    let mut out: Vec<ParsedPlugin> = Vec::new();
    let mut current: Option<ParsedPlugin> = None;
    let flush = |cur: &mut Option<ParsedPlugin>, out: &mut Vec<ParsedPlugin>| {
        if let Some(p) = cur.take() {
            out.push(p);
        }
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(clsid) = trimmed
            .rsplit('\\')
            .next()
            .filter(|_| trimmed.to_ascii_uppercase().contains("\\DECAL\\PLUGINS\\"))
        {
            // A plugin key line, e.g. HKEY_LOCAL_MACHINE\...\Plugins\{GUID}. Nested
            // keys (a further backslash after the GUID) are not plugins.
            flush(&mut current, &mut out);
            if clsid.starts_with('{') && clsid.ends_with('}') {
                current = Some(ParsedPlugin {
                    clsid: clsid.to_ascii_uppercase(),
                    name: String::new(),
                    enabled: None,
                });
            }
            continue;
        }
        let Some(p) = current.as_mut() else { continue };
        // Value rows are indented; split on runs of whitespace into name/type/data.
        if line.starts_with(' ') || line.starts_with('\t') {
            let cols: Vec<&str> = trimmed.splitn(3, "    ").map(str::trim).collect();
            match cols.as_slice() {
                // The default value carries the friendly name.
                [name, ty, data] if name.eq_ignore_ascii_case("(Default)") || *name == "(default)" => {
                    let _ = ty;
                    p.name = data.to_string();
                }
                [name, _ty, data] if name.eq_ignore_ascii_case("Enabled") => {
                    p.enabled = Some(parse_reg_dword(data) != 0);
                }
                _ => {}
            }
        }
    }
    flush(&mut current, &mut out);
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// reg.exe prints a REG_DWORD as `0x1`; be liberal about the format.
fn parse_reg_dword(s: &str) -> u32 {
    let s = s.trim();
    s.strip_prefix("0x")
        .and_then(|h| u32::from_str_radix(h, 16).ok())
        .or_else(|| s.parse().ok())
        .unwrap_or(0)
}

/// Turn one plugin on or off.
///
/// Writes the per-user [`USER_PLUGINS_KEY`] as well as both HKLM views, and the
/// per-user one is the write that counts — Decal reads that, and it would override
/// an HKLM-only write anyway. The HKLM copies are kept in step so the two hives
/// don't disagree about a plugin nobody has touched since.
pub fn set_plugin_enabled(
    prefix: &Path,
    clsid: &str,
    enabled: bool,
    run: RunInPrefix,
) -> Result<(), String> {
    let value = u32::from(enabled);
    let mut reg = String::from("Windows Registry Editor Version 5.00\r\n\r\n");
    for base in REG_VIEWS {
        reg.push_str(&format!(
            "[HKEY_LOCAL_MACHINE\\{base}\\Decal\\Plugins\\{clsid}]\r\n\
             \"Enabled\"=dword:{value:08x}\r\n\r\n"
        ));
    }
    reg.push_str(&format!(
        "[HKEY_CURRENT_USER\\{USER_PLUGINS_KEY}\\{clsid}]\r\n\
         \"Enabled\"=dword:{value:08x}\r\n\r\n"
    ));
    apply_reg(prefix, &reg, run)
}

/// The two places a 32-bit and a 64-bit client each look for `HKLM\Software`.
const REG_VIEWS: [&str; 2] = ["Software", "Software\\Wow6432Node"];

/// Register a plugin from a DLL on disk, and leave it disabled.
///
/// No `RegAsm` and no .NET: this writes Decal's own surrogate-declaration form,
/// the one its managed `FileService` filter is registered with — the adapter
/// surrogate loads `Object` out of `Assembly` in `Path` itself. `clsid` is the
/// plugin's own type GUID, read from the assembly's metadata by the caller.
pub fn add_plugin(
    prefix: &Path,
    dll: &Path,
    clsid: &str,
    object: &str,
    name: &str,
    run: RunInPrefix,
) -> Result<DecalPlugin, String> {
    let dir = dll.parent().ok_or("the plugin DLL has no directory")?;
    let file = dll
        .file_name()
        .ok_or("the plugin path has no file name")?
        .to_string_lossy()
        .into_owned();
    let esc = |s: String| s.replace('\\', "\\\\");
    let path = esc(format!("{}\\", windows_path(prefix, dir)));

    let mut reg = String::from("Windows Registry Editor Version 5.00\r\n\r\n");
    for base in REG_VIEWS {
        reg.push_str(&format!(
            "[HKEY_LOCAL_MACHINE\\{base}\\Decal\\Plugins\\{clsid}]\r\n\
             @=\"{name}\"\r\n\
             \"Enabled\"=dword:00000000\r\n\
             \"Assembly\"=\"{file}\"\r\n\
             \"Path\"=\"{path}\"\r\n\
             \"Object\"=\"{object}\"\r\n\
             \"Surrogate\"=\"{ADAPTER_SURROGATE}\"\r\n\r\n"
        ));
    }
    apply_reg(prefix, &reg, run)?;
    Ok(DecalPlugin { clsid: clsid.to_string(), name: name.to_string(), enabled: false })
}

/// Register a plugin by reading its identity out of the DLL itself.
///
/// The convenience wrapper over [`add_plugin`]: `RegAsm` would do this from a .NET
/// install we do not have, but everything it needs is in the assembly's metadata
/// (see [`crate::clrmeta`]), so we read it directly.
pub fn add_plugin_from_dll(
    prefix: &Path,
    dll: &Path,
    run: RunInPrefix,
) -> Result<DecalPlugin, String> {
    let id = crate::clrmeta::plugin_identity(dll)?;
    add_plugin(prefix, dll, &id.clsid, &id.object, &id.name, run)
}

/// A host path as Wine sees it: `C:\…` for anything under `drive_c`, else the
/// `Z:` drive that maps the real filesystem.
pub fn windows_path(prefix: &Path, path: &Path) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_path_becomes_a_c_drive_path() {
        let prefix = PathBuf::from("/home/h/prefix");
        assert_eq!(
            windows_path(&prefix, &install_dir(&prefix)),
            r"C:\Program Files (x86)\Decal 3.0"
        );
        assert_eq!(windows_path(&prefix, Path::new("/opt/x/y.msi")), r"Z:\opt\x\y.msi");
    }

    /// The output shape `reg query …\Decal\Plugins /s` actually produces, which is
    /// what [`plugins`] parses. Real reg.exe output, trimmed.
    #[test]
    fn a_reg_query_dump_is_parsed_into_plugins() {
        let dump = "\r\n\
            HKEY_LOCAL_MACHINE\\Software\\Wow6432Node\\Decal\\Plugins\r\n\r\n\
            HKEY_LOCAL_MACHINE\\Software\\Wow6432Node\\Decal\\Plugins\\{6B6B9FA8-37DE-4FA3-8C60-52BD6A2F9855}\r\n    \
            (Default)    REG_SZ    Decal Hotkey System\r\n    \
            Enabled    REG_DWORD    0x1\r\n\r\n\
            HKEY_LOCAL_MACHINE\\Software\\Wow6432Node\\Decal\\Plugins\\{ABCDEF01-0000-0000-0000-000000000000}\r\n    \
            (Default)    REG_SZ    Another\r\n    \
            Enabled    REG_DWORD    0x0\r\n";

        let got = parse_plugin_query(dump);
        assert_eq!(got.len(), 2, "{got:?}");
        let hotkey = got.iter().find(|p| p.clsid.starts_with("{6B6B9FA8")).unwrap();
        assert_eq!(hotkey.name, "Decal Hotkey System");
        assert_eq!(hotkey.enabled, Some(true));
        assert_eq!(
            got.iter().find(|p| p.clsid.starts_with("{ABCDEF01")).unwrap().enabled,
            Some(false)
        );
        // CLSIDs come back upper-cased for a stable match against what we write.
        assert!(got.iter().all(|p| p.clsid == p.clsid.to_ascii_uppercase()));
    }

    /// A key with no `Enabled` value must read as "no opinion", not as "off" —
    /// that distinction is the whole basis of the HKLM/HKCU merge.
    #[test]
    fn a_plugin_without_an_enabled_value_has_no_opinion() {
        let dump = "\r\n\
            HKEY_CURRENT_USER\\Software\\Decal\\Plugins\\{6B6B9FA8-37DE-4FA3-8C60-52BD6A2F9855}\r\n    \
            Order    REG_SZ     \r\n";
        let got = parse_plugin_query(dump);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].enabled, None);
    }

    /// The regression this was written for: Decal's agent records a plugin's real
    /// state under HKCU and leaves the HKLM copy at whatever it was installed with,
    /// so a plugin switched on in Decal read back as off.
    #[test]
    fn the_per_user_switch_beats_the_installed_default() {
        let hklm = "\r\n\
            HKEY_LOCAL_MACHINE\\Software\\Wow6432Node\\Decal\\Plugins\\{AAAAAAAA-0000-0000-0000-000000000000}\r\n    \
            (Default)    REG_SZ    Switched On By User\r\n    \
            Enabled    REG_DWORD    0x0\r\n\r\n\
            HKEY_LOCAL_MACHINE\\Software\\Wow6432Node\\Decal\\Plugins\\{BBBBBBBB-0000-0000-0000-000000000000}\r\n    \
            (Default)    REG_SZ    Never Touched\r\n    \
            Enabled    REG_DWORD    0x1\r\n";
        // The user's hive knows about one of them, and carries no name for it.
        let hkcu = "\r\n\
            HKEY_CURRENT_USER\\Software\\Decal\\Plugins\\{AAAAAAAA-0000-0000-0000-000000000000}\r\n    \
            Enabled    REG_DWORD    0x1\r\n    \
            Order    REG_SZ     \r\n";

        let got = plugins(Path::new("/prefix"), &|args| {
            let key = args.iter().find(|a| a.contains("Decal")).copied().unwrap_or("");
            Ok(if key.starts_with("HKCU") { hkcu.to_string() } else { hklm.to_string() })
        });

        assert_eq!(got.len(), 2, "{got:?}");
        let by = |n: &str| got.iter().find(|p| p.name == n).unwrap().enabled;
        // HKCU says on, HKLM's installed default says off. HKCU wins.
        assert!(by("Switched On By User"));
        // Absent from HKCU entirely, so the installed default stands.
        assert!(by("Never Touched"));
        // Names always come from the registration, which HKCU does not carry.
        assert!(got.iter().all(|p| !p.name.is_empty()));
    }

    /// A missing HKCU key is the normal state of a prefix nobody has configured,
    /// and must leave the installed defaults intact rather than blanking them.
    #[test]
    fn an_absent_user_hive_leaves_the_defaults_alone() {
        let hklm = "\r\n\
            HKEY_LOCAL_MACHINE\\Software\\Wow6432Node\\Decal\\Plugins\\{CCCCCCCC-0000-0000-0000-000000000000}\r\n    \
            (Default)    REG_SZ    On By Default\r\n    \
            Enabled    REG_DWORD    0x1\r\n";
        let got = plugins(Path::new("/prefix"), &|args| {
            let key = args.iter().find(|a| a.contains("Decal")).copied().unwrap_or("");
            if key.starts_with("HKCU") {
                Err("key not found".into())
            } else {
                Ok(hklm.to_string())
            }
        });
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].enabled);
    }

    /// The root key itself, and any nested value key, must not be read as a plugin.
    #[test]
    fn the_plugins_root_and_nested_keys_are_not_plugins() {
        let dump = "\r\n\
            HKEY_LOCAL_MACHINE\\Software\\Wow6432Node\\Decal\\Plugins\r\n\r\n\
            HKEY_LOCAL_MACHINE\\Software\\Wow6432Node\\Decal\\Plugins\\{AAA}\\SubKey\r\n    \
            (Default)    REG_SZ    not a plugin\r\n";
        assert!(parse_plugin_query(dump).is_empty());
    }

    /// The ordinary case: one installer in the archive, wherever it sits.
    #[test]
    fn the_only_installer_in_an_archive_is_the_one_to_run() {
        let names = vec!["VirindiInstaller.exe".to_string()];
        assert_eq!(choose_installer(&names, "virindiinstaller1008").as_deref(), Some("VirindiInstaller.exe"));
        // Buried under a folder, which is how most archives are laid out.
        let nested = vec!["VirindiInstaller1008/VirindiInstaller.exe".to_string()];
        assert_eq!(
            choose_installer(&nested, "virindiinstaller1008").as_deref(),
            Some("VirindiInstaller1008/VirindiInstaller.exe")
        );
    }

    /// With several to choose from, the shallowest wins, then the one the archive
    /// is named after, then the conventionally-named one.
    #[test]
    fn the_likeliest_installer_wins_when_an_archive_holds_several() {
        // A top-level installer beats a bundled helper.
        let names = vec!["setup.exe".to_string(), "redist/vcredist.exe".to_string()];
        assert_eq!(choose_installer(&names, "someplugin").as_deref(), Some("setup.exe"));

        // Two at the top: the one matching the archive name.
        let named = vec!["VirindiInstaller.exe".to_string(), "uninstall.exe".to_string()];
        assert_eq!(
            choose_installer(&named, "virindiinstaller1008").as_deref(),
            Some("VirindiInstaller.exe")
        );

        // No name match: fall back to the conventional one.
        let conventional = vec!["tool.exe".to_string(), "install.exe".to_string()];
        assert_eq!(choose_installer(&conventional, "package").as_deref(), Some("install.exe"));
    }

    /// Genuinely ambiguous archives must not be guessed at — running the wrong
    /// executable out of a download is worse than reporting it.
    #[test]
    fn an_ambiguous_archive_is_reported_rather_than_guessed() {
        let names = vec!["alpha.exe".to_string(), "beta.exe".to_string()];
        assert_eq!(choose_installer(&names, "package"), None);
        assert_eq!(choose_installer(&[], "package"), None);
    }

    /// The case this exists for: Virindi's installer is a small executable that
    /// loads SharpZipLib from beside itself, and staging it alone broke every
    /// plugin in the package.
    #[test]
    fn an_installers_libraries_are_recognised_beside_it() {
        let bytes = b"..ICSharpCode.SharpZipLib, Version=0.85.5.452, PublicKeyToken=1b03e..";
        let siblings =
            ["ICSharpCode.SharpZipLib.dll", "VirindiInstaller1008.zip", "unrelated-tool.dll"];
        assert_eq!(
            referenced_siblings(bytes, &siblings),
            vec!["ICSharpCode.SharpZipLib.dll".to_string()]
        );
    }

    /// Names reached through resource strings are UTF-16, not ASCII.
    #[test]
    fn a_reference_is_found_whichever_width_it_is_stored_at() {
        let wide: Vec<u8> =
            "needs Helper.dll".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(referenced_siblings(&wide, &["Helper.dll"]), vec!["Helper.dll".to_string()]);
        // …and offset by one byte, since a UTF-16 run need not start even.
        let mut shifted = vec![0u8];
        shifted.extend_from_slice(&wide);
        assert_eq!(referenced_siblings(&shifted, &["Helper.dll"]), vec!["Helper.dll".to_string()]);
    }

    /// Nothing unrelated gets dragged along, and a stem too short to be meaningful
    /// never matches — the guard against copying a user's whole Downloads folder.
    #[test]
    fn unmentioned_and_short_named_neighbours_are_left_behind() {
        let bytes = b"a installer that mentions nothing in particular";
        assert!(referenced_siblings(bytes, &["SomeLibrary.dll", "big-disk-image.iso"]).is_empty());
        // "a.dll" would match the "a" in almost any file; it is rejected on length.
        assert!(referenced_siblings(b"a a a a a", &["a.dll", "x64.dll"]).is_empty());
    }

    /// The golden vector is real: this is the product code the VC++ 2005
    /// redistributable installs under, paired with the registry key Wine's msi
    /// actually created for it in a live prefix.
    #[test]
    fn a_product_code_packs_the_way_windows_installer_keys_it() {
        assert_eq!(
            pack_product_code("{710f4c1c-cc18-4c49-8cbf-51240c89a1a2}").as_deref(),
            Some("c1c4f01781cc94c4c8fb1542c0981a2a")
        );
        // The code the plugin installers probe for, packed the same way.
        assert_eq!(
            pack_product_code(VC80_LEGACY_PRODUCT_CODE).as_deref(),
            Some("B25099274A207264182F8181ADD555D0")
        );
        // A packed code is always 32 characters, whatever the case of the input.
        assert_eq!(pack_product_code(VC80_PRODUCT_CODE).map(|s| s.len()), Some(32));
    }

    #[test]
    fn a_malformed_product_code_is_rejected_rather_than_mangled() {
        for bad in [
            "710f4c1c-cc18-4c49-8cbf-51240c89a1a2",   // unbraced
            "{710f4c1c-cc18-4c49-8cbf-51240c89a1a}",  // short final group
            "{710f4c1c-cc18-4c49-8cbf}",              // too few groups
            "{710g4c1c-cc18-4c49-8cbf-51240c89a1a2}", // not hex
        ] {
            assert_eq!(pack_product_code(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_reg_dword_is_parsed_from_either_notation() {
        assert_eq!(parse_reg_dword("0x1"), 1);
        assert_eq!(parse_reg_dword("0x0000000a"), 10);
        assert_eq!(parse_reg_dword("  0x1 "), 1);
        assert_eq!(parse_reg_dword("7"), 7);
        assert_eq!(parse_reg_dword("garbage"), 0);
    }

    #[test]
    fn a_failed_query_reads_back_as_empty_rather_than_failing() {
        // reg.exe returns non-zero when the key is absent; that must be "no
        // plugins", not an error the settings panel has to handle.
        let none = plugins(Path::new("/prefix"), &|_| Err("key not found".into()));
        assert!(none.is_empty());
        assert!(!is_installed(Path::new("/nonexistent-prefix")));
    }

    /// The template is only useful if both placeholders survive generation and
    /// every key is written to both WoW64 views -- a 32-bit client reads only the
    /// Wow6432Node one, and getting this wrong makes CoCreateInstance fail with
    /// "class not registered" on keys that look present.
    #[test]
    fn the_registry_template_covers_both_views_and_templates_both_paths() {
        assert!(REG_TEMPLATE.contains("@@INSTALLDIR@@"));
        assert!(REG_TEMPLATE.contains("@@PORTALPATH@@"));

        let plain = REG_TEMPLATE.matches(r"[HKEY_LOCAL_MACHINE\Software\Classes\CLSID\").count();
        let wow = REG_TEMPLATE
            .matches(r"[HKEY_LOCAL_MACHINE\Software\Classes\Wow6432Node\CLSID\")
            .count();
        assert!(plain > 100, "expected the COM registration, got {plain} CLSID keys");
        assert_eq!(plain, wow, "every CLSID key must exist in both views");
    }

    #[test]
    fn rendering_the_template_escapes_the_paths_it_substitutes() {
        let prefix = PathBuf::from("/p");
        let rendered = render_template(&prefix, Path::new("/p/drive_c/Turbine/Asheron's Call"));
        assert!(!rendered.contains("@@INSTALLDIR@@"));
        assert!(!rendered.contains("@@PORTALPATH@@"));
        // Backslashes doubled, because everything around them in a .reg is escaped.
        assert!(
            rendered.contains(r"C:\\Program Files (x86)\\Decal 3.0\\"),
            "install dir was not escaped"
        );
        assert!(rendered.contains(r"C:\\Turbine\\Asheron's Call\\"), "portal path was not escaped");
    }
}

#[cfg(test)]
mod engine_patch_tests {
    use super::*;

    /// The export walker against a real PE. Uses the runtime that is installed on
    /// this machine, so it is only meaningful where one is -- but where it runs it
    /// is the real check: that the three names resolve to code, and that what is
    /// there is one of the two prologues `patch_engine` accepts.
    #[test]
    #[ignore = "needs a provisioned runtime on this machine"]
    fn the_hotpatch_targets_resolve_by_name_in_the_installed_runtime() {
        // The runtime root sits at a different depth in each build -- the Whisky
        // engine unpacks to `Libraries/Wine/`, a Proton copy to
        // `GE-Proton10-34/files/` -- so search for it rather than guess.
        //
        // Search for the *root*, i.e. a directory holding `lib/wine/i386-windows`,
        // and never for `i386-windows` alone: GE-Proton ships six `d3d9.dll`s, and
        // `lib/wine/dxvk/i386-windows/d3d9.dll` is DXVK's, whose prologue is a real
        // `push ebp` and which must not be touched. Production resolves this
        // exactly, from the wine binary's own path; only a test has to look.
        fn find_root(dir: &Path, depth: usize) -> Option<PathBuf> {
            if depth == 0 {
                return None;
            }
            if dir.join("lib/wine/i386-windows").is_dir() {
                return Some(dir.to_path_buf());
            }
            std::fs::read_dir(dir)
                .ok()?
                .flatten()
                .filter(|e| e.path().is_dir())
                .find_map(|e| find_root(&e.path(), depth - 1))
        }
        let root = crate::install::runtime_dir();
        let dir = find_root(&root, 8)
            .unwrap_or_else(|| panic!("no runtime with lib/wine under {}", root.display()))
            .join("lib/wine/i386-windows");
        for (file, exports) in ENGINE_HOTPATCH_TARGETS {
            let bytes = std::fs::read(dir.join(file)).expect("read builtin DLL");
            for name in exports {
                let off = export_offset(&bytes, name)
                    .unwrap_or_else(|| panic!("{file}: {name} did not resolve"));
                let prologue = &bytes[off..off + 2];
                assert!(
                    prologue == [0x8b, 0xff] || prologue == [0x90, 0x90],
                    "{file} {name}@{off:#x}: unexpected prologue {prologue:02x?}"
                );
                println!("{file:>14} {name:<16} off={off:#x} prologue={prologue:02x?}");
            }
        }
    }

    /// A malformed or non-PE buffer must come back `None`, not panic: this walks
    /// attacker-irrelevant but arbitrary files and indexes on values read from them.
    #[test]
    fn rubbish_input_does_not_panic() {
        assert_eq!(export_offset(b"", "Direct3DCreate9"), None);
        assert_eq!(export_offset(&[0u8; 512], "Direct3DCreate9"), None);
        let mut junk = vec![0xffu8; 4096];
        junk[0x3c] = 0x80;
        assert_eq!(export_offset(&junk, "Direct3DCreate9"), None);
    }
}
