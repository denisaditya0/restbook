# restbook — QoL backlog

What the app is missing once the core flows work. This is a working list, not a
plan of record: `02-ui-plan.md` owns milestones, `07-flows.md` owns "where does
this click land", and this file owns "what still feels unfinished".

Every item names the code that makes it true, so an item that gets fixed can be
checked off without re-deriving it. Items are ordered by user pain, not by size.

**Status:** three items are open: **6.3** (the `restbook-ui` crate split),
**6.5** (`workspace/mod.rs` is one impl block holding five jobs), and **6.6**
(two type scales that disagree). All stay deferred. Open work, including those,
is tracked in [`01-milestones.md`](01-milestones.md). The per-item text below is
the record of what was wrong and what fixed it.

Priority:

| Priority | Meaning |
|---|---|
| **P1** | Loses data, or breaks an expectation a user forms in the first ten minutes |
| **P2** | Expected daily-use polish; the absence is noticed every session |
| **P3** | Nice to have; does not block work |

## Closed

| Item | Where |
|---|---|
| 1.1 Closing an unsaved draft asks first | `Workspace::confirm_close_tab`, `PendingDelete::Tab` |
| 1.2 Close X on every closable tab; dot is separate | `RequestPanel::render_tab_strip` |
| 1.3 Leaving-tab draft flush; every draft flushed on quit | `Workspace::select_tab`, `Workspace::flush_draft` |
| 1.4 Close / Close Others / Close to Right / Close All | `TabAction`, `PendingDelete::Tabs` |
| 1.5 Right-click menu on request tabs | `RequestPanel::tab_menu` (`ContextMenuExt`) |
| 1.6 Reopen Closed Tab | `Workspace::reopen_closed_tab`, `ReopenClosedTab` |
| 1.7 Duplicate Tab | `Tab::copy_of` |
| 1.8 Move tab left/right | tab context menu |
| 1.9 Middle-click close; double-click blank strip | `on_aux_click`, `on_double_click` |
| 1.10 Next/Previous tab bindings | `NextTab`, `PreviousTab`, `Workspace::select_relative` |
| 1.11 Tab tooltip with method + URL | `RequestPanel::render_tab_strip` |
| 1.12 Overflow tab list | `RequestPanel::render_tab_strip` |
| 2.1 Separate modified vs would-lose-content | `is_unsaved`, `close_needs_confirmation` |
| 2.2 Dirty bound tabs are not lost on quit | `Workspace::flush`, `save_on_close` |
| 2.3 Save As for a bound tab | `Workspace::save_active_as`, `SaveRequestAs` |
| 2.4 Stable tab identity | `workspace/tabs.rs` (`TabId`, `TabEntry: PartialEq`) |
| 2.5 One owner for tab state | `workspace/tabs.rs` (`TabState`) |
| 3.1 View menu | titlebar window menu, `Workspace::panel_visible` |
| 3.2 Reset Layout command | `Command::ResetLayout`, `Workspace::reset_layout` |
| 3.3 Panel focus/close in the palette | `Command::FocusPanel` / `Command::ClosePanel` |
| 3.4 Panel dropdown menus | each pane's `Panel::dropdown_menu` |
| 3.5 Center layout decision recorded | `07-flows.md` "Default window layout" |
| 4.1 Undo for a delete | `Workspace::undo_delete`, `FlashAction` |
| 4.2 Delete dialogs name the item | `Workspace::open_delete_dialog` |
| 4.3 Batch close confirmation copy | `close_tabs_copy`, `PendingDelete::Tabs` |
| 5.1 Palette matches shortcuts | `CommandItem::keywords` |
| 5.2 Settings keybinding | `OpenSettings`, `secondary-,` |
| 5.3 Close-window policy documented | `docs/12-keyboard-shortcuts.md` |
| 6.1 Interactive preview | `src/preview.rs`, `--preview <pane>\|all` |
| 6.2 Deterministic element tree | `preview::tree` / `write_tree` |
| 6.4 Documented fast loop | `05-ui-guide.md` "The fast loop" |
| New tab opens a blank request | `Workspace::new_tab`, `Tab::blank` |
| Tab strip scrolls horizontally and selects the active tab into view | `RequestPanel::render_tab_strip`, `RequestPanel::set_tabs` |

## 1. Tabs

### 1.1 Closing an unsaved draft destroys it with no prompt — **P1**

`Workspace::close_tab` calls `store.delete_draft(id)` and removes the tab. There
is no dialog, so `Ctrl+W` or a future context-menu Close can silently throw away
a draft the user was working in.

**Fix:** reuse the `PendingDelete` pattern: add `pending_close`, let `close_tab`
park when the tab would lose content, and have `render` raise the dialog via
`window.defer`. The perform half stays the current body.

Refs: `crates/app/src/workspace/mod.rs` (`close_tab`, `pending_delete`,
`open_delete_dialog`), `02-ui-plan.md` U47.

### 1.2 The close button disappears on the tabs that most need it — **P1**

`render_tab_strip` draws the X only when `active && closable && !unsaved`; an
unsaved tab shows a dot instead. Inactive tabs have no close button at all. So
the only way to close a dirty tab is a keybinding, and it destroys the draft
(1.1).

**Fix:** show the X on hover on every closable tab and keep the dot as a
separate status glyph. The dot is the warning; it should not remove the
affordance.

Refs: `crates/app/src/panels/request.rs` (`render_tab_strip`), `02-ui-plan.md`
U14.

### 1.3 Draft persistence only covers the active tab — **P1**

`persist_draft` snapshots `self.active_tab` and writes only that row. Switching
tabs (`select_tab`) snapshots the leaving tab in memory but does not flush its
draft. If the user edits and switches within `SAVE_DEBOUNCE` (400 ms), the
leaving tab's last edits are kept in memory but not in the store; a restart
restores the older draft.

**Fix:** flush the leaving tab's draft on `select_tab`, and flush every draft
tab on quit instead of only the active one (`flush_draft`). A per-tab `dirty`
flag makes both cheap.

Refs: `crates/app/src/workspace/mod.rs` (`persist_draft`, `schedule_draft_save`,
`select_tab`, `flush_draft`, `SAVE_DEBOUNCE`).

### 1.4 No Close Others / Close to Right / Close All — **P1**

The only bulk verb is closing one tab at a time. With one confirmation per dirty
tab this becomes a click storm.

**Fix:** one `TabAction` enum (`Close`, `CloseOthers`, `CloseToRight`,
`CloseAll`) routed from the request panel to the workspace. For a batch, use one
summary dialog ("3 tabs have unsaved changes") with **Discard all / Cancel**,
not one dialog per tab.

Refs: `RequestPanel::Event`, `Workspace::close_tab`; `07-flows.md` request-panel
table.

### 1.5 No right-click menu on request tabs — **P1**

Collections rows already have the exact capability (U52) via
`ContextMenuExt::context_menu`, with one builder serving both the ellipsis and
the right-click. Request tabs are custom cells (`h_flex().id(("request-tab",
index))`) with only `on_click`.

**Fix:** `.context_menu(...)` on the tab cell, calling the same builder and
emitting `Event::TabAction`. The id is already per-tab, so element state does not
collide.

Refs: `crates/app/src/panels/request.rs` (`render_tab_strip`),
`02-ui-plan.md` U52, `gpui_kit::component::menu::ContextMenuExt`.

### 1.6 No Reopen Closed Tab — **P2**

`close_tab` removes the tab and drops its content. There is no history of closed
tabs, so an accidental close is unrecoverable (worse when the draft was already
deleted).

**Fix:** keep a small `Vec<Tab>` stack in `Workspace`, push on close (including
draft payload), clear on launch, and add a command + binding.

Refs: `Workspace::close_tab`.

### 1.7 No Duplicate Tab — **P2**

The workaround is New Tab + re-open from the sidebar, which loses the live
edits. `Tab::copy_of` existed and was removed when New Tab became blank; a
Duplicate verb is where that behavior belongs.

**Fix:** re-add `Tab::copy_of` behind `TabAction::Duplicate`, assigning a new
draft row (do not share the source's `draft` id).

Refs: `Workspace::new_tab` (history of `Tab::copy_of`).

### 1.8 No drag reorder / move a tab — **P2**

Tabs are a fixed-order `Vec`. Dock panels already drag and reorder through
`DockArea`; request tabs do not.

**Fix:** either an up/down pair in the context menu first (cheap), then
`on_drag`/`drag_over` between tab cells; update `active_tab` when moving.

Refs: `RequestPanel::render_tab_strip`, `Workspace::tabs`.

### 1.9 No middle-click close or double-click empty strip — **P2**

Both are standard browser/editor gestures, and both are one listener on an
element that already has an id.

**Fix:** `on_aux_click` (middle) = close; `on_double_click` on the strip's empty
area = new tab.

Refs: `RequestPanel::render_tab_strip`.

### 1.10 No next/previous tab bindings — **P2**

Tabs can only be selected by clicking. The palette binds `Ctrl+N`/`Ctrl+P` for
its own list, but the app has no `Ctrl+Tab`, `Ctrl+Shift+Tab`, or
`Ctrl+PageUp/Down`.

**Fix:** add `NextTab` / `PreviousTab` actions in `actions::bindings()`, handled
by the workspace with wrap-around.

Refs: `crates/app/src/actions.rs`, `Workspace::select_tab`.

### 1.11 Truncated tab labels have no tooltip — **P2**

A tab label is the URL without its scheme, capped at 220px and truncated. Unlike
the titlebar breadcrumb, the tab carries no tooltip with the full address.

**Fix:** `.tooltip(...)` on the tab cell with method + URL, as the breadcrumb
already does.

Refs: `RequestPanel::render_tab_strip`, `chrome::titlebar` (`TitlePart::with_tooltip`).

### 1.12 No overflow menu / tab list — **P3**

The strip now scrolls, but there is no "all open tabs" dropdown for a keyboard
or mouse user who does not want to hunt sideways.

**Fix:** a menu button at the strip's right edge listing `TabEntry`s, reusing the
same select event.

Refs: `RequestPanel::render_tab_strip`.

## 2. Dirty / unsaved semantics

### 2.1 "Edited since save" and "would lose content" are conflated — **P1**

`is_unsaved` is `saved.is_some_and(|baseline| baseline != current)`, so a
never-saved draft with a filled URL is *not* marked unsaved, yet closing it
deletes its draft row. Two different questions share one flag.

**Fix:** define both explicitly:

- `is_modified` — has a saved baseline and differs (drives the dot).
- `would_lose_content` — non-blank request, or an existing draft (drives the
  close confirmation).

Refs: `crates/app/src/panels/request.rs` (`is_unsaved`),
`crates/app/src/workspace/mod.rs` (`Tab.saved`, `Tab.draft`).

### 2.2 Quit does not warn about dirty saved-origin tabs — **P2**

`flush` persists drafts, so draft tabs survive a quit. A tab bound to a
collection node that was edited but not saved is lost with no prompt.

**Fix:** decide one policy and write it down:

- auto-save a draft of dirty bound tabs on quit, or
- prompt on close (hard: async dialogs during `on_window_should_close`).

The first is simpler and fits the existing draft mechanism.

Refs: `Workspace::flush`, `Workspace::flush_draft`, `save_on_close`.

### 2.3 No "Save As" for a bound tab — **P2**

A bound tab's Save always updates in place (`commit_save(node, ...)`); there is
no way to fork it into another collection. The dialog only appears for unbound
tabs, where the button is labelled "Save As".

**Fix:** add `SaveAs` as a separate action/menu item that opens the save dialog
with the current name prefilled and `node = None`, so it creates a new entry.

Refs: `Workspace::save_active`, `Workspace::open_save_dialog`,
`RequestPanel::render_url_row` (`save_label`).

### 2.4 Tab identity is an index, and `set_tabs` compares selected fields — **P2**

Reordering, closing, and the async draft write all use `usize`. `set_tabs`
early-returns when `label`, `method`, and `saved` match, so a new field added to
`TabEntry` will silently not repaint.

**Fix:** introduce a `TabId`, use it in events (`SelectTab(TabId)`,
`CloseTab(TabId)`), and derive `PartialEq` on `TabEntry` so comparison cannot
drift.

Refs: `crates/app/src/panels/request.rs` (`TabEntry`, `set_tabs`),
`crates/app/src/workspace/mod.rs` (`Tab`).

### 2.5 `Tab` (workspace) and `TabEntry` (panel) mirror each other — **P2**

Every tab field is maintained in two structs by hand. This is where the "big"
feeling of tab management comes from, not from the dock.

**Fix:** move both into a `tabs.rs` with one `TabState` and one borrowed
`TabEntry` projection. No new abstraction, just one owner.

Refs: `crates/app/src/workspace/mod.rs`, `crates/app/src/panels/request.rs`.

## 3. Panels and layout

The dock already handles drag, split, close, zoom, collapse, resize, and
`dump()`/`load()`. These items are app-level policy only.

### 3.1 No View menu — **P2**

The titlebar menu has Command palette, History, Copy log path, Copy recent log,
and Settings. Collections and Environment are palette-only.

**Fix:** add a "View" section listing every `PanelName` with a checkmark for the
visible ones; reuse `reveal_panel` / `focus_panel`.

Refs: `Workspace::render` (titlebar dropdown), `Workspace::reveal_panel`.

### 3.2 No Reset Layout command — **P2**

A broken arrangement can only be fixed by deleting `layout.json` and
restarting. `install_layout` already knows the default.

**Fix:** add `ResetLayout` to the palette: call `apply_layout` with the first-run
lists, then persist.

Refs: `Workspace::install_layout`, `Workspace::apply_layout`,
`workspace/persistence.rs`.

### 3.3 Panel close/focus is not in the palette — **P3**

`Command` covers toggling Collections and Environment and focusing History, but
not "close this panel" or "focus the next pane".

**Fix:** add `FocusPanel(PanelName)` and `ClosePanel(PanelName)` commands; the
dock APIs already exist (`remove_panel`, `focus_panel`).

Refs: `panels/command_palette.rs` (`Command`), `Workspace::run_command`.

### 3.4 `Panel::dropdown_menu` is unused — **P3**

The kit gives every dock panel a ▾ menu; none of the five panes adds anything to
it, so the menu is the generic dock menu only.

**Fix:** let a pane contribute its own items (for Request: Save As, Duplicate,
Paste cURL), which also gives dock panels a natural home for panel actions.

Refs: `gpui_component::dock::Panel::dropdown_menu`, the five pane `Panel` impls.

### 3.5 Center layout is fixed to Request-over-Response — **P3**

`apply_layout` builds `[Request, Response]` as a `v_split` and there is no user
command to rearrange the center. Dock dragging may move panels, but there is no
keyboard path and no discoverable affordance.

**Fix:** decide whether the center is user-arrangeable; if yes, expose a reset
and a "move panel" command. If no, say so in the UI guide so it stops being
reported as a bug.

Refs: `Workspace::stack_layout`, `02-ui-plan.md` U2a.

## 4. Dialogs and destructive actions

Prompts are app-only by construction: `crates/core`, `crates/transport`, and
`crates/store` have no GPUI dependency, so the engine cannot ask a question and
never tries to. Core validates and performs a delete; the workspace decides
whether to ask first and how to phrase it. Every item below is therefore a
`crates/app` change, and adding a dialog must not push policy into the engine.

### 4.1 No undo / soft delete — **P2**

Deleting a collection, folder, request, environment, or draft is permanent.
Delete has a confirmation (unlike close), but no recovery.

**Fix:** either a short-lived undo toast ("Collection deleted · Undo") backed by
the deleted value, or a trash marker in the store. The toast path is cheaper and
matches the existing flash.

Refs: `Workspace::perform_delete`, `chrome::flash`.

### 4.2 Delete confirmations are generic — **P3**

"Delete this item? This cannot be undone." does not name the item or show its
path. The dialog can already name environments; items and collections could too.

**Fix:** pass the row's display name and, for items, `collection / folder` path
into `open_delete_dialog`.

Refs: `Workspace::open_delete_dialog`, `collections_panel::RowTarget`.

### 4.3 No batch confirmation copy — **P2**

Once Close Others exists (1.4), the dialog needs a clear summary and a danger
action that does not read like it saves. This is a copy/design decision to make
with 1.4, not separately.

Refs: `open_delete_dialog` (tone to match).

## 5. Keyboard and command palette

### 5.1 The palette matches labels only — **P3**

Typing `ctrl+shift+v` finds nothing; search is over the title text. The shortcut
is already computed for display.

**Fix:** index the formatted shortcut alongside the label and match both.

Refs: `panels/command_palette.rs` (`commands`, `command_hint`).

### 5.2 Settings has no keybinding — **P3**

It is palette-only and titlebar-menu-only. Every other app-level surface has a
shortcut.

**Fix:** bind `secondary-,` (the conventional preferences shortcut) to an
`OpenSettings` action.

Refs: `actions::bindings()`, `Workspace::open_settings`.

### 5.3 No close-window / close-all-windows shortcut owned by the app — **P3**

`secondary-w` closes a request tab, not the window; the platform owns the
window. Fine, but worth stating in `07-flows.md` so it is not mistaken for a
missing binding.

Refs: `actions::bindings()`.

## 6. Developer experience

### 6.1 No interactive preview / storybook — **P2**

`TestAppContext` render tests are headless and record pixels but cannot compare
or display them. Seeing a pane requires the whole window, dock, store, and
updater.

**Fix:** add `--preview <pane>` and `--preview all` to `main.rs` (plain
`env::args`), root view = one pane with a fixture `Document` and `MemoryStore`,
reusing the `open_owned` wiring from `response.rs`'s render tests.

Refs: `crates/app/src/main.rs`, `panels/response.rs` (`mod render_tests`).

### 6.2 No fixture/screenshot path — **P2**

Render tests exercise every response shape, but there is no way to dump the
element tree or a deterministic screenshot for a visual diff.

**Partly done.** `preview::tree` writes a deterministic bounds tree, and
`response.rs` is the one pane that dumps it (`target/preview/response-numbered.txt`).
Two gaps remain:

1. **No pane dumps a tree but the response pane**, so a change to the
   Collections header or the Settings rows has no artifact at all. Extending
   the dump is following an existing pattern: add `debug_selector`s, a
   `TREE_SELECTORS` list, and one `write_tree` call.
2. **No pixels.** The tree proves where a box landed, not whether it reads
   well. GPUI has no headless renderer on Windows, so a real screenshot needs
   Linux (Xvfb + Mesa lavapipe) or macOS (Metal). WSL Debian is a viable host
   for the Linux half; nothing verifies that `--preview` renders under lavapipe
   yet, so that is a spike, not a known-good setup.

**Fix:** (1) first, for every pane that has a `render_tests` module; (2) only if
pixel review is actually wanted, as a separate spike that either produces a
capture or records why it cannot.

Refs: `panels/response.rs` (`render_tests`), `src/preview.rs` (`tree`),
`02-ui-plan.md` U45.

### 6.3 App crate holds everything, so preview needs the full dependency graph — **P3**

**Still open, deliberate.** The preview build links the full app (updater,
reqwest blocking, optionally SQLCipher); use `--no-default-features` to drop
SQLCipher until the `restbook-ui` split is worth doing.

`crates/app` is already excluded from the engine workspace (good), but the
binary still links updater, reqwest blocking, and optionally SQLCipher. A preview
build pays for all of it.

**Fix:** split `restbook-ui` (panes, document, theme, variables) from the thin
`main`; the preview binary can then build without updater. Use
`--no-default-features` in the meantime to drop SQLCipher.

Refs: `crates/app/Cargo.toml`, `scripts/dev.sh`.

### 6.4 No documented "fast loop" — **P3**

The guide says how to run and test, but not that `cargo check` (about 7 s after
deps) is the inner loop and `cargo run --no-default-features` is the fast visual
one.

**Fix:** one short section in `05-ui-guide.md`.

Refs: `docs/05-ui-guide.md` ("Run and verify"), `scripts/dev.sh`.

### 6.5 `workspace/mod.rs` is one impl block holding five jobs — **P3**

**Still open.** Scheduled as `0.1.5`'s whole content — the split is its own
release, never the tail of a feature lane. That is the correction: inside
`0.1.4` it was the last item of a release whose visible work kept displacing
it, and it slipped twice. `test_support.rs` landed in `0.1.4` so the tests have
a harness to move onto.

At the time of writing it is 4,108 lines, of which 601 are tests: roughly 3,500
lines of code in a single `impl Workspace` spanning `new` to `status`. That block
owns the dock layout, tab state, save and delete dialogs, undo, notices, the
status line, the launch update offer, and persistence. `panels/response.rs` is
next at 2,859 (2,000 code) and `panels/collections.rs` at 2,331 (1,996 code),
but those are panes whose surface is genuinely large; the canvas is not supposed
to be.

The cost is not line count by itself. It is that a new feature lands in this
file by default, so two unrelated changes conflict, and reviewing a change
directed at tabs means reading save, undo, and layout too. `chrome.rs` holds two
bars and the flash in 623 lines, which is the size this codebase works well at.

**Fix:** five coherent modules out of the canvas, in this order, each a
behaviour-neutral move:

1. **Tabs** — `Tab`, `TabId`, `Tabs`, `select_tab`, `close_tab`, `move_tab`,
   `handle_tab_action`, `snapshot_active`, `adopt_active`, `push_tabs` (~600
   lines).
2. **Delete and undo** — `PendingDelete`, `Deleted`, `snapshot_delete`,
   `restore`, `deletion_landed`, `undo_delete` (~300 lines).
3. **Persistence** — `install_layout`, `save_layout`, `flush_draft`,
   `persist_draft`, `flush_leaving_draft`, completing the move into
   `workspace/persistence.rs`.
4. **Updates** — `check_updates_on_launch`, `open_update_offer_dialog`,
   `should_offer` (~150 lines). It arrived in `0.1.4` and has no dependency on
   the rest of the canvas.
5. **Dock and flash** — `reveal_panel`, `close_panel`, `toggle_sidebar`,
   `toggle_tools`, `dock_showing`, `panel_visible`, and the flash cluster
   (`flash`, `flash_undo`, `set_flash`, `dismiss_flash`, `schedule_flash_clear`)
   (~350 lines).

Steps 1 and 3 extend decisions already made: `workspace/tabs.rs` holds `TabId`
and `TabState`, and `workspace/persistence.rs` already owns the layout file.
Steps 4 and 5 were not separate jobs when this item was written; they became
visible once the update offer landed on the canvas.

**Do not** fold this into a feature lane. It occupies `0.1.5` whole, in a PR
with nothing else in flight, because it touches the file every other fix
touches. No user-visible change, so no capture; the suite passing before and
after is the evidence.

**The tests move with it.** `test_support.rs` (landed in `0.1.4`) gives the
split a harness: `init_ui` installs the theme before any pane snapshots its
palette and reduces motion, and `DataDir` points `RESTBOOK_DATA_DIR` at a
temporary directory so a test can open a real `Workspace` instead of building
panes by hand. Three private helpers are called from the test module
(`wrap_index`, `close_needs_confirmation`, `close_tabs_copy` for tabs;
`snapshot_delete`, `deletion_landed` for delete/undo), so their visibility drops
to `pub(super)` as they move. `folder_items`, `removed_item` and
`contains_item` are reached from the live `Deleted::restore` path — they are not
dead, and a move must carry them.

**A worked precedent.** Zedis (`github.com/vicanso/zedis`), a GPUI Kit desktop
client of about this size, split its own `src/root.rs` this way and recorded it
in `docs/agents/refactor-plan.md`. Its root was 1,440 lines and is 946; the tab
strip moved to `root/tabs.rs`, the action listeners to `root/handlers.rs`, the
desktop-only fields to a `root/desktop.rs` sub-struct, and the methods stayed
inherent `impl Zedis` blocks in those files, re-exported from the parent. That
is this item's shape, arrived at independently — and it confirms the plan is
mechanical rather than inventive.

The same document is the source of two corrections worth carrying:

- **The field count is a separate, later job.** Their state struct reached 76
  fields and 282 methods; they split it only after the files, by renaming every
  field in a copy and letting `cargo check` report each access, then grouping
  by which methods touch which fields, and keeping public methods as one-line
  delegates. Our ~30 `Workspace` fields should wait for the same treatment.
- **A long `render` is fixed by method extraction, not a module.** They took
  nine render functions of 450–685 lines down to 56–206 by naming the sections
  they already drew, with no layout change. That is the cheaper fix for
  `panels/collections.rs`'s 403-line render block, and it does not touch this
  item.

Refs: `crates/app/src/workspace/mod.rs`, `crates/app/src/workspace/tabs.rs`,
`crates/app/src/workspace/send.rs` (the `impl Workspace` in a sibling file that
already proves the mechanism here).

### 6.6 Two type scales: rem helpers and `Metrics` px disagree — **P2**

**Still open.** The panes render their text two ways, and the two do not give
the same sizes for the same role.

| Spelling | Resolves to | Uses |
|---|---|---|
| `.text_sm()` / `.text_xs()` | 14px / 12px (0.875 and 0.75 rem) | 19 files, ~55 sites |
| `.text_size(px(metrics.font_body))` and friends | 13 / 12 / 11 / 10.5 px | 5 files, 17 sites |

So `.text_xs()` and `font_small` happen to match at 12px, but `.text_sm()` is
14px where `font_body` is 13px, and `font_xs` (11px) has no rem equivalent at
all. A label styled one way next to a label styled the other is a visible
1–2px mismatch with no rule behind it, which is what makes the panes read as
"unwell" rather than as a deliberate density.

The split is not arbitrary: `09-gpui-kit-audit.md` §1.2/§1.3 deleted the raw
`px()` literals in favour of `Metrics`, and the design guide says application
layout should use rem helpers and not `px()` at all. Both changes were right on
their own; neither reconciled them with the other.

**Fix:** pick one scale and delete the other.

1. **Rem helpers, `Metrics` keeps spacing only.** Drop `font_body`, `font_small`,
   `font_xs`, and `font_badge` from `Metrics` and use `text_sm`/`text_xs`/`text_2xs`.
   Matches the design guide and the zoom axis. `font_badge` has no rem slot, so
   it either becomes `text_2xs` or stays as the one audited exception.
2. **`Metrics` everywhere.** Replace the 55 rem sites with `metrics.*` tokens.
   Keeps 13px as the body size, which is denser and is what the panes were
   tuned against, but contradicts the guide and needs an audit row.

(1) is the smaller diff and the one the guide asks for; (2) is the one that
preserves today's look. Decide by taking a capture of each pane before choosing,
since the difference is 1–2px and reads as density, not as a scale.

**Do not** fix this one file at a time: half-converted files are worse than
either end state, and the mismatch is only visible when two roles sit side by
side.

Refs: `crates/app/src/theme.rs` (`Metrics`), `09-gpui-kit-audit.md` §1.2–1.3,
`gpui-kit-design-guides` ("Zoom, base font, and `rem`").

## 7. What Zed has that we deliberately do not

This is here so the absences above do not turn into a refactor.

| Zed feature | Why we skip it |
|---|---|
| Generic `Item` trait + `ItemHandle` | We have one tabbable type (a request). An enum plus `TabId` is cheaper and type-checked. Revisit at 3+ item types. |
| `ItemRegistry` / `SerializableItem` | `PanelName` plus "persist names only" already restores layouts. A registry would rebuild live entities, which is the thing `workspace/persistence.rs` exists to avoid. |
| One `Document` per item | This is **not** deliberate over the long term; U11's single-document snapshot is the known limit (a response landing after a switch belongs to the old tab). Promote to per-tab state when tab-level scroll/pending response starts being requested. |
| Full pane toolbars | `Panel::dropdown_menu` (3.4) covers the app-level actions without a new toolbar per pane. |

Refs: `02-ui-plan.md` "Where GPUI is not like Zed" and U11 ordering note.
