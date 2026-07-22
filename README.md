# betterAC

A native GTK4/libadwaita launcher for Asheron's Call on Linux. Replaces ThwargLauncher.

Pick a server, type your account, press Play.

## Why

ThwargLauncher is a Windows WPF app, so it has to run *inside* the Wine prefix — and that
is where all the DPI pain came from. WPF reads Wine's font metrics, so a prefix scaled for
a HiDPI panel either renders the launcher at quarter size, or, if `LogPixels` and
`WindowMetrics` disagree by even a little, kills it outright with *"Fatal program error"*
before it can draw a single window.

betterAC is a normal Linux app. It scales with GNOME like everything else, and nothing runs
under Wine except the game itself. The whole class of problem goes away.

## Install

From a release, via Homebrew — one tap serves both platforms:

```sh
brew tap haivk/betterac

brew install --cask betterac   # macOS: the signed, notarized app
brew install betterac          # Linux: builds from source against brew's gtk4
```

`--cask` is required on macOS. Homebrew resolves a formula before a same-named
cask, so a bare `brew install betterac` on a Mac finds the Linux formula and stops
with `Error: betterac: Linux is required.`

Linux also has a `.deb`, an AUR package (`yay -S betterac-bin`) and a plain tarball
on the [releases page](https://github.com/haivk/betterAC/releases). All of them need
`umu-launcher`, which is not in Homebrew or Debian — see `packaging/README.md`.

### From this repo

`../install-ac.sh` runs this for you as its last step. On its own:

```sh
./install.sh
```

Installs the prebuilt binary from `dist/` to `~/.local` — binary, `.desktop` entry, icon. No
root, no sandbox, no compiler.

Shipping a binary is the point. Bazzite is atomic and has no compiler on the host, but
betterAC has to *run* on the host — it shells out to `umu-run` — so building it would mean a
toolbox, a Rust toolchain and the GTK4 headers just to produce a 2 MB file that doesn't change.
`dist/betterac-x86_64` is built on Fedora 41, older than any Bazzite, so its glibc floor (2.39)
sits below the host's, and it links only against `libgtk-4`, `libadwaita` and `glib` — which a
GNOME desktop already has. The installer checks with `ldd` and tells you if it's wrong.

To build from source instead:

```sh
toolbox create ac && toolbox enter ac
sudo dnf install -y cargo gtk4-devel libadwaita-devel
./install.sh --build
```

A toolbox shares your `$HOME`, so the binary it builds installs straight to `~/.local` and runs
on the host.

To refresh the shipped binary from a Mac or any machine with Docker:

```sh
docker run --rm --platform linux/amd64 -v "$PWD:/src:ro" -v "$PWD/dist:/out" fedora:41 bash -c '
  dnf install -y -q rust cargo gtk4-devel libadwaita-devel
  cp -r /src /build && cd /build && rm -rf target
  cargo build --release && install -m755 target/release/betterac /out/betterac-x86_64'
```

Requires an AC prefix already built by `../install-ac.sh`, and `umu-run` on the host.

## Servers

The server list is live from [treestats.net](https://treestats.net/servers.json) — 44
servers at time of writing, with **live player counts**. Press **+**, search, click one to
add it.

Counts are shown only when they're actually current. The feed timestamps each one, and a
server whose population was last seen "a day ago" gets its number dimmed and dated rather
than presented as if it were live. Servers are sorted busiest-first, since population is the
thing you're really choosing on.

A snapshot of the list is compiled into the binary, so the browser opens instantly on a
usable list and still works with no network. The live list quietly replaces it when it
arrives.

## The bit that matters

ACE and GDLE take the same three facts in two completely different shapes:

```
ACE    acclient.exe -a ACCOUNT -v PASSWORD -h HOST:PORT
GDLE   acclient.exe -h HOST -p PORT -a ACCOUNT:PASSWORD
```

Get it wrong and the client doesn't error — it just fails to log in. The directory's
`software` field is what picks the shape, and it's inconsistent about it (`GDL` and `GDLE`
both appear, for the same thing). This is the one piece of real logic in the app and it is
covered by tests.

Note GDLE colon-joins the credentials into a single argument, so a password containing a `:`
is genuinely ambiguous there. betterAC refuses it up front rather than let you stare at a
login failure you can't explain.

## gamescope

The game runs inside gamescope, with DXVK off (`PROTON_USE_WINED3D=1`). That is one decision,
not two: wined3d's *"Cannot initialize Direct3D"* was it failing to enumerate a display adapter
on a bare Wayland session, and gamescope — a nested compositor — gives it one. AC is D3D9 from
1999; it doesn't need Vulkan translation to draw. Drop gamescope and DXVK comes back on,
because there the old failure is still real. Both halves are covered by tests.

The game runs at your **current display mode, natively**. betterAC detects it and passes it as
both the output resolution (`-W/-H`) and the game's own (`-w/-h`), 1:1. That is not the same as
passing nothing: nested gamescope defaults to 1280x720, so `-f` alone fullscreens an upscaled
720p image.

The resolution comes from Mutter over D-Bus (`org.gnome.Mutter.DisplayConfig`), which reports
the true hardware mode. GDK is only the fallback, because `Monitor::geometry` is in *logical*
pixels and `scale_factor` is an integer — on a fractionally scaled desktop (125%, 150%) that
pair cannot reconstruct the real mode and overshoots. If neither answers we pass no resolution
at all, since a wrong `-W/-H` is worse than gamescope's own guess.

| Env | Default | |
|---|---|---|
| `BETTERAC_GAMESCOPE=0` | on | No gamescope, DXVK back on. |
| `BETTERAC_GAMESCOPE_ARGS` | detected res + `-f --force-grab-cursor` | Replaces everything, resolution included. |

`--force-grab-cursor` keeps the mouse inside the game — AC is click-to-move and without it the
cursor wanders onto the desktop mid-fight. To make the in-engine UI readable on a HiDPI panel,
render small and upscale: `-w 2752 -h 1152 -F fsr -f`. That's what replaced the Wine DPI knob.

If gamescope isn't installed, betterAC notices and runs the game without it rather than failing
to spawn.

## Passwords

**Stored in plaintext**, in `~/.config/betterac/config.json`. The file is created `0600`, so
other users on the box can't read it — but it is plaintext, and anything running as you can
read it.

Moving to the GNOME keyring later is a small change: `Entry::password` becomes a libsecret
lookup and nothing else in the app has to care.

## Layout

```
src/servers.rs    the treestats directory: fetch, parse, player counts
src/launcher.rs   builds the acclient argv and runs it under Proton
src/config.rs     your added servers + accounts
src/window.rs     main window
src/browser.rs    the "add a server" dialog
data/             .desktop, icon, bundled server snapshot
dist/             the shipped x86_64 binary
```

```sh
cargo test    # 16 tests, all logic, no GTK needed
```

To refresh the bundled snapshot:

```sh
curl -sfL https://treestats.net/servers.json > data/servers-snapshot.json
```
