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
//! The original file is copied alongside as `acclient.exe.orig` before the first
//! write, so a bad patch is always one `cp` away from being undone.

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
pub const PATCHES: &[Patch] = &[
    Patch {
        name: "widescreen-viewport",
        why: "lets the 3D view fill displays wider than 3000 px",
        offset: 0x0D_6F3B,
        expect: &[0x8b, 0xce, 0xe8, 0x1e, 0x8f, 0x1c, 0x00],
        patched: &[0x8b, 0xcf, 0xe8, 0xde, 0x8d, 0x07, 0x00],
    },
    Patch {
        name: "viewport-height",
        why: "takes the 3D view height from the display rather than a UI element",
        offset: 0x0D_6F33,
        expect: &[0x8b, 0xce, 0xe8, 0x36, 0x8f, 0x1c, 0x00],
        patched: &[0x8b, 0xcf, 0xe8, 0xf6, 0x8d, 0x07, 0x00],
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

/// Where the untouched original is kept.
pub fn backup_path(client: &Path) -> PathBuf {
    let mut p = client.as_os_str().to_os_string();
    p.push(".orig");
    PathBuf::from(p)
}

/// Apply every patch in [`PATCHES`] to `client`, returning what happened to each.
///
/// Writes the file only if something actually changed, and takes a `.orig` backup
/// first. A patch that does not match is reported, not fatal: a client we do not
/// recognise should still be playable, just without the fix.
pub fn apply_all(client: &Path) -> Result<Vec<(&'static str, Outcome)>, String> {
    let mut buf =
        std::fs::read(client).map_err(|e| format!("reading {}: {e}", client.display()))?;

    let results: Vec<(&'static str, Outcome)> =
        PATCHES.iter().map(|p| (p.name, apply_to_bytes(&mut buf, p))).collect();

    if !results.iter().any(|(_, o)| *o == Outcome::Applied) {
        return Ok(results);
    }

    // Back up the pristine file before the first write. Never overwrite an
    // existing backup: on a re-patch it is the only surviving original.
    let backup = backup_path(client);
    if !backup.exists() {
        std::fs::copy(client, &backup)
            .map_err(|e| format!("backing up {}: {e}", client.display()))?;
    }
    std::fs::write(client, &buf).map_err(|e| format!("writing {}: {e}", client.display()))?;
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

    #[test]
    fn backup_sits_next_to_the_client() {
        let b = backup_path(Path::new("/games/ac/acclient.exe"));
        assert_eq!(b, PathBuf::from("/games/ac/acclient.exe.orig"));
    }
}
