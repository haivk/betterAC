#!/usr/bin/env bash
#
# Render the package-manager metadata for a release: fill the templates with the
# version, the date, and the real SHA-256 of each artifact.
#
# Everything a package manager needs is derived from artifacts that already
# exist, so this cannot describe a build that was never made -- it reads the
# checksums off the files themselves and fails if one is missing.
#
#   ./packaging/render-metadata.sh dist/           # after building artifacts
#
# Writes into <dir>:
#   betterac.rb          Homebrew cask (macOS)  -> homebrew-betterac tap, Casks/
#   betterac-formula.rb  Homebrew formula (Linux) -> same tap, Formula/betterac.rb
#   PKGBUILD           AUR (-bin)      -> aur.archlinux.org/betterac-bin.git
#   .SRCINFO           AUR metadata    -> same repo, generated not hand-edited
#   ac.betterac.BetterAC.metainfo.xml  AppStream, version + date filled in
#   SHA256SUMS         every artifact, for the release body
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
DIST="${1:-$ROOT/dist}"

say() { printf "\n==> %s\n" "$*"; }
die() { printf "\nerror: %s\n" "$*" >&2; exit 1; }

[ -d "$DIST" ] || die "no such directory: $DIST"

# VERSION can be passed in (CI already knows it from the tag check) so this
# script needs no Rust toolchain; otherwise ask cargo.
VERSION="${VERSION:-$(cd "$ROOT" && cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="betterac"))')}"

# SOURCE_DATE_EPOCH keeps this reproducible if a build has to be re-run. The two
# date(1)s disagree about how to spell "this epoch": -d @N is GNU, -r N is BSD.
if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
  DATE="$(date -u -d "@$SOURCE_DATE_EPOCH" +%Y-%m-%d 2>/dev/null \
       || date -u -r "$SOURCE_DATE_EPOCH" +%Y-%m-%d)"
else
  DATE="$(date -u +%Y-%m-%d)"
fi

DMG="$DIST/BetterAC-${VERSION}-universal.dmg"
TARBALL="$DIST/betterac-${VERSION}-x86_64.tar.gz"
# The Linux formula builds from source, so it checksums this rather than the
# binary tarball. Produced by packaging/source-tarball.sh.
SRC="$DIST/betterac-${VERSION}-src.tar.gz"

# sha256 of a file, portable between the macOS and Linux runners.
sha256() {
  [ -f "$1" ] || die "missing artifact: $1
       Build it first (packaging/macos/build-dmg.sh, packaging/linux/build-tarball.sh),
       or pass the directory the CI artifacts were downloaded into."
  if command -v sha256sum >/dev/null; then sha256sum "$1" | awk '{print $1}'
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}

say "Rendering metadata for $VERSION ($DATE)"

DMG_SHA="$(sha256 "$DMG")"
TARBALL_SHA="$(sha256 "$TARBALL")"
SRC_SHA="$(sha256 "$SRC")"

fill() {
  sed -e "s|@@VERSION@@|$VERSION|g" \
      -e "s|@@DATE@@|$DATE|g" \
      -e "s|@@DMG_SHA256@@|$DMG_SHA|g" \
      -e "s|@@TARBALL_SHA256@@|$TARBALL_SHA|g" \
      -e "s|@@SRC_SHA256@@|$SRC_SHA|g" \
      "$1" > "$2"
  printf "    %s\n" "$2"
}

fill "$ROOT/packaging/homebrew/betterac.rb.in" "$DIST/betterac.rb"
# Not betterac.rb: the cask already owns that name here. They only stop
# colliding once they are in the tap, under Casks/ and Formula/.
fill "$ROOT/packaging/homebrew/betterac-formula.rb.in" "$DIST/betterac-formula.rb"
fill "$ROOT/packaging/aur/PKGBUILD.in"         "$DIST/PKGBUILD"
fill "$ROOT/packaging/shared/ac.betterac.BetterAC.metainfo.xml" \
     "$DIST/ac.betterac.BetterAC.metainfo.xml"

# .SRCINFO is mechanically derived from the PKGBUILD. makepkg --printsrcinfo is
# the source of truth when it is available (an Arch box); everywhere else, write
# the equivalent by hand from the same values so CI on Ubuntu still produces a
# valid file.
say "Generating .SRCINFO"
if command -v makepkg >/dev/null; then
  (cd "$DIST" && makepkg --printsrcinfo > .SRCINFO)
  printf "    %s (via makepkg)\n" "$DIST/.SRCINFO"
else
  cat > "$DIST/.SRCINFO" <<EOF
pkgbase = betterac-bin
	pkgdesc = Launcher for Asheron's Call player servers
	pkgver = $VERSION
	pkgrel = 1
	url = https://github.com/haivk/betterAC
	arch = x86_64
	license = MIT
	depends = gtk4
	depends = libadwaita
	optdepends = umu-launcher: required to launch the game
	optdepends = gamescope: correct fullscreen scaling (strongly recommended)
	optdepends = winetricks: installs the runtime components the client needs
	provides = betterac
	conflicts = betterac
	source = betterac-bin-$VERSION.tar.gz::https://github.com/haivk/betterAC/releases/download/v$VERSION/betterac-$VERSION-x86_64.tar.gz
	sha256sums = $TARBALL_SHA

pkgname = betterac-bin
EOF
  printf "    %s (hand-rolled; no makepkg on this host)\n" "$DIST/.SRCINFO"
fi

say "Checksums"
(
  cd "$DIST"
  # Only the shipped artifacts, not the metadata we just wrote.
  files=$(ls -1 *.dmg *.tar.gz *.deb *.flatpak 2>/dev/null || true)
  [ -n "$files" ] || die "no artifacts found in $DIST"
  if command -v sha256sum >/dev/null; then sha256sum $files > SHA256SUMS
  else shasum -a 256 $files > SHA256SUMS; fi
  sed 's/^/    /' SHA256SUMS
)

say "Done"
