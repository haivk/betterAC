#!/usr/bin/env bash
#
# Build and install the betterAC Flatpak. RUN THIS ON THE LINUX BOX -- there is
# no way to build a Flatpak on macOS, cross or otherwise.
#
#   ./packaging/flatpak/build-local.sh              # build + install + tell you how to run
#   ./packaging/flatpak/build-local.sh --bundle     # also write a single-file .flatpak
#
# Written for Bazzite and other atomic distros: it installs flatpak-builder AS A
# FLATPAK (org.flatpak.Builder) rather than layering it with rpm-ostree, which
# would need a reboot. On a traditional distro an already-installed
# flatpak-builder is used if present.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
MANIFEST="$HERE/ac.betterac.BetterAC.yml"
APP_ID="ac.betterac.BetterAC"
BUNDLE=0
[ "${1:-}" = "--bundle" ] && BUNDLE=1

say()  { printf "\n\033[1m==> %s\033[0m\n" "$*"; }
warn() { printf "  ! %s\n" "$*" >&2; }
die()  { printf "\nerror: %s\n" "$*" >&2; exit 1; }

command -v flatpak >/dev/null || die "flatpak is not installed"

# The versions the manifest asks for. Read them rather than hardcoding twice.
RUNTIME_VERSION="$(grep -m1 '^runtime-version:' "$MANIFEST" | tr -d '"' | awk '{print $2}')"
# The rust-stable extension is versioned by the freedesktop base, not by GNOME:
# GNOME 47 and 48 both sit on 24.08.
RUST_EXT_VERSION="${RUST_EXT_VERSION:-24.08}"

say "Checking the Flathub remote"
# Bazzite (and most distros that ship Flathub) configure it SYSTEM-wide. Adding a
# second user-scoped remote of the same name makes every unscoped `flatpak
# remote-*` call ambiguous, so only add one if the user installation genuinely
# lacks it, and scope every command below explicitly.
if flatpak remotes --user --columns=name 2>/dev/null | grep -qx flathub; then
  FLATHUB_SCOPE=--user
elif flatpak remotes --system --columns=name 2>/dev/null | grep -qx flathub; then
  FLATHUB_SCOPE=--system
else
  say "No Flathub remote found -- adding one for this user"
  flatpak remote-add --if-not-exists --user \
    flathub https://dl.flathub.org/repo/flathub.flatpakrepo
  FLATHUB_SCOPE=--user
fi
echo "    using the $(echo "$FLATHUB_SCOPE" | tr -d -) Flathub remote"

# Already installed, in either scope? Then nothing to fetch. Bazzite ships a pile
# of runtimes system-wide, so this often short-circuits the whole step.
ref_installed() { flatpak info "$1" >/dev/null 2>&1; }

# Deliberately NOT `flatpak remote-ls`: on Flathub that enumerates tens of
# thousands of refs and takes long enough to look like a hang. remote-info asks
# about exactly one ref.
ref_available() {
  flatpak remote-info "$FLATHUB_SCOPE" flathub "$1" >/dev/null 2>&1
}

# List what versions of a runtime Flathub actually has. Slow (it is the big
# query), so this only runs when something is missing and we need to say why.
available_versions() {
  flatpak remote-ls "$FLATHUB_SCOPE" flathub --columns=ref 2>/dev/null \
    | grep -- "$1" | sed 's|.*/||' | sort -u | sed 's/^/    /'
}

say "Runtime, SDK and Rust extension"
for ref in \
  "org.gnome.Platform//$RUNTIME_VERSION" \
  "org.gnome.Sdk//$RUNTIME_VERSION" \
  "org.freedesktop.Sdk.Extension.rust-stable//$RUST_EXT_VERSION"
do
  if ref_installed "$ref"; then
    echo "    have $ref"
    continue
  fi

  if ! ref_available "$ref"; then
    warn "not on Flathub: $ref"
    # Show flatpak's real complaint rather than hiding it -- if this is a network
    # or remote problem rather than a bad version, the message says so.
    echo "  flatpak said:"
    flatpak remote-info "$FLATHUB_SCOPE" flathub "$ref" 2>&1 | sed 's/^/    /' || true
    case "$ref" in
      org.gnome.*)
        echo "  org.gnome.Platform versions Flathub has (this query is slow):"
        available_versions '^org\.gnome\.Platform/'
        die "set runtime-version in $MANIFEST to one of the above" ;;
      *)
        echo "  rust-stable versions Flathub has:"
        available_versions 'rust-stable'
        die "re-run with RUST_EXT_VERSION=<one of the above>" ;;
    esac
  fi

  # Install into the USER installation so nothing here needs root. If flathub is
  # only configured system-wide, the user installation may not know the remote
  # by name -- add it for this user and retry.
  echo "    installing $ref"
  if ! flatpak install -y --user flathub "$ref" 2>/dev/null; then
    flatpak remote-add --if-not-exists --user \
      flathub https://dl.flathub.org/repo/flathub.flatpakrepo
    flatpak install -y --user flathub "$ref"
  fi
done

# flatpak-builder: prefer a native one, else the Flatpak (the atomic-distro path).
if command -v flatpak-builder >/dev/null; then
  BUILDER=(flatpak-builder)
  say "Using the system flatpak-builder"
else
  say "No system flatpak-builder -- installing org.flatpak.Builder"
  flatpak install -y --user flathub org.flatpak.Builder
  BUILDER=(flatpak run org.flatpak.Builder)
fi

cd "$ROOT"
BUILD_DIR="$ROOT/.flatpak-build"
STATE_DIR="$ROOT/.flatpak-builder"

say "Building (first run compiles the whole dependency tree; expect a few minutes)"
"${BUILDER[@]}" \
  --force-clean \
  --user --install \
  --state-dir "$STATE_DIR" \
  --install-deps-from=flathub \
  "$BUILD_DIR" "$MANIFEST"

if [ "$BUNDLE" = "1" ]; then
  say "Writing a single-file bundle"
  REPO="$ROOT/.flatpak-repo"
  "${BUILDER[@]}" --force-clean --repo "$REPO" --state-dir "$STATE_DIR" \
    "$BUILD_DIR" "$MANIFEST"
  flatpak build-bundle "$REPO" "$ROOT/betterac.flatpak" "$APP_ID"
  echo "    $ROOT/betterac.flatpak"
fi

say "Installed"
cat <<EOF

  Run it:            flatpak run $APP_ID
  See its sandbox:   flatpak info --show-permissions $APP_ID
  Uninstall:         flatpak uninstall --user $APP_ID

  What to check, given the launcher-only state of this manifest:
    - the window opens and the server list populates (that is the network
      permission and GTK4/libadwaita working inside the runtime)
    - the FIRST-RUN SETUP CHECKLIST renders -- ten rows, each with its own
      progress bar. That UI has never been run on Linux, only on macOS.
    - pressing "Set up Asheron's Call" is EXPECTED to fail at step 1
      ("Checking dependencies") with a message about umu-run: inside the sandbox
      the host's tools are not on PATH. That is the known gap, not a new bug.
EOF
