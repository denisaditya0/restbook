# restbook

A local-first desktop HTTP client: Postman's concepts, no account, no cloud, and
every request you send saved to an encrypted local history.

Its value is that it is **small and finished**, not that it is extensible. Three
product principles, from `docs/00-scope.md`, decide most arguments before they
start: local-first and encrypted by default, GUI-first but text-friendly, small
surface with finished edges.

You can think of restbook as the app someone reaches for when they do not want to
sign in to an API client.

## What we will not compromise on

1. **The engine stays UI-free.** `crates/core` knows nothing about GPUI, reqwest,
   tokio, or the filesystem. That is what keeps it testable without a window and
   keeps the shell replaceable. Every "just this once" cross-dependency trades
   that away.
2. **One pipeline.** `RequestDef → prepare() → PreparedRequest → Transport →
   ResponseRecord → Store`. Nothing bypasses it — not a UI shortcut, not a dev
   command, not an import path.
3. **The history is encrypted and it stays encrypted.** The app never writes a
   plaintext fallback without saying so on stderr, and never rewrites a database
   it cannot read.
4. **No scope creep.** `docs/00-scope.md` lists what is explicitly out: JS
   scripting, a collection runner, accounts, cloud sync, mocks, plugins. An idea
   that lands in that list is a "no", not a design discussion.

## A small glossary

Use these words in code, comments, commits, and replies. Most confusion in this
repo is two people meaning different things by one of them.

- **engine** — `crates/{core,transport,store}` and `devcli`. Everything that is
  not the window.
- **shell** / **app** — `crates/app`, the GPUI desktop application. Its own Cargo
  workspace, deliberately.
- **workspace** — two things. In the engine: a `restbook.json` directory being
  used as a `FolderStore` (a shareable, Git-friendly collection tree, as opposed
  to the per-user database). In `crates/app`: the `Workspace` struct, which is
  the canvas. Say "storage workspace" or "the canvas" when it matters.
- **canvas** — `crates/app/src/workspace/`. The only code allowed to name a
  `DockPlacement`.
- **pane** — one self-contained region in `crates/app/src/panels/`, a GPUI entity
  that renders content and knows nothing about where it is shown.
- **POV** — where an interaction lands: left dock, right dock, center, chrome, or
  overlay. The five that exist in `docs/07-flows.md`.
- **`Document`** — the one request/response value the panes observe.
- **`RequestDef`** — what the user is editing (unresolved, may contain `{{vars}}`).
- **`PreparedRequest`** — the pure output of `prepare()`: URL folded, auth added,
  body encoded. Nothing has been sent.
- **`ResponseRecord` / `ResponseBody`** — what came back. The body is
  `Memory(Vec<u8>)` or `File(FileBody)`; `File` has no producer yet (B18 step 1).
- **`Store`** — history trait in `core`; `MemoryStore`, `JsonlStore`,
  `FolderStore`, `SqlcipherStore` implement it.
- **draft** — an unsaved request the app persists for you. Has a store row id, no
  collection node.
- **`Origin`** — whether a history row came from `Standalone`, `Saved { node }`, or
  `Draft { draft }`.
- **hit** — a `HistoryHit`: the columns a search row needs, not the whole entry.
- **`Command`** — a palette entry in `panels/command_palette.rs`. Distinct from
  **action** — a typed keybinding target in `actions.rs`. They are two vocabularies.
- **notice** / **flash** — the outcome a pane reports back to the user.

## The two workspaces

```
restbook/                    root workspace (the engine)
├── crates/core              model, prepare(), traits — no UI, no runtime
├── crates/transport         reqwest adapter, owns its tokio runtime
├── crates/store             history stores behind `Store`
├── crates/devcli            headless harness for the pipeline
└── crates/app               GPUI shell — its OWN workspace, own lockfile,
                             own rust-toolchain.toml
```

`crates/app` is excluded in the root `Cargo.toml`. Practical consequences:

- `cargo build` / `cargo test` at the root never compile GPUI. That stays true.
- **A check run at the root says nothing about the app.** Format, clippy, and
  tests are run twice, once per workspace.
- The app links OpenSSL through SQLCipher, so it needs `OPENSSL_DIR`. Use
  `scripts/dev.sh` / `dev.ps1` / `dev.cmd`, which detect it; plain `cargo` will
  fail or, worse, silently take a different path. On a fresh Windows machine,
  `scripts\setup-windows.ps1 -Install` puts the GNU toolchain, MinGW and the
  PATH entries in place first; run it without flags to see what is missing.

## The one rule

**The canvas decides placement. Panes decide content.**

A pane never names a dock region, never calls `open_dialog`, never reads a
sibling's entity. It emits an event; `Workspace` decides where the result
appears. A pane that hardcodes `Left` is not a small mistake — it is now
un-movable, and every later layout change pays for it.

## Three ways to hurt yourself

1. **Pointing the app at anything you care about.** The app walks upward from the
   working directory looking for `restbook.json`, and `RESTBOOK_WORKSPACE` beats
   everything. Launching it from inside a real workspace opens *that* workspace,
   not a sandbox. Use `RESTBOOK_DATA_DIR` to give a test build its own directory,
   and do not leave a `restbook.json` in a tree you launch from by accident.
2. **Trusting a plaintext run as evidence.** `--no-default-features` (and any
   keyring failure) silently switches history to plaintext `history.jsonl`. It is
   the right fallback and the wrong thing to ship untested: a change that only
   passes on the JSONL path has not been tested on the path users run. Name which
   mode you verified.
3. **Blaming the wrong workspace.** Two lockfiles, two toolchains, two fmt runs.
   A version bump touches both `Cargo.toml` files and both lockfiles, and `ci.yml`
   *rejects* a version bump in a pull request — the bump belongs to the release
   commit. If a build behaves strangely, check which workspace you are in before
   changing anything.

## Hit every surface

The most common defect here is a change that works where you tested it and is
missing everywhere else. Before calling shell work done, walk this list:

- **Entry points.** A command usually has three: an **action** + keybinding in
  `actions.rs`, a **palette** row in `panels/command_palette.rs`, and, when a
  mouse user needs a route, an item in the titlebar menu in
  `workspace/mod.rs::render`. The palette's own tests fail on drift — they assert
  every command is in `docs/07-flows.md` and every pane has a focus and a close
  command for both. Do not add a fourth way to do the same thing.
- **Panes.** A new pane costs: a module under `panels/`, the `BasePanel` + `Panel`
  + `EventEmitter<PanelEvent>` impls, a `PanelName` variant, a constructor in
  `Workspace::new`, a row in `preview.rs`'s `Pane::ALL`, and its palette rows.
  `PanelName::all()` is what `parse` reads and what the menu lists, and
  `Pane::ALL` is a separate hand-kept list — a variant that misses either is
  half-visible, so add it in both places in the same change.
- **Persistence.** Anything the user set — layout, zoom, tab state, a new setting
  — is written somewhere and read back on the next launch. A change that is not
  restored after a reopen is not finished. Existing files must keep parsing; bump
  a schema only when you cannot default the new field.
- **Reverse states.** If you added a way in, add the way out and the way to see
  it. Open needs close, show needs hide, and the user must be able to tell which
  state they are in.
- **Storage modes.** Both matter: the encrypted database and a `FolderStore`
  workspace. A destination is a collection whether it is a row or a directory. A
  store change that only works against SQLCipher is half a change.
- **Secret handling.** Tracked workspace files hold structure, local files hold
  secrets. Anything new that could carry a credential needs a `secret` flag and a
  redaction path, or an explicit reason it cannot be shared.
- **Docs.** Every change has an owning document. See [Documentation](#documentation).

## Verify

Engine, from the repository root:

```sh
cargo build
cargo test
cargo clippy --all-targets --message-format short
```

Encrypted path, which is feature-gated:

```sh
sh scripts/dev.sh test -p restbook-store --features sqlcipher
```

Shell, from `crates/app`:

```sh
sh ../../scripts/dev.sh check --message-format short
sh ../../scripts/dev.sh clippy --message-format short
sh ../../scripts/dev.sh test
cargo fmt --check
```

On Windows without `sh`, use `..\..\scripts\dev.ps1` or `dev.cmd` — same
arguments. Root `cargo fmt --check` is a separate run; `cargo fmt --all` at the
root does not reach the excluded app.

- **If a change is not visible in a window, it is not verified.** There is no UI
  devcli. `--preview <pane>` (or `--preview all`) renders panes over fixture data
  without touching your store — that is the capture a reviewer sees in place of a
  running build. Run it from `crates/app` through a `dev` script like every other
  app command: `sh ../../scripts/dev.sh run -- --preview response`.
- **Do not try to reproduce CI.** `ci.yml` runs the full matrix (Linux, both
  workspaces, `-D warnings`, the SQLCipher job). Run the commands above for the
  code you touched; do not invent extra whole-repo runs.
- Prove behavior with `devcli` rather than a network: `send`, `history`,
  `--progress`. For response-body edge cases use the `sample-server` binary in
  `crates/devcli` (`cargo run -p restbook-devcli --bin sample-server`,
  `http://127.0.0.1:8787`), not a live API.
- **Tests are behavior sentences** (`rejects_header_injection`) and state a reason
  in a doc comment. Engine tests must never need a network or a window; the only
  exception is the SQLCipher keyring round trip. Extend an existing test when a
  case fits; do not add a second test at a higher layer for the same behavior.
- **Name the mode you verified.** Write either "encrypted (`sqlcipher`)" or
  "plaintext JSONL (`--no-default-features`)" in your summary. Both are real
  builds, and only the first is the one users run.
- A bug fix ships a test, or a capture, but not both for the same bug: logic/data
  bugs get the failing test first; purely visual ones get before/after previews
  and no test.

## Documentation

Documentation is part of the change, not a follow-up — but only the document that
owns it. `docs/11-repo-map-and-dev-cycle.md` has the ownership table; in short:

| If you changed | Update |
|---|---|
| scope, a non-goal | `00-scope.md` |
| a milestone, a lane, a parity row | `01-milestones.md` |
| the pane contract, visual grammar | `02-ui-plan.md` |
| the pipeline, crate boundaries | `03-backend-plan.md` |
| an engine recipe, pitfall, API shape | `04-backend-guide.md` |
| a pane, action, tab, styling rule | `05-ui-guide.md` |
| where a click lands, a command, a POV | `07-flows.md` |
| an unresolved rough edge | `08-qol-backlog.md`, and remove it when fixed |
| a deliberate GPUI Kit deviation | `09-gpui-kit-audit.md` |
| versions, channels, packaging | `10-release-and-updates.md` |
| repository shape or this process | `11-repo-map-and-dev-cycle.md` |

The floor: do not document what the code already shows. No file catalogs, no
method enumerations, no control-flow narration, no PR summaries inside `docs/`.

Consequences worth knowing: `07-flows.md` is guarded by a test, so a new command
without a row fails `cargo test`. If a documented decision changes, rewrite or
delete the old text — do not append a second account of the new behavior.

## GPUI before you write shell code

`crates/app` is a GPUI Kit application. Two skills in `.agents/skills` are
required reading, not inspiration:

- **`.agents/skills/gpui-kit`** — how to build with the framework. The
  non-negotiable that bites hardest: **never invent an API.** Search the source
  for the real signature; a plausible-looking method that does not exist is the
  most common failure in this codebase. One dependency (`gpui-kit`), GPUI reached
  as `gpui_kit::*`.
- **`.agents/skills/gpui-kit-design-guides`** — required before any screen,
  layout, spacing, colour, component, motion, or interface-copy decision, and
  before reviewing UI work. Read the guide file itself; do not answer from
  training data or from what an existing pane happens to do.

Where the code departs from those rules on purpose, the departure is recorded
with a code reference in `docs/09-gpui-kit-audit.md`. A new departure gets a row
there or it does not happen.

## Taste

- **Complexity belongs at the boundary.** `prepare()` is pure and does no I/O.
  Transport adapters translate; stores persist; panes render; the canvas places.
  A function that needs two of those roles is in the wrong module.
- **`core` owns contracts and never implementations.** If you find yourself
  introducing a concrete client, runtime, or path into `core`, you have crossed
  the seam the whole layout exists to protect.
- **Inferred types over annotations.** Clippy runs with `-D warnings` in CI, so
  do not assign locals to `String`/`Vec<T>` that inference already gives you.
- **Default to no comment.** Write one for a constraint, an external quirk, or an
  invariant the code cannot show — one line where possible. Do not restate a
  name, narrate steps, or record history. The existing code sets the bar: look at
  a neighbouring file before adding prose.
- **Tests express intent.** Name them as facts about behavior, and keep the
  module split the file already has: `tests` for logic, `render_tests` for the
  painted tree, `enter_tests` for keyboard routing. Those three exist because
  each needs different setup, not as a naming convention.
- **Text is plain and copyable.** Every URL, header, and body is a string a user
  pastes in and out. A feature that hides bytes behind a widget only it can read
  is against principle 2.
- **Attributes are parsed at construction, not at use.** A bad keystroke or
  context predicate panics where it is written; the tests pin the ones that
  matter. Do not weaken that to "check later".
- Performative machinery needs a reason. If a rule here fights the task in front
  of you, say so plainly before breaking it — a maintainer signs off, not the
  comment.

## Where to read next

- Changing the engine → `docs/04-backend-guide.md` (recipes) and
  `docs/03-backend-plan.md` (boundaries).
- Adding or changing a pane → `docs/02-ui-plan.md` (the contract) and
  `docs/05-ui-guide.md` (recipes), plus the two skills above.
- Anything cross-cutting → `docs/11-repo-map-and-dev-cycle.md`.
- Picking up work → `docs/01-milestones.md`; rough edges already known →
  `docs/08-qol-backlog.md`.
- New to Rust or coming from another language → `docs/13-rust-primer.md`.
- Build/packaging differences per OS → `docs/06-cross-platform.md` and the README.

## Pull requests

- **PR or direct push, by what you touched:**

  | Touched | Route |
  |---|---|
  | `docs/`, `README`, comments in `.github/**`, a version-free chore | push direct |
  | anything in `crates/`, or a workflow change that alters what CI runs | pull request |

  The line is about blast radius: docs cannot break a build, and a PR is where
  a `crates/` change or a CI-shape change gets the matrix run that a direct
  push to `master` would only get afterwards. Every CI failure this repo has
  hit on `master` would have been caught on the PR.
  `.github/workflows/ci.yml` is that gate, `.github/workflows/release.yml` runs on
  tags. `.github/pull_request_template.md` is the required shape when you do open
  one: problem, fix, evidence, checks.
- One request is one pull request. Split it only when a maintainer asks.
- Conventional commit titles, plain language: `fix(web)`-style scopes do not
  apply here — use the crate: `fix(store): a failed trim no longer drops the row`.
- Evidence: a pane change needs `--preview` before/after captures attached to the
  pull request, never committed. Motion or timing needs a short video.
- Do not commit implementation plans, research notes, or agent scratch files.
  `.restbook/`, `dist/`, `target/`, and `.refs/` are ignored for a reason.
- Report the model and harness that did the work in the template's footer.
