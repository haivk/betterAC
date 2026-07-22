# Packaging & releases

Everything needed to ship betterAC. The logic lives in the scripts here, not in
the workflow YAML, so a release can be rehearsed on a laptop instead of only
inside GitHub Actions.

| Path | What it is |
|---|---|
| `macos/build-dmg.sh` | universal `.app` → sign → `.dmg` → notarize → staple |
| `linux/build-tarball.sh` | binary + desktop + icon + metainfo → `.tar.gz` |
| `../install.sh` | the curl-able bootstrap: finds the latest release, verifies it, runs the tarball's `install.sh` |
| `source-tarball.sh` | `git archive` of the tagged tree → `-src.tar.gz`, what the Linux formula builds |
| `render-metadata.sh` | fills the templates below with the version and real SHA-256s |
| `homebrew/betterac.rb.in` | Homebrew **cask** template — macOS, ships the `.dmg` |
| `homebrew/betterac-formula.rb.in` | Homebrew **formula** template — Linux, builds from source |
| `aur/PKGBUILD.in` | AUR `betterac-bin` template |
| `shared/*.metainfo.xml` | AppStream metadata (used by the `.deb`, AUR and Flatpak) |
| `flatpak/*.yml` | Flatpak manifest — **a scaffold**, see the header in that file |

The `.deb` has no script: it is `cargo deb`, configured under
`[package.metadata.deb]` in `gtk/Cargo.toml`.

The root `install.sh` is not built or attached to a release — it is served from
`raw.githubusercontent.com/haivk/betterAC/main/install.sh`, so it is always the
`main` copy. It depends on two things being present on every release: the
tarball named `betterac-<version>-x86_64.tar.gz`, and `SHA256SUMS`. Renaming
either breaks every published one-liner, so don't.

## Cutting a release

```sh
# 1. Bump the version in ONE place — the workspace inherits it.
vim Cargo.toml            # [workspace.package] version = "0.2.0"
cargo check --workspace   # refresh Cargo.lock

# 2. Tag it. The tag must match, or the workflow stops before building anything.
git commit -am "release 0.2.0" && git tag v0.2.0 && git push origin main v0.2.0
```

That triggers `.github/workflows/release.yml`, which builds the DMG (signed and
notarized), the Linux tarball and `.deb`, renders the metadata, and publishes a
GitHub Release with everything attached.

## Required repository secrets

Settings → Secrets and variables → Actions. Without these the macOS job fails at
the signing step.

| Secret | What |
|---|---|
| `APPLE_CERT_P12` | base64 of the exported *Developer ID Application* certificate: `base64 -i cert.p12 \| pbcopy` |
| `APPLE_CERT_PASSWORD` | the password set when exporting that `.p12` |
| `APPLE_SIGN_IDENTITY` | e.g. `Developer ID Application: Your Name (ABCDE12345)` — `security find-identity -v -p codesigning` |
| `APPLE_ID` | Apple ID email used for notarization |
| `APPLE_APP_PASSWORD` | an **app-specific** password from appleid.apple.com, not your account password |
| `APPLE_TEAM_ID` | the 10-character team ID |

The workflow imports the certificate into a throwaway keychain and deletes it in
an `if: always()` step, so it never lands in the runner's login keychain.

## After the release: the two manual steps

Both are deliberate — while things are still moving, it is better to look at the
rendered file before it goes live.

**Homebrew.** One-time: create a GitHub repo named `homebrew-betterac`. The tap
serves both platforms from one repo — a cask for macOS and a formula for Linux —
so each release copies **two** files into **two** directories:

```sh
cp betterac.rb          <tap>/Casks/betterac.rb      # macOS: the signed .dmg
cp betterac-formula.rb  <tap>/Formula/betterac.rb    # Linux: builds from source
cd <tap> && git commit -am "betterac 0.2.0" && git push
# users:
brew tap haivk/betterac
brew install --cask betterac   # macOS
brew install betterac          # Linux
```

Note the rename: both render out of `dist/` and the cask already owns
`betterac.rb` there, so the formula is written as `betterac-formula.rb` and only
stops colliding once the two are in different tap directories.

**`--cask` is not optional on macOS.** Homebrew resolves a formula before a
same-named cask, so a bare `brew install betterac` on a Mac lands on the Linux
formula and stops at `depends_on :linux` with `Error: betterac: Linux is
required.` That is the accepted cost of one name on both platforms.

The Linux formula builds from source rather than repackaging the release
tarball, because the two link different toolkits: the release binary resolves
gtk4 from the distro (needing ≥ 4.12, which a formula cannot declare or
enforce), while the formula links Homebrew's own. Homebrew bottles `gtk4` and
`libadwaita` for `x86_64_linux` and `arm64_linux`, so only betterAC itself
compiles — and Linux arm64 comes free, which the x86_64-only binary tarball
cannot offer.

This is a personal tap rather than homebrew-cask because upstream has notability
requirements (stars, forks, age) a new repo will not meet.

**AUR.** One-time: register an SSH key with your AUR account. Then per release,
take `PKGBUILD` and `.SRCINFO` off the release page:

```sh
git clone ssh://aur@aur.archlinux.org/betterac-bin.git
cp PKGBUILD .SRCINFO betterac-bin/
cd betterac-bin && git commit -am "betterac-bin 0.2.0" && git push
```

## Rehearsing locally

The macOS path runs end to end on any Mac. With no identity it uses an ad-hoc
signature and skips notarization, which still exercises the build, the universal
check, the DMG and the verification:

```sh
./packaging/macos/build-dmg.sh                     # → dist/BetterAC-<ver>-universal.dmg
./packaging/source-tarball.sh dist                 # → dist/betterac-<ver>-src.tar.gz
./packaging/render-metadata.sh dist                # → cask, formula, PKGBUILD, .SRCINFO, SHA256SUMS
ruby -c dist/betterac.rb && ruby -c dist/betterac-formula.rb
```

`source-tarball.sh` runs anywhere with git — it packs `HEAD`, not your working
tree, so commit before rehearsing or you will checksum the wrong thing. It says
so when the tree is dirty.

Gatekeeper is *expected* to reject an ad-hoc build; the script says so rather
than failing. It does fail if a notarized build is rejected, which is the case
that must never ship.

`linux/build-tarball.sh` needs a Linux box (gtk4 + libadwaita development
files); it cannot run on macOS at all.

### Testing the Linux formula before a release exists

The formula's `url` points at a release asset, so a rendered formula cannot be
installed until that release is published. To test one first, point it at the
local tarball — Homebrew accepts `file://`:

```sh
./packaging/source-tarball.sh dist
scp dist/betterac-<ver>-src.tar.gz <linuxbox>:
# on the box, in a copy of the rendered formula:
#   url    "file:///home/<user>/betterac-<ver>-src.tar.gz"
#   sha256 "<sha256sum of that file>"
brew install --build-from-source ./betterac.rb
brew audit --strict --formula ./betterac.rb
brew linkage --test betterac    # must not list any system gtk4
betterac --version              # what the formula's test block runs
betterac                        # the check that matters: GUI, schemas, icons
```

Launching the GUI is the part worth doing by hand. It is what proves the
`XDG_DATA_DIRS` wrapper in the formula is enough — without it the binary cannot
find gtk4's GSettings schemas under the Homebrew prefix and aborts at startup.

## Debian/Ubuntu caveat

`umu-launcher` is not packaged in Debian or Ubuntu, and `core/src/proton.rs`
refuses to finish setup without it. The `.deb` lists it under `Suggests` and says
so in the description — making it a hard `Depends` would just make the package
uninstallable.
