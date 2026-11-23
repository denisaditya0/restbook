# restbook

A local-first HTTP client for the desktop: Postman's concepts, no account, no
cloud, and every request you send saved to an encrypted local history.

Status: **engine + working request editor**. The HTTP engine, request
preparation, and history stores are in place and exercised by `devcli`. The GPUI
desktop app sends requests with params, headers, bodies, and auth, shows
responses end to end, and replays a history row into the editor; environments
and collections are still to come.

## Layout

```
crates/core       domain model, {{var}} templating, request preparation, traits
crates/transport  reqwest adapter (owns its own tokio runtime)
crates/store      history stores: JSONL (fallback), SQLCipher (feature = "sqlcipher")
crates/devcli     command-line harness for the engine
crates/app        GPUI desktop shell (excluded from the workspace)
scripts/dev.sh    runs cargo with the local toolchain's env detected
scripts/dev.ps1   same, for PowerShell
scripts/dev.cmd   same, for cmd.exe
scripts/package.sh  builds dist/ per OS, applying that platform's branding
scripts/icons.sh    rasterizes crates/app/icons/restbook.svg for each platform
```

`core` has no UI dependency and no runtime ownership. That is deliberate: the
UI can be replaced, and the engine can be tested and driven headlessly.

## Quick start

```sh
cargo build
cargo run -p restbook-devcli -- send https://httpbin.org/get
cargo run -p restbook-devcli -- send https://httpbin.org/post -X POST --json '{"hello":"world"}'
cargo run -p restbook-devcli -- send https://httpbin.org/bearer --var TOKEN=abc \
  -H 'Authorization:Bearer {{TOKEN}}'
cargo run -p restbook-devcli -- send https://httpbin.org/bytes/1048576 --progress
cargo run -p restbook-devcli -- history httpbin
```

`--progress` sends through `Transport::execute_with_progress` and prints live
sent/received byte counts to stderr, which is how the progress path is exercised
without a window.

History for the dev harness is stored as plain JSONL in `.restbook/history.jsonl`
relative to the working directory. That is a development convenience, not the
product storage path.

## Desktop shell

```sh
cd crates/app
sh ../../scripts/dev.sh run
```

On Windows without a POSIX shell, use the native twins from the same directory:

```powershell
cd crates\app
..\..\scripts\dev.ps1 run
```

```bat
cd crates\app
..\..\scripts\dev.cmd run
```

All three find this machine's OpenSSL dev tree (needed by SQLCipher, which is on
by default) and then run whatever cargo command you pass them. From the
repository root they take a package too:

```sh
sh scripts/dev.sh test -p restbook-store --features sqlcipher
```

To hand someone an installable copy:

```sh
sh scripts/package.sh            # or --release
```

What `dist/` holds depends on the machine you run it on:

- **Windows** — `restbook.exe` and, unless the build asked for a static
  OpenSSL (`--vendored`), the `libcrypto-<major>-x64.dll` it imports. If
  [Inno Setup](https://jrsoftware.org/isinfo.php) is installed,
  `restbook-<version>-windows-x86_64.exe` as well: one installer carrying both,
  with a Start-menu entry and an uninstaller.
- **macOS** — `restbook.app`, and `restbook-<version>-macos-<arch>.dmg` around it
  with an `Applications` symlink, so the image installs by dragging.
- **Linux** — a prefix tree plus `restbook-<version>-linux-<arch>.tar.gz`, and
  `restbook-<version>-linux-<arch>.deb` where `dpkg-deb` exists.

The `dev`/`package` scripts are build-time helpers: the built app never needs
them, and its history lives in the data directory, not in `dist/`.

The window has three panes: history on the left, the request editor above the
response viewer on the right.

- Pick a method from the dropdown, type a URL, press **Send** (or
  **Ctrl+Enter** anywhere in the request pane). `prepare()` runs before any I/O,
  so an invalid URL or an unknown `{{variable}}` is reported inline and nothing
  is sent.
- The editor below the URL bar has **Params**, **Headers**, **Body**, and
  **Auth** tabs with per-tab counts, all writing into one `RequestDef`:
  - Params and headers are key/value tables with per-row enable/disable. The
    Headers tab also has a collapsed **Default headers** section listing the
    headers the client adds on every send (`Content-Type`, `Accept`,
    `Accept-Encoding`, `User-Agent`); adding a row with the same name overrides
    one.
  - Body is none, raw (JSON/XML/text/HTML language presets), or
    form-urlencoded. The raw language picks the `Content-Type`; it is not a
    separate header to edit.
  - Auth is none, basic, bearer, or an API key (header or query string).
  Only what is on screen is sent: the editor state is folded into the
  `RequestDef` immediately before every send.
- A second request can be cancelled while it is in flight.
- **Ctrl+Shift+V** (Cmd+Shift+V on macOS) imports a cURL command from the
  clipboard, and **Ctrl+Shift+C** copies the current request back out as one.
  See [Keyboard shortcuts](docs/12-keyboard-shortcuts.md) for the full table.
- Successful responses are recorded and appear in the sidebar. Clicking a row
  restores the **whole request** — params, headers, body, and auth — so it can be
  re-sent as recorded, and shows the stored response. See
  [Storage](#storage) for where that lives and how it is protected.
- JSON bodies are pretty-printed. The response pane shows status, time, and
  size over **Body / Headers / Cookies** tabs; the format control names what
  pretty-printing means for this payload (JSON, HTML, XML, Text) and switches to
  the raw body. Display is capped at 256 KB; the full bytes stay in the response
  record.
- Panes are regions, not tabs: history is the left sidebar, the request editor
  sits above the response in a resizable split. The separator widths are saved
  to `layout.json` next to the history.

The app crate is a separate workspace so the engine build never depends on GPUI.
It is split by responsibility: `workspace` owns the engine handles, the dock,
and persistence and is the only thing that sends; `panels` holds one self-contained
module per pane (history, request, response), each an entity that reports intents
as events instead of knowing its siblings; `panels/editor` holds the four tab
bodies; `document` is the one request/response value the panes observe; and
`theme`/`util` are shared leaves.
It owns one `ReqwestTransport` for the process lifetime, which is what keeps the
tokio runtime and the four pooled clients from being rebuilt per request.

## Storage

Two stores sit behind one `Store` trait (`crates/core/src/store.rs`), and
`crates/app/src/workspace/storage.rs` picks between them once at startup.

### Two storage modes

The choice is made when the window opens, and it is visible in the status bar as
a chip: either the workspace path, or `local database`.

- **Workspace** — a directory holding a `restbook.json` marker. It is found by:
  1. `RESTBOOK_WORKSPACE`, which names a root explicitly and is authoritative
     (the marker must be present; a named directory without one is refused);
  2. otherwise the current directory is walked upward, the way `.git` is found,
     so launching the app from anywhere inside a workspace opens it.
- **Application data directory** — the per-user SQLCipher database described
  below. This is what runs when no workspace is found.

Workspace wins when both are available, because a marker file in the tree you
launched from is an explicit invitation; silently mixing a shared checkout with
private local data would be the worse failure. Layout (`layout.json`) and
preferences (`settings.json`) always live in the data directory: they are UI
preferences, not workspace content.

### Where the files are (data-directory mode)

```
%APPDATA%\restbook\                        <- Windows
~/Library/Application Support/restbook/     <- macOS
$XDG_DATA_HOME/restbook/                    <- Linux
  history.db            encrypted: SQLite + SQLCipher (history, collections,
                        environments, globals, drafts)
  history.jsonl         plaintext fallback, present only when SQLCipher was
                        unavailable
  layout.json           dock arrangement (panel names and widths, no secrets)
  settings.json         app preferences (pretty-print, history search limit,
                        history retention cap)
```

Set `RESTBOOK_DATA_DIR` to move that directory — that is how a portable or test
build gets its own sandbox.

**Everything above is one file per concern, encrypted as a whole.** Collections,
environments, globals, and drafts are rows in the same `history.db`, not separate
files. Sharing in this mode is not possible; that is what a workspace is for —
see [Sharing a workspace](#sharing-a-workspace).

### How encryption works

- A **256-bit master key** is generated on first launch and written to the OS
  credential store — Windows Credential Manager, macOS Keychain, or Secret
  Service — as `restbook` / `master-key`.
- The connection applies it before touching anything else:
  `PRAGMA key = "x'...'"`, then `PRAGMA cipher_memory_security = ON`. The
  blob-literal form is what makes arbitrary key bytes safe to embed.
- **The key is never written to disk** next to the database. That is the whole
  point: copying `history.db` to another machine does not make it readable,
  because the key stayed behind in the keyring.
- SQLCipher encrypts pages, so the file is opaque — there is no plaintext on
  disk, and the schema, the request bodies, and the response bodies are all
  inside the encrypted page set.

### When decryption or the keyring fails

The app **never overwrites a database it cannot read**, and it never silently
loses history. But it does fall back to plaintext, so the failure is announced on
stderr rather than passing unnoticed. The paths:

| What fails | What happens |
|---|---|
| Keyring has no entry | A new key is generated and stored. A fresh `history.db` is written encrypted. |
| Keyring is unavailable (no backend, locked, denied) | `SqlcipherStore::open` returns an error. The app prints `restbook: encrypted history unavailable (<error>); using plaintext JSONL` and uses `history.jsonl`. |
| `history.db` exists but the key no longer matches | The `PRAGMA key` or the first schema statement fails, so `open` returns an error, and the same plaintext fallback applies. The unreadable database is left alone, never rewritten. |
| `history.db` is corrupt or truncated | Same as above: error, fallback, file preserved for recovery. |
| SQLCipher compiled out (`--no-default-features`) | No attempt is made; plaintext `history.jsonl` in the same directory. |
| The data directory cannot be created | The app starts against an in-memory store, so it opens read-only-ish instead of failing to launch. |

Two consequences worth stating plainly:

- **Losing the keyring entry loses the history.** There is no recovery path and
  no backdoor, because there is no copy of the key. Deleting the credential
  manager entry makes `history.db` permanently unreadable, and the app simply
  falls back to a new plaintext `history.jsonl`.
- **The fallback is plaintext.** A failing keyring means history — including
  every response body — lands in `history.jsonl` unencrypted. That is a
  deliberate choice to keep the app usable, which is why the warning exists.
  A `history.jsonl` in the data directory is the signal that encryption is not
  currently protecting you; its absence means `history.db` is in use.

`crates/store/src/sqlcipher.rs` has a roundtrip test that creates the keyring
entry, applies the key, writes a history entry, and searches it back. The
credential-manager detail lives in `docs/00-scope.md` and
`docs/04-backend-guide.md`.

### Sharing a workspace

A workspace is a **directory that is a Git repository**, one file per
collection, so a team can share requests without sharing history:

```
api/
  restbook.json                 marker: "this is a workspace"
  .gitignore                    written once, never overwritten
  collections/<slug>/collection.json   tracked: folders, requests
  environments/<slug>.json            tracked: non-secret vars
  restbook.local.json           gitignored: active env, globals, drafts
  .restbook/history.jsonl       gitignored: every send, with its response body
```

The rule is **tracked files hold structure, local files hold secrets**. History
never leaves the machine — it is the one thing nobody wants in a shared checkout.

A value that must not be shared is written out as `<restbook:redacted>` in the
tracked file and kept in `restbook.local.json` (which the generated `.gitignore`
excludes), then spliced back in when the workspace is read on the same machine.
`Kv` already carries the `secret: bool` that drives this, and the environment
editor exposes it as a checkbox with a reveal toggle. Today that covers two
carriers:

- **Environment variables** — a row marked `secret` is redacted in
  `environments/<slug>.json` and restored on read.
- **Request auth** — a literal basic password, bearer token, or API key value in
  a saved request is redacted in `collection.json`, so the file still shows *that*
  a request is authenticated and *how*, without shipping the credential.

A whole-string `{{variable}}` is left alone in both: it is already indirection,
and putting the value in a local variable is the intended way to share a request
that needs a token.

The store is wired in: opening a workspace switches the app onto it, and the
status bar names the workspace so the mode is never a mystery. One gap remains —
the request editor's key/value tables (`panels/editor/kv_table.rs`) hardcode
`secret: false`, so params and headers cannot be marked secret from the UI yet,
even though the store would honor the flag. Until that lands, treat the *body*
of a shared request as tracked content.

## Design decisions

- **Backend first.** The engine is usable and debuggable without a window.
- **Progress is reported, not streamed.** `Transport::execute` still buffers the
  whole body into `ResponseRecord` (history needs it), but an optional `Progress`
  sink is bumped as bytes move. `execute_with_progress` defaults to plain
  `execute`, so a transport that cannot report progress still satisfies the trait.
- **A fake transport ships in the engine.** `core::FakeTransport` answers from a
  scripted handler, so the whole pipeline can be tested offline; the real
  `ReqwestTransport` is covered by tests that drive a local socket.
- **One source of truth.** `RequestDef` -> `prepare()` -> `PreparedRequest` ->
  `Transport` -> `ResponseRecord`. Nothing bypasses it.
- **The canvas decides placement, panes decide content.** The workspace names
  every dock position; a pane reports what happened and never names a sibling.
- **Runtime isolation.** `reqwest` needs a tokio reactor; GPUI's executor is not
  tokio. `ReqwestTransport` owns a runtime on a dedicated thread and hands plain
  futures back, so the UI never touches tokio.
- **Encryption is the product's job, not the harness's.** The app's default
  store is SQLCipher; the engine keeps it a feature (`sqlcipher`) so `devcli` and
  the library tests stay dependency-light. See `docs/00-scope.md`.

## Plans

- `docs/00-scope.md` — scope, non-goals, inspiration ledger
- `docs/01-milestones.md` — milestone tables, release lanes, reference parity
- `docs/02-ui-plan.md` — GPUI composition, pane contract, visual grammar
- `docs/03-backend-plan.md` — engine pipeline, crate boundaries
- `docs/04-backend-guide.md` — how to change the engine (recipes, commands, pitfalls)
- `docs/05-ui-guide.md` — how to change the GPUI shell (panes, tabs, actions, styling)
- `docs/06-cross-platform.md` — building and running on macOS, Linux, Windows
- `docs/07-flows.md` — where each click, command, and save lands
- `docs/08-qol-backlog.md` — what still feels unfinished, with code refs
- `docs/09-gpui-kit-audit.md` — where the UI drifts from the GPUI Kit guides
- `docs/10-release-and-updates.md` — versioning, channels, artifact matrix, release flow, update manifest
- `docs/11-repo-map-and-dev-cycle.md` — repository map, crate ownership, and the development loop
- `docs/12-keyboard-shortcuts.md` — every binding, platform spelling, and what to check when one does not fire
- [`SECURITY.md`](SECURITY.md) — what is protected, what is not, and how to report a vulnerability

## Windows setup (this machine)

Visual Studio Build Tools are not installed, so Rust uses the GNU toolchain
rather than MSVC:

```sh
rustup toolchain install stable-x86_64-pc-windows-gnu   # pinned in rust-toolchain.toml
scoop install mingw                                     # provides dlltool + as
```

Add both directories to your **user PATH** (System Properties > Environment
Variables > User variables > Path):

```
%USERPROFILE%\.cargo\bin
%USERPROFILE%\scoop\apps\mingw\current\bin
```

After that `cargo build` works from a normal shell with no env vars. TLS uses
`native-tls` (SChannel) instead of `rustls`, because `ring` needs a C compiler;
flip the feature in the workspace `Cargo.toml` if MSVC Build Tools get installed.

### Encrypted store (`sqlcipher` feature)

`rusqlite`'s `bundled-sqlcipher` needs OpenSSL headers and import libraries.
`libsqlite3-sys` reads `OPENSSL_DIR` and panics without it, so the `dev` scripts
detect a prefix (an explicit `OPENSSL_DIR` always wins) and export it before
running cargo:

```sh
sh scripts/dev.sh build -p restbook-store --features sqlcipher
cd crates/app && sh ../../scripts/dev.sh run      # sqlcipher is a default feature
```

Use `scripts\dev.ps1` or `scripts\dev.cmd` instead on Windows without `sh`; the
detection order is the same.

Detection order: `OPENSSL_DIR`, `pkg-config`, a MinGW `opt` next to `gcc` (or
`MINGW_PREFIX`, or scoop's `apps/mingw/current/opt`), then the usual
`/usr/local`, `/mingw64`, Homebrew prefixes. Nothing machine-specific is stored
in the repository, so a toolchain update that moves the prefix keeps working.

If the prefix search fails, run cargo and it names what it could not find. The
app does not require an OpenSSL dev tree at all when built with
`--no-default-features`: SQLCipher is then compiled out and history falls back to
plaintext JSONL in the same data directory. A failed keyring does the same at
runtime, with a warning on stderr.

At *run* time the exe needs the OpenSSL DLL it was linked against. The `dev`
scripts put the detected prefix's `bin` first on `PATH` for `cargo run`/`cargo
test`, and `package.sh` reads that DLL's name out of the exe's own import table
(`libcrypto-3-x64.dll` for OpenSSL 3, `-4-` for OpenSSL 4) and copies it next to
the exe, which is where the Windows loader looks first. (Everything else in the
import table, `icuuc.dll` included, ships with Windows 10+.) A DLL that does not
match makes the process die in the loader, before `main` can log anything:
Windows reports it as *the application was unable to start correctly*.

The release build avoids that question entirely: `package.sh --release
--vendored` compiles OpenSSL into the exe (`sqlcipher-vendored`), which then has
no `libcrypto` import at all. It needs a complete `perl` on the build machine,
which is why local builds keep the dynamic shape; the trade-off is written up in
[`06-cross-platform.md`](docs/06-cross-platform.md#how-sqlcipher-gets-its-crypto).

Verified on this machine: the roundtrip test creates the keyring entry, opens an
encrypted database, writes a history entry, and searches it back. The database
file contains no plaintext.

The master key lives in Windows Credential Manager (`restbook` / `master-key`)
and is created on first launch; losing that entry means `history.db` can no
longer be read. `keyring` 3 needs its platform backend compiled in — without one
it silently uses an in-memory store and the key changes every launch.

### Desktop app

`crates/app/rust-toolchain.toml` pins `stable-x86_64-pc-windows-gnu`, matching the
engine. GPUI compiles and links under GNU, but its build scripts call
`dlltool.exe`, so the MinGW directory above has to be on `PATH`.

If Build Tools with the "Desktop development with C++" workload are ever
installed, the app can be built with MSVC instead by changing the channel in
that file to `stable-x86_64-pc-windows-msvc`.
