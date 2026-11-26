# restbook — UI (GPUI) developer guide

The desktop shell in `crates/app`. It is a **separate Cargo workspace** so the
engine never depends on GPUI. The engine has its own guide in
[`04-backend-guide.md`](04-backend-guide.md); composition rationale and the UI
milestone list stay in [`02-ui-plan.md`](02-ui-plan.md). Building this crate on
macOS or Linux differs in ways this guide does not cover — see
[`06-cross-platform.md`](06-cross-platform.md) for the toolchain pin, keyring
backends and packaging. Where the code departs from the GPUI Kit rules this
guide assumes, [`09-gpui-kit-audit.md`](09-gpui-kit-audit.md) records it with
code refs.

## Run and verify

```sh
cd crates/app
sh ../../scripts/dev.sh run
```

On Windows without `sh`, use `..\..\scripts\dev.ps1` (PowerShell) or
`..\..\scripts\dev.cmd` (cmd.exe); both take the same arguments.

```sh
cd crates/app
sh ../../scripts/dev.sh check --message-format short
sh ../../scripts/dev.sh clippy --message-format short
sh ../../scripts/dev.sh test
cargo fmt --check
```

SQLCipher is a default feature and its build script needs `OPENSSL_DIR`, so use a
`dev` script (plain cargo with the OpenSSL prefix detected) rather than `cargo`
directly. `--no-default-features` drops SQLCipher entirely and uses plaintext
JSONL history — that is the build to test the fallback path.

The same script also puts the detected prefix's `bin` first on `PATH`, because
the app links libcrypto dynamically and the loader needs the matching DLL at run
time. `sh scripts/package.sh [--release]` builds `dist/` with the exe and that
DLL beside it, taking the DLL's name from the exe's import table so the two
cannot disagree; every other entry in the import table is a Windows DLL.
`--vendored` (`sqlcipher-vendored`) compiles OpenSSL into the exe instead, which
is what the release build does; the GNU toolchain here cannot do it, see
[`06-cross-platform.md`](06-cross-platform.md#how-sqlcipher-gets-its-crypto).

On Windows a non-debug build is a **GUI** binary (`windows_subsystem = "windows"`
in `main.rs`), so launching the exe opens no console window beside it. A debug
build deliberately keeps the console: that is where `cargo run` prints the
`log::*` lines. A packaged run has no terminal to print to, so it tees to
`<data dir>\restbook.log` (see `logging.rs`) — including a panic, which would
otherwise leave nothing behind after a crash.

`cargo run` on this machine links GPUI through MinGW/GNU: the first build is
slow, and `dlltool.exe` must be on `PATH` — `scripts\setup-windows.ps1` puts the
MinGW tree there, and the README has the manual steps. There is no devcli for
the UI; if a change is not visible, it is not verified.

### The fast loop

Two commands cover a UI edit, and neither needs `OPENSSL_DIR`:

```sh
cd crates/app
cargo check -p restbook-app --no-default-features   # the inner loop, ~7 s after deps
cargo run   -p restbook-app --no-default-features   # the fast visual build
```

The check is the edit-compile loop: no SQLCipher, no link, no window. Reach for
the run only when the change is visual, because a pane is not verified until it
is on screen. `--no-default-features` drops SQLCipher (plaintext JSONL history)
and is the quickest link; a full `dev.sh run` is for testing the encrypted
store.

Seeing one pane used to need the whole window, dock and store. The preview flag
replaces the root view with one pane over fixture data instead:

```sh
cargo run -p restbook-app --no-default-features -- --preview response
cargo run -p restbook-app --no-default-features -- --preview all
```

`--preview <pane>` takes `history`, `request`, `response`, `environment` or
`collections`, and builds that pane exactly as `workspace/mod.rs` does: the real
pane entity over a fixture `Document` (a finished `POST` with headers, cookies
and a body), a `MemoryStore` seeded with one environment, one collection, one
draft and three sends, and the settings file the app would read. Nothing is
sent, no layout is saved, and the user's history is untouched. `--preview all`
lays every pane out in one grid for checking a shared theme change. An unknown
name opens a window naming the valid ones rather than silently launching the
workspace; a pane whose setup fails is reported in that pane's place, so the
rest of the grid still draws. Adding a pane means an arm in `preview.rs`'s
`Pane` and `build_pane`.

For a diff instead of a look, the render tests can dump a pane's recorded
bounds as text (6.2 in [`08-qol-backlog.md`](08-qol-backlog.md)):

```sh
cargo test -p restbook-app --no-default-features a_pane_has_a_deterministic_element_tree
ls target/preview/    # response-numbered.txt: one line per recorded element
```

The file is deterministic, so two builds compare with `git diff`; it is the
laid-out tree, not a screenshot, and an element only appears if it opted in
with `debug_selector` (a no-op outside test builds).

### Sample responses

Some of the response pane is only observable against a particular body shape,
and a public echo service will not produce one on demand. `sample-server` serves
them locally, bound to 127.0.0.1, with no state and no dependency on the app:

```sh
cargo run -p restbook-devcli --bin sample-server            # :8787
cargo run -p restbook-devcli --bin sample-server -- --port 9000
```

| Path | Shape | What it exercises |
|---|---|---|
| `/one-line` | minified JSON, one ~10 KB line | the Raw view (and the numbered view) must scroll sideways |
| `/long-string` | indented JSON with one ~2.7 KB string | the numbered view must scroll that line sideways |
| `/many-lines` | indented JSON, ~4800 lines | the unnumbered fallback |
| `/huge` | minified JSON, ~925 KB | the "truncated for display" banner |
| `/body?kb=<n>` | minified JSON of about n KiB | size formatting and scrolling at any scale |
| `/stream?chunks=<n>&gap=<ms>` | chunked JSON, one record per write | the live byte counter and Cancel mid-transfer |
| `/abort?chunks=<n>` | chunked JSON that stops mid-body | the transport's short-read error |
| `/empty` | `204`, no body | a finished response with nothing to show |
| `/binary` | bytes that are not UTF-8 | lossy decoding and the raw label |
| `/gzip` | gzipped JSON | transparent decompression; the header disappears |

Formats the viewer has to label or survive:

| Path | Shape | What it exercises |
|---|---|---|
| `/array` | JSON with a top-level array | pretty-printing and highlighting a non-object root |
| `/scalar` | JSON `null` | the smallest valid JSON, and the empty-looking body |
| `/invalid-json` | `application/json` that does not parse | the raw-text fallback |
| `/sniff` | JSON bytes, no `Content-Type` | content sniffing; the format chip still says "Text" |
| `/xml` | indented XML, one long attribute row | the XML label and unhighlighted long lines |
| `/html` | HTML page, one long tag row | the HTML highlight and a row wider than the pane |
| `/text` | plain text, one very long line | a body with no structure at all |
| `/csv` | quoted CSV | commas and escaped quotes |
| `/unicode` | CJK, Arabic, emoji, combining marks | glyph width, selection, unbreakable runs |
| `/deep` | JSON nested ~120 levels | indentation width and the line gutter |

Transport, headers, cookies, requests:

| Path | Shape | What it exercises |
|---|---|---|
| `/error?status=<n>` | that status with a JSON body | the error banner and status colour |
| `/headers?count=<n>` | n `X-Sample-<n>` headers | the Headers tab and its count badge |
| `/cookies` | five `Set-Cookie` headers, one name repeated | the Cookies tab, attributes, quoted values |
| `/redirect?hops=<n>&status=<3xx>` | a chain ending at `/one-line` | follow-redirects on and off (Settings) |
| `/echo` | the request reflected as JSON | `POST`/`PUT`/`PATCH`/`DELETE` bodies, repeated query parameters, headers |

Every path takes `?delay=<ms>` to hold the response open before it starts, and
the bodies are deterministic, so two sends of the same request are
byte-identical. `/?delay=600000` never answers, which is what Cancel is for
before the first byte arrives. Every route but `/echo` is read-only and
answers a body-carrying method with `405` and `Allow: GET, HEAD`.

### Logging

`logging.rs` installs `env_logger` before the window is built. The sink is a
tee, because a GUI binary usually has no terminal behind it — launched from
Explorer, a shortcut, or an editor task, stderr is discarded and a `log::info!`
simply vanishes. Every line therefore goes to **both** stderr (when it is
attached) and a file:

| Where | Path |
|---|---|
| Windows | `%APPDATA%\restbook\restbook.log` |
| macOS | `~/Library/Application Support/restbook/restbook.log` |
| Linux | `$XDG_DATA_HOME/restbook/restbook.log` (or `~/.local/share/…`) |

The directory is the same one `RESTBOOK_DATA_DIR` overrides, so a portable run's
log moves with its data. `RESTBOOK_LOG_FILE` names the file directly. The file
is appended to, with one archive: a start that finds it over 1 MiB renames it to
`restbook.log.1` and begins a fresh file, and so does a start that finds no
startup line for its own version, so an upgrade archives the previous version's
transcript instead of mixing two builds' lines in one file. A failure to open it
is not fatal: the app warns on stderr and runs with stderr alone.

The file is deliberately quiet: startup facts, `warn`/`error`, panics, and one
send outcome. The play-by-play is not in it. When a user needs to report a
problem, **Settings → Storage → Capture for 10 minutes** (also on a failed send's
error banner) opens a separate `restbook-debug-<timestamp>.log`; that capture
carries the interaction transcript, full URLs, headers, bodies, and import/export
reasons, and is deleted on the next launch. Secrets stay redacted in both files.

`RUST_LOG` is the developer switch and the *only* thing that makes the ordinary
file verbose. It outranks the 10-minute window in both directions: when it is
set, the UI capture is inert and the log follows exactly what was asked for.

```sh
# the default when RUST_LOG is unset (quiet)
warn,restbook_app=info,restbook_core=info,restbook_store=info,restbook_transport=info

# developer: full transcript in restbook.log, no timer, no deletion
RUST_LOG=warn,restbook_app=debug,restbook_core=debug,restbook_store=debug,restbook_transport=debug sh ../../scripts/dev.sh run
```

The dev scripts' `--logs` flag supplies this filter for you (below).

Targets are crate names, so the hyphen in `restbook-app` becomes an underscore
(`restbook_app`), and a module can be narrowed further
(`restbook_app::workspace=debug`). `log::debug!` or `log::trace!` added without
setting `RUST_LOG` will **not** appear — that is the first thing to check when a
new line seems missing. Write `log::*` rather than `eprintln!`: only the logger
reaches the file.

The send path also emits `restbook_app::perf` debug lines when a failure is
handled (how long the foreground took to pick it up, and how long the response
history and draft readers took). They exist to localise a stall between the
transport returning and the error view painting:

```sh
RUST_LOG=restbook_app::perf=debug sh ../../scripts/dev.sh run
```

A capture reads as a transcript, so a file attached to a bug report shows what
was done and what came back:

- `restbook_app::ui` (debug) — every command, tab open/close/select, history
  pick, sidebar reveal, saved-request/draft open, response tab and format
  switch, and the copy buttons.
- `restbook_app::send` (debug) — the request that went out, its full URL, and
  a bounded single-line body preview (binary payloads are named, not dumped).
  The ordinary file keeps the outcome line only: method, redacted URL, status,
  duration, and size.

Panel events are not logged by the subscriber by hand. Each pane's `Event` has a
`summary()` that is an **exhaustive `match`**, so adding a variant does not
compile until it says what its log line is; `Workspace::listen` logs the summary
and then handles the variant. Pane events therefore cannot go unlogged, and a
rename of a field cannot silently leave a stale message behind. An event that is
pure UI noise (`Notice`) returns `Option::None`. Keep the sink manual for
everything else: `log::*` has to work before a window exists and inside a panic,
which an event-driven logger could not.

To watch the file while clicking through the app:

```powershell
Get-Content "$env:APPDATA\restbook\restbook.log" -Tail 80 -Wait
```

You do not have to know that path in advance: the titlebar menu has **Copy log
path** and **Copy recent log** (the last 200 lines to the clipboard, with a
confirmation flash), a failed send's error banner has its own copy-log and
capture-debug buttons, and **Settings → Storage** prints the log file that is
actually open — including a `RESTBOOK_LOG_FILE` override. The error itself is a
normal UI event already (the response pane and the status bar), so the file is
the *evidence*, not the only notification.

A panic is not a `log` call, so `logging::init` also installs a panic hook. The
line arrives through the same sink (`panicked: panicked at <file>:<line>:` plus
the message), and `RUST_BACKTRACE=1` adds a stack. Without it a windowed crash
left *nothing* behind — no window, no file line — which is exactly how a
malformed keybinding looks like "the app does not start". The default hook still
runs afterwards, so a terminal run reads as it always did.

Windows paths:

```powershell
..\..\scripts\dev.ps1 --logs run
```

```bat
..\..\scripts\dev.cmd --logs run
```

Both set the `debug` filter and the `target/dev-logs` file themselves; set
`RUST_LOG` only to narrow it (for example `restbook_app::perf=debug`).

`--logs` is the developer switch on all three scripts. It points the app's
`RESTBOOK_LOG_FILE` at `target/dev-logs/<stamp>-app.log`, so the transcript
never lands in the `restbook.log` a user would attach; on cmd.exe it also tees
**cargo's** output to `<stamp>-cargo.log`, which catches a build error that
scrolled past. Unless `RUST_LOG` is already set, `--logs` supplies the full
`debug` filter itself.

## Testing

Same convention as the engine crates
([`04-backend-guide.md`](04-backend-guide.md#testing)): tests sit at the bottom
of the file they test, inside a module that only exists under `cargo test`.

```rust
#[cfg(test)]
mod tests {
    // Named imports: this module lives under the crate's `gpui_kit::*` glob.
    use super::{is_unsaved, tab_label};

    #[test]
    fn a_tab_without_a_url_is_named_for_being_new() {
        assert_eq!(&*tab_label(&RequestDef::new(Method::Get, "")), "(new request)");
    }
}
```

**Naming what you import is the safe default.** `gpui_kit::*` exports a macro
called `test`, which shadows the built-in `#[test]` attribute (and then hits the
recursion limit), so any module that glob-imports the kit — which is most of the
panes — must name its imports in `mod tests` as well. The pattern spread to the
rest of the crate because the import list doubles as the test's dependency
list. Only modules with no kit glob (`util.rs`, `document.rs`) can use
`use super::*`.

Two modules, two jobs:

| Module | For | Needs a window? |
|---|---|---|
| `mod tests` | pure helpers: `tab_label`, `is_unsaved`, `body_preview`, `relative_time`, settings parsing | no |
| `mod render_tests` | anything that lays out or paints a view | yes, a mock one |

### Headless render tests

`TestAppContext` replaces GPUI's platform with a mock, so a real pane can be
built, laid out, and painted with no display and no GPU. The shape, from
`panels/response.rs`:

```rust
#[cfg(test)]
mod render_tests {
    use gpui_kit::{AppContext as _, Entity, TestAppContext, VisualTestContext};

    #[test]
    fn a_reopened_request_finds_its_earlier_sends() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let (view, cx) = open_with(&mut cx, None, store, None); // add_window_view
        let found = view.update(cx, |panel, _| panel.history.len());
        assert_eq!(found, 2);

        draw(cx); // window.draw(cx).clear(cx) — the assertion above is the test
        cx.quit();
    }
}
```

- `cx.add_window_view(|window, cx| …)` is the harness: the closure builds the
  pane exactly as the workspace would, and the returned `VisualTestContext`
  draws it. Keep one `open(…)` helper per pane, with a `_with` variant for the
  state a test needs to differ in (here: a seeded store and no `last_url`).
- `draw(cx)` is a **panic guard, not a screenshot**. Icons and colours are
  recorded, never compared, and the mock font has fixed advances, so a width
  assertion measures layout, not text. Assert on state (`panel.history.len()`)
  when pixels are not the question; draw once when they are not the answer but a
  layout panic would still be a bug — empty lists, zero-height virtualized
  lists, and overflowing fixed-height rows all fail there.
- An element's bounds are only visible to the test if the element opted in:
  `div().id("x").debug_selector(|| "x".to_owned())`, then
  `cx.debug_bounds("x")`. `.id(…)` alone is not recorded, and `debug_selector`
  is a no-op outside test builds.
- `Settings::new(PathBuf::from("restbook-render-test.json"), window, cx)` names a
  file that does not exist, so the defaults load and a test never writes one.

Run them like the engine's, from `crates/app` (the wrapper supplies
`OPENSSL_DIR`):

```sh
sh ../../scripts/dev.sh test
sh ../../scripts/dev.sh test panels::response       # one pane's tests
```

Outside the app crate the same filters work with plain `cargo test <filter>`;
inside it, use the wrapper, because `sqlcipher` is a default feature and
`libsqlite3-sys` needs `OPENSSL_DIR` to build.

## The map

```
src/main.rs              window options, gpui_kit::init, open window
src/actions.rs           the command names a keybinding dispatches, bindings() (the
                         keybinding table, derived from the palette's rows), and
                         shortcut() (a hint read from the live keymap)
src/variables.rs         environments, globals, active selection, reference walking
src/completion.rs        `{{…}}` suggestions: the body's provider, and the Autocomplete state the URL bar and value fields share
src/workspace/
  mod.rs                 THE CANVAS: engine handles, Document, Variables, tabs,
                         typed Panes, dock, palette routing
  chrome.rs              the custom titlebar and the status bar (chrome, not panes)
  send.rs                Workspace::send() -> prepare -> transport -> store
  persistence.rs         dock layout to/from layout.json in the data dir (names + width)
  save_dialog.rs         the first-save dialog: a name and a destination picker
  move_dialog.rs         the move dialog: the same picker, for a saved request
  destination_picker.rs  the shared, searchable, virtualized destination Select
  settings.rs            app preferences (pretty-print, history limit) to/from
                         settings.json, as an observable entity
  settings_view.rs       the Settings dialog: preferences plus read-only storage facts
  storage.rs             data directory, storage-mode selection (workspace vs
                         local database), legacy JSONL migration
src/panels/
  history.rs             sidebar: store search, rows, Picked event
  collections/
    mod.rs               the pane: rows, events, save, rename, delete, drafts
    tree.rs              the tree: filter, sort, insert, remove, move, lookups
  environment.rs         environment/globals editor + variable inspector
  command_palette.rs     the fixed command table (each row's keys included) and the
                         dialog opener
  request.rs             method dropdown, URL bar, Send/Cancel, editor tabs
  response.rs            status summary + Body / Headers / Cookies tabs
  editor/
    mod.rs               RequestEditor: one RequestDef, five tab bodies
    params.rs headers.rs body.rs auth.rs settings.rs
    kv_table.rs          the shared enabled key/value row widget
    kv_raw.rs            one Kv list as wire-form text, for the Table/Raw switch
    parts_table.rs       multipart rows: name, value-or-file, mime, enabled
src/document.rs          one observable request/response value
src/icons.rs             the glyph vocabulary: IconName, the two sizes, and the
                         pane-title shape
src/logging.rs           the log sink: stderr when attached, always a file in the
                         data directory; also installs the panic hook
src/preview.rs            `--preview <pane>` / `--preview all`: one pane over a
                         fixture Document and MemoryStore, plus the render-test
                         element-tree dump
src/theme.rs             Palette, status() and method() colors, bounded_height()
src/util.rs              body decoding, bytes, duration, cookies (+ unit tests)
```

Dependencies point downward: `main` -> `workspace` -> `panels` -> `editor` ->
`theme`/`util`. A pane never names another pane and never names a
`DockPlacement`.

## The three rules

1. **The canvas decides placement, panes decide content.** Only
   `workspace/mod.rs` chooses `Left`/`Center` or a tab group.
2. **A pane never gets built by `DockArea`.** The workspace constructs panes,
   keeps them typed (`Entity<RequestPanel>`), and hands the dock a
   `panel_handle(entity)` wrapper. The wrapper carries the pane's presentation
   (`title`, `title_bar`), which a bare entity would lose to the dock's
   `panel_name` fallback. `DockArea::load` and `PanelRegistry` stay unused.
3. **A pane reports intents as events.** `cx.emit(RequestEvent::Send)`; the
   workspace subscribes and is the only code that calls the transport. Panes
   share state only through `Document`.

## Window layout

The window is a dock area with three regions, wrapped in two lines of app-drawn
chrome and no generic tab strip:

```
titlebar (34px, app-drawn)     workspace chip   …   environment chip, menu, window controls
┌────────────────────────────┐    ┌───────────────────────────────┐
│ Collections                │    │ Request  method ▾ url  Send   │
│ ▸ Acme                     │    │ Params Headers Body Auth Set… │
│   ▸ Users                  │    ├───────────────────────────────┤
│     GET /users             │    │ Response  200 · 812 ms · 1.2k │
│                            │    │ Body Headers Cookies          │
└────────────────────────────┘    └───────────────────────────────┘
status bar (24px)   storage mode · history protection   last exchange / last error
```

The left dock defaults to **Collections**, not History: history is a record of
what you sent, collections are what you work *from*, and a client whose sidebar
is a log is a client you cannot navigate. History is reachable as a panel
command-palette entry rather than an always-open list.

Zaku arranges the same window as: project + variables in the left dock,
response in the bottom dock, request editor tabs in the center. We keep the
response in a center `v_split` because there is one request, not a document
per tab; the split handle is what resizes it. The rule they share is that a
dock is a *region*, not a tab: tab bars are for several panels in one region,
and a single-panel region suppresses its title bar entirely.

## How a send flows

```
Send button (or Ctrl+Enter action)
  └─ RequestPanel emits Event::Send
       └─ Workspace::send
            ├─ panes.request.read(cx).current_request(cx)   // live editor state
            ├─ prepare(&request, &VarContext::new())        // pure, in core;
            │                                               // Generators::now() for `{{$…}}`
            ├─ ReqwestTransport::execute                    // background task
            ├─ append_history                               // store
            └─ Document { response, error, pending } notify
                 └─ RequestPanel + ResponsePanel redraw
```

The editor is read at send time on purpose. Raw-body and auth inputs notify
their own `InputState`, not the `RequestEditor`, so a copy stored earlier can be
one keystroke stale. `RequestPanel::current_request` reads the live widget tree.

`prepare` takes the scope (`{{name}}`) and, internally, `Generators::now()`, so a
`{{$uuid}}` gets a fresh value per send while a plain `{{name}}` cannot shadow
it — a `$`-token never consults a scope. `prepare_with` is the test seam that
fixes the clock and the seed.

## Where data goes

`storage.rs::open_store` is the single decision point, and it returns a
`StoreChoice { store, name, workspace_root, history }`:

- A **workspace** — a directory with a `restbook.json` marker — wins. It is
  located by `RESTBOOK_WORKSPACE` (explicit, must be marked) or by walking up
  from the current directory (implicit, like `.git`). Collections and
  environments become files there and can be shared; history lives in the
  gitignored `.restbook/`, in the same encrypted store the data directory uses,
  and is never shared.
- Otherwise the **per-user SQLCipher database** is opened, as before.

The one rule to keep: the choice is made at startup and never changes mid-run.
Panes get an `Arc<dyn Store>` and cannot tell the modes apart — that is the
point, and switching stores live would strand every pane's cached collections.
An "Open Folder as Workspace…" action therefore relaunches (or reopens the
window) rather than swapping the store behind the panes.

The status bar's left chips report the mode and its protection, so the user can
always answer "where did my data go" and "who can read it":

| Chip | Mode | What it means |
|---|---|---|
| the workspace path | workspace | collections/environments are files in that directory |
| `local database` | data directory | everything is in the per-user database, private to this machine |
| `Encrypted` | encrypted history | the database is SQLCipher, keyed from the system keyring |
| `Not encrypted` | plaintext history | a data directory in a build without SQLCipher, or whose keyring failed, writes a readable JSONL file |
| `Not saved` | in-memory history | the history file could not be opened, or a workspace had no encrypted store and refuses to write plain text; this session is lost on close |

The protection chip's tooltip names the reason, because the bar is one line and
a warning glyph alone would rely on color to be read. `HistoryMode` in
`storage.rs` is what the chip matches on; it is decided once, with the store.

`layout.json` always lives in the data directory, even in workspace mode: it is
a UI preference, not something to commit.

## Recipes

### Add a pane

1. `src/panels/<name>.rs`: a struct, an `Event` enum if it needs to ask for
   something, and these impls:
   - `BasePanel` with `panel_name`;
   - `Panel` with `title`/`tab_name`, and `title_bar` when the pane draws its
     own header (see how Request/Response/History return `false`);
   - `EventEmitter<PanelEvent>`;
   - `Focusable`;
   - `Render`.
2. Re-export it from `src/panels/mod.rs`.
3. In `src/workspace/mod.rs`:
   - add a field to `Panes` and build the entity in `Workspace::new`;
   - add a `PanelName` variant, `as_str`, and include it in `parse`;
   - add an arm in `panel_view` (the `panel_handle(...)` match);
   - if it emits intents, subscribe in `listen`;
   - if a command should reach it, add the palette variant and a `run_command`
     arm in `workspace/mod.rs`.
4. Add it to the first-run layout in `install_layout` if it should be open by
   default. The default is History / Collections / Environment as tabs in the
   left dock, Request over Response in the center.

A saved `layout.json` that names a missing pane drops the name; that is
intentional (no registry, no reconstruction).

### Add an editor tab

1. Add the variant to `EditorTab`, `ALL`, and `label()` in
   `src/panels/editor/mod.rs`.
2. Add the widget state to `RequestEditor` (an `Entity<InputState>`, a `Rows`, a
   `BodyKind`-style enum).
3. Fold it into `RequestEditor::request()` — only the selected variant is read,
   so switching tabs is lossless.
4. Render it in a new `render_<tab>_tab(&self, cx)` in `editor/<name>.rs` and
   dispatch it in `RequestPanel::render_tabs`.
5. Teach `RequestEditor::count()` what the tab's label count is.
6. If the widget notifies its own state (a `TextareaState`, an input), the
   panel will not see it; add an observer like the one on `url_input`, or make
   `Workspace::send` read the live tree (it already does).

### Show a `Kv` list as rows or as text

The Params tab and a `Form` body each offer a Table/Raw switch. The table's rows
stay authoritative; the text is generated from them when Raw opens and parsed
back when it closes or when the request is sent (`RequestEditor::request`,
`body`). `editor/kv_raw.rs` owns the field, and `core::kv_text` owns the format
(see `04-backend-guide.md`): one pair per line, `&` accepted, `%XX`/`+` decoded,
and a disabled row written as `# key=value` so the toggle cannot enable a row the
user switched off.

Rules that keep the two sides honest:

- parse before leaving the view, not only at send time, so a body-kind change
  does not discard edits (`fold_form_raw`);
- never keep a second copy of the rows: the text is a view over them, and
  `request()` reads whichever surface is showing;
- the height is `metrics.body_row_height * 8`, so zoom scales it like the tables.

### Show a header the user did not write

Two kinds of header exist and they live in different places on purpose. The user
writes headers in `editor/headers.rs`; a *derived* header is computed from the
request as a whole and is read-only.

- `restbook_core::default_headers()` (`core/src/prepare.rs`) is the engine's
  list — `Accept`, `Accept-Encoding`, `User-Agent`. `prepare_with` appends it
  after the user's headers, skipping any name the user already set
  (case-insensitive), so a default can never override a typed header.
- `Content-Type` belongs to the body, not the table: `default_header_rows()` in
  `editor/headers.rs` picks it from the `BodyKind` (Raw → the selected preset's
  mime, Form → `application/x-www-form-urlencoded`, Multipart →
  `multipart/form-data`). Raw bodies choose a language preset, so the user never
  types a mime.
- The derived list is drawn by a ghost disclosure button (`Default headers (N)`,
  collapsed by default, so the table stays the user's). It echoes the same
  filtering the engine applies: an enabled user row of the same name removes the
  derived row from the preview. Keeping the preview and `prepare_with` on one
  rule is what stops the two disagreeing.
- The transport applies a user `Content-Type` last, so a header typed into the
  table still wins over the body's mime.

### Add an action and a keybinding

1. Declare it in `src/actions.rs`:
   `actions!(restbook, [SendRequest]);`
2. Give the command a row in `panels/command_palette.rs` and the row its keys:
   `Row::bound(Command::Send, "Send request", &SendRequest, &["secondary-enter"])`.
   The `&[…]` slice is required and is where the keystroke is written down —
   `actions::bindings()` reads it, so the palette, the menu, and the keymap
   cannot disagree. List an alias as a second entry (`&["ctrl-tab",
   "ctrl-pagedown"]`). `secondary` is cmd on macOS and ctrl elsewhere. A key that
   must not fire inside an input, or that an input already owns, gets
   `.in_context("!Input")` (a **context predicate**, not a key-context string); a
   context-scoped key does nothing outside it, which is how `ctrl-n` can be
   next-item without stealing the key from an input. `main` installs the whole
   table with one `cx.bind_keys(actions::bindings())` call.
   - An action that is not a palette command — a panel button like `SaveRequest`,
     a kit action like `ShowCompletions` — has no `Command` row, so it is bound
     directly in `bindings()` with `bound(cx, "secondary-s", SaveRequest, None)`
     (or `load` for an action held as a trait object).
3. Handle it on the element that tracks focus: `.on_action(cx.listener(|this, _: &SendRequest, _, cx| ...))`.
4. Guard the effect (`document.can_send()`) rather than trusting that the action
   only fires in the right state. To show the shortcut somewhere, use
   `actions::shortcut` — see [Add a shortcut hint](#add-a-shortcut-hint).

Actions are dispatched from the focused element upward, so a listener on a pane
root also catches keys pressed in an input inside it. If the element is not in
the focus path, nothing fires — that is the scoping mechanism.

### Add a completion surface

Two surfaces exist and they do not share a mechanism, because GPUI 0.7.3 gives
`EditorState` a completion hook and `InputState` none:

- **`EditorState` (the raw body).** Give it a `CompletionProvider` via
  `VariableCompletions` and let the kit's popover own arrows, Enter, Escape, and
  painting. A new editor surface needs the same one-line install, nothing more.
  The kit also ships the `ShowCompletions` action but binds no key for it; the
  app binds `ctrl-space` to it in the `Input && mode == editor` context, and the
  editor's own `on_action` handler does the rest.
- **`InputState` (URL bar, key/value row values).** Add an `Autocomplete` field
  to the owning pane or row, `cx.subscribe_in` its input (`Change` → `sync`,
  `PressEnter` → accept, `Blur` → `close`), wrap the input in
  `div().relative().flex_1().min_w(px(0.))` appending a `deferred(...)` list, and
  route `up`/`down`/`ctrl-n`/`ctrl-p`/`enter`/`escape` from an `on_key_down`
  filter.

Both call the same `suggestions(variables, query)` so the two lists cannot
disagree, and both write the whole `{{name}}` over the partial text. The
reference's end is the caret plus a `}}` immediately after it: the code editor
auto-closes `{{` into `{{|}}`, and replacing only up to the caret would leave
that pair behind, so the accepted reference would end `}}}}`. Accepting
must be deferred a tick (`window.defer`): `InputState::replace` emits
`Change`, which comes straight back to the subscription that called it.

**Enter accepts on all three surfaces, ahead of the binding.** The kit's own
`enter` binding is still the normal path for the two `InputState` surfaces (they
emit `PressEnter`). The raw body is different: it is multi-line, so that same
binding inserts a newline, and a key-down listener runs *after* bindings and
would accept over a line the user just split. So the editor registers a
keystroke interceptor in `RequestEditor::new`:

- it fires only for a bare Enter while the raw editor owns the caret, and calls
  `completion::accept_body_completion`, which routes the action exactly as the
  kit would. If routing answers `false` while the menu is still open — the
  overlay handler is installed by a render pass, and the registry may not hold
  the editor yet — it writes the first item with `EditorState::insert_completion`,
  the same call a click makes;
- only after an accept does it call `cx.stop_propagation()`, so the binding never
  sees the key and cannot insert its newline. With no reference under the caret
  the intercept is a no-op and Enter breaks the line as before.

`url_popup_key` and `value_popup_key` additionally accept the highlighted row
from an `on_key_down` filter when their input's `PressEnter` subscription did
not. A double accept is impossible there too: an accept closes the list, so the
fallback's `is_open` check is already false by the time it runs.

A key/value row owns its `Autocomplete` and finds itself by **input identity**
(`RequestEditor::kv_row_mut`), never by row index, so a removed row drops its
subscription with it. The key field gets no list — a header or parameter name
does not interpolate.

### Add a shortcut hint

The hint is derived from the keymap, not typed:

1. Bind the action in `actions::bindings()` as usual (see above).
2. Call `actions::shortcut(&SendRequest, window)` where you render the hint; it
   returns `Option<String>` formatted per OS (`⌘⏎` / `Ctrl+Enter`).
3. For a palette command, give its row in `panels/command_palette.rs` the
action (`Row::bound`); a command with no binding uses `Row::plain`. The
formatted shortcut is still computed, never stored.

A hint with no binding is `None`, so a navigation-only palette command renders
blank instead of inventing a key.

### Show an empty state

A blank pane names what would fill it and offers the next actions, rather than a
lone sentence. Use `panels::empty`:

1. `empty_state` centers a capped block (the Response and History panes);
   `empty_state_inline` is the same block padded to the pane edge with no fixed
   width, for a narrow pane (the 260px Collections sidebar).
2. Give it a short title (`Nothing sent yet`), an optional one-line body, and a
   `Vec<Action>`; each `Action` is a label plus the hint from
   `actions::shortcut(.., window)`, so the keys read per OS.
3. `empty_state` also takes an optional footer for a control the pane already
   owns (the response pane's “Earlier sends”).
4. Rows share one width so their shortcuts line up. Each row opts in to an
   `empty-action-<label>` `debug_selector`, which is what a render test asserts
   on; `debug_bounds` needs a `&'static str`, so query the tree with
   `preview::tree::bounds_tree` and a literal selector list.

The same panes' error states stay single lines: an error is a fact, not a task,
and a filtered list's “No matches.” is a result, not a first run.

### Render a large list or a large body

Three surfaces meet more data than fits on screen, and they share one rule with
two different bounds.

- **A list virtualizes; it is bounded by rows.** Collections and History build a
  `uniform_list` over a flattened row vector, so only the visible slice is laid
  out. Collections flattens into `FlatRow`s cached by `(query, revision, sort)`
  in `flat_cache` (`panels/collections/mod.rs`), so an imported tree of
  thousands of nodes is walked once per mutation, not once per repaint; History
  caps at `history_limit` (up to 1000). The kit's `TreeState` (`gpui-base`)
  virtualizes the same way — `uniform_list` over a flattened `entries` vec — so
  migrating Collections to `Tree` keeps this cost model; the difference is that
  `set_items` clears selection, so reselect by item id, not a stored index.
- **A body is capped; it is bounded by bytes and lines.** The response viewer
  clips to `util::MAX_BODY_BYTES` (256 KiB) before drawing — the full record is
  kept, only the view is bounded — and `MAX_NUMBERED_LINES` (3000) picks the
  shape: pretty JSON under the limit is a `uniform_list("response-body-lines")`
  of numbered rows, while raw or over the limit is one `SelectableLine` block.
  Decode, pretty-print, line split, highlight slicing and the `widest_line`
  measure are computed once into `DisplayCache`, keyed by `(ptr, len, pretty)`,
  so a scroll repaint borrows them instead of redoing the work.

Same rule both places: do the proportional-to-size work once per change, cache
it, build only what is on screen. The difference is what bounds it — rows
virtualize, so a list needs no cap; per-item work that grows with the payload
(splitting, measuring, highlighting) does, so a body is clipped. When you add a
surface:

- Give `uniform_list` a fixed row height and a `'static` closure; flatten into a
  cached `Rc<Vec<_>>` keyed by what invalidates it, never inside the render.
- Cap input whose per-item cost scales with its size, and say on screen what was
  clipped (`Showing the first N lines.`).
- A menu whose rows come from user data still needs `.scrollable(true)`; its
  default caps at half the window height (see [Change the look](#change-the-look)).

### Change the look

- Colors come from `Palette` (`src/theme.rs`), built once per pane from the
  active `gpui-component` theme. Add meaning there (`method()`, `status()`), not
  inline `Hsla` values.
- The theme file drives the controls too, not just the panes.
  `apply_appearance` switches the kit theme, then `theme::sync_component_theme`
  lays the file's roles over the live `Theme` — surfaces, text, borders,
  `list_*`, the chrome hairline, the status accents, the primary fill (both
  `primary` and the `button_primary` token), `popover`, `caret`/`selection`,
  `scrollbar*`, and `tab_foreground`, so a `Button`, `Input`, menu, `Dialog`, or
  `TabBar` draws the same palette the panes do. A role the file omits keeps the
  kit value; add the role to `ColorRoles`, the file, and the mapping together.
  The `--preview` root runs the same two calls before it builds its panes.
- Shared formatting (bytes, duration, cookies, JSON pretty-printing) belongs in
  `src/util.rs` with a unit test; do not test view code through the widget tree.
- Layout numbers are view-local, and a pane that cannot shrink **scrolls**. Give
  the body `flex_1()` with its own floor at zero (`min_h_0()`/`min_w_0()`) and an
  overflow style, so the dock shrinks the scrollport and the content scrolls
  instead of clipping. A custom scroll region uses `overflow_*_scrollbar()`
  (gpui-component's `Scrollable`), which draws a scrollbar only when the content
  overflows; keep the hidden `overflow_*_scroll()` only where the wrapper would
  cap the content (the response body, which needs max-content width to scroll
  sideways — see the gotcha below) or take over a handle the caller drives (the
  request tab strip's `scroll_to_item`). A `PopupMenu` whose rows come from user
  data — the collection switcher, the environment switcher — sets
  `.scrollable(true)`, because its default caps at half the window height and
  clips the rest; a fixed menu never needs it.
- The components are `gpui-component`; the imports that matter are
  `component::button::*` (for `.primary()`, `.ghost()`, `.danger()`),
  `component::input`, `component::tab`, `component::menu` (`DropdownMenu`,
  `PopupMenu`, `PopupMenuItem`), and `Disableable` for `.disabled()`.

### Choose an icon

Every glyph is named once, in `src/icons.rs`:

- `IconName` there is `gpui_kit::assets::IconName`, the complete Lucide catalog.
  The default component bundle (`gpui-kit-assets`' `default-icons.txt`) already
  carries most of what the app uses — `PanelLeft`, `PanelRight`, `CircleCheck`,
  `CircleAlert`, and the status-bar chips among them — so those need nothing
  extra. `main.rs` still installs `gpui_kit::assets::AllAssets`, because a handful
  of names the app relies on are **not** in that default list: `Braces`,
  `ClipboardCopy`, `Clock`, `FileUp`, `Pencil`, `Trash`. Before naming a new
  glyph, check it against `default-icons.txt`; if it is absent, it only resolves
  because of `AllAssets`. A name with no SVG under the asset crate's
  `assets/icons` paints **nothing**, silently — a typo is an invisible icon,
  not an error.
- `icons::SMALL` (12) is a glyph deliberately smaller than its line: the tab
  close button, a search prefix, a status-bar chip. `icons::MEDIUM` (14) is a
  pane title. Everything else uses `Icon::new(name)` with no size, which
  inherits the adjacent text size and therefore zooms with it.
- `icons::title(glyph, "Label")` is the pane-title shape; do not repeat the
  `h_flex().gap_1().items_center()` wrapper in each pane.
- `icons::quiet` sets `palette.subtle` for a glyph that labels its row. A glyph
  that carries meaning (a danger state, a method) sets its own color.
- One glyph, one meaning: `Pencil` renames, `Trash` deletes, `Braces` is the
  variable scope, `Folder`/`FolderOpen` a directory, `Globe` an environment,
  `Clock` the send history, `CircleAlert`/`CircleCheck` an outcome.

### Add a status/toolbar item

There is no window-level toolbar. A pane's controls belong in the pane, above
its content (the request row, the response summary). Dock title bars are
suppressed with `title_bar(false)` so they cannot become a second toolbar. If a
shared toolbar is wanted later, the Zaku shape is the target (a
`ToolbarItemView` trait with location enums and `contribute_context` so toolbar
widgets can own keybindings), not a row of buttons in `Workspace::render`.

The one window-level place a text label belongs is the status bar. Its contents
are `chrome::Status`, built by `Workspace::status` from the document; add a
field there rather than drawing a per-pane footer. The titlebar carries no
text about the open request: its label and URL are already in the request tab
strip, and the last-sent URL is in the status bar.

A number the response already prints does **not** belong there. Status, duration,
and size live in the response header, beside the body they describe; the footer
holds only what the pane cannot say (see U43). If you add a field to `Status`,
ask first which pane would print the same thing.

### Add chrome (titlebar / status-bar content)

`src/workspace/chrome.rs` holds both. They are plain functions returning an
element, not panes, and that is deliberate: chrome is drawn for every layout
the user can build, so it cannot be dragged away, and it holds no engine state
of its own. To change what the titlebar shows, edit `chrome::titlebar` and the
values `Workspace::render` passes it; to change the status line, edit
`Workspace::status` and add a field to `chrome::Status`. The titlebar's drag,
double-click-to-zoom, and window controls are `gpui_kit::component::TitleBar`'s
— do not hand-roll them, and keep `main.rs` on `TitleBar::window_options()`.

The titlebar is not a tab strip, by decision (see `docs/02-ui-plan.md`, U13/U14):
the open-request strip lives in the request pane.

### Report the result of an action (the flash)

A mutation the user cannot see the answer to — saving a request, creating a
collection, deleting a node — is reported by a *flash*. It ends up in the kit's
**notification layer**, because the usual route is unavailable: `Ctrl+S` and the
tree's menus reach the workspace through `cx.subscribe`, and a subscribe
callback is handed **no `Window`**, so `window.push_notification` cannot be
called on that path.

The workspace therefore *parks* the message and `render` issues it. Panes report
by emitting `Notice` (defined in `src/notice.rs`, carried by
`CollectionsEvent::Notice` and `RequestEvent::Notice`) — `Done` for a success,
`Failed` for a store error — and `Workspace::notice` routes it through
`chrome::Flash::from_notice` to `Workspace::flash`, which stores it in
`pending_notice` and asks for a redraw. `render` drains that into
`Workspace::issue_notice`, which is the only place with a `Window`.

Two rules that follow from the parking seam:

- **One id for every workspace notice** (`Notification::id::<WorkspaceNotice>`),
  so a second message replaces the first instead of stacking. That is the
  behaviour the hand-drawn toast had, and what a user expects of a confirmation.
- **An undo offer sets `autohide(false)`**; the kit keeps a notification with an
  action up until it is acted on or closed. The old hand-drawn toast needed its
  own timer for this and no longer does.

A notification is a window-level layer, drawn **above dialogs**. That is the
reason for the move: the hand-drawn toast was part of the dock tree, so an open
dialog covered it and a save reported from behind one was invisible.

When you add a mutating action: have it emit `Notice` on the outcome rather than
calling `push_notification`, and report the store's own error text on failure.
One message, one lifetime, one place it is issued.

The rules, restated as a checklist for a new action:

- **Reach the workspace through `Notice`.** A save, create, delete, or rename
  that a pane owns emits `Notice::Done` or `Notice::Failed`; `Workspace::notice`
  routes it through `chrome::Flash::from_notice`. Do not call
  `window.push_notification` from a `cx.subscribe` callback — it has no window.
- **Anchor on the workspace, never a pane.** A toast drawn inside a dock panel
  is clipped to that panel.
- **One lifetime.** `FLASH_DURATION` (2600 ms) clears the message; setting a new
  flash replaces both the message and its timer (`Workspace::flash`,
  `Workspace::set_flash`), so two quick actions cannot clear each other.
- **Confirm a visibility change by reading live state, then flash the result.**
  A control that opens or closes a dock or pane can look like nothing happened
  if the dock lands off-screen or a stale layout fights it. Do **not** flash
  success on the assumption the reveal worked: re-read `is_dock_open`, flash
  `Success` with a plain "shown" message when it did, and `Error` with a
  distinct message when it did not. `Workspace::toggle_tools` is the pattern
  (`reveal_panel`, then `is_dock_open(Right)`, then the branch at the flash);
  `toggle_sidebar` should adopt the same live read. See `docs/07-flows.md`,
  [Dock-toggle confirmations](07-flows.md#dock-toggle-confirmations).

### Ask before an action (a dialog)

A flash reports what happened; sometimes the action needs an answer first. The
`Save request` dialog (`Save As` on a draft, which has no name and no parent) is
the pattern. It lives in `workspace/save_dialog.rs`, apart from the tab model it
saves into:

1. **Set a flag in the handler, open nothing.** An event handler here
   typically has no `Window` — `Ctrl+S` arrives through `cx.subscribe`, and
   `RequestEvent::Save` carries no window either. Do the check you *can* do
   (nothing to save into at all) and flash the error; otherwise set
   `pending_save = true`.
2. **Consume the flag in `Render` and `window.defer` the open.**
   `window.defer(cx, |window, cx| …)` runs after this frame's element tree is
   laid out, which is exactly when `window.open_dialog` wants to be called.
3. **Keep the inputs and the choice alive.** The dialog content closure is
   rebuilt on every notify; an `InputState` constructed inside it is a new entity
   each frame, and the caret jumps to the start on the first keystroke.
   `Workspace::save_name` is one `Entity<InputState>`, prefilled before the
   dialog opens; the destination choice lives in the picker's own
   `Entity<SelectState>` (`workspace/destination_picker.rs`), created once per
   open and captured by the closure — the same rule the export picker follows
   with `export_state`.
4. **Compose the footer yourself.** A plain `Dialog` draws no buttons; wrap
   the actions in a `DialogFooter`, and use `.on_ok` so Enter confirms as well
   as the Save button.
5. **Return `false` to keep the dialog open.** The confirm closure's `bool` is
   "did this succeed?" — an empty name returns `false`, and the field stays up
   with the cursor in it instead of closing on nothing.

A save's destination is a picker, not the sidebar's highlight: the highlight is
how a user navigates, and a save that depended on it would refuse exactly when
nothing had been clicked. `CollectionsPanel::destinations()` returns every
collection and folder as `Destination` rows; the first one is the prefill when
nothing is selected (`tree::default_destination`).

The picker is a `select::Select` over a `SearchableVec`, not a menu: a large
import can leave thousands of folders, and a menu builds one view per row on
every open. The select paints only the rows on screen and filters as the user
types, so its cost does not grow with the workspace. `open_destination_select`
builds the entity, `destination_picker` renders it, and `selected_choice` reads
the committed row at confirm time.

*New collection…* is the first row of that picker, not a separate flow: it turns
the same dialog into a name dialog and the confirm creates the collection and
saves the request into its root (`Workspace::commit_save` takes the collection
name and creates it first). It is also what a first run gets, because a
workspace with no collection has nowhere else to send the save.

The move dialog is the same picker without the name or the *New* row: one
`Destination` is one request's home, so `workspace/destination_picker.rs` builds
it for the save, move, and import dialogs and the lists cannot drift apart. A
move writes the target collection first and the source second
(`CollectionsPanel::move_request`, over `tree::move_to`), so a failure between
them leaves the request in two places rather than in none, and nothing reloads
between the two writes — a reload would hand the second write the store's copy
of the first, putting the request back where it started.

The window menu is the other half of the placement rule. It keeps commands about
the *window* — the palette, settings, the panes it shows, reset, diagnostics — and
gives up anything that belongs to a pane: save, import, and export are verbs
about collections, so they sit in the Collections pane's `+` menu. The request
pane's own ellipsis went with them; every verb it held was already on the strip
or in the palette.

A rename that is not a creation does not deserve a modal: the collections pane
puts a Rename button in the row tools that shows an inline input in the tree.
Both paths end in the same place — `request.name` is a **label**, so renaming a
saved request changes `SavedRequest.name` and leaves `request` (the `RequestDef`
with the URL) alone. The tree, a later rename's prefill, and the tab title all
read the label.

### Add an app-level setting

App preferences are `workspace/settings.rs` — an observable `Settings` entity
over an `AppSettings` struct saved to `settings.json` in the data directory. The
rule is the same as the layout file: plaintext, hand-editable, and every field
falls back to a `Default` that matches the behavior the app had before the file
existed, so a missing or malformed file changes nothing.

To add one:

1. Add the field to `AppSettings` with a `Default` that is today's behavior.
2. If it is read by a pane, hand that pane the `Settings` handle and `observe`
   it, so a change redraws the pane (the response pane's `pretty` and the
   history pane's limit are the two examples).
3. Draw a control for it in `settings_view.rs`, reading `settings.current()`.

A setting that is *about one request* (timeout, redirects, TLS) is not here — it
belongs to the request's Settings editor tab, where it travels with a shared
collection. `settings.json` is deliberately only for things that are neither
request-shaped nor layout-shaped.

### Why Settings is a dialog

The Settings screen is `window.open_dialog`, the same mechanism as the command
palette, opened from the titlebar menu and the palette. It is *not* a second OS
window and *not* a dock pane, and that is a decision, not a shortcut:

- The workspace is one entity per window and owns every value a settings screen
  reads (the store, the workspace root). A second window would have to duplicate
  that state or reach across windows to it; a dialog is drawn by the workspace,
  so it reads the live values with no plumbing.
- A dock pane would be clip-able, drag-able, and closable into a state the user
  cannot get back; a dialog is dismissed with one Escape.

While the dialog is open it reads the live `Settings` entity, so a toggle is
visible to every observing pane before the dialog closes.

The dialog is a **category rail over one page**, the way Zed presents
preferences: `General` (appearance, theme, zoom, response formatting, layout),
`History` (search limit and retention), `Privacy` (clear history, clear
cookies), `Updates`, `Storage` (the paths in use, plus a folder picker that
makes a folder into a workspace when the app is not already in one), and `About`
(the branding identity). The kit's `Settings` component supplies the rail, the page header,
the per-page search field, and the reset control; each page is composed from
`SettingItem`s so a control is a row, not a new layout.

## Per-frame cost

GPUI does no retained-mode caching. The element tree you return from `render` **is**
the scene: every element is laid out and its primitives batched on each frame
(`gpui-pre-*/src/scene.rs`, `element.rs`). There is no dirty-diffing to save you.
A scroll repaints continuously, so any cost in `render` is paid many times a
second. This is the one performance rule in this app:

> **`render` must be O(visible), not O(data).**

The engine is not the variable. Zed is fast because Zed's panes are written this
way, not because its renderer is different — this app uses the same engine,
vendored as `gpui-pre`.

### How to test for it

Before writing a loop that builds elements from a collection, ask what its length
is in the worst case. The failure mode is invisible at demo sizes and only appears
on real payloads, so a quick test with a small body proves nothing.

1. **Read the loop.** Any `for`/`map`/`children` whose iteration count comes from
the data (lines in a body, rows in a table, nodes in a tree) is the suspect.
2. **Test with a payload that is large on purpose.** For the response pane: send
   a request returning a few hundred KB (a real API, or a saved file), then scroll
   it. Test it in **both** directions — smooth at the top and still smooth after
   scrolling a few thousand rows. A list that builds everything is fast-looking
   until it has to scroll.
3. **Suspect work that is not element count.** Decoding, parsing, pretty-printing
   and sorting are the same bug in a different costume: anything in `render` that
   derives a value from the data rather than reading a cached one. This is what
   `util::body_text` did — a `serde_json` round-trip per frame.
4. **Compare payload sizes.** If a 2 KB response is smooth and a 200 KB one is
   not, the cost scales with data and this is the bug. If both are equally slow,
   the cost is constant and the problem is elsewhere.

### The three fixes

- **Virtualize.** Replace `.overflow_scroll().children(rows)` with
  `uniform_list(id, item_count, |range, _, _| ...)`, which renders only the visible
  range. Rows must be a fixed height — `UniformList` measures one item and places
  the rest (`gpui-pre-*/src/elements/uniform_list.rs`); a variable-height tree has
  to be flattened into a row list first, which is what `CollectionsPanel::flatten`
  does: one `FlatRow` per visible collection, folder, or request, already carrying
  its depth and the ancestors the row menu needs. To move data into the closure,
  note it is `'static`: clone `Rc<Vec<FlatRow>>` (a refcount bump) rather than the
  rows.
- **Cache derived values.** Keep the last result in the pane, keyed on something
  that changes when the input does. `ResponsePanel::display` keys the decoded body
  on `(body.as_ptr(), body.len(), pretty)` — sound because the record is replaced
  wholesale on each send, so a stable pointer means the same response. Prefer a
  key over a flag, so a stale cache cannot outlive its input.
- **Cap and label.** If the data is genuinely unbounded, bound it and say so in
  the UI. Two separate caps do this in the response pane: `util::body_text`
  truncates at `MAX_BODY_BYTES` and the pane shows "truncated for display", while
  `body_view` drops line numbering past `MAX_NUMBERED_LINES` and renders one
  nowrap block instead.

### Do not

- Do not "optimize" this by moving work to the store, to a background task, or by
  reaching for `--release` speed. The bug is the per-frame work, not the constant
  in front of it: release shrinks the constant, it does not change the curve, and a
  large enough payload will still stall. Fix the shape, then measure if you must.
- Do not assume the GPU. A GPU-bound problem looks the same at every payload
  size, so if a small response is smooth and a large one is not, the GPU is not
  the answer. On Windows the engine renders through Direct3D (`gpui_windows::
  directx_devices`), not through the `wgpu` context in `gpui-pre-wgpu` that the
  Linux/macOS path uses — so read the log (see [Logging](#logging)) before
  reasoning about which renderer is in play.

## Persistence

Both the dock layout and history live in the per-user data directory:
`%APPDATA%\restbook` on Windows, `~/Library/Application Support/restbook` on
macOS, `$XDG_DATA_HOME/restbook` elsewhere. `RESTBOOK_DATA_DIR` overrides it,
which is how a portable or test build gets its own sandbox.

`workspace/storage.rs` decides that directory and which store serves history: a
fresh `SqlcipherStore` (the default build), falling back to plaintext
`JsonlStore` in the same directory when SQLCipher is compiled out or the keyring
is unavailable. The status bar names that fallback instead of leaving it to
stderr, which a GUI user never reads. A legacy `.restbook/history.jsonl` from an
older working-directory build is imported once — into the database when it is
new, or copied next to the new JSONL file.

A `restbook.json` workspace wins over that directory (`open_store`), and its
history goes through the same `open_history` with one difference: the workspace
directory is `<workspace>/.restbook/`, and plain text is not an acceptable
fallback there. When SQLCipher cannot open, the session's history stays in
memory and the status bar says `Not saved` — writing a readable file into the
tree a user shares is the one outcome the encrypted store exists to prevent. A
plaintext `history.jsonl` left by an older build is imported into the database
once and then deleted, not renamed: an `.imported` copy beside it would keep the
leak open.

`workspace/persistence.rs` writes the whole `DockAreaState` to `layout.json` on
a debounced layout change, and flushes on window close. `Workspace::flush` is
the single close path: it runs before the platform destroys the window (and
again from the quit observers, for a quit that closes no window first), stops
the debounce timers, cancels a request still on the wire, and writes the live
draft synchronously. It is idempotent and gated on pending work, so a close
never writes a layout or draft that the debounce already wrote. The file is an envelope:
our `layout_version` plus the library's state. On startup the saved tree is read
for **panel names and the left dock width**; the workspace re-adds the same
entities and calls `set_dock_size`. Everything else in the file (split sizes,
open state) is rewritten on the next save but not replayed. Do not switch to
`DockArea::load`: it rebuilds panels from a registry, which this app deliberately
avoids. Extending the file (window bounds, open items) means adding fields next
to the dock state, not changing how panes are constructed.

`layout_version` exists because a saved layout outlives the app that wrote it.
A file from an older version is discarded rather than migrated, so changing the
default arrangement cannot leave a user stuck on the old one — without it, a
sidebar saved as History keeps restoring History after the default became
Collections, and nothing in the UI says why. Bump `LAYOUT_VERSION` whenever the
default arrangement changes; the user gets the new default and a fresh file.

## Gotchas

- `gpui_kit::component::dock::Panel` is an extension trait; the base trait with
  `panel_name` is `dock::BasePanel`. A pane implements both.
- `DockLayout::panel(entity)` stores a bare entity; the skin then cannot read
  the presentation trait and falls back to `panel_name`. Use
  `panel_handle(entity)` + `tabs_layout`/`add_panel_view` whenever `title` or
  `title_bar` matters.
- `cx.emit(...)`, not `this.emit(...)`.
- `cx.listener` returns an opaque `Fn` that is not `Clone`; wrap it in `Rc` when
  passing it to a shared widget (`kv_table::render` does this).
- `InputState::set_value(value, window, cx)` needs a `Window`; an observer does
  not have one. `RequestEditor::pending_url` + `flush_pending` is the pattern;
  `pending_request` (used by history restore) rides the same flush, and the URL
  must be written *last* because its notification re-mirrors the editor into the
  document.
- `.disabled()` needs `Disableable as _`; `.ghost()/.primary()/.danger()` come
  from `component::button::*`, not from `Button` alone.
- Menu items have no entity handle. Capture `cx.entity().downgrade()` and
  `update` it inside `on_click` (see `ResponsePanel::render_format_switch`).
- `#[test]` inside a module that does `use super::*` hits the GPUI `#[test]`
  macro and the recursion limit. Use named imports (see `util.rs` tests).
- `WeakEntity::update` returns `Result`; `Entity::update` returns the value
  directly, so `.ok()` on it is a type error.
- Some inputs (`TextareaState`, auth fields) notify themselves, not the editor.
  If a value updates late, look for the missing observer before blaming the
  model layer.
- A library action can be bound as-is — `ShowCompletions` from
  `gpui_kit::base::input` is the one example — because the widget that renders
  the element already registered the handler. Declare an action in `actions.rs`
  only when the app itself is the handler.
- Key bindings dispatch **before** `on_key_down` listeners, and they are
  context-scoped. That is why `ctrl-n`/`ctrl-p` are bound in the palette's
  `"Command"` context instead of globally: a global binding would fire while
  typing anywhere. `div().on_key_down` is bubble-only and cannot swallow a key;
  `capture_key_down` or a context-scoped binding can.
- A pane-scoped key is two halves that must agree: `.key_context("Collections")`
  on the pane's root (the `track_focus` element) and a binding whose predicate
  names the same word. The context only applies when the pane holds focus, so a
  pane key that never fires usually means the click that selects a row did not
  also call `window.focus(&focus_handle, cx)`.
- A binding's context argument is a **predicate**, not a key context string.
  The grammar spells a key/value pair `mode == editor`; the element declares it
  as `Input mode=editor`, and copying that string into `KeyBinding::new` panics
  at startup with `unexpected character ''m''` (the parser stopped after
  `Input`). `KeyBinding::load` parses eagerly and unwraps, so every row's key
  is built through `actions::bindings()` in a unit test instead of at first
  launch.
- A Windows close produces a few `ERROR` lines without a target in
  `gpui-pre` itself (`window not found`, `Invalid window handle`):
  `WindowsWindow::drop` destroys an HWND the OS already destroyed for an
  OS-initiated close, and a window-scoped task can wake after its window was
  removed. `Workspace::flush` marks the process as closing and the log sink
  (`logging::Closing`) drops target-less errors from then on, so a clean
  shutdown ends at `window closing; last state written` and no state is lost.
- `Root` binds `tab` to `focus_next` before any `on_key_down` runs, so a list
  cannot accept on Tab from an `on_key_down` filter alone.
- `Kbd::format(&Keystroke)` is for display (`⌘⏎` on macOS, `Ctrl+Enter`
  elsewhere); `Kbd::new` builds a widget. Hints read the binding through
  `window.highest_precedence_binding_for_action` so they cannot drift.
- A dialog's content closure is rebuilt on every notify. An `InputState` made
  inside it is a new entity each frame, so the caret resets after one keystroke;
  create the input on the owning entity and clone its handle into the closure.
- `window.open_dialog` wants to run after the frame is built, and event handlers
  (`cx.subscribe`, `RequestEvent`) have no `Window`. Set a `pending_*` flag in
  the handler, then `window.defer` the open from `render` — see
  [Ask before an action](#ask-before-an-action-a-dialog). The update offer takes
  the same route: the launch check has no window, so it parks `pending_update`
  and `render` defers the dialog. A pane never places an overlay itself; it
  reports the intent.
- The tab list can be **empty**. `active_tab` is only a valid index when
  `!tabs.is_empty()`, so every read is a `get` (or `ensure_active_tab` when an
  action needs to revive the editor). Closing the last tab calls `adopt_active`
  with no tab, which clears the editor and lets the request pane draw its
  “No request open” state instead of describing a tab that is gone.
- A pane event's log line comes from its `summary()`, not from a `log::info!` in
  the subscriber. Annotate the `cx.subscribe` closure's event parameter
  (`event: &CollectionsEvent`) once you use it beyond the `match`; GPUI's
  callback is generic, so `|this, _, event, cx|` otherwise reports `E0282`.
  `ResponsePanel`'s summary logs the tab *index*, not the label: pulling the
  label out with `ResponseTab::ALL.get(*index).copied()` moves the `E0282` to the
  `if let` binding.
- The response pane's history control lives in the **response header**, so it
  only exists when the tab holds a response. A saved request that is reopened
  has a request but no response, while its rows are still in the store; the
  idle state therefore has its own “Earlier sends” control
  (`render_earlier_sends`), or closing a tab would look like it deleted the
  history.
- `RequestPanel` folds the live editor into the document on every editor
  notify, but an adopted request is only *queued*
  (`RequestEditor::pending_request`) until the next render.
  `sync_to_document` returns early while `RequestEditor::has_pending()`, or the
  fold writes the previous request back over the one being opened — which
  relabelled the tab “(new request)” and gave the history a blank query for that
  frame.
- Scrolling is slow on a large response but fine on a small one: that is per-frame
  O(n) work, not the GPU. See [Per-frame cost](#per-frame-cost).
- A scroll container's content size is the union of its **children's bounds**,
  not of the glyphs that overflow them. So `overflow_scroll` around nowrap text
  only scrolls horizontally if the child keeps its max-content width: the
  container needs `flex().flex_row()` (a plain `div` is `display: block`, where
  the child stretches) and the child needs `flex_none()` (a shrinkable flex item
  stops at the scrollport). The raw body block is the one place that relies on
  this, and the response pane's render test asserts it.
- The status bar is not a second response header. Status, duration, and size are
  printed once, in the response pane's own header; the footer carries the
  storage chip, the history-protection chip, the last sent URL, and engine
  messages (`chrome::Status`). Adding a number to both is the duplication U43
  removed.
- `gpui_kit::component::IconName` is the component's default list (~100 glyphs).
  Anything outside it — `Pencil`, `Trash`, `ClipboardCopy` — comes from
  `crate::icons::IconName` (the full Lucide catalog, embedded by `AllAssets`);
  a name missing from the embedded set paints *nothing*, with no error.

## Next: completion, generators, and keys

What is not done, with the seam each patch would use. This doubles as the
`U34`-`U38` list in [`02-ui-plan.md`](02-ui-plan.md).

| Improvement | Where | Why it is worth it |
|---|---|---|
| Collection / Request scopes in completion | `core/src/vars.rs` `scoped`/`scoped_names`, `variables.rs` `secret_of` | Only Global + Environment produce names, so a collection-scoped `{{baseUrl}}` is invisible; the inspector has the same gap |
| `Tab` accepts a completion | a `"Completion"`-scoped `KeyBinding`, or `capture_key_down` | Blocked by `Root`'s `tab → focus_next`; needs the same scoping trick `Ctrl+N` uses |
| Scroll to the selected completion | both lists' `defer`ed container + a `ScrollHandle` | Fine at 9 generators, broken once a faker-style list lands |
| More generators toward Yaak | `core/src/generator.rs` `call`/`describe`/`names` | Yaak offers `uuid.*`, `timestamp.*`, `hash.*`, `hmac.*`, `base64.*`, `url.*`, `json.*`, `regex.*`; the existing nine are the common request-header cases only |
| Request phase timings | `core/src/transport.rs` (already records `sent`/`headers_received`/`received`) rendered in the response summary | Local-only by scope: restbook records no telemetry, so this is a display feature and nothing else |
| Horizontal scroll in the numbered body | `panels/response.rs` `body_view` | A long single line is clipped in the pretty view: fixed-height rows cannot wrap, and a `uniform_list` is sized to its viewport. Raw already scrolls |

Yak-sized generator sets are cheap because a generator is a `match` arm over a
name returning a string; the expensive part is the editor UI around 140 names,
which is why the scroll-to-selection row is listed next to them.

Save-path follow-ups, from U39. None of these is a model change:

- **`Save As` on a bound tab.** `Ctrl+S` on a saved request overwrites its node,
  and there is no way to fork it into a second node under a new name; the draft
  dialog can be reused, with the destination defaulting to the same parent.
- **Say why an empty name was refused.** The confirm closure returns `false` so
  the dialog stays open, but nothing in the dialog tells the user that the name
  is the problem; an inline hint under the field is one element.
- **A rename shortcut.** Rename is a row-tool button on selected nodes; a
  bindable action (`F2`) needs the selection, which the collections pane already
  owns.

## Reference: Zaku

`.refs/zaku` holds a shallow clone of [buildzaku/zaku](https://github.com/buildzaku/zaku),
a Zed-derived GPUI API client — the same shape as this app, one generation
ahead. It is gitignored and not a dependency. Useful paths when looking for a
pattern:

| Zaku | What to read it for |
|---|---|
| `crates/zaku/src/zaku.rs` | window composition: which pane goes in which dock |
| `crates/workspace/src/dock.rs` | a dock holds one panel; left/bottom placements only |
| `crates/workspace/src/pane.rs` | the tab bar lives in the center editor, not in docks |
| `crates/request_editor/src/request_editor.rs` | method dropdown, tab counts, KV sections, invalid-request state |
| `crates/response_panel/src/response_panel.rs` | Body/Headers/Cookies tabs, summary line, cookie parsing |
| `crates/workspace/src/toolbar.rs` | a toolbar built from pane-contributed items |
| `crates/workspace/src/item.rs` | multi-tab items (`Item`, `ItemHandle`, serialization) |
| `crates/workspace/src/status_bar.rs` | left/right status items |

Zaku is **AGPL-3.0**. Read it for architecture and UX; do not paste source into
this MIT/Apache-2.0 project.
