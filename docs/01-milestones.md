# restbook — milestones

The single place to cut a ticket from. `02-ui-plan.md` owns the rules a pane has
to satisfy, `03-backend-plan.md` owns the pipeline and crate boundaries, and
`08-qol-backlog.md` owns the closed record of what used to feel unfinished; this
file owns *what ships when*.

## Status and release lanes

One place to see what ships when. A row is usually several pull requests; a
version bump is still only ever made in the release commit (`docs/10`).

The order follows one question: **does the item create, mutate, or restore a
tab↔collection binding?** Nothing that does may ship before `0.2.0`; everything
that does not is free to land earlier, which is why most of what used to sit in
`0.2.x` now has a `0.1.x` slot. An item moving between lanes must keep that
test, or `0.2.1` stops being the only place bindings change shape.

| Version | Lane | Contents | Done when |
|---|---|---|---|
| `0.1.3` | small fixes | Settings › Updates downloads, verifies and opens the update; one release channel. That pair was planned as `0.1.2` and is the tree `master` already holds; the version was never cut, so it ships here. Then: menu trimmed to its commands (the View rows duplicate the edge toggles); Settings › Storage gets a folder picker instead of the *"add a restbook.json marker to a folder"* instruction; `Tab` accepts in the completion lists (U35); Updates says `GitHub releases` instead of printing the URL; the environment chip stops truncating to `No enviro…` and moves out of the Collections pane into the titlebar, leaving that pane collection-only; the response copy controls move from the header strip into the panel body | A `0.1.1` install upgrades in place from the published installer, `latest.json` points at the tag, and one before/after capture per visible item; no setting row remains an instruction |
| `0.1.4` | response pane, and the canvas made tractable | Find in the body (U56): a query field with match count and next/previous over the pretty rows *and* the raw block; stop parsing every `set-cookie` header on each frame just to size a badge (`panels/response.rs:1013` → store the count with the record); stop cloning the row set every frame (`panels/response.rs:951`). **The workspace toast moves to the kit's notification layer**, deferred from `0.1.3`, which is what puts it above dialogs. **An update offer that does not need Settings**: a launch check that finds a newer release opens a dialog — *Install now* / *Later* / *Skip this version* — reusing `Updates::install`, which already downloads, verifies the `sha256` and hands the package to the OS. A skip is remembered (`settings.json`), so it does not nag. Auto-**install** without a prompt stays out until builds are signed (`10-release-and-updates.md` R4). **Then the test harness, and no split.** `crates/app/src/test_support.rs`: `init_ui` (theme installed, motion reduced) and a `DataDir` guard that points `RESTBOOK_DATA_DIR` at a temporary directory — what makes a full-window test possible, since `Workspace::new` opens the real store otherwise. That is why the dock tests in `workspace/mod.rs` build panes by hand today. Behaviour-neutral for the app | `--preview response` captures; a test that the cookie count is computed once per response, not per frame; no toast can render under an open dialog; an available update offers itself on launch and a skipped version does not; the full suite passing |
| `0.1.5` | the canvas split, whole | `workspace/mod.rs` into five modules — updates, delete/undo, tabs, persistence, dock/flash (`08-qol-backlog.md` 6.5). **One step per pull request**, merged in that order, each behaviour-neutral and with the suite green before the next: a 1,600-line move reviewed in one diff cannot show a relocation from a rewrite. The split is its own release rather than the tail of a feature lane, which is what let it slip inside `0.1.4`. Its worked precedent is Zedis's own `src/root.rs` (1,440 → 946 lines, `docs/agents/refactor-plan.md`). Everything `0.1.5` used to hold — raw editors for `Form` bodies and params, single-cookie edit/delete (U58) — moves to `0.1.6` | The full suite passing unchanged across every step, and `impl Workspace` at roughly 900 lines of code at the end; no capture, because nothing visible changes |
| `0.1.6` | request editing, history scoping | Raw editors for a `Form` body and for the params list, over a text↔`Kv` round-trip in `core` (`kv_text`: repeated keys and percent-encoding survive); edit or delete a single cookie (U58). **The right-dock History pane takes over the per-request list**: an *All* / *This request* scope beside its search box, scoped by `Store::history_for_origin` with a URL fallback for a `Standalone` request, and “Earlier sends…” reveals that pane already scoped instead of sliding in `history_sheet.rs`, which is deleted. **Also the redirect credential fix (`B27`)**: a redirect to another host stops carrying `Authorization`, `Cookie`, and `Proxy-Authorization`, and an `https → http` downgrade is refused unless the request is `insecure` | Round-trip tests in `core`; captures of both raw views and of the History pane in both scopes; deleting one cookie leaves the others in place (the jar is not persisted, so a reopen starts a fresh session); picking a row in the pane restores the stored response and no overlay opens; a redirect to another host drops the credential headers |
| `0.1.7` | **files: import and export** | An **Import…** file dialog over `ImportFormat::detect`, landing one new collection per collection the file holds (a HAR can be several) and reporting the `BulkImport.skipped` items with their reasons; an **Export…** picker offering *This collection* (Postman v2.1, HAR, or a one-collection bundle) and *Everything* (bundle only, the one format that carries environments). Import also appears in the empty request state beside *New request*, so a blank window can start from a cURL paste or a file instead of only a new tab | Captures of the import result and the export picker; a two-host HAR yields two collections; a malformed file changes nothing and names the failing item; a one-collection export carries no other collection or environment |
| `0.1.8` | history filters | Method, status, and date-range filters plus per-row delete, over `Store::search_history` gaining filter parameters (`B21`); the *All* / *This request* scope shipped in `0.1.6` | `B21` tests; a filtered list and a deleted row both survive a reopen; a capture of the filter row |
| `0.1.9` | keyboard and scale | A `?` shortcuts sheet built from the binding table; a test that every `Command` has both a binding and a palette entry; a generated 3,000-request workspace test asserting the tree stays responsive; a recent-workspaces list and switcher; **one type scale** — the rem helpers and `Metrics` font sizes disagree by 1–2px for the same role (`08-qol-backlog.md` 6.6), so pick one and convert the other | The audit test and the scale test pass, with the scale run's timing recorded in the test's comment. The type-scale change is a whole-tree conversion, never a per-file one, with a capture per pane |
| `0.2.0` | **collection and save behaviour** | Saving picks its destination (collections listed, plus *New collection…*) instead of requiring a sidebar selection first, which retires `save_destination()` as a gate and the *"nothing to save into"* flash with it; selection returns to being navigation; a bound tab can be re-bound (move to another collection); create-and-save in one step; the collection filter stops deep-cloning the tree per keystroke (`panels/collections.rs:1694`, `:1753`). **Both storage modes are in scope**: a destination is a collection whether it is a database row or a directory under a `restbook.json` workspace | A request can be saved with nothing selected in the sidebar; a saved request can be moved; a 3,000-request tree filters without a visible stall |
| `0.2.1` | state that depends on it | Persist the open tabs (`layout.json` schema bump, plus a rule for a tab whose bound node was deleted); import *into an existing* collection, reusing the picker the save dialog gained | A restart restores the same tabs and the same active tab; a deleted node drops or demotes its tab by the documented rule |
| `0.2.2` | **request and response affordances** | `Copy as…` becomes a menu — cURL first, then likely raw HTTP and a fetch snippet (the set is fixed when the lane opens) — and a paste shows a confirm dialog with the parsed method, URL, and header/body counts before it replaces the tab, instead of applying the clipboard immediately. The body find becomes a real bar: an explicit open state, a search icon, clear, `Escape` to close and `Enter`/`Shift+Enter` to step, plus match markers in the gutter and on the scrollbar. The Headers tab gains a filter, and the response summary gains a **Timings** view: one row per redirect hop, each a send → TTFB → body bar over the total, with the total at the end (U38, over `B25`). Settings gains a **Network** page for a default timeout and redirect policy; each request's Settings tab keeps its override, where empty means *use the default* | Captures of the copy menu, the paste dialog, an empty and a non-empty find, the Network page, and a redirected request drawing one bar per hop; a malformed paste changes nothing and says why; a request without a timeout follows the global default and one with a timeout overrides it |
| `0.2.3` | **first run, quit, and layout** | What a new window opens on, plus the empty sidebar, idle response, and no-hit history (X4; the blank request state's import action ships in `0.1.7`). The save-and-quit decision — auto-save on quit or prompt — applied to a bound tab as well as a draft, with the migration it needs (X3). The last of the responsive width rule: settle the per-split minimum (accept the 100 px floor or carry a `gpui-base` patch) and write the rule in `07-flows.md` (X1). A **side-by-side** center option beside the stacked Request-over-Response, reached from a button and a palette command, persisted in `layout.json`; this replaces the “center split is fixed” decision currently recorded in `07-flows.md` | A fresh data directory opens on the chosen screen; quitting with an edited bound tab does what the decision says; a window at the documented minimum still reaches Send and every pane; the flip control changes the split and the choice survives a reopen |
| `0.2.4` | **one command table and keyboard policy** | The palette, the titlebar menu, and `actions::bindings()` derived from one `Command` table, so a command cannot exist in two of the three (X5). The keyboard/focus rules that follow: `Tab` accepts a completion (U35), `F2` renames, and a focus-next-pane gesture (X6) | The `07-flows.md` audit test passes over the derived table; every binding still parses eagerly in its test; a capture per changed screen |
| `0.2.5` | **one `Document` per tab** | A response that lands after a tab switch goes to the tab that sent it, and each tab keeps its own scroll and focus state (X2). Every `Entity<Document>` consumer changes signature, which is why it sits alone in a lane | A late response never paints into another tab; the full suite green; a capture of two tabs with independent responses |
| `0.2.6` | **collection inheritance, one level** | A collection can carry variables, auth, and default headers that its saved requests inherit — variables resolved between Environment and Request (`B22`), auth as an explicit `Inherit` state the Auth tab shows rather than a silent fallback (`B26`), and default headers overridden by name. Collection level only: folder scopes and a workspace scope stay out. `Collection` gains `#[serde(default)]` fields, `VarContext` gains the tier, and `prepare` takes the inherited context as an argument so `core` stays pure; both stores, `FolderStore`, and import/export read and write the new fields | A request picks up a collection variable and the inspector names Collection as the winning scope; a request set to inherit sends the collection's auth; a collection file written before the field existed still parses; an export carries the collection's variables and auth |
| `0.2.7` | **security hardening** | The send path and the log stop carrying secrets they do not need: query values and userinfo are elided from the sent-URL line and from transport errors, and the response-body preview drops to `debug`, because `restbook.log` is meant to be attachable to a bug report. The plaintext history and local workspace files are created `0600` where the platform supports it, and the per-entry body cap is enforced (`B21`). Import gains a file-size cap and a malformed-fixture corpus | A log of a request with a token in its query contains no token; an over-cap response stores a truncated body with its flag set; a malformed import reports `Skip` rather than panicking; `cargo audit` is clean in CI |
| `0.3.0` | **response value reuse** | A value picked from another response: `{{chain:login.token}}` plus a *From response…* picker that writes it, with the transport sending the source request first when no response exists yet | The engine half is a `B` item; the UI half is the picker and the completion entry |
| `0.3.1` | encrypted secrets in a shared workspace | Encrypt values inside the committed file, share the workspace key out of band, keep the key in the OS keychain | A clone without the key shows a redacted value; with it, the real one |
| `0.4.0` | one file per request | `folder.rs` already reserves a directory per collection for exactly this layout; implement it and migrate an existing workspace on open | Both reference clients diff one request, not a whole collection |
| `0.4.1` | request panel redesign | The editor's layout, aimed at the mainstream clients (a reference capture is required before the first line) | Biggest visual change, and the one most likely to churn while the save behaviour is still moving |

Two items are deliberately unassigned: **streamed response bodies to disk** (`B19` step 2 + U57) and **drag to reorder** (U60) are wanted but block on engine work and on a gesture decision respectively; **signing** (R4) blocks on certificates, not on code.

A third belongs to the engine before it can be a lane: **post-response variable extraction**. `vars.rs` resolves Global and Environment scopes, but nothing writes a value *back* from a response, which is what Bruno does declaratively and what any assert-or-chain workflow needs. It is the engine half of `0.3.0` and the largest functional gap this app has against the reference clients.

`00-scope.md` already names Bruno as the reference for *offline, data ownership*: `.bru` plain text, one request per file, Git-native. That model is exactly `FolderStore`'s workspace mode, which the app can read but not yet create from the UI — which is why the workspace picker is in `0.1.3` and the collection lanes treat a destination as a path as readily as a row.

Checked while drawing the lanes up, so nobody re-opens them: history retention and the history search limit are both honoured (`send.rs` reads `history_max_entries` and calls `trim_history`; `panels/history.rs` reads `history_limit`), and quit really does flush drafts and bound edits (`save_on_close`, `flush_on_quit`, `flush_leaving_draft`).

## UI milestones

`done` means it is on disk and the app builds and runs.

| # | Deliverable | State |
|---|---|---|
| U1 | Window, theme, shell | done |
| U2 | Canvas: `DockArea` over `Document`, dockable panes, layout persisted to the data dir | done |
| U2a | Window shape: History sidebar left, Request over Response in the center split, single-panel dock titles suppressed | done |
| U3 | Request editor: params, headers, body (none/raw/form), auth (none/basic/bearer/apikey), method dropdown, counted tabs | done |
| U4 | Response viewer: status, timing, size, pretty/raw toggle | done |
| U4a | Response **headers** and cookies as inspectable tabs | done |
| U5 | History pane: search, recorded on send, row click restores the whole request and the stored response | done |
| U6 | Panes report intents as events instead of reaching into siblings | done |
| U7 | Widget extraction: kv table as its own module, format helpers unit-tested | done |
| U8 | Multipart and binary body editors | done |
| U9 | Environment editor + variable inspector with scope precedence shown | done |
| U10 | Collections pane: nested folders, drag to reorder, search | done (reorder via up/down) |
| U11 | Tabs: several requests open at once, one `Document` per tab | done (one `Document`, per-tab snapshots) |
| U12 | curl paste, keyboard shortcuts, command palette | done |
| U13 | Zaku-style visual pass: segmented request tabs, dense sidebar tree, one-line response header with gutter, palette roles | done |
| U14 | Window chrome: custom titlebar (workspace chip, breadcrumb, window controls), window-wide status bar, dense 26px tree rows, unsaved markers on tabs | done |
| U15 | Sidebar becomes a saved-things explorer: single Collections pane, `+` menu (New Request / Folder / Collection), filter, environment chip; History moves per-request | done |
| U16 | Save / Save As: a Save control on the request, `Ctrl+S`, update-in-place when a tab is bound to a collection node | done |
| U17 | Unsaved drafts: open unsaved tabs persist across restarts and appear as an Unsaved list in the sidebar | done |
| U18 | Layout versioning: `layout.json` carries an app-owned version, so a default-arrangement change is not overridden by a stale saved layout | done |
| U19 | Storage modes: a workspace found by marker wins over the per-user database, named in the status bar | done |
| U20 | Request Settings tab: timeout, follow-redirects, and insecure TLS, all folded into `RequestDef` | done |
| U21 | Action confirmations: a transient workspace flash for save/create/delete/rename/import, routed from panes that have no `Window` to notify through | done |
| U22 | Settings view: an app-level preferences dialog (`settings.json`) with pretty-print and history-limit, plus read-only storage facts; opened from the titlebar menu and the palette | done |
| U23 | Theme as a data file: a JSONC theme (`crates/app/themes/default-dark.jsonc`) embedded with `include_str!`, overlaid by `<data dir>/themes/<name>.jsonc`, carrying both colors and a `metrics` block (font sizes, row/bar heights, padding); panes read `Metrics` instead of `px(…)` literals | done |
| U24 | Zoom and appearance: `Metrics::scaled` as the one multiplier behind `Ctrl+=` / `Ctrl+-` / `Ctrl+0` (also palette commands), and a System/Light/Dark switch plus theme picker in Settings, applied live through a workspace observer | done |
| U25 | First-click Send: opening a saved row or a history pick writes the request into `Document` on the same tick, so Send is armed without a second click | done |
| U26 | Syntax highlighting: the raw request body is a code `EditorState` (grammar follows the mime) and the response body resolves per-line highlight runs once into the display cache; `gpui-kit`'s `tree-sitter` / `tree-sitter-html` features carry the grammars, so no new dependency | done |
| U27 | Live response state: a derived `ResponseState` (Idle / Fetching / Canceled / Completed / Error) on `Document` with a wall-clock start and frozen send stats; the response pane owns a 100 ms heartbeat that repaints the elapsed clock and byte count while a request is in flight, stopping the instant `pending` clears | done |
| U28 | Selectable, copiable response: each body row is a `SelectableLine` over the public selection API (paints the highlighted `StyledText`, keeps the gutter a sibling so a drag copies body text only), the large-body fallback is a `SelectableText`, and the header/status summary is both selectable and paired with Copy buttons | done |
| U29 | `{{…}}` completion: variables and generators suggested while typing a reference — the raw body uses the kit's completion popover (`EditorState` has a provider hook), the URL bar and the key/value rows draw their own list because `InputState` has none; secret values are masked | done (URL bar, raw body, and key/value rows) |
| U30 | Send-time generators: `{{$uuid}}`, `{{$uuid7}}`, `{{$timestamp}}`, `{{$timestampMs}}`, `{{$isoTimestamp}}`, `{{$timestamp('FMT')}}`, `{{$randomInt}}`, `{{$randomBoolean}}`, `{{$randomAlphaNumeric}}` | done |
| U31 | Shortcut hints read from the live keymap: `actions::shortcut(&dyn Action, window)` formats the highest-precedence binding with `Kbd::format`, so a tooltip shows `⌘⏎` on macOS and `Ctrl+Enter` elsewhere, and a hint cannot drift from its binding. Palette rows, Send/Save/Save As/New tab tooltips, and the response idle line use it | done |
| U32 | `Ctrl+N` / `Ctrl+P` as next/previous: bound to `SelectDown`/`SelectUp` in the palette's `"Command"` context (so they do nothing elsewhere) and handled by both completion lists, alongside Up/Down | done |
| U33 | `ctrl-space` opens the raw body's completion list: one `KeyBinding` for the kit's `ShowCompletions`, scoped to the editor's key context so no other input sees it (`Ctrl`, not `secondary`, because `⌘Space` is Spotlight) | done |
| U39 | First-save dialog and saved-request rename: a draft asks for a name (prefilled `METHOD url`, so Enter keeps the old result) and names the destination the sidebar selection implies, instead of saving silently under a generated label; a saved request gains Rename beside its row, and a rename is a label change only | done |
| U40 | Icon vocabulary: `src/icons.rs` names `gpui_kit::assets::IconName` (the full Lucide catalog, via `AllAssets`), the two explicit sizes, and the pane-title shape; rename becomes `Pencil`, delete `Trash`, the Environment pane `Braces`, History `Clock`, an outcome `CircleAlert`/`CircleCheck` instead of a bare `×`, and the status bar says which storage mode it is showing | done |
| U41 | Log sink: `src/logging.rs` tees `env_logger` to stderr **and** `<data dir>/restbook.log`, so a GUI run with no terminal still leaves a file; `RESTBOOK_LOG_FILE` moves it, `RUST_LOG` sets the filter, and the engine's `eprintln!` warnings (store, variables) became `log` calls so they reach the same file | done |
| U42 | Keybinding table as data: `actions::bindings()` returns every `KeyBinding`, `main` installs it with one call, and a unit test builds it, because `KeyBinding::new` parses eagerly and panics on a bad keystroke or context. Fixed the `ctrl-space` context, which copied the editor element's `Input mode=editor` string into a binding and crashed the first launch (the predicate grammar needs `Input && mode == editor`) | done |
| U43 | One home per number: the response header owns status, duration, and size, so the status bar stopped repeating them; it keeps the storage chip, the environment, the last sent URL, and engine messages (`chrome::Status` lost `status`/`duration`/`size`) | done |
| U44 | Readable bodies and honest copy buttons: a raw (or oversized) body is one selectable, syntax-highlighted block that scrolls in both directions instead of a clipped nowrap row; the header groups each copy button with what it copies — `[JSON ⌄] [copy body]` only on the body tab, then the summary, then `[copy summary]` — with distinct glyphs and tooltips | done |
| U42b | Panic hook: `logging::init` installs one, so a crash is written to the log file (with a stack under `RUST_BACKTRACE`) instead of vanishing with the missing stderr; the default hook still runs, so a terminal run is unchanged | done |
| U46 | Response history as a sheet: the response header's clock control hands the request's hits to the workspace, which opens them in a right-side sheet (deferred from render), so the list is no longer a dropdown clipped at the window edge and it scrolls | done |
| U47 | Real tab closing and a deliberate start: closing the last tab empties the editor to a “No request open” state instead of silently resetting it, the close button works on every tab, and the window opens a blank request rather than restoring the last draft (drafts stay in the sidebar's Unsaved list and reopen from the store) | done |
| U48 | Always-reachable History: a titlebar clock button and a titlebar-menu item reveal the global History pane, which was previously only reachable through the command palette or after a send | done |
| U49 | Interaction log: every intent (commands, tab changes, history picks, response tab/format, copies) is an `info` line on `restbook_app::ui`, and each send logs its outcome plus a bounded one-line body preview on `restbook_app::send`, so the existing log file is a transcript a bug report can attach | done |
| U50 | The log is findable, not guessed: `logging::file_path()`/`tail()` expose the file that `init` actually opened, the titlebar menu copies its path or the last 200 lines (with a flash), a failed send's error banner has a copy-log button, and Settings → Storage names it beside the other storage facts | done |
| U51 | History survives a reopened tab: a saved request that is closed and opened again starts with no response, so the header (and the history control next to it) was not drawn and its stored sends looked deleted. The idle state now offers “Earlier sends (N) · last 200 · 3 minutes ago”, and a queued adoption is no longer folded back out of the still-stale widgets, which used to relabel the tab “(new request)” for a frame | done |
| U45 | Horizontal scroll for the numbered body: the pretty rows sit in an `overflow_x_scroll` scrollport whose content is at least as wide as the widest line, measured once when the body is decoded (not per frame), so a long line scrolls instead of clipping and the gutter rides the same scroll. `uniform_list` still virtualizes vertically, and Raw keeps its own both-ways block (U44); both axes are asserted headlessly | done |
| U52 | Right-click a row: a collection, folder, or saved request opens the same menu as its ellipsis at the pointer position, with one builder serving both triggers; Move up/down are disabled wherever a move would be a no-op (the ends of a sibling list, and collections, which sit at the top level) | done |
| U53 | Dock toggles in the status bar: the sidebar and the History + Environment dock get buttons at the window's bottom corners (`PanelLeft` under the sidebar, `PanelRight` under the tools dock), each with its shortcut read live from the keymap (`secondary-\` / `secondary-/`), replacing the removed titlebar History button. Opening flashes a confirmation, and the tools toggle re-reads `is_dock_open` so a dock that stayed shut reports an error instead of claiming success | done |
| U54 | Per-OS branding: one `icons/restbook.svg` source is rasterized by `scripts/icons.sh` into the `.ico` (embedded in the Windows exe by `build.rs`, with VERSIONINFO and an `AppUserModelID`), the `.icns` for the macOS bundle, and the hicolor PNG set for Linux; `branding::APP_ID` is the identity the installer and the `.desktop` file must both match | done |
| U55 | Request-panel redesign: engine-supplied default headers (`Accept`, `Accept-Encoding`, `User-Agent`) fill what the user did not override and are listed read-only under **Headers → Default headers** (a collapsed disclosure, so the table stays the user's); Raw bodies choose a language preset (JSON/XML/Text/HTML) instead of a `Content-Type` header, with the mime shown as a hint; Paste cURL moves into the tab strip beside `+` and the titlebar menu, and a successful import opens the Save As dialog when a collection is selected; **Copy as cURL** (`secondary-shift-c`) exports the live request through `curl::to_curl` | done |

### UX milestones (behaviour-changing)

`U` items add surface; `X` items change what the app *does*. Each one either
redefines a habit a user already has, or has to migrate state that is already on
disk (`layout.json`, `settings.json`, drafts in the store). They therefore need
a decision before code, and a migration (or a bumped `LAYOUT_VERSION`) when they
land. Ordered by how much of the current behaviour they invalidate.

| # | Deliverable | What it changes |
|---|---|---|
| X1 | Responsive width policy | The app has no per-split minimum: `gpui-base` floors every dock and every centre slot at `PANEL_MIN_SIZE` (100 px) and exposes no `size_range` to the app, so panes were designed to survive that instead. First slice **done**: a window floor (`MIN_WINDOW_WIDTH`/`MIN_WINDOW_HEIGHT` in `main.rs`), the URL row collapsing at 340 px, and scrolling tab strips in both panes. Remaining: state the rule in `07-flows.md`, decide whether a dock may auto-collapse below a threshold, and either accept 100 px as the floor or carry a `gpui-base` patch that exposes a per-split minimum |
| X2 | One `Document` per tab | Today the workspace keeps one `Document` and snapshots it into the tab being left (U11), so a response that lands after a switch is written into whichever tab is active (audit §7.1). Per-tab documents make a late response land in the tab that sent it, and let a tab keep its own scroll/focus state. Every `Entity<Document>` consumer changes signature, so this is the largest `workspace/` change left |
| X3 | Save and quit semantics | Quitting persists drafts but silently drops edits to a tab bound to a collection node (backlog 2.2); a bound tab cannot `Save As` (2.3); an empty name in the save dialog is refused with no explanation. The decision is auto-save-on-quit versus prompt, and it changes what a user may safely assume about closing the window |
| X4 | First run and empty states | U47 chose a blank request over restoring the last draft, and drafts reopen only from the sidebar's Unsaved list. First-run layout, the empty sidebar, the idle response, and history-with-no-hits are still mostly blank. Deciding what a new window opens on changes the default `layout.json` and the first ten minutes |
| X5 | One command table | The palette, the titlebar window menu, and `actions::bindings()` are three hand-kept lists (audit §5.2), and there is no View menu for a `PanelName` grid. Deriving all three from one `Command` table (plus `Command::FocusPanel`/`ClosePanel`, backlog 3.3) changes where a command is discoverable rather than adding one |
| X6 | Keyboard and focus policy | `Tab` currently moves focus, so accepting a completion needs arrows or the mouse (U35); rename has no binding (F2); there is no focus-next-pane. Changing `Tab` inside the completion lists changes a habit on every input in the app |

### Not yet

Candidates discussed after U30, roughly by value. The detailed rationale lives
under [“Next”](05-ui-guide.md#next-completion-generators-and-keys) in the UI
guide.

| # | Deliverable | Notes |
|---|---|---|
| U36 | Scroll the completion list to the selection | A 9-item generator list fits; 140 faker names would not |
| U37 | More generator shapes toward Yaak | `hash.*`, `base64.*`, `url.*`, `regex.*`, faker names — the `Generators::call` dispatch is the seam |
| U57 | Save response body to a file | `ResponseBody::File` exists (B18 step 1) but nothing in the UI can write one, so a downloaded body only lives in memory |
| U60 | Drag to reorder | Tabs and collection rows reorder only through the context menu's Move up/down (backlog 1.8). Drag is the gesture users try first |

Already scheduled or shipped, so no longer a candidate here: `U34` is in `0.2.6`, `U35` is grouped under X6 (`0.2.4`), `U38` is in `0.2.2`, `U56` shipped in `0.1.4`, and `U58`/`U59` are in `0.1.6`/`0.1.7`.

### Ordering notes

- **U4a is done**, with the Body/Headers/Cookies tab bar and the format
  control. The bytes were already in `ResponseRecord.headers`, so it was a
  render, not plumbing.
- **U5 is done, including the restore.** `HistoryEvent::Picked { id }` hands the
  row to the workspace, which calls `Store::get_history` (backend B6) and gives
  the editor the whole `RequestDef` — params, headers, body, auth — so a row can
  be re-sent as recorded. The stored response is shown without sending. This is
  the one place where the two plans were order-dependent; see
  [`03-backend-plan.md`](03-backend-plan.md).
- **Storage location.** History and the layout live in the per-user data
  directory (`%APPDATA%\restbook`, `~/Library/Application Support/restbook`,
  `$XDG_DATA_HOME/restbook`), overridable with `RESTBOOK_DATA_DIR`. SQLCipher is
  the app's default store, so `history.db` is encrypted; a
  `--no-default-features` build falls back to plaintext JSONL in the same
  directory. Legacy files from the old working-directory `.restbook/` are
  migrated once.
- **U8** reuses the same table widget the params and form-body tabs use; the
  model already carries `Body::Multipart(Vec<FormPart>)` and `Body::Binary`,
  so this is editor-side only.
- **U11** is the one item that changes the canvas shape: a `Document` per tab
  means the panes observe *the active document*, not *the document*. The panes
  do not change at all — they already take the document in their constructor.
  Only `workspace/` grows.
- **U8/U9/U10** are additive and can be done in any order; each is one pane or
  one widget.
- **U8 is done** with `parts_table.rs`: multipart rows carry name, value-or-path,
  mime, and enabled; binary is one path input. Both restore through
  `apply_request`, so a history pick no longer degrades them to `None`.
- **U9 is done** as two modules. `variables.rs` owns environments, globals, and
  the active selection over the store; `panels/environment.rs` edits them and
  renders the inspector, naming the scope that won each `{{name}}` and flagging
  the unresolved ones. Shared reference walking lives in `variables.rs` so the
  request pane can reuse it.
- **U10 is done** without drag-and-drop: reordering is up/down buttons beside a
  row. Everything else in the milestone — nested folders, search that keeps
  ancestors, all CRUD persisted immediately — is in place.
- **U11 shipped smaller than the plan.** Rather than one `Document` per tab, the
  workspace keeps one `Document` and snapshots it into the tab being left
  (`request`, `response`, `error`, `last_url`), then adopts the next tab through
  `RequestPanel::adopt_request`. Panes still observe a single document, so the
  tab strip is additive and no pane changed. The tradeoff is that a response
  arriving after a switch belongs to the tab that was active when it landed;
  promoting this to per-tab documents is the next step if that matters.
- **U29** lives in `completion.rs` and has two entry points on purpose. The raw
  body is an `EditorState`, which the kit gives a `CompletionProvider` hook, so
  the kit's own popover owns arrows/Enter/Escape. The URL bar is an `InputState`
  with no such hook, so `Autocomplete` holds the reference under the caret and
  the request pane draws a `deferred()` list under the field and routes the
  keys. Both share one `suggestions()` so the two lists cannot disagree.
  `is_completion_trigger` answers `true` for every edit: the kit's own latch
  goes stale on backspace, and an empty answer is what closes the popover.
  Typing `$` narrows the list to generators, because a `$`-token can only be one.
- **U29** accepts by writing the whole `{{name}}` over the partial text, so a
  reference is always closed. The replaced range extends over a `}}` the code
  editor's auto-close inserted after the caret: typing `{{` yields `{{|}}`, and
  stopping the edit at the caret would leave the pair behind as `}}}}`. The
  write is deferred a tick because `InputState::replace` emits
  `InputEvent::Change`, which comes straight back to the same subscription, and
  an entity cannot be updated while it is being updated.
- **U29's Enter path is belt and braces.** For the two `InputState` surfaces the
  kit's `enter` binding accepts through `PressEnter`, with a key-down filter as
  a fallback for a miss. The raw body cannot use a key-down fallback: its
  binding is multi-line and inserts a newline first, so the editor registers a
  keystroke interceptor that accepts before dispatch and stops propagation only
  when a row was accepted. `accept_body_completion` routes the action exactly as
  the kit would and, if the overlay handler is not installed yet, writes the
  first item with `insert_completion` — the same call a click makes. With no
  reference open the interceptor does nothing and Enter still breaks the line.
- **U29** also covers the value field of a key/value row (`params`, `headers`,
  `body → form`). Each row owns its `Autocomplete` and subscribes to its own
  input, so it is found by input identity rather than by list index and a
  removed row simply drops its subscription. The key field gets no list: a
  header or parameter name does not interpolate.
- **U30** is a `$`-prefixed token in the same braces. A generator is the clock
  and a seeded counter only — no stored value — so it lives in
  `core::generator` beside the template engine, and `prepare_with`/`resolve_with`
  take one so a test can fix the clock and the draws. A `$`-token never consults
  a scope, so a user variable cannot shadow it. The same rule keeps the
  inspector quiet: `variables::references` skips every `$`-prefixed name,
  because only a scope lookup can be unresolved, and a generator is rendered at
  send time.
- **U31 is a display concern only.** The keys were already bound; what was
  missing was showing them. `shortcut()` asks the window for the binding that
  would fire, so the label is derived rather than typed, and the one
  OS-formatted string comes from `Kbd::format` (no separators on macOS, `+`
  elsewhere). The palette's command table therefore holds only
  `(Command, title)` — its right column is computed.
- **U32 is context-scoped on purpose.** `ctrl-n`/`ctrl-p` are bound in the
  palette's `"Command"` context, not globally, so they cannot steal a key from
  an input, and they are the literal `ctrl-` stroke rather than `secondary-`,
  because on macOS `secondary` is `cmd` and `⌘N` is a window command. The
  completion lists match the same literal pair in their own `on_key_down`
  filters, because an `InputState` has no overlay action routing.
- **U33 needed no app-side handler.** The kit's `Editor` already registers
  `on_action_show_completions`, so the only missing piece was a binding. It is
  scoped to the editor's own context — the element sets `Input mode=editor`,
  and the binding predicate says the same thing as `Input && mode == editor` —
  so the same stroke still does nothing in the URL bar, a value field, or a
  plain textarea, and none of those could show that popover anyway. It is
  `ctrl-`, not `secondary-`, because `⌘Space` belongs to Spotlight on macOS.
- **U42 fixed a launch crash that no build could catch.** The `ctrl-space`
  binding had been written with the element's context string, and
  `KeyBinding::new` parses its arguments eagerly and unwraps, so the first real
  run panicked during `cx.bind_keys` — a cold path that neither `cargo build`
  nor a UI-less test ever reached. Moving the table into `actions::bindings()`
  makes a unit test reach it, and keeps every keystroke in one place for the
  next key that needs adding.
- **U42b is the reporting half of U42, and came from its report.** The panic printed to
  stderr and the window never appeared: from a detached launch the file would
  have said nothing at all. So the sink now also owns panics — one hook in
  `logging::init`, logged before the default hook prints. It is a *reporting*
  fix, not a recovery one: the process still dies, only visibly.
- **U43 is about ownership, not trimming.** `200 OK · 549 ms · 13.3 KB` is the
  response's own verdict, and it belongs in the response's header, where it sits
  beside the body it describes. Printing the same three values in the window
  footer made the footer read as a duplicate of the pane; the footer keeps what
  the pane cannot say — which URL went out, and why nothing did. Nothing was
  deleted from the model: `ResponseRecord` still carries all of it.
- **U44's horizontal scroll hinges on three properties.** GPUI computes a scroll
  container's content size as the union of its *children's bounds*, so a
  stretched child reports the pane width and there is nothing to scroll to. The
  block has to be a flex row (`display: block` is the `div` default, where a
  child stretches), the row has to be horizontal, and the child must not shrink
  (`flex_none`); a shrinkable child stops at the scrollport even as a flex item.
  The render test asserts the content box is wider than the viewport, so
  dropping any of the three fails. The block also gets the whole-text highlight
  runs, so raw keeps its colours — the old fallback painted plain text.
- **U44 grouped the two copies by referent.** Both were a bare `Copy` glyph at
  the end of the strip, which left the reader guessing which copied what. Each
  now sits next to its subject (`[JSON ⌄] [copy body]` … `summary [copy
  summary]`), the body pair only appears on the body tab (the other tabs show no
  payload), and the tooltips name the contents instead of the pane.
- **U39 asks at the one moment the answer is unguessable.** A silent first save
  picked `METHOD url` as the name and took whatever collection was selected as
  the destination; both are cheap to confirm and expensive to repair later
  (rename is a second, less obvious action). So the draft save shows both and
  lets Enter accept them. A *bound* tab still saves silently on `Ctrl+S`,
  because it already has a name and a parent — asking there would be a
  keystroke of friction on every save. The name is a **label**: `rename_item`
  changes `SavedRequest.name`, never `request`, so the stored `RequestDef` and
  its URL are untouched and a rename cannot alter what is sent.
- **U39 defers, it does not open in the handler.** `Ctrl+S` and the row menus
  reach the workspace through `cx.subscribe`, and `RequestEvent::Save` carries
  no `Window` — so the handler sets `pending_save` and `render` consumes it with
  `window.defer` to call `open_dialog`. Opening the dialog inside `render`
  would land before the element tree is built; the deferral puts it after.
- **U39 keeps the name input on the workspace.** A dialog's content closure is
  re-run on every notify, so an `InputState` created inside it is a new entity
  each frame and the caret resets on the first keystroke. `save_name` is one
  `Entity<InputState>` on `Workspace`, prefilled in `open_save_dialog` and
  handed to the `Input` element. The confirm closure returns `false` for an
  empty name, which is the dialog API's "do not close": the field stays up
  with the user's cursor in it.
- **U40 is one vocabulary, not a new icon set.** The glyphs were already
  `gpui-component`'s; what was missing was a single place that says which one a
  surface uses, at which size. `Replace` (two arrows) for Rename and `Close`
  (`×`) for Delete and for a failed flash were the visible drift. Sizes were
  literals in six files (11, 12, 13, 14); the 11px status-bar glyphs were the
  odd ones out, and the pane titles were three copies of the same wrapper.
  Everything now goes through `src/icons.rs`. Two names it needs — `Pencil`,
  `Trash` — are not in the default component bundle, so `main.rs` installs
  `AllAssets`; a name that is not embedded does not fail, it just paints
  nothing, which is why the icon set is a module rather than string literals at
  the call sites.
- **U41 is about the sink, not the levels.** The app had no logger problem so
  much as a visibility problem: a GUI binary launched without a console drops
  stderr, and `main.rs` had no `log::*` call at all, so nothing was ever
  written anyway. `env_logger` still formats; `logging::init` swaps the target
  for a tee, so a terminal run keeps its live output and a detached run leaves
  `restbook.log` beside `settings.json`. The default filter keeps `debug` off
  (a bug report's file stays small) and that is the usual reason a freshly
  added line does not appear — set `RUST_LOG` for it. `restbook-store` gained
  the `log` facade so a skipped collection is a line in that file rather than
  an `eprintln!` nobody sees.
- **U12 is done**: `restbook_core::import::curl::parse` turns a command into a
  request (it moved out of the app into `core/import/` with the other
  importers), the palette routes all eight commands through one workspace
  callback, and every binding is in `main.rs`.
- **U13 is a look-and-feel pass, not a new feature.** The open-request tabs moved
  into the request pane and became a segmented strip (`GET | name | ×`) instead
  of pills on a window-wide bar; the sidebar grew icon actions and a method
  badge; the response header collapsed to one line (`Body | Headers | Cookies`
  … `JSON ▾ · status · time · size`) over a line-numbered body; the kv rows use
  a checkbox and bordered inputs. `theme.rs` gained named roles (`tab_bar`,
  `tab_active`, `sidebar`, `hover`, `selected`, …) so a pane reads `palette`
  instead of `cx.theme()`. Self-chromed panes return `inner_padding() == false`
  to remove the seam above their own header.
- **U14 is chrome, not a pane.** `main.rs` hides the platform titlebar
  (`WindowDecorations::Client`, `TitleBar::window_options()`); `chrome.rs`
  draws the titlebar (workspace chip, request/URL breadcrumb, window controls)
  and a 24px status line (active environment on the left, the last exchange or
  the last error on the right). Because neither is dockable, the dock no longer
  owns the full window height: render is titlebar → dock (`flex_1`) → status
  bar. The tree rows dropped to a fixed 26px with one shared indent, and a tab
  that was saved to a collection and then edited shows a dot instead of its
  close button.
- **U20 is the fifth editor tab.** The three fields (`timeout_ms`,
  `follow_redirects`, `insecure`) were already in `RequestDef` and already
  honored by the transport, so this is editor-side only: a blank timeout stores
  `None` rather than `0` (a zero deadline would fail every request instantly),
  and `follow_redirects: true` is stored as `None` so a shared collection does
  not carry a field nobody changed.
- **U21 is scoped to the paths that cannot notify.** A pane with a `Window`
  could still `push_notification`, but every action is reported one way — the
  flash — so the app does not show two kinds of toast depending on how the same
  command was invoked. Panes emit `Notice`; the workspace owns the toast and its
  timer, which is also the only place that can replace a pending one.
- **U22 is a dialog, not a window.** The workspace is one entity per window and
  already owns everything a settings screen reads, so a second window would only
  duplicate state; a dialog reads the live values and escapes with one key.
  Settings are scoped to app-level choices — request-shaped options stay in the
  request's Settings editor tab, so a shared collection keeps carrying them.
- **U52 needed no trigger, synthetic or app-level.** The kit's
  `ContextMenuExt::context_menu` is the pointer API: it records the
  `MouseDownEvent::position` of a right press, and its `prepaint` anchors the
  popup there, so the row itself is the trigger and nothing is drawn at the
  pointer. Both the ellipsis and the right-click call `row_menu`, so the two
  triggers cannot disagree. Rows rendered from one call site would otherwise
  share element state, which is why the `context_menu` id is derived from each
  row's own `ElementId`. A collection is not moved by `move_item` (that walks
  `collection.items`), so its two verbs are always disabled rather than offered
  and ignored; the inline rename input keeps its own right-click menu while it
  is open.
- **U53 moved the dock toggles to the status bar because that is the edge they
  open onto.** The sidebar control sits in the bottom-left corner under the
  sidebar; the tools control in the bottom-right under the tools dock. The
  titlebar keeps window chrome and the breadcrumb — the surface a reader drags
  the window by — rather than doubling as a second toolbar. The reveal flashes a
  confirmation because an off-screen or layout-thwarted dock is otherwise
  indistinguishable from a dead button, and `toggle_tools` reads `is_dock_open`
  back so success is reported only when the dock actually opened. The two
  shortcuts are read from the keymap, not hard-coded in a tooltip, so a stroke
  cannot drift from its binding (the U31 rule).
- **U54 keeps one icon source and lets each OS take what it reads.** Windows
  needs the `.ico` inside the exe's resource section (`build.rs`, MSVC only — GNU
  `ld` drops the section), macOS the `.icns` in the bundle, Linux the hicolor PNG
  set; `scripts/icons.sh` derives all four from `restbook.svg` so they cannot
  diverge. `branding::APP_ID` is shared by the installer's `AppUserModelID` and
  the `.desktop` file, so a shortcut and a running window resolve to one icon.

## Engine milestones

`done` means it is on disk, builds, and is covered by a test or exercised by
`devcli`.

| # | Deliverable | State |
|---|---|---|
| B1 | Workspace, `RequestDef`/`Body`/`Auth`/`ResponseRecord`, `{{var}}` templating | done |
| B2 | `prepare()`: params, headers, all body kinds, auth, timeout, redirects, TLS, header-injection guard | done |
| B3 | reqwest transport owning its tokio runtime; cancellation | done |
| B4 | `Store` trait; `MemoryStore` + `JsonlStore` | done |
| B5 | `SqlcipherStore` over SQLCipher, key from OS keyring, search by URL | done, feature-gated |
| B6 | `Store::get_history(id)` returning the full `HistoryEntry` | done |
| B7 | Restoring a history entry into the editor with body and auth intact | done |
| B8 | `Store` trait: collections, folders, environments as persisted rows | done |
| B9 | Variable scope resolution: precedence between global, environment, request, and live state | done (global + environment) |
| B10 | Non-strict preview mode: render the request with unresolved vars marked, send disabled | done |
| B11 | Proxy, custom CA, mTLS, redirect metadata in the record | done |
| B12 | Cookie jar shared with the transport | done (read, clear, and per-cookie edit/delete) |
| B13 | Import (curl, HAR, Postman v2.1, OpenAPI 3) and export | done |
| B14 | OAuth 2.0 (PKCE, client credentials) | done (client credentials + code exchange; app wires client credentials) |
| B15 | Progress reporting: `Progress` sink + `Transport::execute_with_progress` | done |
| B16 | Offline test seam: `FakeTransport` + transport integration tests | done |
| B17 | `FolderStore`: a Git-friendly workspace on disk | done |
| B18 | Streamed response bodies to disk (large downloads, resume) | step 1 done: `ResponseBody::{Memory, File}` enum; transport still memory-only |

### Planned (B19..)

Engine work only: each item lands in `core`, `transport` or `store` behind an
existing trait, so it is testable without a window (and usually without a
socket). A UI half, where one exists, is named.

| # | Deliverable | Seam and notes |
|---|---|---|
| B19 | Streamed bodies to disk, step 2 | `Transport::execute_with_progress` already counts bytes; it must spill past a threshold into `ResponseBody::File` instead of `bytes()`, honour a resume offset, and report the path in `ResponseRecord`. UI half: U57 |
| B20 | OAuth 2.0 authorization-code flow | `core/src/oauth.rs` builds the authorize URL and exchanges a code with PKCE, and `OAuthToken::refresh_token` is parsed but never used. Missing: a loopback redirect listener, the browser hand-off, refresh-on-expiry, and the app wiring (only client credentials are wired today) |
| B21 | History search and retention | `Store::search_history` is a URL `LIKE` today. FTS5 over URL/name/body, keyset pagination, and a retention pass (age and count) so the encrypted database does not grow without a bound; a per-entry body cap (truncate with a flag) bounds what one response can add, because count alone does not |
| B22 | Collection and request scopes in `vars.rs` | Scope precedence stops at Global + Environment (B9). Adding the two narrower scopes gives U34 its data, and the inspector's `secret_of` follows the same table |
| B23 | Streamed uploads | A multipart part or a `Body::Binary` path is read whole into memory at prepare time (`PreparedBody::File`). Streaming from a path keeps a large upload out of RAM; the same threshold setting as B19 applies |
| B24 | `FolderStore` external-change detection | A folder workspace is plain files on disk, so another editor can change them while the app holds a snapshot. The app has no Git integration of its own; a user's checkout is the same hazard. Detecting a changed file (mtime/hash) before a write is the difference between a merge and silent clobbering |
| B25 | Exchange timings | `ResponseRecord` gains a timings value and `RedirectHop` gains its elapsed time, both `#[serde(default)]` so a history row from before the field still parses; the transport marks the boundaries it already has — before `send()`, after headers, after the body — once per hop. DNS, connect, and TLS timings are not available from `reqwest`/`hyper`, so the timeline is send → TTFB → body, not a browser waterfall. UI half: U38 in `0.2.2` |
| B26 | Collection-level auth and header inheritance | `Collection` gains `auth` and `headers`, both `#[serde(default)]`; `prepare` takes an explicit inherited context so `core` stays pure; the send path resolves `Auth::Inherit` by walking the saved request's collection, and default headers merge by name unless the request sets the same name. UI half: the Auth tab shows the inherit state and what it resolves to; the Headers tab lists inherited rows read-only. Import and export carry the fields |
| B27 | Redirect credential stripping | The transport follows redirects by hand (`Policy::none()`), so it inherits none of `reqwest`'s protections: `Authorization`, `Cookie`, and `Proxy-Authorization` currently survive a hop to another host. Strip them when the host or scheme changes, and refuse an `https → http` downgrade unless the request is `insecure`. UI half: none; rider on `0.1.6` |
| B28 | Export redaction | `import::export` writes `Environment.vars` and collection values verbatim, including `secret: true` entries, while `FolderStore` redacts. Redact secret values by default and make “include secrets” an explicit, warned choice. UI half: the export picker's checkbox, in `0.1.7` |

### Network settings (B11 + B12)

`core/src/net.rs` holds `NetworkConfig`: `proxy`, `extra_roots` (PEM
certificates), `client_cert` (a PEM certificate/key pair for mTLS), and
`default_timeout_ms`. `core` stays reqwest-free by default; `validate()` checks
the PEM blocks and parses the proxy URL only under the `network-validation`
feature, which `restbook-transport` enables. A bundle of certificates is read
permissively, so an empty anchor list is an error rather than a silently liberal
trust store.

`ReqwestTransport::set_config` takes a new configuration and rebuilds its client
on the next send; `ClientCache` holds the trust and insecure variants behind a
generation counter, so changing a setting does not rebuild two clients per
request.

Redirects are followed **by hand** (`redirect::Policy::none`) so each hop is
recorded. `ResponseRecord` gained `final_url` and `redirects: Vec<RedirectHop>`,
which is what lets the UI show `301 → 302 → 200` instead of only the destination.
A 303, and a 301/302 in reply to anything but GET/HEAD, drop the body and the
entity headers on the way through.

Cookies live in one `SharedJar` per transport, a `cookie_store::CookieStore`
behind reqwest's `CookieStore` trait. reqwest's own `Jar` cannot be read,
cleared, or changed one row at a time, which is why it is not used: the
`Transport` trait exposes `cookies(url)`, `clear_cookies()`, `delete_cookie`, and
`update_cookie` so the UI can list, forget, or re-value one cookie without
touching the rest. The jar is process-wide and in memory, so a restart starts a
fresh session.

### Progress reporting (B15)

`core::transport::Progress` is a cheap, clonable sink: sent/received byte
counters, an elapsed stamp, and a `headers_received` flag, all atomics behind an
`Arc` so the transport can bump them from its I/O thread while the UI reads them.
`Progress::snapshot()` is the read side and never blocks.

The trait gained `execute_with_progress(request, cancel, progress)`.
`ReqwestTransport` overrides it: it streams the response body in chunks (rather
than `response.bytes()`), marking headers and bumping `received` as data arrives,
and reports a `Content-Length` denominator — suppressed for a response that
advertises a content encoding, because that length counts compressed bytes and
would make the counter overshoot. The whole body is still returned in the
`ResponseRecord`; this is progress reporting, not streaming-to-disk (see B18).

`execute` still exists and defaults to `execute_with_progress(.., Progress::new())`,
so a transport that cannot report progress only implements `execute`. `devcli send
--progress` drives it from a terminal and prints the counters to stderr.

### The offline test seam (B16)

`core::testing::FakeTransport` answers from a scripted `Reply` (`Status`, `Json`,
`Record`, `Error`) or a handler closure, and records every `PreparedRequest` it
was asked to send. It is always compiled — no dependencies — so a test can run
the whole pipeline (plus a `Store`) deterministically with no socket. It is the
counterpart to zaku's `FakeHttpClient`.

The real `ReqwestTransport` is covered by integration tests that drive a bare
`TcpListener`: a real send with progress assertions, upload-size reporting, and a
cancellation test against a server that stalls mid-body. Between the two, the
`Transport` seam is exercised both offline and over a socket.

### Folder workspaces (B17)

`store::FolderStore` is the Git-friendly shape: one file per collection under a
workspace root, marked by `restbook.json`, with a generated `.gitignore` that
keeps the tracked tree free of secrets. Local-only state (the active
environment, global variables) goes to `restbook.local.json`, and history to a
`.restbook/` state directory — both gitignored. `devcli export <file> --workspace
<dir>` writes an import into one. It sits beside `JsonlStore` and `SqlcipherStore`
behind the same `Store` trait; the app still ships SQLCipher as its default.

### Import and export (B13)

Every format is translated into the same one thing — `import::BulkImport`, a list
of `Collection`s, a list of `Environment`s, the names that were already present,
and a `Skip { at, reason }` per item that could not be read. Importers never
touch the `Store`: the caller saves each collection, and a partial import is
explained rather than refused.

| Format | Reads | Writes |
|---|---|---|
| curl | one command: URL, verb, headers, body, form parts, basic auth | — |
| Postman v2.1 | folders, headers, raw/urlencoded/formdata/file bodies, basic/bearer/apikey auth | yes, one collection |
| HAR 1.2 | the newest `limit` entries, minus assets and transport headers, grouped by host | yes |
| OpenAPI 3 | one request per operation: `{{param}}` paths, required query rows, a JSON skeleton from required properties | — |
| bundle | this app's own file: every collection and environment | yes |

`ImportFormat::detect` tells the JSON formats apart by shape, since one suffix
covers three of them. OpenAPI is import-only on purpose: generating a document
from stored requests would be inventing a description of an API, and a wrong
schema is worse than none.

`devcli import <file>` parses and reports without saving:

```
$ devcli import api.yaml --verbose
OpenApi: Imported 3 requests
Petstore:
  /pets/{petId}:
    GET Read a pet  https://api.pets.test/v1/pets/{{petId}}
```

One behaviour worth knowing when reading the HAR tests: deduping keys on
`METHOD url body`, not `METHOD url`. A browser replays a URL with different
payloads, and collapsing those would silently drop requests.

### OAuth 2.0 (B14)

`core/src/oauth.rs` is pure shape-building, like the rest of the engine:
`OAuthConfig::authorize()` returns the URL the browser opens plus the `state`
and PKCE verifier to keep, `client_credentials_request()` and
`code_exchange_request()` return a `TokenRequest` (a form plus optional HTTP
basic client auth), and `parse_token_response()` reads the reply, turning an
`{"error": …}` body into `Error::OAuth` rather than a serde message.

`OAuthConfig` defaults `pkce` to `true`; a hand-written `Default` is what makes
that true, since a derive would yield `false`. The verifier and state come from
`Uuid::new_v4()` rather than a new RNG dependency.

`prepare()` never performs I/O. It adds `authorization: Bearer …` only when the
request's `Auth::OAuth2` already carries an `access_token`; otherwise it fails
with `Error::OAuth`, and cURL export emits the header only when a token exists.
The token is `serde(skip)`, so it is never written to a saved request.

The transport implements `OAuthTransport::fetch_token` by POSTing the form to
the token URL with the same client, proxy, and TLS policy as a normal send, and
prefers an OAuth error body over the HTTP status. `devcli oauth` drives both
grants; the app's Auth tab fetches client-credentials tokens, and the
authorization-code browser flow is the one part still deferred (see the
Known gaps in [`00-scope.md`](00-scope.md)).

### How a history row restores (B6 + B7)

`search_history` returns `HistoryHit` rows: method, URL, status, timing — enough
to draw the list, not enough to re-send. The full request is recovered with:

```rust
fn get_history(&self, id: i64) -> Result<Option<HistoryEntry>>;
```

`MemoryStore` indexes its vector, `JsonlStore` scans, and `SqlcipherStore` joins
`requests` and `executions`. The request itself is persisted as a whole
`RequestDef` in `requests.request_json`; rows written before that column existed
fall back to method, URL, and body, so the restore degrades instead of failing.

On the UI side, `HistoryEvent::Picked { id: i64 }` carries the row id, and
`workspace/` asks the store for the entry, hands the `RequestDef` to the editor
(`RequestPanel::adopt_request`), and puts the stored `ResponseRecord` in the
document without sending. The history pane does not change beyond the event
payload — it already owns the store. See [Ordering notes](#ordering-notes) above.

### Why B9 and B10 are one design

Scope precedence has to be visible, not implicit. B9 decides *what a name
resolves to*; B10 shows the user *which layer won* and what is still unresolved.
Splitting them produces a resolver with no way to explain itself, which is the
usual reason people distrust environment features.

Both are on disk now. `crates/core/src/vars.rs` holds `ScopedVars`: five layers
in fixed precedence, `lookup()` for the winner, `context()` for `prepare()`, and
`preview()` for the non-strict render. `crates/app/src/variables.rs` pushes
`Global` and `Environment`; `Collection`, `Request`, and `Live` exist but have
no producer yet. The workspace builds the context per send, and the request pane
lists unresolved names and keeps Send inert while any exist.

## Kept and deferred deliberately

- **Plaintext JSONL.** `devcli` writes plaintext by design; only the app path is
  encrypted. The store crate must not decide which is in use.
- **No scripting.** No `pm.*`, no JS engine. This is a product boundary, not a
  scheduling decision.
- **OAuth's browser listener.** The engine builds the authorize URL, derives the
  PKCE challenge, exchanges a code, and fetches client-credentials tokens; the
  app fetches client-credentials tokens and shows the settings. The local
  redirect listener that completes the authorization-code flow in-app is the
  one piece still deferred.
- **Collection and folder inheritance.** `0.2.6` adds collection-level variables, auth, and default headers (`B22`, `B26`). Folder-level scopes (Bruno) and a workspace-level scope (Yaak) stay out until the collection form has settled; restbook's “workspace” is either a folder of collections or the database, so a third tier needs a definition before it can exist.
- **No Git integration.** `FolderStore` is a plain directory a user can version with Git, but the app never invokes Git and holds no repository state. `B24`'s external-change detection is about any other editor touching the files, not an embedded client.
- **Other protocols.** GraphQL, gRPC, WebSocket, and SSE are outside the
  HTTP-only scope in `00-scope.md`. They are the one boundary most likely to be
  revisited far out; a change starts at `Transport`, not in the UI.

## Quality of life

> **Status:** every item in that file is implemented except **6.3** (the
> `restbook-ui` crate split) and **6.5** (`workspace/mod.rs` is one impl block
> holding five jobs), which both stay deferred. The per-item text is kept as the
> record of what was wrong and what fixed it, not as an open request.

The rest of [`08-qol-backlog.md`](08-qol-backlog.md) is that record, kept because
each entry names the code that makes it true.

## Reference parity

Ideas from the clients this app is measured against, with what we would actually
build. `00-scope.md` forbids vendored sources: Zaku is AGPL and is a reference
for interaction only, so every row here is "copy the idea, write our own code".

| Idea | From | What we would build | Engine/UI | Lane |
|---|---|---|---|---|
| Response value reuse | Yaak (pick a value), Bruno (declarative) | `{{chain:login.token}}` plus a "From response…" picker that writes it; the transport sends the source request first when no response exists | engine: transport + `vars` | 0.3.0 |
| Encrypted secrets in Git | Yaak | encrypt values inside the committed file, share the workspace key out of band, keep the key in the OS keychain | engine: `FolderStore` + keyring | 0.3.1 |
| One file per request | Yaak, Bruno | `folder.rs` already reserves a directory per collection "for a request-per-file layout"; implement and migrate | engine: `FolderStore` | 0.4.0 |
| Review and commit in-app | Yaak, Zaku (`crates/git`) | git status for a workspace plus a commit of the changed request files | engine + UI | 0.4.x |
| Import dialog | all three | file picker over `ImportFormat::detect`, with `BulkImport.skipped` reported | UI over existing engine | 0.1.7 |
| Recent workspaces | Zaku (`recent_projects`) | a list and switcher for `restbook.json` folders | UI + small persistence | 0.1.9 |
| CLI and agent access | Yaak | `send` / `import` / `export` on `crates/devcli` | engine (exists) | 0.3.x |
| Scale proof | Yaak (3,286-request claim) | a generated 3k-request workspace test that asserts the tree stays responsive | test only | 0.1.9 |
| Versioned migrations | Zaku (`sql`, `migrator`) | formalize the schema version + migration list before the collection work touches the database | engine | with 0.2.0 |
| Keyboard completeness | none of them advertise it | a `?` sheet built from the binding table, plus a palette audit test | UI | 0.1.9 |
| Crash dumps and symbol upload | Zaku | **not doing** — conflicts with "no telemetry" | — | — |
| Plugin runtime | Yaak, Bruno | **not doing** — out of scope | — | — |
