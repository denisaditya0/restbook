# restbook — UI plan (GPUI)

How the desktop shell is composed, and what comes next. The engine has its own
plan in [`03-backend-plan.md`](03-backend-plan.md); scope and non-goals stay in
[`00-scope.md`](00-scope.md). For the practical side — recipes for panes, tabs,
actions, styling, and the GPUI gotchas — see [`05-ui-guide.md`](05-ui-guide.md).
For what is deliberately still missing (tab verbs, dirty semantics, panel
management), see [`08-qol-backlog.md`](08-qol-backlog.md); milestones here stay
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

The milestone tables — what is done, what is not, and the release lanes they
belong to — live in [`01-milestones.md`](01-milestones.md). This document keeps
the rules a pane has to satisfy and the visual grammar those milestones are
judged against.

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
[`08-qol-backlog.md`](08-qol-backlog.md) section 7, so a gap is not mistaken for
an oversight.
