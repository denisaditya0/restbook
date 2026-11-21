# restbook — release and updates

How a build becomes a downloadable artifact, what the app checks for, and what
still has to be built. Scope and non-goals stay in
[`00-scope.md`](00-scope.md); per-OS prerequisites are in
[`06-cross-platform.md`](06-cross-platform.md).

**Status:** `.github/workflows/release.yml` builds and publishes a release for
the three targets below that fit the free-tier budget; the rest of the matrix
is still a specification. `scripts/package.sh` produces one artifact per OS
with its branding applied — a Windows installer, a macOS `.dmg`, a Linux `.deb`
plus a portable tarball. `crates/app/src/updater.rs` checks *and* installs:
Settings › Updates fetches the manifest, compares versions, and then downloads
the entry for this platform, verifies it against the digest the manifest
publishes, and hands it to the OS — on Windows that is the same Inno installer
a user would download, and the app quits so the exe can be replaced; macOS and
Linux open the package (`open`, `xdg-open`) and stay running. The manifest URL
is compiled in from the build's release channel, so a packaged app checks on
its own (R1).

A local release build is **not** possible on the development machine: a release
profile makes `gpui-pre-windows`' build script compile HLSL with `fxc.exe`, and
there is no Windows SDK there. `windows-2025` has it, which is one of the
reasons the workflow exists.

## Versioning

One number, three places that must agree:

| Place | Owns |
|---|---|
| `[workspace.package] version` in `Cargo.toml` | the engine crates |
| `[package] version` in `crates/app/Cargo.toml` | the app, which is a separate workspace |
| the git tag | the release |

`crates/app` is excluded from the root workspace on purpose (GPUI must stay out
of the engine build), so it carries its own version instead of inheriting
`workspace.package`. A bump touches both, plus `Cargo.lock` in each workspace.
`updater::CURRENT_VERSION` is `env!("CARGO_PKG_VERSION")`, which for the app
crate means the app's own number — so a stale app version makes the app
advertise the wrong build to itself.

Tags are the bare version: `0.2.0`. No `v` prefix, no build metadata, no
prerelease suffix, so the tag, the manifest, and `semver::Version` all agree.

## One channel

There is one release channel and one manifest, `latest.json`, attached to
whatever tag is newest. `verify` refuses a tag whose version carries a
prerelease suffix: the updater's only URL is `releases/latest`, which never
serves a prerelease, so a `0.2.0-beta.1` tag would publish something no client
could ever find — or quietly take the `latest` marker from a real release.

Versions are chosen by the size of the change, not by channel:

| Change | Version |
|---|---|
| Bug fix, doc correction, CI change | patch — `0.1.3` |
| A batch of user-visible features | minor — `0.2.0` |
| First release that promises compatibility, or a breaking change | major — `1.0.0` |

A beta channel used to exist for the cases where a bad release cannot be undone
for someone who already installed it — a `history.db` migration, an installer
layout change, signing. If one of those needs a shakedown, that is when to add
the channel back, with the mirror and the semver ordering described in this
file's history.

## Where the manifest URL comes from

`updater::manifest_url()` used to read `RESTBOOK_UPDATE_URL` at **runtime** with
`std::env::var` only, and a GUI app launched from Explorer or Finder has no
useful environment — a packaged build reported itself unconfigured and could
never learn about a later release. The URL is now baked in:

```rust
// crates/app/src/updater.rs — runtime env still wins, so a test or a developer
// can point one build anywhere; then the build-time env; then the channel.
pub(crate) fn manifest_url() -> Option<String> {
    runtime_override().or_else(build_override).or_else(release_url)
}

fn manifest_url_for(repo: &str) -> String {
    format!("https://github.com/{repo}/releases/latest/download/latest.json")
}
```

`RELEASE_REPO` defaults to `denisaditya0/restbook`; the release workflow sets
`RESTBOOK_UPDATE_REPO` to `github.repository`, so a fork's builds poll the fork.
Both `option_env!` variables are tracked by Cargo, so changing either rebuilds
the crate.

This has to ship in the **first** release. A `0.1.1` that checks for updates
cannot reach a `0.1.0` that does not, because the URL is compiled into the
binary the user already installed.

## Where the manifest lives

GitHub Releases hosts the binaries, and the manifest is just another asset — so
no server, no API token, no rate limit:

```
https://github.com/<owner>/restbook/releases/latest/download/latest.json
```

`releases/latest` resolves to the newest **non-prerelease** release, which is
exactly what the release job publishes (`gh release edit --latest`). The URL is
static: updating the channel means attaching a fresh `latest.json`, and no client
ever calls `api.github.com` (60 unauthenticated requests per hour).

## The manifest contract

The shape is fixed by `updater::Manifest`; extra keys are ignored, so the
manifest can grow without breaking old builds.

```json
{
  "version": "0.2.0",
  "notes": "Import dialog, faster history, smaller binary.",
  "assets": {
    "windows-x86_64": {
      "url": "https://github.com/owner/restbook/releases/download/0.2.0/restbook-0.2.0-windows-x86_64.exe",
      "sha256": "…",
      "signature": "…"
    },
    "windows-aarch64": { "…": "…" },
    "macos-aarch64": { "…": "…" },
    "macos-x86_64": { "…": "…" },
    "linux-x86_64": { "…": "…" },
    "linux-aarch64": { "…": "…" }
  }
}
```

Keys are `updater::platform_key()`, i.e. `std::env::consts::OS` and `ARCH`:
`windows`, `macos`, `linux`; `x86_64`, `aarch64`. Deriving them instead of
hand-writing a table is what stops a new target from silently requesting the
wrong asset. `sha256` and `signature` are already parsed and currently unused;
the installer phase consumes them before writing anything, and an entry may
omit them, so a channel can be published before signing exists.

## Artifact matrix

One native runner per target. Nothing is cross-compiled: GPUI needs the target's
X11/Wayland/fontconfig development packages, and Apple's SDK and `codesign` only
exist on macOS.

**Wired today (3 of 6).** The workflow ships the entries marked ✅; the other
two are one matrix row each when their user base justifies the extra billed
minutes (macOS is 10×, Windows 2×, Linux 1×).

| Job | Runner | Target | Artifact |
|---|---|---|---|
| Windows x86_64 ✅ | `windows-2025` | `x86_64-pc-windows-msvc` | `restbook-<v>-windows-x86_64.exe` |
| Windows ARM64 | `windows-11-arm` | `aarch64-pc-windows-msvc` | `restbook-<v>-windows-aarch64.exe` |
| macOS ARM64 ✅ | `macos-26` | `aarch64-apple-darwin` | `restbook-<v>-macos-aarch64.dmg` |
| macOS x86_64 | `macos-26-intel` | `x86_64-apple-darwin` | `restbook-<v>-macos-x86_64.dmg` |
| Linux x86_64 ✅ | `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | `restbook-<v>-linux-x86_64.deb` |
| Linux ARM64 | `ubuntu-24.04-arm` | `aarch64-unknown-linux-gnu` | `restbook-<v>-linux-aarch64.deb` |

Linux also ships `restbook-<v>-linux-<arch>-portable.tar.gz`, the same prefix
tree without a package manager in front of it. It is not in the manifest, which
holds one URL per platform key, so it is renamed `-portable` to keep its key
(`linux-x86_64-portable`) from overwriting the `.deb`'s (`linux-x86_64`).

One interim deviation from the table above:

- **The Linux artifact is built `--no-default-features`.** `scripts/package.sh`
  does that on Linux, which means plaintext JSONL rather than SQLCipher there.
  Change it when a Linux build with the encrypted store is worth the extra
  minutes (and the `libssl-dev` dependency).

The Windows `.exe` is Inno Setup's output, built by `scripts/package.sh` from
`crates/app/packaging/restbook.iss`. It is built with `--vendored`, so OpenSSL
is linked into the exe and the installer carries that single file — a DLL only
joins it when a build links OpenSSL dynamically, which local builds still do.
The macOS `.dmg` and the Linux `.deb` come from the same script, over the
same bundle / prefix tree the bare artifacts use, so the layout a user installs
cannot drift from the one the tarball carries.

Runner labels move with GitHub's images — the labels above are the ones a
working pipeline used, not a commitment. What matters is the indirection: one
native runner per target, never a cross-compile.

Two constraints decide the runners:

- **Linux x86_64 builds on the oldest image we support.** glibc symbols are
  versioned at link time: a binary built on 24.04 (glibc 2.39) fails on 22.04
  (2.35) with `GLIBC_2.39 not found`. Building on 22.04 keeps the floor low.
- **`.exe` means the installer, not the binary.** Windows ships an Inno Setup
  package (app + icon, Start-menu and optional desktop shortcut with an
  `AppUserModelID` matching the one `branding::APP_ID` and the build resource
  carry). A bare exe also works, but it gets a generic taskbar identity. Note
  that the icon and version block in the exe come from `crates/app/build.rs`,
  which needs the MSVC toolchain: the GNU `ld` drops the resource section. The
  matrix below builds `windows-msvc` already.

`RUSTUP_TOOLCHAIN: stable` must be set for the whole workflow: both
`rust-toolchain.toml` files pin `stable-x86_64-pc-windows-gnu`, which does not
exist on the Linux and macOS runners.

CI builds the app crate on Linux with `--no-default-features`: SQLCipher costs
several minutes of C, and no CI job opens a window. macOS keeps the default,
because CommonCrypto needs no OpenSSL there. The encrypted path gets its own
`ci.yml` job instead (`--features sqlcipher`, over a D-Bus session for the
keyring), so a pull request cannot merge with `SqlcipherStore` uncompiled —
but the release matrix still builds no artifact with it on Linux.

## Pull requests

The gate before a change reaches `master`, and therefore before it can be tagged
for release. `.github/workflows/ci.yml` runs on every pull request, on pushes to
`master`, and by hand. It is Linux only: the release matrix already builds the
three shipped targets natively, while a pull request needs a verdict, not
packaging.

| Job | Proves |
|---|---|
| `lint` | `cargo fmt --all --check` in **both** workspaces, and that the version did not move |
| `clippy` | `--all-targets -D warnings` over the engine and the app (`--no-default-features`) |
| `test` | `cargo test --workspace`, then the app crate's tests |
| `sqlcipher` | `restbook-store --features sqlcipher`, over a D-Bus session for the keyring |

`sqlcipher` is not redundant with `test`: SQLCipher is off everywhere else in
CI for its build cost, so without this job nothing compiles `SqlcipherStore`
until a release tag does. That is the encrypted history path — the one where a
bug corrupts data rather than merely annoying someone.

Two rules the workflow enforces that are easy to miss in a hand review:

- **A version bump is rejected in a pull request.** `release.yml` derives the
  channel from the version's prerelease suffix and refuses a tag that does not
  match `crates/app/Cargo.toml`, so moving the version decides the next
  release's identity. The check only fires when a branch actually moves the
  version, so a tree whose versions sit ahead of its newest tag stays green.
  Bump both files in the release commit instead — see [Flow](#flow).
- **Formatting runs per workspace.** `crates/app` is excluded from the root
  workspace, so a root `cargo fmt --all` never sees the shell's files.

What CI deliberately cannot check is whether a pane *looks* right: it has no
display and no human. A change to a pane still needs before/after captures from
the developer preview, attached to the pull request:

```sh
cargo run -p restbook-app -- --preview all       # every pane in one window
cargo run -p restbook-app -- --preview response  # the one that changed
```

Branch protection is what turns this into a gate rather than advice: `lint`,
`clippy`, `test` and `sqlcipher` are meant to be required on `master`, with
direct pushes disallowed.

## Flow

What `.github/workflows/release.yml` does, in order:

1. **Bump both versions, commit, tag, push the tag.** The tag must equal the app
   version or the workflow fails rather than shipping a mislabeled build. The
   bump is its own commit on `master`, after the pull requests it covers are
   merged; `ci.yml` rejects one inside a pull request.

   The tag is pushed **after** that commit's `ci.yml` run finishes green, not
   alongside the bump: step 3 reads the check runs for the tag's SHA, and a tag
   pushed early fails with "never ran for `<sha>`" — which is how a good tree
   turns into a release that had to be re-tagged. `scripts/release.sh` does the
   bump and prints the tag command; it never pushes one.
2. **Verify** on a Linux runner: tag/version equality for both workspaces, and
   ancestry on the default branch.
3. **Refuse a commit CI has not passed.** The verdict comes from `ci.yml`'s
   check runs for the tag's SHA, read through the API: a missing result or any
   job that is not `completed / success` fails the release. Re-running the suite
   here instead cost 21 minutes per release for answers CI had already produced,
   and it meant tagging *after* CI is green rather than racing it.
4. **Build the three artifacts** in parallel on native runners, each uploading
   an Actions artifact. No job touches the release yet, so a failed matrix
   leaves nothing on GitHub.
5. **Assemble and publish** in one job: create the release as a **draft**, write
   `latest.json` from the downloaded bytes' own checksums, upload every asset,
   then un-draft and mark it `latest`.

A re-run resumes rather than duplicates: the release is created only if the tag
has none, and uploads use `--clobber`, so replacing one failed artifact does not
touch the others.

Triggers:

```sh
git tag 0.1.0 && git push origin 0.1.0      # a release
```

`workflow_dispatch` takes the same tag plus a `dry_run` flag, which builds and
tests without creating anything — the way to check the matrix after a runner
label moves.

## What the app does with it

- On launch, if `settings.check_updates` is true and a manifest URL is
  configured, one background check runs and nothing else — an available version
  becomes a flash message pointing at Settings.
- Settings › Updates always checks on demand, shows the compiled version, and
  names the manifest URL it reads. It renders its own state inline — one button
  whose meaning follows it (Check now, Download & install, busy), and one sentence
  in the tone the state deserves.
- `Outcome::Available` with no asset for this platform is still reported as an
  update, with the platform named: "newer exists, but nothing to install here".

## Milestones (R1..)

`R` items are the distance between the tree and a downloadable release. None of
them is on disk; each one is independent of the UI/engine milestone tables.

| # | Deliverable | State |
|---|---|---|
| R1 | Baked-in manifest URL | **Done.** `updater::manifest_url()` derives the channel URL from the compiled version (`RELEASE_REPO`, overridable with `RESTBOOK_UPDATE_REPO`) and bakes it in, so a packaged build checks without any environment; `RESTBOOK_UPDATE_URL` still overrides it at build or run time |
| R2 | CI matrix | **3 of 6 done**: `.github/workflows/release.yml` builds Windows x86_64, macOS arm64 and Linux x86_64 on native runners, verifies tag/version/ancestry, runs the test suites, then publishes a draft→release with a derived channel manifest. The ARM64 and macOS Intel rows remain |
| R3 | Installers | **Done on all three platforms, at different depths.** Windows: `scripts/package.sh` compiles `crates/app/packaging/restbook.iss` with Inno Setup when `ISCC.exe` is installed (the release workflow does), producing a per-user `.exe` with a Start-menu entry and an uninstaller. macOS: a `.dmg` with an `Applications` symlink (drag to install; no uninstaller beyond dragging the `.app` out). Linux: a `.deb` for `apt`, plus the portable tarball. Only the Windows package installs itself with a wizard |
| R4 | Signing and notarization | Nothing signed. `sha256`/`signature` are already parsed from the manifest and unused, so the contract does not change when this lands |
| R5 | Download and install | **Done.** Settings › Updates downloads this platform's asset, verifies it against the manifest's `sha256`, and hands it to the OS: Windows runs the installer and quits so the exe can be replaced; macOS and Linux open the package and stay running. `signature` is still parsed and unused (R4) |

Until R2-R3 exist for a given target, a release is a **local artifact**, not a channel: build it
with `sh scripts/package.sh --release` and hand it to whoever needs it. Note
that the pinned Windows GNU toolchain drops the `.rsrc` section (see
`crates/app/build.rs`), so such an artifact carries neither the icon nor the
version block; only the MSVC build in the R2 matrix has them.

## Not done yet

- No notarized or signed install: Windows SmartScreen warns, and an unsigned
  macOS build needs right-click → Open the first time. macOS also has no
  uninstaller — the `.app` is dragged to the Trash, and the history in
  `~/Library/Application Support/restbook` stays behind (as it does on the other
  platforms).
- The `.deb` has no repository behind it (`apt install ./*.deb`, not
  `apt install restbook`), and the Linux build still omits SQLCipher, so its
  history is plaintext.
- No AppImage, `.rpm`, or Flatpak: Debian-based distros are the only ones with a
  packaged install today.
- No download/install phase on a platform whose manifest entry is missing: the
  app says a newer version exists but has nothing to install here.
- No delta updates, no mirrors, no self-hosted fallback.
- Only three of six targets are built; the workflow does not yet produce the
  two ARM64 artifacts or the macOS Intel one.
