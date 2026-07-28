//! The host tools the Linux runtime needs, and installing them.
//!
//! ## Only one of them is actually required
//!
//! * **`umu-run`** — required. It is how every Windows-side command reaches the
//!   prefix; without it nothing runs at all.
//! * **`gamescope`** — recommended, not required. [`crate::proton::launch`] wraps
//!   the client in it when present and falls back to plain umu-run (with DXVK
//!   rather than wined3d) when it is not. Worth installing — it is what gives
//!   wined3d a display adapter to enumerate on a bare Wayland session — but its
//!   absence is not a reason to refuse to set up.
//! * **`winetricks`** — *not needed*, despite years of setup instructions saying
//!   so. [`crate::decal`] runs a **bundled, pinned** copy of the script rather
//!   than whatever the distro ships, and the step that used the host one
//!   (`winetricks -q vcrun2019`) was deleted once the client's import tables
//!   showed it never needed those runtimes. Checking for it only ever produced a
//!   false failure.
//!
//! ## Installing
//!
//! betterAC is not Bazzite-only and has not been for a while. This detects the
//! host's package manager and installs what is missing, escalating through
//! `pkexec` (a graphical prompt) or `sudo` (a terminal one). Set
//! `BETTERAC_NO_DEPS=1` to be told what to install instead of having it done.


use std::process::Command;

/// A tool betterAC drives on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool {
    /// The binary looked for on `PATH`.
    pub bin: &'static str,
    /// False for tools the launcher works without.
    pub required: bool,
}

pub const UMU: Tool = Tool { bin: "umu-run", required: true };
pub const GAMESCOPE: Tool = Tool { bin: "gamescope", required: false };
pub const TOOLS: [Tool; 2] = [UMU, GAMESCOPE];

/// Is `bin` on `PATH`?
pub fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// A host package manager, and how to install with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manager {
    pub name: &'static str,
    /// Refresh the package index first, where a stale one makes the install ask
    /// the mirror for versions it no longer has and 404.
    pub refresh: Option<&'static [&'static str]>,
    /// The install verb, including any non-interactive flags when we run it
    /// ourselves, and written to be correct to *show* either way.
    pub command: &'static [&'static str],
    /// May betterAC run this itself?
    ///
    /// False on Arch, and the reason is a rule rather than a preference: pacman
    /// has no supported way to install one package without a full system upgrade.
    /// `-S` alone resolves against a possibly-stale database and 404s; `-Sy pkg`
    /// is the classic partial-upgrade footgun; `-Syu` is correct and means
    /// upgrading the whole system — measured on Omarchy, adding umu-launcher and
    /// gamescope wanted **78 packages, 1.7 GB, including an NVIDIA driver
    /// update**. A game launcher should not do that to a machine unasked, so we
    /// print the command and let the user run it.
    ///
    /// `--noconfirm` is also unsafe there: it silently answered a 14-way
    /// `lib32-vulkan-driver` provider prompt with the default, which was the
    /// NVIDIA stack — wrong on an AMD host.
    pub unattended: bool,
    /// True for image-based distros where a package layered now only exists
    /// after a reboot, so setup cannot simply carry on.
    pub reboot_required: bool,
}

/// The host's package manager.
///
/// Order matters: an image-based Fedora has **both** `rpm-ostree` and `dnf`, and
/// using `dnf` there would appear to succeed and change nothing that survives.
/// `rpm-ostree` therefore has to be tested first.
pub fn detect() -> Option<Manager> {
    const CANDIDATES: &[Manager] = &[
        Manager {
            name: "rpm-ostree",
            refresh: None,
            command: &["rpm-ostree", "install", "--idempotent", "-y"],
            unattended: true,
            reboot_required: true,
        },
        // Shown, never run -- and shown as `-Syu`, the only correct form.
        Manager {
            name: "pacman",
            refresh: None,
            command: &["pacman", "-Syu", "--needed"],
            unattended: false,
            reboot_required: false,
        },
        Manager {
            name: "dnf",
            refresh: None,
            command: &["dnf", "install", "-y"],
            unattended: true,
            reboot_required: false,
        },
        // A stale apt index 404s exactly like a stale pacman one; unlike Arch,
        // refreshing it is not a partial upgrade, so we can just do it.
        Manager {
            name: "apt-get",
            refresh: Some(&["apt-get", "update"]),
            command: &["apt-get", "install", "-y"],
            unattended: true,
            reboot_required: false,
        },
        Manager {
            name: "zypper",
            refresh: None,
            command: &["zypper", "--non-interactive", "install"],
            unattended: true,
            reboot_required: false,
        },
        // Void discourages partial updates for the same reason Arch does.
        Manager {
            name: "xbps-install",
            refresh: None,
            command: &["xbps-install", "-Su"],
            unattended: false,
            reboot_required: false,
        },
    ];
    CANDIDATES.iter().find(|m| on_path(m.name)).cloned()
}

/// What `tool` is called in `mgr`'s repositories, or `None` where the distro does
/// not package it.
///
/// Debian and Ubuntu do not package umu-launcher at all, which is why the `.deb`
/// idea was dropped: a package that cannot depend on the one thing it needs is
/// worse than no package. Those users install umu from its own releases.
pub fn package(mgr: &Manager, tool: Tool) -> Option<&'static str> {
    match (mgr.name, tool.bin) {
        (_, "gamescope") => Some("gamescope"),
        ("pacman", "umu-run") => Some("umu-launcher"),
        ("rpm-ostree", "umu-run") | ("dnf", "umu-run") => Some("umu-launcher"),
        _ => None,
    }
}

/// How to become root for a package install: a graphical prompt when there is a
/// desktop to show it on, a terminal one otherwise, and nothing when already root.
///
/// `pkexec` matters for the GUI: setup runs on a background thread with no
/// terminal, so `sudo` there would block forever on a password prompt nobody can
/// see.
pub fn escalator() -> Option<&'static str> {
    if is_root() {
        return None;
    }
    let graphical = std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var_os("DISPLAY").is_some();
    if graphical && on_path("pkexec") {
        return Some("pkexec");
    }
    on_path("sudo").then_some("sudo")
}

fn is_root() -> bool {
    // No libc dependency for one number: /proc/self/status is stable on Linux and
    // this crate is deliberately dependency-light.
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|l| l.split_whitespace().next().map(str::to_string))
        })
        .is_some_and(|uid| uid == "0")
}

/// The full argv for installing `packages`, escalation included.
///
/// Split out from running it so the command can be shown to the user verbatim —
/// what we would have run is exactly what they can run by hand.
pub fn install_command(mgr: &Manager, packages: &[&str], escalate: Option<&str>) -> Vec<String> {
    escalate
        .into_iter()
        .chain(mgr.command.iter().copied())
        .map(String::from)
        .chain(packages.iter().map(|p| p.to_string()))
        .collect()
}

/// Install `packages`, returning the command's own complaint on failure.
///
/// Callers must check [`Manager::unattended`] first; this does not, so that the
/// decision lives in one place rather than being re-derived here.
pub fn install(mgr: &Manager, packages: &[&str]) -> Result<(), String> {
    // Refresh first where a stale index is a known 404 source. Best-effort: if it
    // fails the install below will produce the better error anyway.
    if let Some(refresh) = mgr.refresh {
        let argv = escalator()
            .into_iter()
            .chain(refresh.iter().copied())
            .map(String::from)
            .collect::<Vec<_>>();
        let _ = Command::new(&argv[0]).args(&argv[1..]).output();
    }
    let argv = install_command(mgr, packages, escalator());
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .map_err(|e| format!("could not run {}: {e}", argv[0]))?;
    if out.status.success() {
        return Ok(());
    }
    let why = [&out.stderr, &out.stdout]
        .iter()
        .map(|b| String::from_utf8_lossy(b))
        .find(|s| !s.trim().is_empty())
        .map(|s| s.lines().rev().take(4).collect::<Vec<_>>().join("\n  "))
        .unwrap_or_default();
    Err(format!("{} failed:\n  {why}", argv.join(" ")))
}

/// Where to get a tool the distro does not package.
pub fn manual_hint(tool: Tool) -> &'static str {
    match tool.bin {
        "umu-run" => "umu-launcher is not in your distro's repositories.\n       \
                      Get it from https://github.com/Open-Wine-Components/umu-launcher/releases",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr(name: &'static str) -> Manager {
        detect_named(name).expect("known manager")
    }
    /// The table lookup without touching PATH, so the tests are hermetic.
    fn detect_named(name: &'static str) -> Option<Manager> {
        const ALL: &[Manager] = &[
            Manager {
                name: "rpm-ostree",
                refresh: None,
                command: &["rpm-ostree", "install", "--idempotent", "-y"],
                unattended: true,
                reboot_required: true,
            },
            Manager {
                name: "pacman",
                refresh: None,
                command: &["pacman", "-Syu", "--needed"],
                unattended: false,
                reboot_required: false,
            },
            Manager {
                name: "apt-get",
                refresh: Some(&["apt-get", "update"]),
                command: &["apt-get", "install", "-y"],
                unattended: true,
                reboot_required: false,
            },
        ];
        ALL.iter().find(|m| m.name == name).cloned()
    }

    #[test]
    fn only_umu_is_required() {
        const { assert!(UMU.required) }; // without umu-run nothing reaches the prefix
        const { assert!(!GAMESCOPE.required) }; // the launch falls back to DXVK
        // winetricks is deliberately absent: decal runs a bundled copy, and the
        // step that used the host one is gone. Checking for it failed setups that
        // would have worked.
        assert!(!TOOLS.iter().any(|t| t.bin == "winetricks"));
    }

    #[test]
    fn arch_gets_umu_from_multilib_under_its_real_name() {
        // The binary is `umu-run`; the package is `umu-launcher`. Searching for
        // the binary name would find nothing.
        assert_eq!(package(&mgr("pacman"), UMU), Some("umu-launcher"));
        assert_eq!(package(&mgr("pacman"), GAMESCOPE), Some("gamescope"));
    }

    #[test]
    fn debian_has_gamescope_but_not_umu() {
        assert_eq!(package(&mgr("apt-get"), GAMESCOPE), Some("gamescope"));
        assert_eq!(package(&mgr("apt-get"), UMU), None, "Debian does not package umu");
        assert!(manual_hint(UMU).contains("umu-launcher/releases"));
    }

    #[test]
    fn an_image_based_host_is_flagged_as_needing_a_reboot() {
        assert!(mgr("rpm-ostree").reboot_required);
        assert!(!mgr("pacman").reboot_required);
    }

    #[test]
    fn the_command_is_exactly_what_we_would_run() {
        let c = install_command(&mgr("apt-get"), &["gamescope"], Some("pkexec"));
        assert_eq!(c, ["pkexec", "apt-get", "install", "-y", "gamescope"]);
        // Already root: no escalation prefix at all.
        assert_eq!(install_command(&mgr("apt-get"), &["gamescope"], None),
                   ["apt-get", "install", "-y", "gamescope"]);
    }

    /// Arch is shown a command, never run one — and the command shown is `-Syu`.
    ///
    /// `-S` alone resolves against a stale database and 404s (measured on Omarchy);
    /// `-Sy pkg` is a partial upgrade; `--noconfirm` silently answers provider
    /// prompts, which picked the NVIDIA driver stack on that box.
    #[test]
    fn pacman_is_never_run_unattended_and_is_shown_the_full_upgrade() {
        let pac = mgr("pacman");
        assert!(!pac.unattended, "Arch has no supported single-package install");
        let shown = install_command(&pac, &["umu-launcher", "gamescope"], Some("sudo"));
        assert_eq!(shown, ["sudo", "pacman", "-Syu", "--needed", "umu-launcher", "gamescope"]);
        assert!(!shown.contains(&"--noconfirm".to_string()), "must not answer provider prompts");

        // The managers we do drive ourselves are the conventional ones.
        assert!(mgr("apt-get").unattended);
        assert!(mgr("rpm-ostree").unattended);
    }

    /// A stale apt index 404s the same way pacman's does; refreshing it first is
    /// safe on Debian, so it is done rather than left to fail.
    #[test]
    fn apt_refreshes_its_index_first() {
        assert_eq!(mgr("apt-get").refresh, Some(["apt-get", "update"].as_slice()));
        assert_eq!(mgr("pacman").refresh, None, "refreshing Arch alone is the footgun");
    }
}
