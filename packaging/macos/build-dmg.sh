#!/usr/bin/env bash
#
# Build, sign, notarize and staple the macOS app, and wrap it in a DMG.
#
# The logic lives here rather than in release.yml so it can be rehearsed on a
# developer Mac without pushing a tag: with no signing identity set it falls back
# to an ad-hoc signature and skips notarization, which still exercises the build,
# the universal check, the DMG and the verification steps.
#
#   ./packaging/macos/build-dmg.sh                      # ad-hoc, no notarization
#   SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" \
#   NOTARIZE=1 APPLE_ID=… APPLE_APP_PASSWORD=… APPLE_TEAM_ID=… \
#     ./packaging/macos/build-dmg.sh                    # what CI runs
#
# Output: dist/BetterAC-<version>-universal.dmg
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
DIST="$ROOT/dist"

SIGN_IDENTITY="${SIGN_IDENTITY:--}"     # "-" is ad-hoc
NOTARIZE="${NOTARIZE:-0}"

say() { printf "\n==> %s\n" "$*"; }
die() { printf "\nerror: %s\n" "$*" >&2; exit 1; }

# The version is the workspace version, single-sourced from Cargo.toml.
VERSION="$(cd "$ROOT" && cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="betterac"))')"
APP_NAME="BetterAC"
DMG="$DIST/${APP_NAME}-${VERSION}-universal.dmg"

command -v xcodegen >/dev/null || die "xcodegen not found -- brew install xcodegen"

# ------------------------------------------------------------------------ build
say "Building $APP_NAME $VERSION (universal)"
cd "$ROOT/macos"
xcodegen >/dev/null

# The generic destination is load-bearing: with a concrete one ("My Mac"),
# xcodebuild pins ARCHS to the host arch and silently produces a single-slice
# app, which then fails on the other kind of Mac.
DERIVED="$(mktemp -d)"
trap 'rm -rf "$DERIVED"' EXIT
xcodebuild -project "$APP_NAME.xcodeproj" -scheme "$APP_NAME" \
  -configuration Release -destination 'generic/platform=macOS' \
  -derivedDataPath "$DERIVED" \
  CODE_SIGNING_ALLOWED=NO \
  build >/dev/null

APP="$DERIVED/Build/Products/Release/$APP_NAME.app"
[ -d "$APP" ] || die "no app at $APP"

# Fail loudly if the universal build silently degraded -- this is the exact
# regression the generic destination protects against.
ARCHS="$(lipo -archs "$APP/Contents/MacOS/$APP_NAME")"
say "Architectures: $ARCHS"
for want in arm64 x86_64; do
  case " $ARCHS " in *" $want "*) ;; *) die "app is missing the $want slice (got: $ARCHS)" ;; esac
done

# ------------------------------------------------------------------------- sign
# Hardened runtime (--options runtime) is required for notarization. The
# entitlements are what let a hardened app still drive the Wine engine it
# downloads at runtime: JIT, unsigned executable memory, and no library
# validation. No --deep: the bundle has no nested frameworks (the Rust side is a
# static library linked into the binary), and --deep is the wrong tool anyway.
say "Signing with identity: $SIGN_IDENTITY"
codesign --force --timestamp --options runtime \
  --entitlements "$ROOT/macos/$APP_NAME.entitlements" \
  --sign "$SIGN_IDENTITY" "$APP"

codesign --verify --strict --verbose=2 "$APP"

# -------------------------------------------------------------------------- dmg
say "Building DMG"
mkdir -p "$DIST"
rm -f "$DMG"
STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
# The drag-to-install convention: the .app next to a symlink to /Applications.
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "$APP_NAME $VERSION" -srcfolder "$STAGE" \
  -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"

if [ "$SIGN_IDENTITY" != "-" ]; then
  codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG"
fi

# --------------------------------------------------------------------- notarize
if [ "$NOTARIZE" = "1" ]; then
  : "${APPLE_ID:?APPLE_ID is required when NOTARIZE=1}"
  : "${APPLE_APP_PASSWORD:?APPLE_APP_PASSWORD is required when NOTARIZE=1}"
  : "${APPLE_TEAM_ID:?APPLE_TEAM_ID is required when NOTARIZE=1}"

  say "Notarizing (this waits on Apple, typically a few minutes)"
  xcrun notarytool submit "$DMG" \
    --apple-id "$APPLE_ID" --password "$APPLE_APP_PASSWORD" --team-id "$APPLE_TEAM_ID" \
    --wait

  say "Stapling"
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
else
  say "Skipping notarization (NOTARIZE!=1)"
fi

# ------------------------------------------------------------------------ verify
say "Verification"
codesign -dv --verbose=4 "$APP" 2>&1 | sed 's/^/    /'
# Gatekeeper's verdict. Only meaningful for a real Developer ID + notarized
# build; an ad-hoc signature is expected to be rejected here, so don't fail on it.
if spctl -a -t open --context context:primary-signature -v "$DMG" 2>&1 | sed 's/^/    /'; then
  say "Gatekeeper: accepted"
else
  if [ "$NOTARIZE" = "1" ]; then
    die "Gatekeeper rejected a notarized build -- do not ship this"
  fi
  say "Gatekeeper: rejected (expected for an unsigned/ad-hoc local build)"
fi

say "Done: $DMG"
shasum -a 256 "$DMG" | sed 's/^/    /'
