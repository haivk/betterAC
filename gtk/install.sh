#!/usr/bin/env bash
#
# Install betterAC into ~/.local -- no root, no sandbox.
#
# By default this installs the prebuilt binary in dist/. Bazzite is an atomic
# distro with no compiler on the host, and betterAC has to *run* on the host (it
# shells out to umu-run), so building it here would mean a toolbox, a Rust
# toolchain and the GTK4 dev headers just to produce a 2 MB file that never
# changes. The binary is built on Fedora 41 -- older than any Bazzite -- so its
# glibc floor (2.39) sits below the host's and it links only against libgtk-4,
# libadwaita and glib, which every GNOME desktop already has.
#
# Pass --build to compile from source instead. That needs a toolbox:
#
#   toolbox create ac && toolbox enter ac
#   sudo dnf install -y cargo gtk4-devel libadwaita-devel
#   ./install.sh --build
#
# Deliberately not a Flatpak: the launcher's entire job is to run umu-run on the
# host, and a Flatpak can only do that by punching --talk-name=org.freedesktop.Flatpak
# through the sandbox, at which point the sandbox is decoration. A plain ~/.local
# install is honest about what this is.
#
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$HOME/.local/bin"
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor/scalable/apps"
PREBUILT="$HERE/dist/betterac-x86_64"

BUILD=0
[[ "${1:-}" == "--build" ]] && BUILD=1

if [[ -t 1 ]]; then B=$'\e[1m'; DIM=$'\e[2m'; G=$'\e[32m'; Y=$'\e[33m'; R=$'\e[31m'; X=$'\e[0m'
else B=""; DIM=""; G=""; Y=""; R=""; X=""; fi
ok()   { printf "  ${G}✓${X} %s\n" "$*"; }
info() { printf "    %s\n" "$*"; }
warn() { printf "  ${Y}!${X} %s\n" "$*"; }
die()  { printf "\n${R}${B}error:${X} %s\n" "$*" >&2; exit 1; }

printf "\n${B}betterAC${X} -- installing to ~/.local\n\n"

# ------------------------------------------------------------------- the binary

SRC_BIN=""

if [[ $BUILD -eq 0 && -f "$PREBUILT" && "$(uname -m)" == "x86_64" ]]; then
  SRC_BIN="$PREBUILT"
  ok "prebuilt binary ${DIM}($(du -h "$PREBUILT" | cut -f1), x86_64)${X}"
else
  if [[ $BUILD -eq 0 ]]; then
    if [[ ! -f "$PREBUILT" ]]; then
      info "${DIM}no prebuilt binary in dist/ -- building from source${X}"
    else
      info "${DIM}prebuilt binary is x86_64, this host is $(uname -m) -- building from source${X}"
    fi
  fi

  command -v cargo >/dev/null || die "cargo not found.
       Bazzite is atomic, so the Rust toolchain goes in a toolbox, not on the host:
         toolbox create ac && toolbox enter ac
         sudo dnf install -y cargo gtk4-devel libadwaita-devel
       then re-run this from inside the toolbox. A toolbox shares your \$HOME, so
       the binary it builds installs straight to ~/.local and runs on the host."

  for pc in gtk4 libadwaita-1; do
    pkg-config --exists "$pc" \
      || die "$pc development files not found -- sudo dnf install gtk4-devel libadwaita-devel"
  done
  ok "toolchain"

  printf "\n  building (release)...\n\n"
  cargo build --release --manifest-path "$HERE/Cargo.toml" || die "build failed"
  # betterac is now a workspace member, so cargo puts the binary in the workspace
  # target dir one level up (betterAC/target), not gtk/target.
  SRC_BIN="$HERE/../target/release/betterac"
  ok "built"
fi

# ------------------------------------------------------------------ install it

mkdir -p "$BIN" "$APPS" "$ICONS"
install -m755 "$SRC_BIN" "$BIN/betterac"
ok "$BIN/betterac"

install -m644 "$HERE/data/betterac.desktop" "$APPS/ac.betterac.BetterAC.desktop"
install -m644 "$HERE/data/betterac.svg"     "$ICONS/ac.betterac.BetterAC.svg"
update-desktop-database "$APPS" 2>/dev/null || true
gtk4-update-icon-cache -qtf "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
ok "desktop entry + icon ${DIM}(GNOME app grid)${X}"

# ---------------------------------------------------------------- sanity checks

# A prebuilt binary that cannot resolve its libraries is the one real failure mode
# of shipping one, so check rather than find out at first launch. Inside a toolbox
# this is expected to differ from the host; say so instead of crying wolf.
if command -v ldd >/dev/null && ldd "$BIN/betterac" 2>/dev/null | grep -q "not found"; then
  if [[ -f /run/.toolboxenv ]]; then
    warn "unresolved libraries in here, but you are in a toolbox -- what matters is the host"
  else
    warn "betterac is missing shared libraries on this host:"
    ldd "$BIN/betterac" 2>/dev/null | grep "not found" | sed 's/^/      /'
    warn "re-run with ${B}--build${X} to compile against what you actually have"
  fi
fi

command -v umu-run >/dev/null \
  || warn "umu-run is not on PATH -- the game cannot launch without it
      (it ships with Bazzite; from inside a toolbox you will not see the host's copy)"

case ":$PATH:" in
  *":$BIN:"*) ;;
  *) warn "$BIN is not on your PATH -- launch it from the GNOME app grid instead" ;;
esac

printf "\n${G}${B}Done.${X}  Open ${B}Asheron's Call${X} from the app grid, or run ${B}betterac${X}.\n\n"
