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
    /// The install verb, already including any non-interactive flags.
    pub install: &'static [&'static str],
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
            install: &["rpm-ostree", "install", "--idempotent", "-y"],
            reboot_required: true,
        },
        Manager { name: "pacman", install: &["pacman", "-S", "--needed", "--noconfirm"], reboot_required: false },
        Manager { name: "dnf", install: &["dnf", "install", "-y"], reboot_required: false },
        Manager { name: "apt-get", install: &["apt-get", "install", "-y"], reboot_required: false },
        Manager { name: "zypper", install: &["zypper", "--non-interactive", "install"], reboot_required: false },
        Manager { name: "xbps-install", install: &["xbps-install", "-Sy"], reboot_required: false },
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
        .chain(mgr.install.iter().copied())
        .map(String::from)
        .chain(packages.iter().map(|p| p.to_string()))
        .collect()
}

/// Install `packages`, returning the command's own complaint on failure.
pub fn install(mgr: &Manager, packages: &[&str]) -> Result<(), String> {
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
        const ALL: &[(&str, &[&str], bool)] = &[
            ("rpm-ostree", &["rpm-ostree", "install", "--idempotent", "-y"], true),
            ("pacman", &["pacman", "-S", "--needed", "--noconfirm"], false),
            ("apt-get", &["apt-get", "install", "-y"], false),
        ];
        ALL.iter().find(|(n, ..)| *n == name).map(|(n, i, r)| Manager {
            name: n,
            install: i,
            reboot_required: *r,
        })
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
        let c = install_command(&mgr("pacman"), &["umu-launcher", "gamescope"], Some("pkexec"));
        assert_eq!(
            c,
            ["pkexec", "pacman", "-S", "--needed", "--noconfirm", "umu-launcher", "gamescope"]
        );
        // Already root: no escalation prefix at all.
        let c = install_command(&mgr("pacman"), &["gamescope"], None);
        assert_eq!(c, ["pacman", "-S", "--needed", "--noconfirm", "gamescope"]);
    }
}
