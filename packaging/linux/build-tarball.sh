#!/usr/bin/env bash
#
# Build the Linux release tarball: the binary plus everything a user needs to
# install it without root.
#
# The tarball deliberately mirrors what gtk/install.sh already does (binary into
# ~/.local/bin, desktop file and icon under ~/.local/share as
# ac.betterac.BetterAC.*), and ships that same script rather than a second,
# subtly-different installer.
#
# Build it on the OLDEST distro you are willing to support: the binary links
# against the host's glibc, gtk4 and libadwaita, so the build machine's glibc is
# the floor for every user. CI uses ubuntu-22.04 (glibc 2.35) for this reason.
#
#   ./packaging/linux/build-tarball.sh
#
# Output: dist/betterac-<version>-x86_64.tar.gz
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
DIST="$ROOT/dist"

say() { printf "\n==> %s\n" "$*"; }
die() { printf "\nerror: %s\n" "$*" >&2; exit 1; }

VERSION="$(cd "$ROOT" && cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="betterac"))')"
ARCH="$(uname -m)"
NAME="betterac-${VERSION}-${ARCH}"

for pc in gtk4 libadwaita-1; do
  pkg-config --exists "$pc" || die "$pc development files not found
       Debian/Ubuntu: sudo apt install libgtk-4-dev libadwaita-1-dev
       Fedora:        sudo dnf install gtk4-devel libadwaita-devel"
done

say "Building betterac $VERSION ($ARCH)"
cd "$ROOT"
cargo build --release -p betterac

say "Staging $NAME"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
PKG="$STAGE/$NAME"
mkdir -p "$PKG/data"

install -m755 "$ROOT/target/release/betterac" "$PKG/betterac"
install -m644 "$ROOT/gtk/data/betterac.desktop" "$PKG/data/betterac.desktop"
install -m644 "$ROOT/gtk/data/betterac.svg" "$PKG/data/betterac.svg"
install -m644 "$ROOT/packaging/shared/ac.betterac.BetterAC.metainfo.xml" \
  "$PKG/data/ac.betterac.BetterAC.metainfo.xml"
install -m755 "$ROOT/gtk/install.sh" "$PKG/install.sh"
install -m644 "$ROOT/README.md" "$PKG/README.md"
install -m644 "$ROOT/LICENSE" "$PKG/LICENSE"

# install.sh looks for a prebuilt binary at dist/betterac-x86_64 and otherwise
# tries to compile. Inside the tarball there is no source tree to fall back to,
# so put the binary exactly where it expects to find it.
mkdir -p "$PKG/dist"
ln "$PKG/betterac" "$PKG/dist/betterac-${ARCH}" 2>/dev/null \
  || cp "$PKG/betterac" "$PKG/dist/betterac-${ARCH}"

say "Packing"
mkdir -p "$DIST"
tar -C "$STAGE" -czf "$DIST/$NAME.tar.gz" "$NAME"

say "Done: $DIST/$NAME.tar.gz"
sha256sum "$DIST/$NAME.tar.gz" | sed 's/^/    /'
