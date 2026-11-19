# restbook — UI plan (GPUI)

How the desktop shell is composed, and what comes next. The engine has its own
plan in [`02-backend-plan.md`](02-backend-plan.md); scope and non-goals stay in
[`00-scope.md`](00-scope.md). For the practical side — recipes for panes, tabs,
actions, styling, and the GPUI gotchas — see [`04-ui-guide.md`](04-ui-guide.md).
For what is deliberately still missing (tab verbs, dirty semantics, panel
management), see [`07-qol-backlog.md`](07-qol-backlog.md); milestones here stay
about what ships next, not every gap.

## The one rule

**The canvas decides placement. Panes decide content.**

A pane is a GPUI entity that renders one region and knows *nothing* about where
it is shown — centre of the window, a tab, a popup, or a detached area. Only the
workspace names a `DockPlacement`.

This is the difference between the two kinds of "split" that are easy to
conflate:

| Split | Question | Cost of being wrong |
|---|---|---|
| Module split | Which file does this live in? | Cosmetic. Rename and move on. |
| Composition split | Who chooses where this appears? | Structural. A pane that calls `open_dialog`, reads a sibling's entity, or hardcodes `Left`, is now un-movable. |

The module tree follows the composition split; it is not the point in itself.

## Layering

```
main.rs        window, gpui_kit::init, build Workspace
  |
workspace/     THE CANVAS — the only place that names a DockPlacement
  |  mod.rs        DockArea, Panes, layout install/save, tab snapshots
  |  chrome.rs     the custom titlebar and the status bar (chrome, not panes)
  |  send.rs       prepare() -> Transport -> record history
  |  persistence.rs layout JSON read/write (names + sidebar width)
  |  storage.rs     where the data directory is, and which store serves history
  |
panels/        one self-contained module per pane
  |  request.rs    impl BasePanel; owns the request tab strip and the editor tabs
  |  response.rs   impl BasePanel; summary, Body/Headers/Cookies tabs
  |  history.rs    impl BasePanel; store search + rows
  |  editor/       the four tab bodies (params, headers, body, auth)
  |
document.rs    the one request/response value the panes observe
theme.rs       `Palette`: one color role per thing a pane draws
util.rs        shared leaves
```

Dependencies point downward only. A pane never names another pane.

## What "a pane" has to satisfy

`impl BasePanel` requires one method:

```rust
fn panel_name(&self) -> &'static str   // "request", "response", "history"
```

Everything else — `toolbar_buttons`, `closable`, `zoomable` — has a default.
A new pane also needs `impl Panel` (the presentation half: `title`, `tab_name`,
`title_bar`) and `EventEmitter<PanelEvent>`. The three current panes answer
`title_bar(false)`: each draws its own header, so a single-panel dock renders
no title strip at all.

A new pane costs a file, the trait impls, a `PanelName` variant, and a
constructor in `Workspace::new`.

The pane cannot be constructed by `DockArea`. The workspace builds it, holds it
as a typed `Entity`, and hands the dock `panel_handle(entity)` — the wrapper
that lets the skin read the presentation trait. A bare `Entity` still docks,
but the skin falls back to drawing `panel_name` as the title.

## Communication

Three channels, in order of preference:

1. **`Document`** — shared observed state. A pane reads it; the canvas writes it.
   Request being edited, response being shown. Most things are this.
2. **Events** — a pane emits an intent it cannot serve (`RequestEvent::Send`,
   `HistoryEvent::Picked`). The workspace subscribes and performs the effect.
   Panes stay ignorant of each other.
3. **`WindowExt`** — `open_dialog`, `open_sheet`, `push_notification`. Called by
   the canvas, never by a pane. A pane that opens its own dialog has quietly
   decided it is allowed to be modal, which is a placement decision.

## Widgets are not panes

A widget takes plain data and a callback and renders a row/table/field. It is
allowed to be reused in Params, Headers, a form body, later an environment
editor — so it must not know about panes, docks, or `DockPlacement`. The kv
table is the canonical example and the reason the extractable logic lives in
small functions that unit-test without an `App`.

Deletion test for any new module: *if I delete this pane, does anything else
break?* If yes, it was not a pane.

## Milestones

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
| X1 | Responsive width policy | The app has no per-split minimum: `gpui-base` floors every dock and every centre slot at `PANEL_MIN_SIZE` (100 px) and exposes no `size_range` to the app, so panes were designed to survive that instead. First slice **done**: a window floor (`MIN_WINDOW_WIDTH`/`MIN_WINDOW_HEIGHT` in `main.rs`), the URL row collapsing at 340 px, and scrolling tab strips in both panes. Remaining: state the rule in `06-flows.md`, decide whether a dock may auto-collapse below a threshold, and either accept 100 px as the floor or carry a `gpui-base` patch that exposes a per-split minimum |
| X2 | One `Document` per tab | Today the workspace keeps one `Document` and snapshots it into the tab being left (U11), so a response that lands after a switch is written into whichever tab is active (audit §7.1). Per-tab documents make a late response land in the tab that sent it, and let a tab keep its own scroll/focus state. Every `Entity<Document>` consumer changes signature, so this is the largest `workspace/` change left |
| X3 | Save and quit semantics | Quitting persists drafts but silently drops edits to a tab bound to a collection node (backlog 2.2); a bound tab cannot `Save As` (2.3); an empty name in the save dialog is refused with no explanation. The decision is auto-save-on-quit versus prompt, and it changes what a user may safely assume about closing the window |
| X4 | First run and empty states | U47 chose a blank request over restoring the last draft, and drafts reopen only from the sidebar's Unsaved list. First-run layout, the empty sidebar, the idle response, and history-with-no-hits are still mostly blank. Deciding what a new window opens on changes the default `layout.json` and the first ten minutes |
| X5 | One command table | The palette, the titlebar window menu, and `actions::bindings()` are three hand-kept lists (audit §5.2), and there is no View menu for a `PanelName` grid. Deriving all three from one `Command` table (plus `Command::FocusPanel`/`ClosePanel`, backlog 3.3) changes where a command is discoverable rather than adding one |
| X6 | Keyboard and focus policy | `Tab` currently moves focus, so accepting a completion needs arrows or the mouse (U35); rename has no binding (F2); there is no focus-next-pane. Changing `Tab` inside the completion lists changes a habit on every input in the app |

### Not yet

Candidates discussed after U30, roughly by value. The detailed rationale lives
under [“Next”](04-ui-guide.md#next-completion-generators-and-keys) in the UI
guide.

| # | Deliverable | Notes |
|---|---|---|
| U34 | Collection/Request-level scopes in completion | `Variables::scoped`/`scoped_names` only produce Global + Environment today; the inspector and `secret_of` follow the same two. Engine half is B22 |
| U35 | `Tab` accepts in the URL/popup lists | Blocked by `Root` binding `tab → focus_next` before `on_key_down`; needs a context-scoped binding, like U32. Grouped under X6 |
| U36 | Scroll the completion list to the selection | A 9-item generator list fits; 140 faker names would not |
| U37 | More generator shapes toward Yaak | `hash.*`, `base64.*`, `url.*`, `regex.*`, faker names — the `Generators::call` dispatch is the seam |
| U38 | Request phase timings | The transport already tracks `sent`/`headers_received`/`received`; render locally only (scope forbids analytics) |
| U56 | Find in response body | A query field with match count and next/previous, for the pretty rows and the raw block alike. Today a large body can only be read, not searched |
| U57 | Save response body to a file | `ResponseBody::File` exists (B18 step 1) but nothing in the UI can write one, so a downloaded body only lives in memory |
| U58 | Cookie management | The Cookies tab lists what the response set; `Transport::clear_cookies` exists but is wired to nothing, and a cookie cannot be edited or deleted from the UI |
| U59 | History filters and per-row delete | The history sheet/pane searches by URL text (`Store::search_history`) and has Clear All; method, status, and date ranges are not offered, and one row cannot be removed |
| U60 | Drag to reorder | Tabs and collection rows reorder only through the context menu's Move up/down (backlog 1.8). Drag is the gesture users try first |

### Ordering notes

- **U4a is done**, with the Body/Headers/Cookies tab bar and the format
  control. The bytes were already in `ResponseRecord.headers`, so it was a
  render, not plumbing.
- **U5 is done, including the restore.** `HistoryEvent::Picked { id }` hands the
  row to the workspace, which calls `Store::get_history` (backend B6) and gives
  the editor the whole `RequestDef` — params, headers, body, auth — so a row can
  be re-sent as recorded. The stored response is shown without sending. This is
  the one place where the two plans were order-dependent; see
  [`02-backend-plan.md`](02-backend-plan.md).
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

## Visual grammar

The target is the shape of a modern GPUI API client (Zaku): dense 26–38px rows,
13px text, hairlines rather than boxes, one selected row fill, method-colored
verbs, and monospace for URLs and bodies. Content sits on `background`; strip
and header surfaces use `tab_bar`; the editor tab band and other recessed
strips use `segmented`; list panes use `sidebar`; the titlebar uses `title_bar`
and the status line `status_bar`, both over `chrome_border` hairlines.

Chrome (titlebar, status bar) is drawn by the app, not by the platform, so the
sidebar and the tab strip run to the top edge and a window of rows reads as one
surface. `workspace/chrome.rs` holds both; they are functions returning an
element, not panes, because a status bar the user could drag into a tab strip
would be a worse product than one that is always at the bottom.

Not done, in rough order of visual payoff:

- Request tabs inside the titlebar, as the reference does. That is a change to
  the U13 decision (tabs live in the request pane), so it needs a deliberate
  call; the breadcrumb the titlebar shows today is the cheaper half of the same
  idea.

## Theming

A theme is a data file, not code. `crates/app/src/theme.rs` embeds the built-in
`themes/default-dark.jsonc` with `include_str!` so the packaged app needs no
loose files (see `scripts/package.sh`), and overlays a user file at
`<data dir>/themes/<name>.jsonc` chosen by the `theme` key in `settings.json`.
Every field is optional, so a file can patch one color or one metric and inherit
the rest.

The built-in file carries a soft charcoal `colors` block so dark mode does not
inherit the component theme's near-black; its `"appearance": "dark"` scopes
those colors to dark mode, and in light mode every role still falls through to
the live component theme (`cx.theme()`). A user file with no `appearance`
overlays in both modes. Every color role is optional, so a file only needs the
ones it wants to *differ* from, and an omitted role keeps the component theme's
value.

Appearance is a separate axis (`Appearance`: System / Light / Dark) and a
separate `settings.json` key. `ThemeMode` has no `System` variant, so `System` is
resolved against the `WindowAppearance` the platform reports for the window, and
the workspace observes OS appearance changes to re-resolve live; `Light`/`Dark`
pin it and release the window back to the OS only for `System`.

The same file carries `metrics` — font sizes, row and bar heights, padding — so a
pane asks for a role (`metrics.row_height`) rather than writing `px(26.)`.
Zoom (U24) is one multiplier over that block (`Metrics::scaled`), which is what
lets `Ctrl+=` scale the whole window without editing a `px(…)` anywhere.

## Where GPUI is not like Zed

Zed is built on GPUI and composes its UI from dockable panels, so the primitives
here (`DockArea`, `BasePanel`, `PanelView`, `WindowExt`) are the same family as
Zed's — `gpui-kit` is a facade over `gpui-base` / `gpui-component`. Two
differences to keep in mind:

- Zed resolves panels through a registry keyed by name; this app deliberately
  does **not**. Panes are built by the workspace and kept typed, and a saved
  layout is read for its *names only*. A stale name in `layout.json` is dropped
  rather than reconstructing something unexpected.
- Zed's pane traits carry far more (toolbars, menus, dropdowns). Here only
  `panel_name` is required, so a pane stays a leaf as long as possible.

The consequences of those choices — no registry, no per-pane toolbar, no
`Item` abstraction for request tabs — are listed in
[`07-qol-backlog.md`](07-qol-backlog.md) section 7, so a gap is not mistaken for
an oversight.
