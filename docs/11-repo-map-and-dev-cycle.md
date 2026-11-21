# restbook — repository map and development cycle

Where everything lives, which crate owns what, and the loop a change goes
through. This is the orientation document; the plans and guides it points at stay
authoritative for their own subject.

Related documents:

- [`00-scope.md`](00-scope.md) — scope, non-goals, inspiration ledger
- [`01-milestones.md`](01-milestones.md) — the milestone tables, release lanes,
  and reference parity; the place a ticket is cut from
- [`02-ui-plan.md`](02-ui-plan.md) / [`03-backend-plan.md`](03-backend-plan.md) —
  the pane contract, visual grammar, engine pipeline and crate boundaries
- [`04-backend-guide.md`](04-backend-guide.md) / [`05-ui-guide.md`](05-ui-guide.md) —
  how to change the engine and the shell
- [`06-cross-platform.md`](06-cross-platform.md) — other OSes
- [`07-flows.md`](07-flows.md) — click → effect → POV
- [`08-qol-backlog.md`](08-qol-backlog.md) — known rough edges
- [`09-gpui-kit-audit.md`](09-gpui-kit-audit.md) — UI drift from the kit rules
- [`10-release-and-updates.md`](10-release-and-updates.md) — artifacts and updater

**Status:** Windows (GNU toolchain) is the only OS this has been built and run
on. `.github/workflows/ci.yml` checks every pull request on a Linux runner, and
`.github/workflows/release.yml` builds Windows x86_64, macOS arm64 and Linux
x86_64 on tags; the manual smoke path is still run by hand.

## Two workspaces, on purpose

```
restbook/                    root Cargo workspace (the engine)
├── crates/core              domain model, prepare(), traits — no UI, no runtime
├── crates/transport         reqwest adapter, owns a tokio runtime
├── crates/store             history stores: memory, JSONL, folder, SQLCipher
├── crates/devcli            terminal harness for the engine
└── crates/app               GPUI desktop shell (its OWN workspace)
```

`crates/app` is excluded from the root workspace in `Cargo.toml`. GPUI moves
quickly, and keeping it out means:

- `cargo build`/`cargo test` at the root never downloads or compiles GPUI;
- the engine can be tested headlessly, with no window or GPU;
- the app has its own `Cargo.lock` and its own `rust-toolchain.toml`.

Both workspaces are ordinary Cargo projects; there is no `Makefile` or task
runner. Build commands are `cargo` plus the `scripts/dev.*` wrappers.

## Repository map

```
.
├── Cargo.toml               engine workspace + shared dependency versions
├── Cargo.lock               engine lockfile
├── rust-toolchain.toml      Windows GNU pin for the engine
├── README.md                product overview, storage, Windows setup
├── crates/
│   ├── core/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs           public surface and re-exports
│   │       ├── model.rs         RequestDef, Body, Auth, ResponseRecord, collections
│   │       ├── prepare.rs       RequestDef -> PreparedRequest (pure)
│   │       ├── template.rs      {{var}} resolution
│   │       ├── vars.rs          variable scopes and preview
│   │       ├── generator.rs     {{$uuid}}, {{$timestamp}}, {{$random*}}
│   │       ├── transport.rs     Transport trait, CancelToken, Progress
│   │       ├── store.rs         Store trait, Draft, HistoryEntry/Hit, Origin
│   │       ├── net.rs           proxy, extra roots, client certs
│   │       ├── error.rs         Error/Result
│   │       ├── testing.rs       FakeTransport for offline tests
│   │       └── import.rs + import/{curl,har,postman,openapi}.rs
│   ├── transport/
│   │   ├── Cargo.toml
│   │   └── src/lib.rs           ReqwestTransport, client cache, shared cookie jar
│   ├── store/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs           MemoryStore + shared store helpers
│   │       ├── jsonl.rs         JsonlStore (plaintext, dev/fallback)
│   │       ├── folder.rs        FolderStore (Git-friendly workspace on disk)
│   │       └── sqlcipher.rs     SqlcipherStore (feature = "sqlcipher")
│   ├── devcli/
│   │   ├── Cargo.toml
│   │   └── src/main.rs          send / history / import / export
│   └── app/
│       ├── Cargo.toml           standalone workspace; sqlcipher is a default feature
│       ├── Cargo.lock
│       ├── rust-toolchain.toml  the same GNU pin
│       ├── themes/default-dark.jsonc
│       └── src/
│           ├── main.rs          log init, gpui_kit init, open window
│           ├── actions.rs       action vocabulary + keybinding table
│           ├── document.rs      shared request/response value the panes observe
│           ├── theme.rs         Palette and Metrics
│           ├── variables.rs     variable state for the shell
│           ├── completion.rs    {{name}} completion provider
│           ├── icons.rs         icon vocabulary
│           ├── logging.rs      stderr + <data dir>/restbook.log
│           ├── notice.rs       the outcome a pane reports (flash message)
│           ├── updater.rs      manifest fetch/compare (phase 1, read-only)
│           ├── util.rs          shared leaves
│           ├── workspace/       THE CANVAS
│           │   ├── mod.rs           dock tree, layout install/save, tab snapshots
│           │   ├── chrome.rs        titlebar and status bar
│           │   ├── send.rs          prepare -> transport -> record history
│           │   ├── persistence.rs   layout.json read/write
│           │   ├── storage.rs       data directory + which store
│           │   ├── settings.rs      settings state
│           │   ├── settings_view.rs settings dialog
│           │   └── history_sheet.rs history overlay
│           └── panels/          ONE MODULE PER PANE
│               ├── mod.rs           pane exports
│               ├── request.rs       request tabs, URL bar, editor tabs
│               ├── response.rs      summary + Body/Headers/Cookies
│               ├── history.rs       store search + rows
│               ├── collections.rs   saved requests
│               ├── environment.rs   variable sets
│               ├── command_palette.rs
│               └── editor/          the four tab bodies
│                   ├── mod.rs
│                   ├── params.rs
│                   ├── headers.rs
│                   ├── body.rs
│                   ├── auth.rs
│                   ├── kv_table.rs
│                   ├── parts_table.rs
│                   └── settings.rs
├── scripts/
│   ├── dev.sh               cargo with OPENSSL_DIR detected (POSIX)
│   ├── dev.ps1              same, PowerShell
│   ├── dev.cmd              same, cmd.exe; `--logs` tees to target/dev-logs
│   ├── package.sh           build dist/ (Windows exe + libcrypto DLL)
│   └── sample-server.py     local response fixtures for the UI
├── docs/                    the numbered documents listed above
├── CHANGELOG.md             what shipped for the user; the release body
├── .github/workflows/
│   ├── ci.yml               pull requests: fmt, clippy, tests, encrypted store
│   └── release.yml          tag → build 3 native targets → draft release + manifest
├── .github/pull_request_template.md   the shape a pull request is reviewed in
├── .agents/skills/          GPUI Kit coding/design guides for agents
├── .refs/                   ignored reference checkouts
├── .restbook/               ignored dev history (devcli)
├── dist/                    ignored packaged output
└── target/                  ignored build output
```

Ignored paths are in `.gitignore`; nothing generated is checked in except the two
`Cargo.lock` files and `skills-lock.json`.

## Ownership rules

| Crate / area | Owns | Must not |
|---|---|---|
| `core` | model, templating, `prepare()`, `Store`/`Transport` traits | depend on GPUI, reqwest, tokio, or the filesystem |
| `transport` | reqwest, tokio runtime, redirects, cookies, TLS | know about the UI or storage |
| `store` | concrete stores behind `Store` | own the app's keyring policy |
| `devcli` | a harness that proves the pipeline works headlessly | become the only test path |
| `app/workspace` | engine handles, dock placement, persistence, sends | draw pane content |
| `app/panels` | one region's content and its own events | name a sibling pane or a dock position |

Two rules matter more than the rest:

1. **One pipeline.** `RequestDef → prepare() → PreparedRequest → Transport →
   ResponseRecord → Store`. Nothing bypasses it.
2. **The canvas decides placement, panes decide content.** A pane emits an
   intent; only `workspace` chooses where it appears.

## The development cycle

### 1. Decide which side of the split owns the change

- Model, validation, transport behavior, persistence → engine (`core`,
  `transport`, `store`).
- Layout, interaction, rendering → `crates/app`.
- Both → design the contract in `core` first, then consume it in the app.

Read the relevant plan (`02`/`03`) and guide (`04`/`05`) before editing. The
`docs/00-scope.md` non-goals are load-bearing: no JS scripting, runner, cloud,
or plugin system.

### 2. Make the engine work first

The engine is testable without a window, so most behavior starts there:

```sh
cargo test
cargo clippy --all-targets --message-format short
```

For the encrypted path, which is feature-gated:

```sh
sh scripts/dev.sh test -p restbook-store --features sqlcipher
```

On Windows without `sh`, substitute `scripts\dev.ps1` or `scripts\dev.cmd`.
The wrappers exist because `sqlcipher` builds SQLCipher and `libsqlite3-sys`
needs `OPENSSL_DIR`; they detect a prefix, put its `bin` first on `PATH`, and
then forward every argument to cargo. An explicit `OPENSSL_DIR` always wins.

Exercise behavior end to end with the harness:

```sh
cargo run -p restbook-devcli -- send https://httpbin.org/get
cargo run -p restbook-devcli -- send https://httpbin.org/post -X POST --json '{"hello":"world"}'
cargo run -p restbook-devcli -- send https://httpbin.org/bytes/1048576 --progress
cargo run -p restbook-devcli -- history httpbin
cargo run -p restbook-devcli -- import some-har.json --verbose
cargo run -p restbook-devcli -- export some-har.json --workspace ./workspace
```

`devcli` writes plaintext JSONL to `.restbook/history.jsonl` in the working
directory. That is a harness convenience; the app uses the per-user data
directory and SQLCipher by default.

### 3. Then the shell

```sh
cd crates/app
sh ../../scripts/dev.sh run          # or ..\..\scripts\dev.ps1 run / .cmd run
```

Verification for the app, from `crates/app`:

```sh
sh ../../scripts/dev.sh check --message-format short
sh ../../scripts/dev.sh clippy --message-format short
sh ../../scripts/dev.sh test
cargo fmt --check
```

There is no CLI for the UI: **if a change is not visible in a window, it is not
verified.** For response-body edge cases, start the fixture server:

```sh
python scripts/sample-server.py       # http://127.0.0.1:8787
```

`05-ui-guide.md` lists what each fixture path exercises.

### 4. Manual smoke path

Before considering a user-facing change done, run the shortest path that would
break if it were wrong:

1. Launch the app; no error appears on stderr or in `restbook.log`.
2. Send a request; status, time, and size appear.
3. Pick the new history row; the whole request (params, headers, body, auth) is
   restored and the stored response shown.
4. Re-send the restored request.
5. Close and reopen: `history.db` and `layout.json` still work in the same data
   directory.

Run it a **second** time as well: a changing master key shows up as a failure to
open the encrypted database on the next launch.

Since no CI job can open a window, a pane change also needs captures. The
developer preview renders one pane, or all of them, over fixture data without
touching your store:

```sh
cargo run -p restbook-app -- --preview all
```

Attach the before and after of the affected pane to the pull request. That
comparison is what a reviewer sees instead of a running build.

### 5. Open a pull request, not a push

`.github/workflows/ci.yml` is the pre-merge gate, and `.github/workflows/release.yml`
stays the post-merge one. What runs where:

| When | Workflow | Checks |
|---|---|---|
| Every pull request, and pushes to `master` | `ci.yml` | fmt in both workspaces, clippy (`-D warnings`), both test suites, the SQLCipher store, and no version bump |
| A tag is pushed | `release.yml` | tag = version = engine version, tag on the default branch, and every `ci.yml` job green for that commit (`verify` reads the check runs through the API rather than rebuilding the suite) |

`.github/pull_request_template.md` carries the shape a pull request is expected
to have: the problem, the fix, the evidence, the checks. The template is the
contract, not decoration — the evidence section is where the `--preview`
captures go, attached rather than committed.

So a change that reaches a release is checked twice: on the pull request, and by
`release.yml` reading that commit's CI verdict before it builds. The two things
CI cannot cover are the smoke path and the `--preview` captures, and they are
the reason a pull request still needs a human.

There is one channel: a release is a tag on `master` whose version carries no
suffix, cut after the pull request is merged. The version bump belongs to that
commit, which is why CI rejects one in a pull request (`0.1.x` for fixes,
`0.2.0` for a feature batch, `1.0.0` for the first supported release).

### 6. Update the documents that own the change

Documentation is part of the change, not a follow-up. Pick the owner:

| If you changed | Update |
|---|---|
| scope, a non-goal | `00-scope.md` |
| a milestone state, a lane, or a reference-parity row | `01-milestones.md` |
| an engine recipe, pitfall, or API shape | `04-backend-guide.md` |
| a pane, action, tab, styling rule | `05-ui-guide.md` |
| a platform assumption or prerequisite | `06-cross-platform.md`, README |
| where a click lands, a command, a POV | `07-flows.md` |
| an unresolved rough edge | `08-qol-backlog.md` (and remove it when fixed) |
| a deliberate GPUI Kit deviation | `09-gpui-kit-audit.md` |
| versions, channels, packaging, the manifest | `10-release-and-updates.md` |
| what shipped, in the user's words | `CHANGELOG.md` |
| repository shape or the process itself | this file |

Tests are behavior sentences (`rejects_header_injection`), and `07-flows.md`
has a test guarding its command list against drift.

### 7. Package or release

`scripts/package.sh` is the only artifact step; it dispatches on the host OS:

```sh
sh scripts/package.sh            # debug
sh scripts/package.sh --release
```

What lands in `dist/` depends on where it runs:

- **Windows** — `restbook.exe` and, when the build linked OpenSSL
  dynamically, the `libcrypto-<major>-x64.dll` it imports (read from its import
  table, so it follows OpenSSL's major version). The release build passes
  `--vendored`, so it links OpenSSL statically and `dist/` holds the exe alone.
  Plus the `restbook-<version>-windows-x86_64.exe` installer compiled from
  `crates/app/packaging/restbook.iss` when Inno Setup is installed. A build that
  imports a DLL no OpenSSL tree here provides is an error, not a silent
  `dist/`: the exe would fail in the loader.
- **macOS** — `restbook.app`, plus `restbook-<version>-macos-<arch>.dmg` with an
  `Applications` symlink beside the bundle.
- **Linux** — `restbook-<version>-linux/` (prefix tree),
  `restbook-<version>-linux-<arch>.tar.gz`, and
  `restbook-<version>-linux-<arch>.deb` when `dpkg-deb` exists.

Version bumps touch both workspaces plus both lockfiles; tags are the bare
version (`0.2.0`), with no prerelease suffix. The updater checks the release
channel, downloads this platform's asset, verifies its `sha256` and opens it —
the contract is in `10-release-and-updates.md`.

## Test strategy at a glance

| Layer | How it is tested |
|---|---|
| `core` model, templating, prepare, import | unit tests in each module; no network, no window |
| transport | integration tests against a real local socket |
| pipeline without a server | `core::FakeTransport` |
| stores | unit tests; SQLCipher is feature-gated and touches the keyring |
| app logic | `#[test]`/GPUI test support where the behavior is observable headlessly |
| app rendering/interaction | `ci.yml` runs the mock-platform tests; layout and looks still need a manual run with `--preview` |
| flows documentation | a test compares the palette command list with code |

Engine tests must never need a network or a window. The single exception is the
SQLCipher keyring round trip.

## Feature flags and fallbacks

| Flag | Default | Effect |
|---|---|---|
| `restbook-store/sqlcipher` | off in the engine | builds SQLCipher; needs a C toolchain and `OPENSSL_DIR` |
| `restbook-app` default | on | app ships encrypted history; `--no-default-features` falls back to plaintext JSONL |
| `restbook-core/network-validation` | off | enables PEM/proxy validation by handing values to reqwest; the transport enables it |

A keyring failure is not fatal: the app warns on stderr and uses plaintext JSONL
in the same data directory. The same directory holds `history.db`,
`layout.json`, and `restbook.log`.

## Known gaps

- CI covers pull requests on Linux and releases on the three shipped targets;
  the Windows and macOS shells are compiled only at release time.
- Only Windows has been built and run locally; other OSes are instructions, not
  verified recipes.
- The updater checks and installs from Settings › Updates: it downloads the
  platform's asset, verifies its `sha256`, and opens it (Windows quits so the
  installer can replace the exe; macOS and Linux leave the app running). System
  notifications are not used, and nothing installs without that click.
- `package.sh` covers all three platforms but only the Windows path has been
  run here; the macOS and Linux packaging (`.dmg`, `.deb`) has only been
  syntax-checked, not executed.
- `devcli` persists plaintext by design; only the app is encrypted.
- A UI change is not covered by any automated check. `--preview` captures in the
  pull request are the only evidence a reviewer gets.
