#!/usr/bin/env bash
#
# Get betterAC onto a Linux or macOS machine with one command.
#
#   curl -fsSL https://raw.githubusercontent.com/haivk/betterAC/main/install.sh | bash
#
# This is the BOOTSTRAP: it finds a release, downloads the right artifact for this
# machine, checks it against the published SHA256SUMS, and installs it.
#
#   Linux  the tarball into ~/.local -- no root, no sandbox. Proves the binary
#          actually runs here first, then hands off to the installer inside the
#          tarball (gtk/install.sh), which does the real work.
#   macOS  the signed, notarized DMG into /Applications.
#
# On macOS, Homebrew is the nicer path if you have it -- it knows how to upgrade
# and uninstall:
#
#   brew tap haivk/betterac https://github.com/haivk/betterAC && brew install --cask betterac
#
# Every commit to main is released, versioned by date (2026.07.27.42), so the
# newest release is simply the newest build.
#
# Knobs:
#   BETTERAC_VERSION=2026.07.27.42  install an exact build instead of the newest
#   BETTERAC_BASE_URL=...           a mirror, or a file:// release for testing
#   BETTERAC_APP_DIR=...            macOS: install somewhere other than /Applications
#
# Why ~/.local on Linux and not a Flatpak: betterAC's job is to drive umu-run on
# the host, which a sandbox cannot see -- and even bundling Wine does not save it.
# Measured on 2026-07-22: inside a Flatpak, `wine cmd` runs fine but acclient.exe
# is killed by the sandbox seccomp filter with SIGSYS (it dies in wine-preloader,
# doing 32-bit address-space setup), and neither --allow=devel nor --device=all
# lifts it. A plain ~/.local install is honest about what this program is.
set -euo pipefail

REPO="haivk/betterAC"

if [[ -t 1 ]]; then B=$'\e[1m'; DIM=$'\e[2m'; G=$'\e[32m'; Y=$'\e[33m'; R=$'\e[31m'; X=$'\e[0m'
else B=""; DIM=""; G=""; Y=""; R=""; X=""; fi
ok()   { printf "  ${G}✓${X} %s\n" "$*"; }
info() { printf "    %s\n" "$*"; }
warn() { printf "  ${Y}!${X} %s\n" "$*"; }
die()  { printf "\n${R}${B}error:${X} %s\n" "$*" >&2; exit 1; }

printf "\n${B}betterAC${X} -- fetching a release\n\n"

# ------------------------------------------------------------------- guardrails

case "$(uname -s)" in
  Linux)  PLATFORM=linux ;;
  Darwin) PLATFORM=macos ;;
  *)      die "unsupported OS: $(uname -s). betterAC builds for Linux and macOS." ;;
esac

# Running this inside a checkout almost always means "I want to build my code",
# which is the opposite of downloading a release. When piped from curl there is
# no BASH_SOURCE, so this falls back to the working directory -- which is the
# right thing to check in that case anyway.
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-.}")" 2>/dev/null && pwd || echo .)"
if [[ -f "$SELF_DIR/Cargo.toml" && "${1:-}" != "--force" ]]; then
  die "this looks like a betterAC checkout, and this script downloads a *release*.
       To install what you have here:   ./gtk/install.sh --build   (Linux)
                                        ./packaging/macos/build-dmg.sh  (macOS)
       To install the latest release:   ./install.sh --force"
fi

command -v curl >/dev/null || die "curl is required"

if command -v sha256sum >/dev/null; then SHA() { sha256sum "$1"; }
elif command -v shasum  >/dev/null; then SHA() { shasum -a 256 "$1"; }
else SHA() { echo ""; }; fi

if [[ "$PLATFORM" == linux ]]; then
  ARCH="$(uname -m)"
  [[ "$ARCH" == "x86_64" ]] || die "no prebuilt binary for $ARCH -- only x86_64 is published.
       Build from source instead: clone the repo and run ./gtk/install.sh --build"
  command -v tar >/dev/null || die "tar is required"

  # glibc is the one hard floor: the binary is built on ubuntu-24.04.
  if command -v ldd >/dev/null; then
    GLIBC="$(ldd --version 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+$' || true)"
    if [[ -n "$GLIBC" ]]; then
      if [[ "$(printf '%s\n2.39\n' "$GLIBC" | sort -V | head -1)" != "2.39" ]]; then
        die "glibc $GLIBC is older than the 2.39 the release binary needs.
       Build from source instead: clone the repo and run ./gtk/install.sh --build"
      fi
      ok "glibc $GLIBC"
    fi
  fi
else
  # Matches the app's macOS 13.0 deployment target (and the cask's depends_on).
  MACOS="$(sw_vers -productVersion)"
  if [[ "$(printf '%s\n13.0\n' "$MACOS" | sort -V | head -1)" != "13.0" ]]; then
    die "macOS $MACOS is older than the 13.0 (Ventura) the app is built for."
  fi
  ok "macOS $MACOS"
  command -v hdiutil >/dev/null || die "hdiutil is required"
fi

# ------------------------------------------------------------------ the release
#
# `releases/latest/download/...` is GitHub's own redirect to the newest release,
# which is all the resolution needed now that every commit to main publishes one.

if [[ -n "${BETTERAC_BASE_URL:-}" ]]; then
  BASE="$BETTERAC_BASE_URL"
  ok "source ${DIM}$BASE${X}"
elif [[ -n "${BETTERAC_VERSION:-}" ]]; then
  BASE="https://github.com/$REPO/releases/download/v$BETTERAC_VERSION"
  ok "build $BETTERAC_VERSION ${DIM}(pinned)${X}"
else
  BASE="https://github.com/$REPO/releases/latest/download"
  ok "newest build"
fi

TMP="$(mktemp -d)"
MOUNT=""
cleanup() {
  [[ -n "$MOUNT" ]] && hdiutil detach "$MOUNT" -quiet 2>/dev/null || true
  rm -rf "$TMP"
}
trap cleanup EXIT

# SHA256SUMS first, and it is doing two jobs: it is what the download is verified
# against, and it is where the artifact's *name* comes from. Deriving the name
# rather than constructing it means renaming an artifact cannot silently break
# every published one-liner, and that the rolling and tagged channels work
# identically even though their assets are named differently.
curl -fsSL -o "$TMP/SHA256SUMS" "$BASE/SHA256SUMS" \
  || die "could not fetch $BASE/SHA256SUMS
       Check https://github.com/$REPO/releases"

# `[.]` rather than `\.`: awk processes escapes in a -v assignment, so a
# backslash arrives mangled and gawk warns about it. A character class means the
# same thing and survives the round trip intact.
if [[ "$PLATFORM" == linux ]]; then PATTERN='[.]tar[.]gz$'; else PATTERN='[.]dmg$'; fi
ASSET="$(awk -v p="$PATTERN" '$2 ~ p {print $2}' "$TMP/SHA256SUMS" | head -1)"
[[ -n "$ASSET" ]] || die "this release has no $PLATFORM artifact.
       SHA256SUMS lists:
$(awk '{print "         " $2}' "$TMP/SHA256SUMS")"

printf "\n  downloading ${B}%s${X}\n\n" "$ASSET"
curl -fSL --progress-bar -o "$TMP/$ASSET" "$BASE/$ASSET" \
  || die "download failed: $BASE/$ASSET"

# ---------------------------------------------------------------- verify it

WANT="$(awk -v a="$ASSET" '$2 == a {print $1}' "$TMP/SHA256SUMS" | head -1)"
GOT="$(SHA "$TMP/$ASSET" | awk '{print $1}')"
[[ -n "$WANT" ]] || die "SHA256SUMS has no entry for $ASSET"
[[ -n "$GOT"  ]] || die "no sha256sum or shasum on this system -- cannot verify the
       download, and this will not install something it could not check."
[[ "$WANT" == "$GOT" ]] || die "checksum mismatch -- refusing to install
       expected $WANT
       got      $GOT"
ok "sha256 verified"

# ------------------------------------------------------------------- install it

if [[ "$PLATFORM" == linux ]]; then
  NAME="${ASSET%.tar.gz}"
  tar -xzf "$TMP/$ASSET" -C "$TMP"
  [[ -x "$TMP/$NAME/install.sh" ]] || die "tarball has no install.sh -- is it corrupt?"

  # The definitive portability test, and much better than sniffing library
  # versions: the binary needs gtk4 >= 4.12 and libadwaita >= 1.5, and a too-old
  # toolkit fails at load time with missing symbols, not at ldd. So just run it.
  if ! "$TMP/$NAME/dist/betterac-$ARCH" --version >/dev/null 2>&1; then
    printf "\n"
    warn "the release binary does not run on this machine:"
    "$TMP/$NAME/dist/betterac-$ARCH" --version 2>&1 | sed 's/^/      /' | head -5
    die "this usually means gtk4 or libadwaita is too old (needs gtk4 >= 4.12,
       libadwaita >= 1.5). Options:
         - install a newer toolkit from your distro, or
         - build from source: clone the repo and run ./gtk/install.sh --build"
  fi
  ok "binary runs here ${DIM}($("$TMP/$NAME/dist/betterac-$ARCH" --version))${X}"

  # Not exec: the EXIT trap that removes $TMP has to survive, and the installer
  # copies out of $TMP while it runs.
  printf "\n"
  "$TMP/$NAME/install.sh"
else
  MOUNT="$TMP/mnt"
  mkdir -p "$MOUNT"
  hdiutil attach "$TMP/$ASSET" -nobrowse -quiet -mountpoint "$MOUNT" \
    || die "could not mount $ASSET"

  APP="$MOUNT/BetterAC.app"
  [[ -d "$APP" ]] || die "no BetterAC.app inside the disk image"

  # /Applications unless told otherwise, falling back to the per-user one when it
  # is not writable (a managed Mac, or a non-admin account).
  DEST="${BETTERAC_APP_DIR:-/Applications}"
  [[ -n "${BETTERAC_APP_DIR:-}" ]] && mkdir -p "$DEST"
  [[ -w "$DEST" ]] || { DEST="$HOME/Applications"; mkdir -p "$DEST"; }

  # Replaced rather than copied over: cp -R into an existing bundle merges, which
  # can leave files from the old version behind inside the new one.
  if [[ -d "$DEST/BetterAC.app" ]]; then
    [[ -f "$DEST/BetterAC.app/Contents/Info.plist" ]] \
      || die "$DEST/BetterAC.app exists but is not an app bundle -- move it aside first"
    rm -rf "$DEST/BetterAC.app"
    info "replacing the existing install"
  fi
  cp -R "$APP" "$DEST/" || die "could not copy BetterAC.app into $DEST"

  # Quarantine is deliberately left alone. The DMG is signed with a Developer ID
  # and notarized, so Gatekeeper opens it without a prompt; stripping the flag
  # would only paper over an unsigned build, which is exactly the failure that
  # should be loud.
  ok "installed to ${DIM}$DEST/BetterAC.app${X}"
  printf "\n  Open it from Launchpad, or: ${B}open '%s/BetterAC.app'${X}\n" "$DEST"
  printf "\n  ${DIM}Tip: Homebrew also handles upgrades and uninstall:\n"
  printf "    brew tap haivk/betterac https://github.com/haivk/betterAC\n"
  printf "    brew install --cask betterac${X}\n\n"
fi
