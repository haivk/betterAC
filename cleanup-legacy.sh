#!/usr/bin/env bash
#
# Remove every earlier version of betterAC / Asheron's Call from this machine, so
# a fresh install starts from nothing.
#
#   curl -fsSL https://raw.githubusercontent.com/haivk/betterAC/main/cleanup-legacy.sh | bash
#
# betterAC has been installed three quite different ways over its life, and each
# left its files somewhere else:
#
#   1. the original setup script  -- a prefix in ~/Games, GE-Proton in Steam's
#                                    compatibility tools, downloads in ~/.cache
#   2. a Flatpak                  -- ac.betterac.BetterAC, plus ~/.var/app data
#   3. the current tarball        -- ~/.local/bin, ~/.local/share, ~/.config
#
# This finds all three, shows you exactly what it found, and removes it once you
# say so. Then run the normal installer:
#
#   curl -fsSL https://raw.githubusercontent.com/haivk/betterAC/main/install.sh | bash
#
#   --dry-run   list what would be removed and stop
#   --yes       do not ask (for scripts; the prompt is skipped)
#   --deep      also remove things betterAC *downloaded* but does not own
#               exclusively -- see the warning printed for that section
set -euo pipefail

DRY=0
ASSUME_YES=0
DEEP=0
for a in "$@"; do
  case "$a" in
    --dry-run) DRY=1 ;;
    --yes|-y)  ASSUME_YES=1 ;;
    --deep)    DEEP=1 ;;
    -h|--help) sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) printf "ignoring unknown option: %s (try --help)\n" "$a" >&2 ;;
  esac
done

if [[ -t 1 ]]; then B=$'\e[1m'; DIM=$'\e[2m'; G=$'\e[32m'; Y=$'\e[33m'; R=$'\e[31m'; X=$'\e[0m'
else B=""; DIM=""; G=""; Y=""; R=""; X=""; fi
say()  { printf "\n${B}%s${X}\n" "$*"; }
ok()   { printf "  ${G}✓${X} %s\n" "$*"; }
gone() { printf "  ${DIM}·${X} %s ${DIM}%s${X}\n" "$1" "$2"; }
warn() { printf "  ${Y}!${X} %s\n" "$*"; }
die()  { printf "\n${R}${B}error:${X} %s\n" "$*" >&2; exit 1; }

[[ "$(uname -s)" == "Linux" ]] || die "this is the Linux cleanup.
       On macOS: drag BetterAC out of /Applications, or 'brew uninstall --cask betterac',
       then remove ~/Library/Application Support/betterac"

# --------------------------------------------------------------------- discover
#
# Everything is *found* first and only then removed, so the report is a complete
# picture rather than a running commentary on damage already done.

FOUND=()       # paths to remove
LABEL=()       # what each one is, in plain words
SHARED=()      # --deep only: downloaded by us, but usable by other software
SHARED_LABEL=()

add()        { [[ -e "$1" || -L "$1" ]] && { FOUND+=("$1"); LABEL+=("$2"); }; return 0; }
add_shared() { [[ -e "$1" ]] && { SHARED+=("$1"); SHARED_LABEL+=("$2"); }; return 0; }

human() { du -sh "$1" 2>/dev/null | cut -f1 || echo "?"; }

# --- the prefix, which may not be where we would have put it -------------------
#
# Read from the config before the config is deleted: someone who pointed betterAC
# at a different disk would otherwise keep the largest thing on the machine.
PREFIX=""
CONFIG="$HOME/.config/betterac/config.json"
if [[ -f "$CONFIG" ]]; then
  if command -v python3 >/dev/null; then
    PREFIX="$(python3 -c 'import json,sys
try: print(json.load(open(sys.argv[1])).get("prefix",""))
except Exception: print("")' "$CONFIG" 2>/dev/null || true)"
  else
    PREFIX="$(sed -n 's/.*"prefix"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$CONFIG" | head -1)"
  fi
fi

# Guard against a corrupt or hand-edited config turning this into `rm -rf /` or
# `rm -rf ~`. Same rule ac_core::reset uses: absolute, inside $HOME, and deep
# enough that it cannot be a home directory or a filesystem root.
safe_prefix() {
  local p="$1"
  [[ -n "$p" && "$p" == /* && "$p" != "$HOME" && "$p" == "$HOME"/* ]] || return 1
  [[ "$(tr -cd '/' <<<"$p" | wc -c)" -ge 3 ]] || return 1
  return 0
}
if [[ -n "$PREFIX" ]] && ! safe_prefix "$PREFIX"; then
  warn "config names a prefix this script will not touch: $PREFIX"
  PREFIX=""
fi

# --- iteration 3: the current tarball install ---------------------------------
add "$HOME/.local/bin/betterac"                                              "launcher binary"
add "$HOME/.config/betterac"                                                 "settings, saved servers and passwords"
add "$HOME/.local/share/betterac"                                            "betterAC's own Proton copy"
[[ -n "$PREFIX" ]] && add "$PREFIX"                                          "game + Windows prefix (from config)"
add "$HOME/Games/asherons-call"                                              "game + Windows prefix (default location)"

# Desktop entries and icons are matched rather than named, because the identifier
# changed between versions and a stale one means a dead icon in the app grid
# forever.
#
# Matched on **betterac**, never on "asheron". Other launchers install entries for
# this game too -- a Lutris install puts `lutris_asherons-call.png` right here --
# and deleting another program's files would be inexcusable. Our own names all
# contain `betterac` (the binary, and the `ac.betterac.BetterAC` app id), so that
# is the discriminator. The cost is that a stray icon from some very old build
# might survive; that is the right way round.
while IFS= read -r f; do add "$f" "app grid entry"; done < <(
  grep -rlsi 'betterac' "$HOME/.local/share/applications" --include='*.desktop' 2>/dev/null || true
)
while IFS= read -r f; do add "$f" "app icon"; done < <(
  find "$HOME/.local/share/icons" -iname '*betterac*' 2>/dev/null || true
)

# Anything else this machine has for Asheron's Call belongs to another program.
# Say so, so an unchanged Lutris/Steam entry does not look like a failed cleanup.
OTHERS=()
while IFS= read -r f; do OTHERS+=("$f"); done < <(
  { grep -rlsi 'asheron' "$HOME/.local/share/applications" --include='*.desktop' 2>/dev/null || true
    find "$HOME/.local/share/icons" -iname '*asheron*' 2>/dev/null || true
  } | grep -vi betterac || true
)

# --- iteration 2: the Flatpak -------------------------------------------------
FLATPAK_ID=""
if command -v flatpak >/dev/null && flatpak list --app --columns=application 2>/dev/null | grep -qx "ac.betterac.BetterAC"; then
  FLATPAK_ID="ac.betterac.BetterAC"
fi
add "$HOME/.var/app/ac.betterac.BetterAC"                                    "Flatpak data"

# --- iteration 1: the original setup script -----------------------------------
add "$HOME/Desktop/AC"                                                       "original setup script + game files"
add "$HOME/Downloads/acinstaller"                                            "downloaded copy of the old installer"
add "$HOME/acinstaller"                                                      "old installer, unpacked in home"

# --- shared, --deep only ------------------------------------------------------
add_shared "$HOME/.cache/acinstaller"    "downloaded installers (re-downloaded on next setup)"
add_shared "$HOME/.cache/winetricks"     "winetricks cache -- shared with any other Wine app"
add_shared "$HOME/.local/share/umu"      "umu runtime -- shared with any other umu/Proton game"
while IFS= read -r d; do
  add_shared "$d" "GE-Proton in Steam's tools -- other Steam games may use it"
done < <(find "$HOME/.local/share/Steam/compatibilitytools.d" -maxdepth 1 -name 'GE-Proton*' 2>/dev/null || true)

# ----------------------------------------------------------------------- report

printf "\n${B}betterAC -- legacy cleanup${X}\n"

# `--deep` has to survive this check: once the app itself is gone -- a partial
# cleanup, or simply a second run -- FOUND is empty, and an early exit here would
# silently skip the caches that are the entire point of asking for --deep.
NOTHING=0
if [[ ${#FOUND[@]} -eq 0 && -z "$FLATPAK_ID" ]]; then
  if [[ $DEEP -eq 0 || ${#SHARED[@]} -eq 0 ]]; then NOTHING=1; fi
fi
if [[ $NOTHING -eq 1 ]]; then
  say "Nothing to remove"
  printf "  No previous betterAC or Asheron's Call install was found.\n\n"
  printf "  Install with:\n"
  printf "    ${B}curl -fsSL https://raw.githubusercontent.com/haivk/betterAC/main/install.sh | bash${X}\n\n"
  exit 0
fi

say "Found"
[[ -n "$FLATPAK_ID" ]] && printf "  %-52s ${DIM}%s${X}\n" "$FLATPAK_ID" "installed Flatpak"
for i in "${!FOUND[@]}"; do
  printf "  %-52s ${DIM}%8s  %s${X}\n" "${FOUND[$i]/#$HOME/~}" "$(human "${FOUND[$i]}")" "${LABEL[$i]}"
done

if [[ ${#SHARED[@]} -gt 0 ]]; then
  if [[ $DEEP -eq 1 ]]; then
    say "Also removing (--deep)"
  else
    say "Left alone ${DIM}(pass --deep to remove these too)${X}"
  fi
  for i in "${!SHARED[@]}"; do
    printf "  %-52s ${DIM}%8s  %s${X}\n" "${SHARED[$i]/#$HOME/~}" "$(human "${SHARED[$i]}")" "${SHARED_LABEL[$i]}"
  done
  [[ $DEEP -eq 1 ]] && warn "these are used by other software too -- only --deep removes them"
fi

if [[ ${#OTHERS[@]} -gt 0 ]]; then
  say "Not ours ${DIM}(another program installed these -- left alone)${X}"
  for f in "${OTHERS[@]}"; do printf "  %s\n" "${f/#$HOME/~}"; done
fi

if [[ $DRY -eq 1 ]]; then
  printf "\n${DIM}--dry-run: nothing was removed.${X}\n\n"
  exit 0
fi

# ---------------------------------------------------------------------- confirm

if [[ $ASSUME_YES -eq 0 ]]; then
  printf "\n${B}Remove everything listed above? This cannot be undone.${X} [y/N] "
  # /dev/tty, not stdin: this script is meant to be piped from curl, and stdin is
  # the script itself in that case.
  if [[ -r /dev/tty ]]; then read -r reply < /dev/tty; else
    printf "\n"; die "not running interactively -- re-run with --yes to confirm"
  fi
  [[ "$reply" =~ ^[Yy] ]] || { printf "\nNothing was removed.\n\n"; exit 0; }
fi

# ------------------------------------------------------------- stop anything up
#
# A running launcher holds its binary open, and a live wineserver writes into the
# prefix while it is being deleted -- it flushes the registry and recreates
# dosdevices entries, which empties the directory and then fails to remove it.

say "Stopping anything still running"
# -x, never -f: a pattern match would also match this script's own command line.
if pkill -x betterac 2>/dev/null; then ok "closed the launcher"; fi
if pkill -x wineserver 2>/dev/null; then ok "ended Wine sessions"; sleep 2; fi
[[ -n "$FLATPAK_ID" ]] && flatpak kill "$FLATPAK_ID" >/dev/null 2>&1 && ok "stopped the Flatpak"
sleep 1

# ----------------------------------------------------------------------- remove

say "Removing"
FREED=0
remove() {
  local p="$1" label="$2" size
  [[ -e "$p" || -L "$p" ]] || return 0
  size="$(du -sm "$p" 2>/dev/null | cut -f1 || echo 0)"
  if rm -rf "$p" 2>/dev/null; then
    FREED=$((FREED + ${size:-0}))
    gone "removed ${p/#$HOME/~}" "$label"
  else
    warn "could not remove ${p/#$HOME/~} -- remove it by hand"
  fi
}

if [[ -n "$FLATPAK_ID" ]]; then
  if flatpak uninstall -y --delete-data "$FLATPAK_ID" >/dev/null 2>&1; then
    gone "uninstalled $FLATPAK_ID" "Flatpak"
  else
    warn "could not uninstall the Flatpak -- try: flatpak uninstall $FLATPAK_ID"
  fi
fi

for i in "${!FOUND[@]}"; do remove "${FOUND[$i]}" "${LABEL[$i]}"; done
if [[ $DEEP -eq 1 ]]; then
  for i in "${!SHARED[@]}"; do remove "${SHARED[$i]}" "${SHARED_LABEL[$i]}"; done
fi

# The app grid keeps showing a removed launcher until these are refreshed.
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null && ok "refreshed the app grid" || true
gtk4-update-icon-cache -qtf "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

# ----------------------------------------------------------------------- finish

say "Done"
printf "  Freed roughly ${B}%s MB${X}.\n" "$FREED"
printf "\n  Now install the current version:\n"
printf "    ${B}curl -fsSL https://raw.githubusercontent.com/haivk/betterAC/main/install.sh | bash${X}\n\n"
