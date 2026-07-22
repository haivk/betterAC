# Packaging & releases

Everything needed to ship betterAC. The logic lives in the scripts here, not in
the workflow YAML, so a release can be rehearsed on a laptop instead of only
inside GitHub Actions.

| Path | What it is |
|---|---|
| `macos/build-dmg.sh` | universal `.app` → sign → `.dmg` → notarize → staple |
| `linux/build-tarball.sh` | binary + desktop + icon + metainfo → `.tar.gz` |
| `render-metadata.sh` | fills the templates below with the version and real SHA-256s |
| `homebrew/betterac.rb.in` | Homebrew cask template |
| `aur/PKGBUILD.in` | AUR `betterac-bin` template |
| `shared/*.metainfo.xml` | AppStream metadata (used by the `.deb`, AUR and Flatpak) |
| `flatpak/*.yml` | Flatpak manifest — **a scaffold**, see the header in that file |

The `.deb` has no script: it is `cargo deb`, configured under
`[package.metadata.deb]` in `gtk/Cargo.toml`.

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

**Homebrew.** One-time: create a GitHub repo named `homebrew-betterac`. Then per
release, take `betterac.rb` off the release page:

```sh
cp betterac.rb <tap>/Casks/betterac.rb
cd <tap> && git commit -am "betterac 0.2.0" && git push
# users:
brew tap haivk/betterac && brew install --cask betterac
```

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
./packaging/render-metadata.sh dist                # → cask, PKGBUILD, .SRCINFO, SHA256SUMS
```

Gatekeeper is *expected* to reject an ad-hoc build; the script says so rather
than failing. It does fail if a notarized build is rejected, which is the case
that must never ship.

The Linux scripts need a Linux box (gtk4 + libadwaita development files). They
cannot run on macOS at all.

## Debian/Ubuntu caveat

`umu-launcher` is not packaged in Debian or Ubuntu, and `core/src/proton.rs`
refuses to finish setup without it. The `.deb` lists it under `Suggests` and says
so in the description — making it a hard `Depends` would just make the package
uninstallable.
