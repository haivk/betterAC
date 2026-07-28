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
#   betterac.rb   Homebrew cask (macOS) -> homebrew-betterac tap, Casks/
#   SHA256SUMS    every artifact -- the release body, and what install.sh verifies
#                 downloads against *and* reads the artifact names out of
#
# Homebrew (macOS) is the only package manager left. The AUR package, the Flatpak,
# the .deb and the Linux brew formula were dropped in favour of the one-line
# installer -- Linux installs the tarball into ~/.local via install.sh.
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

# sha256 of a file, portable between the macOS and Linux runners.
sha256() {
  [ -f "$1" ] || die "missing artifact: $1
       Build it first (packaging/macos/build-dmg.sh, packaging/linux/build-tarball.sh),
       or pass the directory the CI artifacts were downloaded into."
  if command -v sha256sum >/dev/null; then sha256sum "$1" | awk '{print $1}'
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}

say "Rendering metadata for $VERSION ($DATE)"

# The cask is rendered only when a DMG is actually here. It is skipped on a
# release built without the Apple signing secrets: that path deliberately keeps
# the unsigned DMG out of the release (Gatekeeper would refuse to open it), so
# there is nothing for a cask to point at and publishing one would be a lie.
if [ -f "$DMG" ]; then
  DMG_SHA="$(sha256 "$DMG")"
  sed -e "s|@@VERSION@@|$VERSION|g" \
      -e "s|@@DATE@@|$DATE|g" \
      -e "s|@@DMG_SHA256@@|$DMG_SHA|g" \
      "$ROOT/packaging/homebrew/betterac.rb.in" > "$DIST/betterac.rb"
  printf "    %s\n" "$DIST/betterac.rb"
else
  printf "    no DMG in %s -- skipping the Homebrew cask\n" "$DIST"
fi

say "Checksums"
(
  cd "$DIST"
  # Only the shipped artifacts, not the metadata we just wrote.
  files=$(ls -1 *.dmg *.tar.gz 2>/dev/null || true)
  [ -n "$files" ] || die "no artifacts found in $DIST"
  if command -v sha256sum >/dev/null; then sha256sum $files > SHA256SUMS
  else shasum -a 256 $files > SHA256SUMS; fi
  sed 's/^/    /' SHA256SUMS
)

say "Done"
