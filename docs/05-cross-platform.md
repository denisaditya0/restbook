# restbook — developing on another OS

The engine is plain Rust. The desktop shell adds GPUI, and two things are
genuinely OS-specific: the C crypto behind SQLCipher, and where the master key
can be stored. Everything else is `PathBuf` and `cfg!`.

**Status:** Windows (GNU toolchain) is the only OS this has been built and run
on. The other sections describe what the code already does and what has to be
installed; treat them as instructions, not as verified recipes.

## What is OS-specific

| Concern | Where | Notes |
|---|---|---|
| Data directory | `crates/app/src/workspace/storage.rs` | `%APPDATA%`, `~/Library/Application Support`, `$XDG_DATA_HOME` (falls back to `~/.local/share`); `RESTBOOK_DATA_DIR` wins. |
| Master key | `crates/store/Cargo.toml` + `sqlcipher.rs` | Per-target `keyring` backend — see below. |
| SQLCipher crypto | `libsqlite3-sys` build script | OpenSSL on Windows/Linux, CommonCrypto on macOS. |
| Window system | GPUI | X11/Wayland on Linux, Cocoa on macOS. Untested here. |
| Branding | `crates/app/src/branding.rs` + `scripts/package.sh` | One identity, three OS surfaces — see [Branding per OS](#branding-per-os). |
| Packaging | `scripts/package.sh` + `crates/app/packaging/` | One artifact per OS: an Inno Setup installer on Windows, a `.dmg` on macOS, a `.deb` (plus a portable tarball) on Linux. |

## Toolchain

Both `rust-toolchain.toml` files pin `stable-x86_64-pc-windows-gnu`, because the
machine this was built on has no MSVC Build Tools and GPUI links through MinGW's
`ld`. That pin is wrong anywhere else. A directory override beats a toolchain
file, so you do not have to edit anything:

```sh
rustup override set stable        # repo root; covers crates/app too
rustup toolchain list             # sanity check
```

The override is stored in `~/.rustup/settings.toml`, not in the repository.

## Contributing from macOS or Linux

The repository is two workspaces — the engine at the root, the app in
`crates/app` — and only the engine builds without GPUI. Working on the app
elsewhere is a short, fixed sequence.

1. **Override the toolchain** (see [Toolchain](#toolchain)). The pins name a
   Windows GNU toolchain that is not installed anywhere else; one override at
   the repository root covers `crates/app` as well.

2. **Install the C prerequisites** for your OS (below). On macOS, do not set
   `OPENSSL_DIR` — it switches SQLCipher from CommonCrypto to Homebrew OpenSSL
   for no gain.

3. **The engine first.** It needs no window, no GPU and no keyring backend:

   ```sh
   cargo test
   cargo clippy --all-targets --message-format short
   ```

4. **Then the app**, which is its own workspace, so the manifest matters:

   ```sh
   cd crates/app
   cargo run                          # encrypted history
   cargo run --no-default-features    # plaintext JSONL history
   ```

   `scripts/dev.sh` is a Windows convenience: it exists to find OpenSSL for
   `libsqlite3-sys`. Elsewhere plain `cargo run` is the shortest path.

5. **Expect one keyring prompt.** macOS asks once per code signature; Linux
   needs a running Secret Service. Without one the app falls back to plaintext
   JSONL and says so on stderr rather than failing to start. To exercise the
   encrypted path headlessly:

   ```sh
   dbus-run-session -- cargo test -p restbook-store --features sqlcipher
   ```

6. **Before sending a patch:** `cargo fmt --all -- --check` in both workspaces,
   then the checklist at the end of this file.

The `cfg` surface is deliberately small: the keyring backend, the data
directory, and the packaging scripts. Branding is *not* a `cfg` — the identity is
the same string on every OS and each platform picks up the part it recognises;
see [Branding per OS](#branding-per-os). If a change needs a third `#[cfg]` for
the UI, that is worth asking about first.

## Continuous integration

There is no `.github/workflows/` directory yet. The intended shape is two
workflows, and both are specified in
[`09-release-and-updates.md`](09-release-and-updates.md):

- **pull request** — `windows-2025`, `ubuntu-22.04`, `macos-26`: engine tests,
  clippy and `fmt --check`, plus a `cargo check` of the app crate (default
  features on Windows and macOS, `--no-default-features` on Linux to keep the
  job short).
- **tag** — the six-target packaging matrix, then the draft → upload → publish
  flow that writes the channel manifest.

Both must set `RUSTUP_TOOLCHAIN: stable`, because the toolchain pins in this
repository name a Windows-only toolchain that the Linux and macOS runners do
not have.

## Prerequisites

### Linux

```sh
sudo apt install build-essential pkg-config libssl-dev libdbus-1-dev
```

- `libssl-dev` — SQLCipher's C build (`crypto` is linked from the default search
  path on Linux, so **no `OPENSSL_DIR` is needed**).
- `libdbus-1-dev` — the Secret Service keyring backend.
- GPUI additionally needs X11/Wayland and fontconfig development packages. If
  the build fails on `x11`, `wayland` or `fontconfig` crates, install the
  matching `-dev` packages; the set is the same one Zed documents for Linux.

### macOS

```sh
xcode-select --install            # clang, make, perl
```

No OpenSSL is needed: `libsqlite3-sys` uses `SQLCIPHER_CRYPTO_CC` (CommonCrypto,
linking `Security`/`CoreFoundation`) whenever `OPENSSL_DIR` is **not** set. Do
not set it — setting it switches the build to OpenSSL and adds a Homebrew
dependency for nothing.

### Windows

See the README's *Windows setup* section. The `dev` scripts exist mainly for
this platform, where `libsqlite3-sys` panics with "Missing environment variable
OPENSSL_DIR" instead of falling back. `dev.sh` is for a POSIX shell (Git Bash,
MSYS2); `dev.ps1` and `dev.cmd` are the native PowerShell and cmd.exe twins.

## Build and test

```sh
cargo test                        # engine, no GPUI, no C except sqlcipher
cargo clippy --all-targets --message-format short

cd crates/app
sh ../../scripts/dev.sh run       # or dev.ps1 / dev.cmd on native Windows
```

`scripts/dev.sh` is a convenience on Windows and a no-op-ish wrapper elsewhere:
it detects an OpenSSL prefix and prepends its `bin` to `PATH`. `dev.ps1` and
`dev.cmd` do the same without needing `sh`. None is needed at runtime — the
built app does not call them.

Dropping encryption entirely works on every OS and is the fastest way to get the
UI building on a new machine:

```sh
cd crates/app
cargo run --no-default-features      # any OS: no dev script needed
sh ../../scripts/dev.sh run --no-default-features   # Windows, with OPENSSL_DIR detection
```

## The keyring

`keyring` 3 has **no default backend**; without one it compiles an in-memory
store and the key changes every launch, which makes the database unreadable on
the next start. `crates/store/Cargo.toml` therefore enables one per target:

| OS | Feature | Store |
|---|---|---|
| Windows | `windows-native` | Credential Manager, `restbook` / `master-key` |
| macOS | `apple-native` | Keychain; first access prompts once per signature |
| Linux | `sync-secret-service`, `crypto-rust` | Secret Service via D-Bus |

Headless Linux (containers, CI, bare WMs) has no Secret Service. The app then
falls back to plaintext JSONL and says so on stderr — it does not fail to start,
and it does not pretend the data is encrypted. For tests that need the encrypted
path there, run under a session bus:

```sh
dbus-run-session -- cargo test -p restbook-store --features sqlcipher
```

There is no key override environment variable yet, so a CI that cannot run a
Secret Service should build with `--no-default-features` and test the SQLCipher
path on a developer machine.

The key is as important as the file: losing the credential entry means
`history.db` can never be opened again. Do not move a database between machines
and expect it to work.

## Data directory

| OS | Path |
|---|---|
| Windows | `%APPDATA%\restbook` |
| macOS | `~/Library/Application Support/restbook` |
| Linux | `$XDG_DATA_HOME/restbook`, else `~/.local/share/restbook` |

An old working-directory `.restbook/history.jsonl` is imported once on first
start and renamed to `*.jsonl.imported`.

## Packaging per OS

`scripts/package.sh` dispatches on the host OS and applies that platform's
branding, which is a different shape in each case because each OS reads branding
from a different place:

- **Windows** — a bare exe plus the DLLs it imports, and, when Inno Setup's
  `ISCC.exe` is on the machine, one installer compiled from
  `packaging/restbook.iss` that carries both. The icon and the version
  block are compiled *into* the binary by `crates/app/build.rs`, so there is
  nothing to copy beside it except `libcrypto-3-x64.dll` (the import table
  references it, and the loader searches the exe's directory first). **Omit the
  icon unless the build used the MSVC toolchain** — see [The one trap](#the-one-trap-the-gnu-linker-drops-resources).
  The installer is per-user (`PrivilegesRequired=lowest`, so no UAC prompt), puts
  a Start-menu entry and an uninstaller in place, and stamps the shortcut with
  `AppUserModelID` = `branding::APP_ID`; without that string a pinned taskbar
  button and the running window resolve to different identities.
- **macOS** — a `.app` bundle: the binary in `Contents/MacOS`, and
  `Info.plist` plus `Contents/Resources/restbook.icns` rendered from
  `packaging/Info.plist`. No crypto library is needed when the build used
  CommonCrypto; if `OPENSSL_DIR` was set, Homebrew's dylib has to be copied in
  and its install name rewritten with `install_name_tool`. `package.sh` codesigns
  ad-hoc, which is enough to run locally but needs right-click → Open on a
  downloaded copy. On a Mac it also wraps the bundle in a `.dmg` with an
  `Applications` symlink beside it, which is the drag-to-install idiom; a plain
  image of the bundle alone reads as an archive of a folder.
- **Linux** — a tarball laid out as an install prefix: the binary under `bin/`,
  and under `share/` a `.desktop` entry and the hicolor icon set. Both are named
  for `branding::APP_ID`, which is also the window's `app_id`; that agreement is
  what gives the app its icon in a dock and groups its window in a taskbar. Build
  with `--no-default-features` unless the target has OpenSSL, since SQLCipher
  links `crypto` dynamically there too. Where `dpkg-deb` exists, the same tree is
  also packaged as a `.deb` — its `Architecture:` field spells the arch dpkg's
  way (`amd64`), while the file name keeps the release spelling (`x86_64`) like
  every other artifact.

Only Windows has a true installer (one wizard, Start-menu entry, uninstaller).
macOS ships a drag-to-`/Applications` `.dmg`, and Linux ships both a `.deb` for
`apt` and a tarball to unpack by hand. Nothing is signed or notarized. The
artifact matrix, naming, and update manifest that these steps feed are in
[`09-release-and-updates.md`](09-release-and-updates.md).

No platform sets a window icon at compile time: on macOS the Dock icon is bundle
metadata, and on Linux the icon is installed next to the desktop entry
(`WindowOptions.icon` exists but is X11-only).

## Branding per OS

One identity, declared once in `crates/app/src/branding.rs`, and three OS
surfaces that each read only the part they understand. There is no `#[cfg]` in
the identity itself: a name that differed between builds of the same version
would defeat the point of an identifier.

| Surface | Owner | What the OS does with it |
|---|---|---|
| Window title | `branding::WINDOW_TITLE` via `main`'s `TitlebarOptions` | Taskbar hover text, the window list. |
| `app_id` | `branding::APP_ID` via `WindowOptions.app_id` | Linux: the window's `app_id`, matched against the `.desktop` file and the icon. Ignored elsewhere. |
| Version block + icon | `crates/app/build.rs` (`winres`) | Windows only: the resource section the taskbar, Explorer and Alt-Tab read. |
| Bundle metadata | `crates/app/packaging/Info.plist` | macOS only: `CFBundleName` is the Dock label and app-menu title, `CFBundleIdentifier` is what the Keychain prompt keys on, `CFBundleIconFile` names the `.icns`. |
| Icon theme + entry | `crates/app/packaging/restbook.desktop`, hicolor PNGs | Linux only: `share/applications/<app_id>.desktop` and `share/icons/hicolor/<n>x<n>/apps/<app_id>.png`, both named for the `app_id`. |

`crates/app/icons/restbook.svg` is the single icon source;
`scripts/icons.sh` rasterizes it to the PNG set, the `.ico` and (on macOS) the
`.icns`. On a machine with no librsvg, `scripts/generate-icons.py` renders the
same SVG with Pillow.

### The one trap: the GNU linker drops resources

`winres` emits a COFF object with a `.rsrc` section, and Rust puts the app
manifest in the same section. The MSVC linker merges both contributions; the
GNU `ld` keeps one and **silently discards the rest**, so a build with the
`stable-x86_64-pc-windows-gnu` toolchain — the one the pins in this repository
name — links with a manifest and no icon or version block. `build.rs` detects
this and warns rather than leaving an unexplained generic icon. Build with
`--target x86_64-pc-windows-msvc` to get the icon, which is what the artifact
matrix in [`09-release-and-updates.md`](09-release-and-updates.md) already uses.

### Beyond the icon

Presentation branding is not in this module and does not need to be per-OS: the
palette is `crates/app/src/theme.rs` and the in-window mark is the workspace chip
in `crates/app/src/workspace/chrome.rs`. Neither changes by platform, which is
the intended shape — only the OS's own labels and images differ.

## Portability traps

- `env!("CARGO_MANIFEST_DIR")` is baked into `storage.rs` for the legacy
  import. It is a development convenience: on a machine that does not have that
  directory the path simply does not exist and is skipped.
- JSONL is written with `writeln!`, so lines end in `\n` on every OS; nothing
  reads `\r\n`-dependent.
- Paths are always `Path`/`PathBuf`. Do not concatenate with `/`.
- File names (`history.db`, `layout.json`) differ in case only where they do not
  collide; keep it that way for case-insensitive filesystems.
- `SqlcipherStore` keeps one `Mutex<Connection>`; SQLite's locking handles a
  second app instance. Do not add a lock file.

## Checklist for a new OS

1. `cargo test` at the root — engine, no window.
2. `cd crates/app && sh ../../scripts/dev.sh run` — window opens, no stderr.
3. Run it a **second** time. No sqlcipher errors: that is the key-stability check.
4. `%APPDATA%`-equivalent contains `history.db` and nothing named `*.unreadable`.
5. Send a request, click its history row, re-send it.
6. `cargo run --no-default-features` — JSONL fallback still works.
7. `cargo fmt --check`, `clippy`.

## Known gaps

- Only Windows has been run here. `.github/workflows/release.yml` builds
  Windows x86_64, macOS arm64 and Linux x86_64 on tags, so the other two get
  built (and packaged) for the first time there; the ARM64 rows in
  [`09-release-and-updates.md`](09-release-and-updates.md) are still unwired.
- `package.sh` builds an installer on Windows (`.exe`), a drag-to-install image
  on macOS (`.dmg`) and a package on Linux (`.deb`), but only the Windows path
  has been run here; the other two are syntax-checked, not executed. Nothing is
  signed on any platform.
- The Windows icon and version block need the MSVC toolchain; the pins in this
  repository name GNU, where the linker drops them (see the note above).
- No key override for headless CI.
- The toolchain pin has to be overridden off Windows; the release workflow does
  that with `RUSTUP_TOOLCHAIN=stable` instead of editing the files.
