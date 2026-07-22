#!/usr/bin/env bash
#
# Render the AppStream metainfo template to a real file.
#
# packaging/shared/ac.betterac.BetterAC.metainfo.xml is a TEMPLATE: its <release>
# element carries @@VERSION@@ and @@DATE@@. Every package that installs it has to
# render it first, or it ships literal "@@VERSION@@" to /usr/share/metainfo,
# which is invalid AppStream -- appstreamcli rejects it and GNOME Software shows
# the placeholder. (render-metadata.sh also renders a copy, but that one is for
# the release page; it runs after the packages are already built.)
#
#   ./packaging/render-metainfo.sh <outfile>
#
# VERSION comes from the environment when the caller already knows it (CI, the
# Homebrew formula) and from cargo otherwise.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
SRC="$ROOT/packaging/shared/ac.betterac.BetterAC.metainfo.xml"
OUT="${1:-$ROOT/dist/ac.betterac.BetterAC.metainfo.xml}"

die() { printf "\nerror: %s\n" "$*" >&2; exit 1; }

[ -f "$SRC" ] || die "missing template: $SRC"

VERSION="${VERSION:-$(cd "$ROOT" && cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="betterac"))')}"

# Same SOURCE_DATE_EPOCH handling as render-metadata.sh: -d @N is GNU, -r N BSD.
if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
  DATE="$(date -u -d "@$SOURCE_DATE_EPOCH" +%Y-%m-%d 2>/dev/null \
       || date -u -r "$SOURCE_DATE_EPOCH" +%Y-%m-%d)"
else
  DATE="$(date -u +%Y-%m-%d)"
fi

mkdir -p "$(dirname "$OUT")"
sed -e "s|@@VERSION@@|$VERSION|g" -e "s|@@DATE@@|$DATE|g" "$SRC" > "$OUT"

grep -q '@@' "$OUT" && die "unfilled placeholder left in $OUT"

printf "    metainfo: %s (%s, %s)\n" "$OUT" "$VERSION" "$DATE"
