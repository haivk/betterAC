# Packaging & releases

Everything needed to ship betterAC. The logic lives in the scripts here, not in
the workflow YAML, so a release can be rehearsed on a laptop instead of only
inside GitHub Actions.

| Path | What it is |
|---|---|
| `macos/build-dmg.sh` | universal `.app` → sign → `.dmg` → notarize → staple |
| `linux/build-tarball.sh` | binary + desktop entry + icon → `.tar.gz` |
| `../install.sh` | the curl-able bootstrap for **both** platforms: finds a release, verifies it, installs it |
| `render-metadata.sh` | renders the Homebrew cask and writes `SHA256SUMS` |
| `homebrew/betterac.rb.in` | Homebrew **cask** template — macOS, ships the `.dmg` |

**Homebrew (macOS) is the only package manager.** The AUR package, the Flatpak,
the `.deb` and the Linux Homebrew formula were all dropped in favour of the
one-line installer. That is not laziness about maintenance: betterAC's job is to
drive `umu-run` on the host, which Debian and Ubuntu do not package at all, and a
Flatpak cannot do it — the sandbox SIGSYS-kills `wine-preloader`, so the game
never starts. A tarball into `~/.local` is what this program honestly is.

The root `install.sh` is not built or attached to a release — it is served from
`raw.githubusercontent.com/haivk/betterAC/main/install.sh`, so it is always the
`main` copy. The one thing it requires of every release is **`SHA256SUMS`**: it
verifies the download against it *and* reads the artifact's name out of it, so
artifacts can be renamed freely without breaking published one-liners.

## Cutting a release

```sh
git push origin main
```

That is the whole procedure. **Every commit to `main` is a release**: the workflow
builds it, creates the tag, publishes the GitHub Release and updates the Homebrew
cask. There is nothing to tag by hand and no stable/unstable split.

### Why the versions are dates

Releases are versioned `YYYY.MM.DD.<run number>` — `2026.07.27.42`. Two reasons,
one of them load-bearing:

- **Homebrew compares version strings to decide an upgrade exists.** Republishing
  every commit under one version would leave Mac users pinned to whatever they
  installed first, with `brew upgrade` reporting nothing to do.
- The run number disambiguates several builds landing on the same day, and only
  ever increases.

It **cannot** live in `Cargo.toml`: four components is not semver, which Cargo
requires. So the version is computed in the workflow and passed to the build
scripts as `$VERSION`; the workspace version in `Cargo.toml` is only the fallback
for a local build, and is otherwise unused. The Linux binary gets it too, as
`BETTERAC_VERSION`, so `betterac --version` and the in-app updater both know
which build this is — see
`core/build.rs` for why Cargo has to be told that variable exists.

## Required repository secrets

Settings → Secrets and variables → Actions.

| Secret | What |
|---|---|
| `APPLE_CERT_P12` | base64 of the exported *Developer ID Application* certificate: `base64 -i cert.p12 \| pbcopy` |
| `APPLE_CERT_PASSWORD` | the password set when exporting that `.p12` |
| `APPLE_SIGN_IDENTITY` | e.g. `Developer ID Application: Your Name (ABCDE12345)` — `security find-identity -v -p codesigning` |
| `APPLE_ID` | Apple ID email used for notarization |
| `APPLE_APP_PASSWORD` | an **app-specific** password from appleid.apple.com, not your account password |
| `APPLE_TEAM_ID` | the 10-character team ID |

It must be a *Developer ID Application* certificate — neither "Apple Development"
nor "Mac App Distribution" can notarize for distribution outside the App Store —
and that requires a paid Apple Developer Program membership.

**Missing secrets do not break the release.** The macOS job builds an ad-hoc DMG
instead, and that DMG is left on the run page and never published: Gatekeeper
would refuse to open it, and the cask (which is skipped in that case) says as
much. Linux still releases normally. The workflow imports the certificate into a
throwaway keychain and deletes it in an `if: always()` step, so it never lands in
the runner's login keychain.

## The Homebrew tap is this repository

There is no separate `homebrew-betterac` repo. The cask lives at
`Casks/betterac.rb` here, and **CI writes it** — rendered from the DMG that was
just built, checksum and all, then committed to `main`. Nothing to copy by hand.

```sh
brew tap haivk/betterac https://github.com/haivk/betterAC
brew install --cask betterac
```

The URL is spelled out because Homebrew's one-argument form assumes a repo named
`homebrew-<name>`; the two-argument form "makes no assumptions" and takes any repo.
The cask *token* is plain `betterac` either way — that is the name users type, and
it was never tied to the repo name.

Two consequences worth knowing:

- The cask step is **skipped when the build is not signed**. There would be no
  published DMG for it to point at, and pointing it at one macOS refuses to open is
  worse than leaving the previous version in place.
- That commit pushes to `main`. It cannot cause a build loop: pushes made with
  `GITHUB_TOKEN` do not trigger workflows, and the trigger additionally ignores
  `Casks/**`.

This is a personal tap rather than homebrew-cask because upstream has notability
requirements (stars, forks, age) a new repo will not meet.

## Rehearsing locally

The macOS path runs end to end on any Mac. With no identity it uses an ad-hoc
signature and skips notarization, which still exercises the build, the universal
check, the DMG and the verification:

```sh
SIGN_IDENTITY=- NOTARIZE=0 ./packaging/macos/build-dmg.sh   # → dist/BetterAC-<ver>-universal.dmg
./packaging/render-metadata.sh dist                         # → dist/betterac.rb, dist/SHA256SUMS
ruby -c dist/betterac.rb
```

Both build scripts take `VERSION` from the environment, so a rehearsal can use the
same dated version CI would: `VERSION=2026.07.27.1 ./packaging/...`. Left unset they
fall back to the workspace version.

Gatekeeper is *expected* to reject an ad-hoc build; the script says so rather
than failing. It does fail if a notarized build is rejected, which is the case
that must never ship.

`linux/build-tarball.sh` needs a Linux box (gtk4 + libadwaita development files);
it cannot run on macOS at all.

### Testing the installer before a release exists

`install.sh` takes `BETTERAC_BASE_URL`, so point it at a directory holding the
artifacts and `SHA256SUMS` — it accepts `file://`. Put **both** artifacts there to
check that each platform picks the right one out of a shared `SHA256SUMS`:

```sh
# macOS — BETTERAC_APP_DIR keeps the test out of the real /Applications
BETTERAC_BASE_URL="file://$PWD/dist" BETTERAC_APP_DIR=/tmp/t/Applications \
  ./install.sh --force

# Linux — a throwaway HOME keeps it out of the real ~/.local
HOME=/tmp/t BETTERAC_BASE_URL="file:///path/to/dist" bash install.sh --force
```

`--force` is needed because the script refuses to run inside a checkout: being in
one almost always means you wanted to build your own code, not download a release.
