#!/usr/bin/env bash
#
# Get betterAC onto any Linux desktop, into ~/.local -- no root, no sandbox.
#
#   curl -fsSL https://raw.githubusercontent.com/haivk/betterAC/main/install.sh | bash
#
# This is the BOOTSTRAP: it finds the latest release, downloads the tarball,
# checks it against the published SHA256SUMS, proves the binary actually runs on
# this machine, and then hands off to the installer inside the tarball
# (gtk/install.sh) which does the real work. If you already have the tarball, or
# a git checkout, run that one directly instead -- this adds nothing there.
#
#   BETTERAC_VERSION=0.2.0   pin a version instead of taking the latest
#
# Why ~/.local and not a Flatpak: betterAC's job is to drive umu-run on the host,
# which a sandbox cannot see -- and even bundling Wine does not save it. Measured
# on 2026-07-22: inside a Flatpak, `wine cmd` runs fine but acclient.exe is killed
# by the sandbox seccomp filter with SIGSYS (it dies in wine-preloader, doing
# 32-bit address-space setup), and neither --allow=devel nor --device=all lifts
# it. A plain ~/.local install is honest about what this program is.
set -euo pipefail

REPO="haivk/betterAC"
ARCH="$(uname -m)"

if [[ -t 1 ]]; then B=$'\e[1m'; DIM=$'\e[2m'; G=$'\e[32m'; Y=$'\e[33m'; R=$'\e[31m'; X=$'\e[0m'
else B=""; DIM=""; G=""; Y=""; R=""; X=""; fi
ok()   { printf "  ${G}✓${X} %s\n" "$*"; }
info() { printf "    %s\n" "$*"; }
warn() { printf "  ${Y}!${X} %s\n" "$*"; }
die()  { printf "\n${R}${B}error:${X} %s\n" "$*" >&2; exit 1; }

printf "\n${B}betterAC${X} -- fetching the latest release\n\n"

# ------------------------------------------------------------------- guardrails

[[ "$(uname -s)" == "Linux" ]] || die "this installs the Linux build; on macOS use:
       brew tap haivk/betterac && brew install --cask betterac"

[[ "$ARCH" == "x86_64" ]] || die "no prebuilt binary for $ARCH -- only x86_64 is published.
       Build from source instead: clone the repo and run ./gtk/install.sh --build"

# Running this inside a checkout almost always means "I want to build my code",
# which is the opposite of downloading a release. When piped from curl there is
# no BASH_SOURCE, so this falls back to the working directory -- which is the
# right thing to check in that case anyway.
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-.}")" 2>/dev/null && pwd || echo .)"
if [[ -f "$SELF_DIR/Cargo.toml" && "${1:-}" != "--force" ]]; then
  die "this looks like a betterAC checkout, and this script downloads a *release*.
       To install what you have here:   ./gtk/install.sh --build
       To install the latest release:   ./install.sh --force"
fi

command -v curl >/dev/null || die "curl is required"
command -v tar  >/dev/null || die "tar is required"

if command -v sha256sum >/dev/null; then SHA() { sha256sum "$1"; }
elif command -v shasum  >/dev/null; then SHA() { shasum -a 256 "$1"; }
else SHA() { echo ""; }; fi

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

# ------------------------------------------------------------------ the release

VERSION="${BETTERAC_VERSION:-}"
if [[ -z "$VERSION" ]]; then
  # The /releases/latest redirect gives the tag without burning an API rate limit.
  TAG_URL="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
    "https://github.com/$REPO/releases/latest" 2>/dev/null || true)"
  VERSION="${TAG_URL##*/tag/v}"
  [[ -n "$VERSION" && "$VERSION" != "$TAG_URL" ]] \
    || die "could not work out the latest version.
       Check https://github.com/$REPO/releases and set BETTERAC_VERSION=x.y.z"
fi
ok "version $VERSION"

# Overridable for a mirror, and so this script can be exercised against a local
# release before one is published.
BASE="${BETTERAC_BASE_URL:-https://github.com/$REPO/releases/download/v$VERSION}"
NAME="betterac-${VERSION}-${ARCH}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

printf "\n  downloading...\n\n"
curl -fSL --progress-bar -o "$TMP/$NAME.tar.gz" "$BASE/$NAME.tar.gz" \
  || die "download failed: $BASE/$NAME.tar.gz"

# ---------------------------------------------------------------- verify it

if curl -fsSL -o "$TMP/SHA256SUMS" "$BASE/SHA256SUMS" 2>/dev/null; then
  WANT="$(grep -F "$NAME.tar.gz" "$TMP/SHA256SUMS" | awk '{print $1}' | head -1)"
  GOT="$(SHA "$TMP/$NAME.tar.gz" | awk '{print $1}')"
  [[ -n "$WANT" ]] || die "SHA256SUMS has no entry for $NAME.tar.gz"
  [[ -n "$GOT"  ]] || die "no sha256sum or shasum on this system -- cannot verify the
       download, and this will not install something it could not check. Install
       coreutils and try again."
  [[ "$WANT" == "$GOT" ]] || die "checksum mismatch -- refusing to install
       expected $WANT
       got      $GOT"
  ok "sha256 verified"
else
  warn "no SHA256SUMS published for this release -- cannot verify the download"
fi

tar -xzf "$TMP/$NAME.tar.gz" -C "$TMP"
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
         - build from source: clone the repo and run ./gtk/install.sh --build, or
         - use Homebrew, which brings its own toolkit:
             brew tap haivk/betterac && brew install betterac"
fi
ok "binary runs here ${DIM}($("$TMP/$NAME/dist/betterac-$ARCH" --version))${X}"

# --------------------------------------------------------------- hand off

# Not exec: the EXIT trap that removes $TMP has to survive, and the installer
# copies out of $TMP while it runs.
printf "\n"
"$TMP/$NAME/install.sh"
