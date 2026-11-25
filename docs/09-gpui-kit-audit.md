# restbook — GPUI Kit conformance audit

Where `crates/app` departs from the GPUI Kit guides. The guides are the
authority; this file records the gap between them and the code, so a fix can
start from a list instead of a re-read.

The rules audited are the two skills installed at
`.agents/skills/gpui-kit/references/coding-guides.md` and
`.agents/skills/gpui-kit-design-guides/references/design-guides.md` (source:
`longbridge/gpui-kit`, installed with `npx skills add longbridge/gpui-kit`).
[`05-ui-guide.md`](05-ui-guide.md) says how to build here; this file says where
the build has drifted. Milestones stay in [`02-ui-plan.md`](02-ui-plan.md), open
polish in [`08-qol-backlog.md`](08-qol-backlog.md).

Audited 2026-11-18 against the working tree: all of `crates/app/src`, with the
engine crates (`core`, `store`, `transport`) out of scope. Every item below was
checked against the named lines; the code and the guides both still win over
this file.

Priority follows `08-qol-backlog.md`: **P1** loses data or breaks a first-ten-
minutes expectation, **P2** is noticed every session, **P3** is polish.

## Status — fix pass complete

Every numbered item was fixed on 2026-11-18, except the three recorded under
**Accepted exceptions**. The sections below keep the finding and the fix each
one got, so they read as history, not as a to-do list.

- **§1 Theme and metrics** — done. `Metrics` gained `indent`, `icon_sm`, and
  `tool_lane` (all scaled by `scaled`); `Palette` gained `danger_wash` and
  `danger_border`, so the response highlight no longer uses opacity literals.
  The duplicate `px()` constants are gone, including `chrome::STATUS_HEIGHT`
  (the status bar and flash now take `&Metrics`). Dialog widths use the rem
  helpers, and the dock widths multiply by `theme::zoom(cx)` at all five call
  sites — the audit listed four.
- **§2 Stable identity** — done. `kv_table::Row`, `parts_table::PartRow`, and the
  environment `Row` carry an `id` from an atomic counter, and every remove/action
  payload is that id. Request tabs, completions, and crumbs key on `node` /
  `draft` / `label` / segment; appearance, body-kind, and auth-kind key on the
  enum variant name.
- **§3 Render purity** — done, with one exception recorded in place. The
  environment panel loads through `defer_load`; `delete_environment` is the one
  caller with no window, so it still queues. `RequestPanel` no longer flushes the
  editor: the flush moved to the editor's own `observe_in`, because
  `window.defer` cannot reach a `Context<RequestPanel>` and deferring past the
  synchronous document write leaves `has_pending()` true during it.
- **§4 Component semantics** — done except §4.1 (rejected, see exceptions) and
  §4.5/§4.6 (both are deletions, held back deliberately). `.primary()` as a
  selection marker became `.ghost().selected(..).toggled(..)`; icon commands are
  `Button`s; the tab close is a real `Button` with a tooltip.
- **§5 Keyboard and actions** — done. Three `on_action` handlers for
  `ZoomIn`/`ZoomOut`/`ResetZoom` on the workspace root, plus the two guards below.
- **§6 Copy** — done, table-driven; `07-flows.md` was updated alongside so its
  command list still matches.
- **§7 Async and error surfacing** — done, and the first item turned out to be
  two bugs; see §7.1 for what the fix actually had to cover.
- **§8 Naming** — done (`is_sendable`, `has_move_target`).
- **§9 Virtualization** — done, `uniform_list("history-rows", …)`.

`check`, `clippy --all-targets`, and `test` (144 passing) are green on the fixed
working tree.

## Every item is a fix

This file no longer carries "maybe" items. Each numbered item below has a
concrete fix. Two categories were removed after checking them against the
component APIs:

- Things that turned out correct (recorded under **Accepted exceptions**).
- Things that were guesses about the framework, not findings.

If an item cannot be fixed as described, move it to **Accepted exceptions**
with the reason — do not leave it undecided.

## Verdict

What already holds, so it is not re-litigated below:

- **One dependency.** No `use gpui::` anywhere; everything is reached through
  `gpui_kit`.
- **Capability layout.** `panels/` and `workspace/` keep state, views, and
  commands together; the engine stays behind `core`/`store`/`transport`.
- **No stray color literals.** No raw hex/`rgb`/`hsla` outside `theme.rs` and
  the theme JSONC files, where they are the theme definition.
- **Real components.** `Button`, `PopupMenu`, `ContextMenu`, `TabBar`, `Input`,
  `Tooltip`, `Checkbox` are used instead of re-rolled widgets in most places.
- **Some UI tests exist.** `#[gpui_kit::test]` with real key dispatch in
  `completion.rs` and `request.rs`.

| Area | State |
|---|---|
| Crate/dependency boundaries | Follows |
| Theme *values* (no raw colors) | Follows |
| Theme *plumbing* (Palette/Metrics used everywhere) | Fixed — §1 |
| Stable element identity | Fixed — §2 |
| Render purity | Fixed — §3 |
| Component semantics | Fixed by named builders — §4 (4.1, 4.5, 4.6 excepted) |
| Keyboard/actions | Follows — §5 (zoom handlers added) |
| Copy | Fixed — §6 |
| Async/error surfacing | Fixed — §7 |
| Virtualization | Follows — §9 |

## 1. Theme and metrics — **P2**

`theme.rs` is the product token layer: panes read `Palette` and `Metrics` so a
theme file or a zoom step reaches everything. The module doc says so
(`theme.rs:1-6`: "not `cx.theme()` or inline `px(…)`"). Several panes
contradict it.

### 1.1 Panes read `cx.theme()` instead of `Palette`

`cx.theme().muted_foreground` bypasses the theme file, so those strings keep the
component theme's muted color while their neighbours change.

Sites: `panels/editor/auth.rs:76,109`, `editor/body.rs:68,136,156,180`,
`editor/headers.rs:22`, `editor/params.rs:22`, `editor/settings.rs:17,85`,
`panels/command_palette.rs:104`, `workspace/settings_view.rs:104,121,135,302,
402,413`, `workspace/mod.rs:1160,1168`.

**Fix:** the editor already owns `self.palette` (`editor/mod.rs:99`, rebuilt at
`:197`), so those become `self.palette.muted` / `self.palette.danger`. For
`settings_view.rs`, `command_palette.rs`, and `workspace/mod.rs`, call
`Palette::new(cx)` (a global read, `theme.rs:476`) and reuse it within the
function. Do not add a second palette type.

### 1.2 Literal `px()` that duplicates an existing metric

- `panels/collections.rs:610` `h(px(36.))` = `Metrics::header_height`.
- `collections.rs:933`, `panels/request.rs:561` `text_size(px(10.5))` =
  `font_badge`.
- `panels/response.rs:39` `ROW_HEIGHT = 18.` and `:48` `BODY_TEXT_SIZE = 12.`
  re-declare `body_row_height` and `font_small`; the pane already holds
  `metrics`.
- `panels/history.rs:280` `h(px(36.))`; `workspace/chrome.rs:18`
  `STATUS_HEIGHT = 24.`.

**Fix:** delete the four constants; read `metrics.header_height`,
`metrics.font_badge`, `metrics.body_row_height`, `metrics.font_small`,
`metrics.status_height`. `response.rs` already takes `metrics`, so
`BODY_TEXT_SIZE` → `self.metrics.font_small` at `:260,1011,1062` and
`ROW_HEIGHT` → `self.metrics.body_row_height` at `:1074`.

### 1.3 Literals that never zoom

Values outside `Metrics`, so zoom scales the row and not what sits in it:

- `collections.rs:34` `INDENT = 12.`, `:44` `TOOLS_WIDTH = 22.`,
  `:1458`/`:1305` `pl(px(6. + depth * INDENT))`, `:1112` `size(px(15.))`,
  `:1119` `text_size(px(9.5))`, `:957` `h(px(24.))`, `:754` `max_w(px(120.))`.
- `request.rs:604` `h(px(38.))`, `:731` `h(px(32.))`, `:972` `h(px(30.))`,
  `:541` `max_w(px(220.))`, `:693` `min_w(px(76.))`, `:811` `min_w(px(84.))`.
- `editor/body.rs:83` `h(px(180.))`; `editor/kv_table.rs:380` `max_h(px(240.))`;
  `editor/parts_table.rs:251` `w(px(140.))`; `editor/settings.rs:24,31,46,70`
  `w(px(96.))`/`w(px(120.))` (the 96 column is repeated three times);
  `panels/environment.rs:654` `w(px(160.))`; `workspace/mod.rs:1191`
  `w(px(460.))`; `command_palette.rs:120` `w(px(520.))`.
- `panels/history.rs:193,201,230,242` `px(5./44./28./2.)`;
  `panels/response.rs:1036,1044` `px(12. + 7. * …)`, `px(24.)`.
- `workspace/mod.rs:66,71` `HISTORY_WIDTH`/`TOOL_WIDTH` as raw `f32`, so the
  dock panes stay 260/400 px at every zoom.
- `response.rs:1388-1390` `palette.danger.opacity(0.12/0.4)` literals.

**Fix:** add three fields to `Metrics` (`indent`, `icon_sm`, `tool_lane`) and
derive the rest from existing fields — the three table-band heights come from
`header_height`, the row paddings from `gutter`/`chip_padding`. Dialog widths
are not tokens: use the rem helpers (`w_128()`, `w_96()`), which is what the
guide asks for application layout. Dock widths multiply by
`theme::zoom(cx)` at the call sites (`mod.rs:695,718,719,1729`). Add a
`danger_wash` role to `Palette` for the response highlight instead of opacity
literals.

### 1.4 Hard-coded font family

`workspace/settings_view.rs:285,475` `.font_family("monospace")` although
`Palette::mono_font` exists and `workspace/chrome.rs:253` uses it.

**Fix:** `Palette::new(cx).mono_font`.

## 2. Stable identity — **P1**

### 2.1 Index-keyed rows that can be removed

The guide's named failure: "never derive identity from a mutable list index".

- `editor/kv_table.rs:234,250,266,288,294` — rows removed by
  `Rows::remove(index)`.
- `editor/parts_table.rs:183,199,210,225,230,237,243,250` — same; a part has no
  stable id at all.
- `panels/environment.rs:422,431,447,452,469,491,506` — removable rows.
- `editor/kv_table.rs:344,378` — completion id keyed by index into a list that
  resets every keystroke.

After a removal every later row changes identity, so retained keyed/focus state
attaches to a neighbour.

**Fix:** give `kv_table::Row` (and the parts row) a `id: u64` from the same
`new_id()`-style counter, key every element on that id, and replace the
`RowAction::Remove(index)` payloads with the id. `Rows` already owns the row
struct, so the change is local to `kv_table.rs`/`parts_table.rs` and their
handlers.

### 2.2 Index-keyed tabs, completions, crumbs

`request.rs:535` `("request-tab", index)` and `:591`
`("request-tab-close", index)`; `request.rs:837` `("url-completion", index)`;
`workspace/chrome.rs:65` `("crumb", index)`.

**Fix:** tabs key on the `Tab`'s `node`/`draft` identity (both already exist,
`workspace/mod.rs:162-180`); completions key on `row.label`; crumbs key on the
segment string, which is unique within one trail.

### 2.3 Identity derived from a display label

`workspace/settings_view.rs:150` `format!("appearance-{label}")`,
`editor/body.rs:49` `body-kind-{label}`, `:73` `mime-{mime}`,
`editor/auth.rs:24` `auth-kind-{label}`.

**Fix:** key the variant, not its label: `body-kind-Raw`, `auth-kind-Bearer`,
`appearance-Light`. Those are enum names, so a relabel or a localization cannot
re-key the control. `mime` is already a stable value — keep it.

## 3. Render purity — **P2**

Both writes below exist for one reason: `InputState::set_value`,
`EditorState::set_value`, and row `cx.new` all take a `&mut Window`. The
handlers that change the target do not always have one, so the write is queued
and flushed in `render`.

**Fix (both):** replace "queue + flush in render" with `window.defer(cx, …)` at
the handler. `defer` runs after the frame with a window in hand, which is the
timing this code is already using, without mutating another entity inside
render. Keep the queue only if a handler has no window at all.

**What was actually possible:** `window.defer` takes `&mut App`, not
`Context<T>`, so it cannot call back into the pane that owns the write. The
environment panel keeps its queue but drains it from `defer_load`, and the
request panel's flush moved to the editor's own `observe_in` — the seam that
notifies it of the document write. See §3.1 and §3.2.

### 3.1 `EnvironmentPanel::render` load path

`panels/environment.rs:694-706` takes `self.pending` and calls `load()`
(`:186-203`), which runs `cx.new` per row, `set_value`, and `focus()` while
rendering. `follow` (`:180`) is called from `save`/`delete_environment`, which
have no window.

### 3.2 `RequestPanel::render` flushes another entity

`request.rs:1000` `editor.update(cx, |e, cx| e.flush_pending(window, cx))` at the
top of `render`; `:955-959` calls `render_params_tab` and friends through
`update`, mutating the editor from the parent's render. `flush_pending`
(`editor/mod.rs:239-251`) exists because `adopt_request` (`:256`) is called from
a history pick.

## 4. Component semantics — **P2**

All four items use a builder that already exists; none needs a new component.

### 4.1 The request tab strip is hand-built

`request.rs:534-553` renders each tab as `h_flex` + `.on_click` +
`.cursor_pointer`, with a custom close `div` at `:588-599`, while the *same
file* uses `TabBar` for editor tabs (`:922`). No tab stop, no arrow-key
navigation, no focus ring, no accessible close button. The comment cites
"Zaku's segmented strip" as the reason — but `TabVariant::Segmented` exists.

**Fix:** `TabBar::new("request-tabs").segmented().track_scroll(&self.tabs_scroll)
.max_width(px(220.))` with `Tab` children carrying the method badge and label,
`suffix()` for the `+` button, and `Tab::closable(...)`/`on_close` for the X.
`tabs_scroll` already exists on the panel, so the scroll-into-view code
(`set_tabs`) keeps working.

### 4.2 `.danger()` on Cancel

`request.rs:766` gives Cancel `danger`. Cancelling a request destroys nothing.

**Fix:** drop `.danger()`; `outline()` if it needs to stand apart from Save.

### 4.3 `.primary()` used as a selection marker

- `workspace/settings_view.rs:153-157` — the current appearance mode.
- `editor/body.rs:49-52,64-66` and `editor/auth.rs:27-31` — the current body
  kind / auth kind.

**Fix:** `Selectable::selected(is_current)` for the styling and
`Button::toggled(is_current)` for the accessibility state (both are documented
on `Button`, `button.rs:507`). `ButtonGroup` (`button_group.rs:44`) is the
container if the strip should read as one control. Do not use `primary`.

### 4.4 Custom clickable rows and commands

Rows are `h_flex`/`div` with `on_click`, no `track_focus`, no tab stop, so
selection is pointer-only:

- `collections.rs:919-929` (drafts), `:1098-1105` (collection name),
  `:1273-1278` (folder), `:1423-1429` (request).
- `panels/history.rs:189-256` (row and its per-row delete `:239-251`).

**Fix:** the icon commands become `Button::new(...).icon(...).ghost().compact()
.tooltip(...)` — that alone fixes the missing accessible name and keyboard
path. The selectable rows stay `div`s for now but get
`track_focus`+`tab_stop(true)`+`on_action(Enter/Space)` in a second pass; a
full `List`/`VirtualList` migration is only worth it with §8.

### 4.5 `.cursor_pointer()` on non-links

10 sites: `collections.rs:927,1104,1277,1428`, `kv_table.rs:351`,
`history.rs:196,244`, `request.rs:543,589,837`.

**Fix:** delete the calls. The guide reserves the pointing hand for links;
`Link` itself is correctly unused in this app.

### 4.6 Inconsistent Add affordance

`editor/kv_table.rs:303-306` renders Add as a default button;
`editor/parts_table.rs:257-263` renders the same command `.ghost()`, with a
comment claiming it matches.

**Fix:** drop `.ghost()` in `parts_table.rs` — a form's Add is an ordinary
command, matching `kv_table.rs`.

## 5. Keyboard and actions — **P1**

### 5.1 Zoom shortcuts are bound but unhandled

**Fixed.** `actions.rs:56-59` binds `secondary-=`, `secondary-+`, `secondary--`,
`secondary-0` to `ZoomIn`/`ZoomOut`/`ResetZoom`. No `on_action` handled them, so
the shortcuts silently did nothing; only the palette path worked
(`workspace/mod.rs:1652-1654` via `Command::ZoomIn`).

Three `on_action` handlers now sit on the workspace root next to
`ToggleCommandPalette` (`workspace/mod.rs:2126-2142`), all calling the existing
`zoom_by`/`set_zoom` — the same two methods the palette reaches, so a zoom is
one code path whichever opened it.

### 5.2 Two vocabularies for one command

`actions.rs` (`Action`s) and `panels/command_palette.rs:20-34` (`Command` enum
+ `run_command` match) describe the same commands twice. The zoom divergence in
§5.1 was the direct result.

**Fix: keep both, but make them share the implementation — do not make the
palette dispatch actions.** The obvious unification (have the palette call
`window.dispatch_action`) does not work in GPUI and was checked against the
source before being rejected:

- `Window::dispatch_action` resolves the *currently focused* node and walks its
  ancestor chain (`gpui-pre-0.3.9/src/window.rs:2546`, `6473`).
- `open_dialog` renders into a window overlay (`gpui-component-0.7.3/src/root.rs:239`),
  which is a sibling of the workspace view, not a descendant. The focused node
  while the palette is open is inside the dialog.
- So `SendRequest`, `SaveRequest`, `FocusUrl`, `PasteCurl` — all handled on
  `RequestPanel`'s root div (`panels/request.rs:1013-1031`) — would never be
  reached from the palette.

What stays, and why it is not duplication: `Action`s are the *keymap*
vocabulary (one per keystroke), `Command` is the *palette* vocabulary (one per
row, and it includes four commands with no action: `FocusHistory`,
`ToggleEnvironment`, `ToggleCollections`, `OpenSettings`). Both funnel into the
same workspace methods. The rule for a new command:

1. Add an `Action` + binding if it has a keystroke.
2. Add a `Command` variant + label if the palette should list it.
3. Both call one private `Workspace` method — never a second copy of the body.

That third rule is the one §5.1 broke, and it is the only part that needs
watching in review.

### 5.3 Tests do not assert handler coverage

`actions.rs:96-100` only proves each keystroke parses, and GPUI exposes no way
to enumerate listeners on an element tree, so a generic "every binding has a
handler" test is not available. The cheap approximation is two guards, both
now in place:

- `actions::tests::every_zoom_action_has_a_keystroke` fails if a zoom action
  loses its binding (`KeyBinding::action()` is public, so the bound actions can
  be listed).
- `theme::tests::a_zoom_step_is_reversible_and_stops_at_the_limits` covers the
  arithmetic the handlers run.

The second one is deliberately a pure test, not a `#[gpui_kit::test]` driving
`set_zoom`: the theme is a process global and cases run in parallel, so a test
that writes zoom leaks into neighbours. That was observed — it made
`request::enter_tests` flaky while it was written that way.

Still open: the kv/parts completion keyboard path (`editor/mod.rs:449-495`) has
no test. Add it when that file is next touched.

## 6. Copy — **P3**

| Where | Now | Fix |
|---|---|---|
| `collections.rs:671` | `New collection...` | `New collection…` |
| `workspace/mod.rs:2079` | `Settings` menu item | `Settings…` |
| `workspace/mod.rs:2022` | `Command palette` | `Command palette…` |
| `command_palette.rs:48` | `Open settings` | `Settings…` |
| `command_palette.rs:120` | `Command Palette` | `Command palette` |
| `workspace/mod.rs:1169` | `NAME` | one shared eyebrow style, or drop the caps |
| `settings_view.rs:105` | `.to_uppercase()` | uppercase in the style, not the data |
| `workspace/mod.rs:1209-1214` | `Delete collection` / `Delete item` | name the object in the title (`Delete “Work”?`) and drop the body question |
| `response.rs:958` | `truncated for display` | `Showing the first N lines.` |
| `response.rs:1338` | `Ctrl+Enter` fallback | omit the hint when `shortcut()` is `None` |
| `settings_view.rs:388` | `Update check failed: {error}` | `Couldn’t check for updates. {error}` |

## 7. Async and error surfacing — **P1**

### 7.1 A response can land in the tab that is active when it arrives

There is one shared `Document` (`workspace/mod.rs:86`); switching tabs
snapshots it into the leaving `Tab` and loads the next (`Tab`, `:162`).
`send()` clones that entity (`send.rs:76`) and the completion writes
`document.response` (`send.rs:199-203`). Switch tabs mid-send and the arriving
response is adopted into the newly selected tab's document, while `finish_send`
clears the pending state of the shared document.

**Fixed**, and the write-up above understates it: the index was captured before
`cx.spawn`, which left two leaks.

- `select_tab` → `snapshot_active` writes the *shared* document's
  `response`/`error`/`last_url` into the tab being left. A tab sent from and then
  left mid-flight had those overwritten with its own blank state, so the arriving
  response replaced `tab.response` but the URL was gone.
- `document.pending` crossed the switch, so the tab the user landed on showed
  "Sending" and could not send — for good, because the resolution path wrote to
  the sender's snapshot and never cleared the document.

What the code does now:

- `Tab` carries a stable `id` (`new_tab_id()`, monotonic); the send is recorded
  in `Workspace::sending` by that id, not by the index it started from, so a
  close or a switch cannot redirect the result. `send()` refuses a second send
  while `sending` is set, which is the one-send-at-a-time rule the document
  already implied.
- Both completion paths branch on `active_tab_id() == Some(send_id)`. The
  off-screen branch writes `response`, `error`, *and* `last_url` into the
  sender's snapshot, then clears the visible document's inherited marker with
  `Document::forget_send` — which drops only `pending`/`progress`/`started_at`,
  leaving that tab's own frozen stats and outcome alone. `finish_send` would have
  written a zeroed summary over them.
- `select_tab` clears the marker too when the tab landed on is not the sender,
  so the new tab is sendable immediately instead of waiting for a stranger's
  request. Closing the sending tab frees the slot and clears the marker the same
  way; the orphaned transfer resolves into nothing.

The progress tick gates on the same id, so a switched-away tab stops ticking and
resumes nothing when it comes back: it gets the finished response, not a live
clock. A per-tab `Document` entity is still the larger alternative and would
change `select_tab`'s snapshot model.

### 7.2 Failures only reach the log

- `variables.rs:72,88,106` — Save/Delete write failures `log::error!` and return.
- `workspace/mod.rs:1298-1319` — history delete/clear failures.
- `document.rs:279` — a store failure renders as "no history".

**Fixed.** The variables panel emits the existing `Notice::Failed`, which the
workspace already routes to the flash and the status bar, and the history
pane's own delete/clear do the same through `Workspace::notice`. The read path
took the shorter route the audit missed: `Document` no longer proxies the
search at all — `history.rs` calls `Store::search_history` itself and renders the
failure in place of the empty state, so the "no history" message can no longer
stand in for an error. The dead `try_search_history`/`record_history_error`
plumbing went with it.

## 8. Naming — **P3**

- `document.rs:262` `can_send`, `collections.rs:1790,1796` `can_move` — new
  `can_` readers are forbidden.
  **Fix:** `is_sendable`, `has_move_target`; update the call sites at
  `request.rs:711,1016` and `collections.rs:1492-1493` plus the tests.

## 9. Virtualization — **P2**

- `panels/history.rs:166-259` renders one element per hit; `history_limit` goes
  to 1000 (`workspace/settings.rs:79-80`).
- `panels/response.rs:1070` and `panels/collections/mod.rs` (`render_list`) use
  `uniform_list` — the pattern exists, and the tree is the proof it carries a
  hierarchy.

The collections tree was the exception here on the grounds that it is
user-authored, shallow, and nested virtualization would need a flattened model
first. `0.2.2` built that model (`FlatRow`, `CollectionsPanel::flatten`) after an
imported spec landed ~6,000 expanded rows and the tree rebuilt all of them every
frame, so a hierarchy is no longer a reason to opt out: flatten, then
`uniform_list` over the visible range.

**Fix:** `uniform_list("history-rows", hits.len(), …)` at a row height derived
from `Metrics` (the rows are single-line and already uniform), keeping
`self.selected` keyed by `hit.id`.

## Accepted exceptions

Checked and deliberately left alone:

- Raw hex in `theme.rs` tests (`:580-595`), `themes/*.jsonc`, and `theme.rs:321`
  color parsing — the theme definition.
- `main.rs:63-70` window `px()` — window geometry and traffic-light placement
  are physical/platform facts, not rem layout.
- `px()` fed from `metrics.*` (e.g. `collections.rs:922,1139`) — the intent of
  `Metrics`.
- `collections.rs:996-1006` filter cache written during render — keyed on
  `(query, revision)`, so it has an invalidation owner.
- `request.rs:808-817` `Send` as `.primary()` — Send is the pane's default
  commit (`secondary-enter`).
- In-field completion popups (`request.rs:866`, `kv_table.rs:375`) instead of
  `Popover` — `Popover` moves focus to itself
  (`gpui-component/src/popover.rs:275-279`), which would break typing, and
  `Combobox` is a select, not free-text completion. This is a component gap,
  not a misuse.
- "Check now" can spawn overlapping update checks
  (`settings_view.rs:333-356`) — user-initiated, idempotent manifest GET, and
  the notification replaces rather than stacks. Not worth a global flag.
- `Settings::update` (`workspace/settings.rs:283`) — a `pub(crate)`
  whole-object setter; the naming table governs public API.
- `RowAction`/`PartAction` duplication — two small typed tables with different
  payloads; a shared abstraction would cost more than it saves.
- `#[allow(clippy::large_enum_variant)]` at `collections.rs:49` — documented
  and justified in place.
- `workspace/mod.rs:1926-1953` deferred overlays — the dialog stack is rebuilt
  after the element returns, so render-time deferral is the seam; extract it
  into a named `flush_pending_overlays` when touching the function.
- **§4.1 `TabBar` migration — rejected on the API.** Checked against
  `gpui-component-0.7.3/src/tab/tab.rs`: `Tab` has no `closable`/`on_close`, so
  the `Tab::closable(..)`/`on_close` in the fix note cannot be written. `TabBar`
  also paints from `cx.theme()` and animates its selection indicator, which would
  re-open §1.1, fight the per-method tab colors, and force `set_tabs`'
  `scroll_to_item(index)` to be re-derived from bounds. The gap that mattered —
  no accessible close — is closed with a real `Button` and a tooltip.
- **§4.5 `cursor_pointer` removal — held back.** The ten remaining sites
  (`collections.rs`, `kv_table.rs`, `history.rs`, `request.rs`) are all selectable
  rows, so the pointing hand goes with the row-to-`List` migration in §4.4's
  second pass, not before it.
- **§4.6 `parts_table` `.ghost()` — held back.** It is a one-word deletion, but
  it changes what a form's Add looks like, so it belongs with the same pass that
  settles §4.4.
- **Snapshot copy writes the Windows clipboard itself
  (`snapshot/clipboard.rs:24`).** GPUI on Windows publishes an image only under
  its own registered `"PNG"` format, which Paint, Firefox and chat clients do not
  paste from, so `cx.write_to_clipboard` produces a clipboard they cannot read.
  The snapshot is written as a `CF_DIB` bitmap instead. The GPUI call stays the
  path on every other platform.

## Suggested fix batches

All eight batches are done; the list is kept as the order they were taken in.

1. **Keyboard truth** (§5.1, §5.3) — **done.** Zoom handlers on the workspace
   root, a binding guard, and a pure test of the zoom arithmetic. §5.2 was
   resolved by keeping the two vocabularies and documenting the shared-method
   rule, not by deleting one.
2. **Mechanical layout** (§1.1, §1.2, §1.4) — **done**, along with the 38
   `min_w(px(0.))`/`min_h(px(0.))` sites, which are now `min_w_0()`/`min_h_0()`.
   §4.5 and §4.6 are the held-back deletions, see **Accepted exceptions**.
3. **Identity** (§2.1, §2.2, §2.3) — **done.** Row ids first, then
   tabs/completions/crumbs, then the enum-variant keys.
4. **Send correctness** (§7.1) — **done.** Tab identity on the response, and the
   two leaks it was hiding.
5. **Copy** (§6) — **done**, one pass, table-driven.
6. **Component semantics** (§4.1, §4.3) — **done** for 4.3; 4.1 rejected, see
   **Accepted exceptions**.
7. **Tokens and layout** (§1.3, §3) — **done.** Three `Metrics` fields, dock
   zoom, deferred writes.
8. **Virtualization** (§9) and error surfacing (§7.2) — **done.**

What is left is one test, named in §5.3, and the three accepted exceptions.

## Keeping this current

Re-run the audit when the guides update (`npx skills update`) or when a panel is
rewritten. The mechanical checks are cheap to repeat:

```sh
grep -rn "min_w(px(0\.))\|min_h(px(0\.))" crates/app/src
grep -rn "cx\.theme()" crates/app/src
grep -rn "cursor_pointer" crates/app/src
grep -rn '"[^"]*\.\.\.[^"]*"' crates/app/src
```

What each check should still find, so a clean run is not mistaken for new
drift:

| Check | Expected hits |
|---|---|
| `min_w/min_h(px(0.))` | none — all 38 became `min_w_0()`/`min_h_0()` |
| `cx.theme()` | `theme.rs:501` (`Palette::new`, the token layer itself) and `response.rs:704` (`highlight_theme` is a text-selection argument, not a color role), plus doc comments |
| `cursor_pointer` | 9 sites, all selectable rows — §4.5, held back on purpose |
| `"..."` in a literal | none |

What each check should still find, so a clean run is not mistaken for drift:

| Check | Expected hits |
|---|---|
| `min_w/min_h(px(0.))` | none |
| `cx.theme()` | `response.rs` only — `highlight_theme` is a text-selection argument, not a color role |
| `cursor_pointer` | 10 sites, all selectable rows — §4.5, held back on purpose |
| `"..."` in a literal | none |
