//! Binary patches applied to the End-of-Retail `acclient.exe`.
//!
//! The client we ship is the community End-of-Retail build (PE link 2015-06-12),
//! which carries a few defects that only show up on hardware nobody had while the
//! game was live. They are not fixable from config or from the UI, so we patch the
//! executable in place, once, right after `ac-updates.zip` unpacks over the retail
//! install.
//!
//! Every patch is a same-length byte swap guarded by the bytes it expects to find.
//! That buys three things worth having:
//!
//!   * **Idempotence.** Re-running setup sees the patched bytes and reports
//!     `AlreadyApplied` instead of corrupting the file a second time.
//!   * **Fail-safe.** If the update bundle ever ships a different client, the
//!     expected bytes will not match, and we skip that patch and say so rather
//!     than writing into the wrong instruction.
//!   * **No relocation.** Equal-length swaps mean nothing in the PE shifts: no
//!     section resize, no relocation fixups, no checksum concerns.
//!
//! Offsets are **file offsets**, not virtual addresses. In this binary `.text` is
//! mapped 1:1 (section VA == raw pointer), so file offset == RVA and
//! VA = 0x400000 + offset. Each patch records the VA in its comment so the site
//! can be found again in a disassembler.
//!
//! Nothing is left behind. The patched image is built in memory, written to a
//! temporary file beside the client, and renamed over it. The rename is atomic,
//! so an interrupted install leaves the client either wholly unpatched or wholly
//! patched — never truncated — and no copy of the unpatched client survives.

use std::path::{Path, PathBuf};

/// One same-length byte swap at a known file offset.
pub struct Patch {
    /// Stable identifier, used in progress messages and logs.
    pub name: &'static str,
    /// What it fixes, in plain words — this reaches the user on a mismatch.
    pub why: &'static str,
    /// Offset into the file. See the module note: file offset, not VA.
    pub offset: usize,
    /// Bytes that must be there for the patch to apply.
    pub expect: &'static [u8],
    /// Bytes to write. Must be the same length as `expect`.
    pub patched: &'static [u8],
    /// Skip this patch when the client is not going to run under macOS/Wine.
    ///
    /// Three of the five are gated, and for two different reasons.
    ///
    /// The **window-style** pair is meaningless elsewhere: it exists to get a
    /// macOS fullscreen Space out of winemac.drv. On Linux AC runs fullscreen
    /// inside gamescope and never shows a window frame at all.
    ///
    /// **`login-resolution` is gated because on Linux it would be a regression**,
    /// which is a stronger statement than "unnecessary" and the reason this flag
    /// is not just an optimisation. On macOS it trades a stretched pre-world
    /// screen for a correctly-proportioned one drawn small in the corner, and that
    /// trade was the user's explicit choice. Under gamescope there is no such
    /// trade: AC's hardcoded 800x600 becomes an 800x600 device that gamescope —
    /// a nested compositor sized to the display — scales up to fill the screen,
    /// with the aspect handled for us. Removing the hardcoding would replace a
    /// full-screen menu with the corner box for no gain at all.
    ///
    /// Leaving the Linux client byte-identical for all three keeps that path
    /// exactly as it was.
    pub macos_only: bool,
}

/// Every patch we apply, in order.
///
/// ## widescreen-viewport (VA 0x4d6f3b)
///
/// The 3D world is drawn into a viewport taken from a UI element rather than from
/// the device. `0x4d6f20` builds the rect from four accessors on that element
/// (`GetX/GetY/GetW/GetH` at 0x69fe00/0x69fe30/0x69fe60/0x69fe70) and hands it to
/// `device->SetViewport`. The element is **hardcoded 3000 px wide** in AC's layout
/// data, so on any display wider than 3000 the scene stops short and leaves a
/// black bar on the right, while the HUD — which uses the real device width —
/// still reaches the edge. No monitor of AC's era was wide enough to expose it.
///
/// The fix takes the width from the device instead of the element:
///
/// ```text
///   8b ce  e8 1e 8f 1c 00     mov ecx,esi ; call 0x69fe60  (element GetW -> 3000)
///   8b cf  e8 de 8d 07 00     mov ecx,edi ; call 0x54fd20  (Device::GetWidth)
/// ```
///
/// `edi` already holds the device (loaded at 0x4d6f23) and is callee-saved, so it
/// survives the sibling accessor calls. Height, x and y are left alone: they are
/// already full-height and zero.
///
/// Deliberately *not* patched: `0x695fc2`, the generic "render 3D into a UI panel
/// rect" path. It is shared with the inventory paperdoll, and bypassing it makes
/// the character preview render full-screen instead of inside its panel. The
/// portal/loading screen still shows the bar for this reason.
///
/// ## viewport-height (VA 0x4d6f33)
///
/// The sibling call, seven bytes earlier in the same argument push sequence. It
/// took the viewport *height* from the same UI element, via `GetH` (0x69fe70):
///
/// ```text
///   8b ce  e8 36 8f 1c 00     mov ecx,esi ; call 0x69fe70  (element GetH)
///   8b cf  e8 f6 8d 07 00     mov ecx,edi ; call 0x54fd30  (Device::GetHeight)
/// ```
///
/// The element happens to be full-height today, so this changes nothing visible
/// on its own. It is here because leaving it half-driven by the element is a trap:
/// anything that later scales element rects would silently drag the 3D viewport
/// along with it. With both patches the viewport is entirely device-driven.
///
/// The two sites are adjacent but disjoint — 0x4d6f33..0x4d6f3a and
/// 0x4d6f3b..0x4d6f42 — and `edi` is loaded with the device at 0x4d6f23, before
/// either, so it is live for both.
///
/// ## login-resolution (VA 0x4393d0, macOS only)
///
/// AC's splash, login and character-select screens pin themselves to 800x600 and
/// ignore the configured resolution, so without this the client would spend the
/// first part of every session as a small window and only reach full size on world
/// entry.
///
/// The pinning goes through `SetForcedResolution(force, w, h)` (0x43a990), which
/// three callers invoke with a hardcoded `(1, 800, 600)`. It writes a flag at
/// 0x8381a0 and the size at 0x818b04/0x818b08, and `GetDesiredDisplayMode`
/// (0x439370) — the single source of truth for *both* window sizing and device
/// creation — then overrides the configured resolution with it:
///
/// ```text
///   8a 0d a0 81 83 00     mov cl,[0x8381a0]   ; the force flag
///   84 c9                 test cl,cl
///   74 12                 je 0x4393e4         ; -> jmp: skip the override always
///   8b 15 04 8b 81 00     mov edx,[0x818b04]  ; forced width  (800)
///   …
/// ```
///
/// One byte — `je` becomes `jmp` — makes the override unreachable, so every screen
/// falls back to the configured resolution, which [`crate::prefs::apply`] pins to
/// the display size on every launch. That is the same value the in-world view
/// already renders at, so nothing new has to be validated.
///
/// Patched here rather than at the three call sites: those are scattered (0x4049ab,
/// 0x4ead23, 0x4eafe1) and only two of them share a searchable byte pattern, while
/// this one site covers every caller.
///
/// What this does **not** fix, measured rather than assumed: those screens still
/// *draw* at 800x600. A wined3d trace shows the backbuffer and the only viewport are
/// both the full display size, so the 800x600 is the UI layout itself — fixed-pixel
/// artwork anchored top-left — and it sits in the top-left corner of the window.
/// There is no lever to change that: the forced-resolution globals have exactly four
/// code references each, all inside the display-mode functions, so nothing in the UI
/// reads them.
///
/// **The alternative was built, measured and reverted on 2026-07-26 — do not
/// re-attempt it.** The idea was to leave the client at its own 800x600 device and
/// give the *window* a 4:3 rect the size of the display, letting the renderer scale
/// one over the other, the way the monitor does on real hardware. It cannot work
/// here: `WINEDEBUG=+d3d` shows every present of a session as
/// `src_rect (0,0)-(800,600), dst_rect (0,0)-(800,600)`, unchanged whatever the
/// window does, because wined3d resolves the NULL destination AC passes to the
/// *backbuffer* size rather than to the window's client rect. A bigger window only
/// puts the same picture in the corner of a bigger frame — strictly worse than this.
/// Scaling those screens for real needs a d3d9 renderer whose present blits through
/// a scaling path (DXVK's does; wined3d's does not), or an exclusive-fullscreen
/// 800x600 mode, which a MacBook's built-in panel does not offer.
///
/// Known side effect: the `ForceDisplayResolution` console command (registered at
/// 0x43bf3e) stops taking effect. It was never confirmed to work in the first place
/// — it only resizes the window, so it could never rescue a failed device anyway.
///
/// ## window-style-create (VA 0x43bc0b) and window-style-restyle (VA 0x43a577), macOS only
///
/// Two changes to the style of AC's window, for the price of one immediate each:
///
///   * **add `WS_THICKFRAME|WS_MAXIMIZEBOX`.** winemac.drv only grants a window the
///     native macOS fullscreen capability (`NSWindowCollectionBehaviorFullScreenPrimary`
///     — the green button, and with it the ability to live in its own Space) when the
///     Win32 style has a resizable frame. AC never sets one, so its window could only
///     ever be the borderless display-covering overlay.
///   * **drop `WS_MINIMIZEBOX`.** winemac maps it to
///     `NSWindowStyleMaskMiniaturizable`; without it the yellow button is disabled and
///     Cmd-M does nothing, so the game cannot be minimised out from under the player.
///
/// AC builds that style branchlessly, in two places that mirror each other: once
/// for the `dwStyle` it hands `CreateWindowExA` (0x43bd30), and once for the
/// `SetWindowLongA` inside `ApplyDisplayMode` (0x43a510).
///
/// ```text
///   creation:  and edx,0x80ca0000 ; add edx,0x82000000     [| WS_VISIBLE later]
///   restyle:   and eax,0x7f360000 ; add eax,0x12ca0000
/// ```
///
/// A `neg`/`sbb` on a flag leaves the register 0 or -1 first, so one style is the
/// *add* immediate alone and the other is mask+add. Note the two sites test
/// *opposite* flags — the creation site asks "is windowed", the restyle site asks
/// "is fullscreen" — so which immediate is which swaps between them. Either way
/// both end up at 0x92000000 fullscreen (a popup overlay) and 0x12CA0000 windowed:
/// caption, sysmenu, minimize box, and no `WS_THICKFRAME`. (The creation site ORs
/// in `WS_VISIBLE` separately, a few instructions later.)
///
/// Both patches move the windowed result from 0x12CA0000 to 0x12CD0000 (+0x50000
/// frame, -0x20000 minimize box) while leaving the fullscreen one bit-identical, so
/// `BETTERAC_FULLSCREEN` and the virtual `Desktop` mode are untouched:
///
/// ```text
///   creation:  and edx,0x80cd0000   (add unchanged: there it *is* the fullscreen style)
///   restyle:   and eax,0x7f330000 ; add eax,0x12cd0000
/// ```
///
/// The creation site is the one that does the work — measured on a live client,
/// `ApplyDisplayMode` never runs during a normal session, because the login screen
/// asks for its mode before the render backend exists and `SetForcedResolution`
/// (0x43a990) returns early on a null 0x86734c. The restyle site is patched anyway
/// so that a window AC *does* restyle later keeps its frame instead of silently
/// losing the Space.
///
/// Between them AC applies the frame itself, which is what let the old
/// `acwindow.exe` helper — a Win32 process that polled for the window and re-added
/// the bits from outside — be deleted outright.
pub const PATCHES: &[Patch] = &[
    Patch {
        name: "widescreen-viewport",
        why: "lets the 3D view fill displays wider than 3000 px",
        offset: 0x0D_6F3B,
        expect: &[0x8b, 0xce, 0xe8, 0x1e, 0x8f, 0x1c, 0x00],
        patched: &[0x8b, 0xcf, 0xe8, 0xde, 0x8d, 0x07, 0x00],
        macos_only: false,
    },
    Patch {
        name: "viewport-height",
        why: "takes the 3D view height from the display rather than a UI element",
        offset: 0x0D_6F33,
        expect: &[0x8b, 0xce, 0xe8, 0x36, 0x8f, 0x1c, 0x00],
        patched: &[0x8b, 0xcf, 0xe8, 0xf6, 0x8d, 0x07, 0x00],
        macos_only: false,
    },
    Patch {
        name: "login-resolution",
        why: "brings the login and character-select screens up at the display resolution",
        offset: 0x03_93C8,
        expect: &[0x8a, 0x0d, 0xa0, 0x81, 0x83, 0x00, 0x84, 0xc9, 0x74, 0x12],
        patched: &[0x8a, 0x0d, 0xa0, 0x81, 0x83, 0x00, 0x84, 0xc9, 0xeb, 0x12],
        macos_only: true,
    },
    Patch {
        name: "window-style-create",
        why: "lets macOS give the game window its own fullscreen Space, and stops it minimising",
        offset: 0x03_BC0B,
        expect: &[0x81, 0xe2, 0x00, 0x00, 0xca, 0x80, 0x81, 0xc2, 0x00, 0x00, 0x00, 0x82],
        patched: &[0x81, 0xe2, 0x00, 0x00, 0xcd, 0x80, 0x81, 0xc2, 0x00, 0x00, 0x00, 0x82],
        macos_only: true,
    },
    Patch {
        name: "window-style-restyle",
        why: "keeps that window style when the client re-applies it",
        offset: 0x03_A577,
        expect: &[0x25, 0x00, 0x00, 0x36, 0x7f, 0x05, 0x00, 0x00, 0xca, 0x12],
        patched: &[0x25, 0x00, 0x00, 0x33, 0x7f, 0x05, 0x00, 0x00, 0xcd, 0x12],
        macos_only: true,
    },
];

/// What happened to one patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Expected bytes found and replaced.
    Applied,
    /// Patched bytes already present — nothing to do.
    AlreadyApplied,
    /// Neither expected nor patched bytes present. The client is not the build
    /// this patch was written against, so it is skipped rather than forced.
    Skipped,
}

/// Apply one patch to an in-memory image. Split out from the file handling so the
/// byte logic is testable without touching a 4.8 MB executable.
pub fn apply_to_bytes(buf: &mut [u8], p: &Patch) -> Outcome {
    debug_assert_eq!(p.expect.len(), p.patched.len(), "{}: patches must be same-length", p.name);
    let end = p.offset + p.expect.len();
    if end > buf.len() {
        return Outcome::Skipped;
    }
    let here = &buf[p.offset..end];
    if here == p.patched {
        return Outcome::AlreadyApplied;
    }
    if here != p.expect {
        return Outcome::Skipped;
    }
    buf[p.offset..end].copy_from_slice(p.patched);
    Outcome::Applied
}

/// Where the patched image is staged before it replaces the client.
///
/// Deliberately in the same directory: [`std::fs::rename`] is only atomic within
/// a filesystem, and the system temp dir is often a different one.
fn staging_path(client: &Path) -> PathBuf {
    let mut p = client.as_os_str().to_os_string();
    p.push(".patching");
    PathBuf::from(p)
}

/// The patches that apply on this platform — everything in [`PATCHES`] except the
/// [`macos_only`](Patch::macos_only) ones when the client will not run under
/// macOS/Wine.
pub fn applicable() -> impl Iterator<Item = &'static Patch> {
    PATCHES.iter().filter(|p| !p.macos_only || cfg!(target_os = "macos"))
}

/// Apply every patch in [`applicable`] to `client`, returning what happened to each.
///
/// Writes only if something actually changed. A patch that does not match is
/// reported, not fatal: a client we do not recognise should still be playable,
/// just without the fix.
///
/// The rewrite is staged and renamed rather than written in place, so a crash or
/// a full disk cannot leave a half-written executable, and no unpatched copy of
/// the client is left on disk afterwards.
pub fn apply_all(client: &Path) -> Result<Vec<(&'static str, Outcome)>, String> {
    let mut buf =
        std::fs::read(client).map_err(|e| format!("reading {}: {e}", client.display()))?;

    let results: Vec<(&'static str, Outcome)> =
        applicable().map(|p| (p.name, apply_to_bytes(&mut buf, p))).collect();

    if !results.iter().any(|(_, o)| *o == Outcome::Applied) {
        return Ok(results);
    }

    let staged = staging_path(client);
    let swap = || -> std::io::Result<()> {
        std::fs::write(&staged, &buf)?;
        // A fresh file takes the process umask; keep whatever mode the client had.
        let mode = std::fs::metadata(client)?.permissions();
        std::fs::set_permissions(&staged, mode)?;
        std::fs::rename(&staged, client)
    };
    if let Err(e) = swap() {
        // Best effort: the client itself is still intact either way.
        let _ = std::fs::remove_file(&staged);
        return Err(format!("writing {}: {e}", client.display()));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Patch = Patch {
        name: "test",
        why: "test",
        offset: 4,
        expect: &[0xaa, 0xbb],
        patched: &[0x90, 0x90],
        macos_only: false,
    };

    #[test]
    fn applies_once_then_reports_already_applied() {
        let mut buf = vec![0u8; 8];
        buf[4] = 0xaa;
        buf[5] = 0xbb;
        assert_eq!(apply_to_bytes(&mut buf, &P), Outcome::Applied);
        assert_eq!(&buf[4..6], &[0x90, 0x90]);
        // Idempotent: setup steps re-run, and a second pass must not corrupt.
        assert_eq!(apply_to_bytes(&mut buf, &P), Outcome::AlreadyApplied);
        assert_eq!(&buf[4..6], &[0x90, 0x90]);
    }

    #[test]
    fn unknown_bytes_are_left_alone() {
        let mut buf = vec![0u8; 8];
        buf[4] = 0x12;
        buf[5] = 0x34;
        assert_eq!(apply_to_bytes(&mut buf, &P), Outcome::Skipped);
        assert_eq!(&buf[4..6], &[0x12, 0x34], "a non-matching client must not be written to");
    }

    #[test]
    fn short_file_is_skipped_not_panicking() {
        let mut buf = vec![0u8; 5];
        assert_eq!(apply_to_bytes(&mut buf, &P), Outcome::Skipped);
    }

    #[test]
    fn real_patches_are_same_length_and_distinct() {
        for p in PATCHES {
            assert_eq!(p.expect.len(), p.patched.len(), "{}: length must match", p.name);
            assert_ne!(p.expect, p.patched, "{}: patch is a no-op", p.name);
        }
    }

    #[test]
    fn real_patches_do_not_overlap() {
        // widescreen-viewport and viewport-height are seven bytes apart in the
        // same push sequence. Overlapping ranges would make the pair order
        // dependent, and the second would see bytes the first had already moved.
        let mut spans: Vec<_> = PATCHES.iter().map(|p| (p.offset, p.offset + p.expect.len(), p.name)).collect();
        spans.sort();
        for w in spans.windows(2) {
            let (_, a_end, a) = w[0];
            let (b_start, _, b) = w[1];
            assert!(a_end <= b_start, "{a} overlaps {b}");
        }
    }

    fn named(name: &str) -> &'static Patch {
        PATCHES.iter().find(|p| p.name == name).expect("no such patch")
    }

    /// Both window-style patches are arithmetic, and getting an immediate wrong
    /// would either lose the resizable frame (no Space) or corrupt the exclusive
    /// fullscreen style. Decode the instructions and check the styles the client
    /// actually ends up with, rather than trusting the hex by eye.
    #[test]
    fn the_style_patches_only_touch_the_windowed_style() {
        const WS_VISIBLE: u32 = 0x1000_0000;
        const WS_MAXIMIZEBOX: u32 = 0x0001_0000;
        const WS_MINIMIZEBOX: u32 = 0x0002_0000;
        const WS_THICKFRAME: u32 = 0x0004_0000;

        // Both sites are an `and imm32` / `add imm32` pair on a register set to 0 or
        // -1 by a neg/sbb on a flag, so one style is `add` alone and the other is
        // mask+add. They differ in encoding *and* in the sense of that flag: the
        // restyle site tests "is fullscreen" on eax (`25`/`05`), the creation site
        // tests "is windowed" on edx (`81 e2`/`81 c2`), so the two styles come out
        // the opposite way round. The creation site ORs in WS_VISIBLE afterwards, so
        // normalise both by setting it.
        let styles = |b: &[u8]| {
            let (mask_at, add_at, add_is_windowed) = match (b[0], b[1]) {
                (0x25, _) => {
                    assert_eq!(b[5], 0x05, "expected `add eax,imm32` after `and eax,imm32`");
                    (1, 6, true)
                }
                (0x81, 0xe2) => {
                    assert_eq!(&b[6..8], &[0x81, 0xc2], "expected `add edx,imm32`");
                    (2, 8, false)
                }
                _ => panic!("not an and/add immediate pair: {b:02x?}"),
            };
            let imm = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
            let (mask, base) = (imm(mask_at), imm(add_at));
            let (a, b) = (base | WS_VISIBLE, mask.wrapping_add(base) | WS_VISIBLE);
            if add_is_windowed { (b, a) } else { (a, b) } // (fullscreen, windowed)
        };

        for name in ["window-style-create", "window-style-restyle"] {
            let p = named(name);
            let (was_fullscreen, was_windowed) = styles(p.expect);
            let (now_fullscreen, now_windowed) = styles(p.patched);

            assert_eq!(was_windowed, 0x12CA_0000, "{name}: the client's windowed style moved");
            assert_eq!(was_fullscreen, 0x9200_0000, "{name}: the client's fullscreen style moved");
            assert_eq!(
                now_fullscreen, was_fullscreen,
                "{name}: exclusive fullscreen must come out bit-identical -- \
                 BETTERAC_FULLSCREEN and BETTERAC_DESKTOP use it"
            );
            assert_eq!(
                now_windowed & !was_windowed,
                WS_THICKFRAME | WS_MAXIMIZEBOX,
                "{name}: the windowed style must gain exactly the frame winemac looks for"
            );
            assert_eq!(
                was_windowed & !now_windowed,
                WS_MINIMIZEBOX,
                "{name}: the minimize box must be the only thing removed"
            );
        }
    }

    /// The login fix is one opcode: `je` over the forced-resolution override
    /// becomes `jmp`. Anything else at this site would be writing into the wrong
    /// instruction, so pin the shape.
    #[test]
    fn the_login_patch_only_turns_the_conditional_jump_into_an_unconditional_one() {
        let p = named("login-resolution");
        let differ: Vec<usize> =
            (0..p.expect.len()).filter(|&i| p.expect[i] != p.patched[i]).collect();
        assert_eq!(differ, vec![8], "only the jump opcode may change");
        assert_eq!((p.expect[8], p.patched[8]), (0x74, 0xeb), "je -> jmp");
        assert_eq!(p.expect[9], p.patched[9], "the jump displacement must not move");
    }

    /// The macOS-only patches change how the client windows itself, which is
    /// meaningless (and unwanted) under gamescope. Guard the split so a future
    /// patch does not silently reach the Linux client.
    #[test]
    fn only_the_window_patches_are_macos_only() {
        let mac: Vec<&str> = PATCHES.iter().filter(|p| p.macos_only).map(|p| p.name).collect();
        assert_eq!(mac, vec!["login-resolution", "window-style-create", "window-style-restyle"]);

        let applied: Vec<&str> = applicable().map(|p| p.name).collect();
        if cfg!(target_os = "macos") {
            assert_eq!(applied.len(), PATCHES.len());
        } else {
            assert!(!applied.contains(&"window-style-create"), "{applied:?}");
            assert!(applied.contains(&"widescreen-viewport"), "{applied:?}");
        }
    }

    #[test]
    fn staging_file_is_a_sibling_so_the_rename_stays_atomic() {
        let s = staging_path(Path::new("/games/ac/acclient.exe"));
        assert_eq!(s.parent(), Path::new("/games/ac/acclient.exe").parent());
    }

    #[test]
    fn patching_leaves_nothing_behind_and_is_repeatable() {
        let dir =
            std::env::temp_dir().join(format!("ac-patch-{}-{:p}", std::process::id(), &PATCHES));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let client = dir.join("acclient.exe");

        // A stand-in client: just large enough to hold every patch site.
        let end = PATCHES.iter().map(|p| p.offset + p.expect.len()).max().unwrap();
        let mut img = vec![0u8; end + 16];
        for p in PATCHES {
            img[p.offset..p.offset + p.expect.len()].copy_from_slice(p.expect);
        }
        std::fs::write(&client, &img).unwrap();

        let first = apply_all(&client).unwrap();
        assert!(first.iter().all(|(_, o)| *o == Outcome::Applied), "{first:?}");

        // Only what `apply_all` was asked to do. On Linux the macos_only patches
        // are not in `applicable()`, so their sites must still hold the *expected*
        // bytes -- asserting over all of PATCHES here failed on Linux from the day
        // the split was introduced, which nothing noticed because the Linux build
        // could not compile its test targets at all.
        let after = std::fs::read(&client).unwrap();
        for p in PATCHES {
            let want = if applicable().any(|a| a.name == p.name) { p.patched } else { p.expect };
            assert_eq!(&after[p.offset..p.offset + want.len()], want, "{}", p.name);
        }

        // The whole point: no .orig, no .patching, nothing but the client.
        let left: Vec<_> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left.len(), 1, "expected only acclient.exe, found {left:?}");

        let again = apply_all(&client).unwrap();
        assert!(again.iter().all(|(_, o)| *o == Outcome::AlreadyApplied), "{again:?}");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
