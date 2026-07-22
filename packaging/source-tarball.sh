#!/usr/bin/env bash
#
# Pack the source tree as a release artifact.
#
# The Homebrew formula builds betterAC from source (it links against Homebrew's
# own gtk4, not the distro's), so it needs a source tarball with a stable URL and
# a checksum we control. GitHub auto-generates one per tag, but checksumming that
# would mean render-metadata.sh reaching over the network -- and its whole point
# is that it reads checksums off files that exist, so it can never describe a
# build that was never made. So we ship our own.
#
# git archive takes the tracked tree, which is exactly right: Cargo.lock is
# tracked (--locked needs it) and nothing generated or ignored can leak in.
#
#   ./packaging/source-tarball.sh [dist-dir]
#
# Output: <dist>/betterac-<version>-src.tar.gz
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
DIST="${1:-$ROOT/dist}"

say() { printf "\n==> %s\n" "$*"; }
die() { printf "\nerror: %s\n" "$*" >&2; exit 1; }

git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
  || die "not a git repository: $ROOT
       This packs the tracked tree with git archive; there is nothing to pack
       without a repo."

VERSION="${VERSION:-$(cd "$ROOT" && cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="betterac"))')}"

NAME="betterac-${VERSION}-src"
mkdir -p "$DIST"

say "Packing $NAME from $(git -C "$ROOT" rev-parse --short HEAD)"

# The prefix is the version without the -src suffix: it is what the tree unpacks
# to, and the formula's `cd` target. Uncommitted edits are deliberately NOT
# included -- a release describes a commit.
git -C "$ROOT" archive --format=tar.gz \
  --prefix="betterac-${VERSION}/" \
  -o "$DIST/$NAME.tar.gz" HEAD

if [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
  printf "    note: working tree is dirty; packed HEAD, not your edits\n"
fi

say "Done: $DIST/$NAME.tar.gz"
if command -v sha256sum >/dev/null; then sha256sum "$DIST/$NAME.tar.gz" | sed 's/^/    /'
else shasum -a 256 "$DIST/$NAME.tar.gz" | sed 's/^/    /'; fi
