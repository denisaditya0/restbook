# restbook — interaction flows

The click → effect → **POV** map: for every trigger, what it does and which
panel it shows up in. This is the document to read when asking "if I click this,
where does it land?".

It is derived from the source, and every row cites the code that makes it true.
The command list is guarded against drift by a test — see
[Keeping this honest](#keeping-this-honest).

## The two docks and three other POVs

Everything the app does lands in one of five places. If a new interaction does
not fit one of these, that is a design smell, not a missing bucket.

| POV | What it is | Where the code lives |
|---|---|---|
| **Left dock** | the sidebar: Collections | `workspace/mod.rs` `reveal_panel`, `install_layout` |
| **Right dock** | the tools dock: History / Environment | `workspace/mod.rs` `reveal_panel`, `install_layout` |
| **Center** | Request over Response, in a vertical split | `workspace/mod.rs` `install_layout` |
| **Chrome** | the titlebar (34px) and status bar (24px), app-drawn | `workspace/chrome.rs` |
| **Overlay** | a window-level dialog, sheet, palette, or toast | `window.open_dialog` / `open_sheet_at`, `chrome::flash` |

The layering rule that makes this predictable is in
[`01-ui-plan.md`](01-ui-plan.md): *the canvas decides placement, panes decide
content.* A pane never names a dock region; it emits an event and the workspace
chooses. So "where does this appear" always has one answer: look at
`Workspace::run_command` and the panel subscriptions in `Workspace::listen`.

## Default window layout

Set once in `Workspace::install_layout` (`workspace/mod.rs`) on first run:

| Region | Panes | Source |
|---|---|---|
| Left dock | `Collections` only, 260px wide | `install_layout` |
| Center | `Request` over `Response` (a `v_split`) | `install_layout` |

History and Environment are **constructed but not docked** by default. They are
revealed on demand, which is the deliberate simplification noted in
`04-ui-guide.md`: the sidebar is what you work *from*, not a log of what you
sent. A saved `layout.json` restores the user's own arrangement by name.

The center split is fixed to **Request over Response**: there is no command to
make it horizontal or to move a pane into a dock. Panes can be focused and
closed from the titlebar's **View** menu, the palette (`Focus <pane>` /
`Close <pane>`), and each pane's own dock menu, and **Reset layout** restores
this default. Rearranging the center is a deliberate non-goal for now.

## Global chrome (always visible)

| Element | Shows | Source |
|---|---|---|
| Titlebar chip | workspace name, initial in a rounded square | `chrome::titlebar` |
| Titlebar breadcrumb | tab label › URL (URL is its own tooltip, truncated at 240px) | `Workspace::breadcrumb` |
| Titlebar trailing | window-menu dropdown (palette, settings, clipboard, View panes, reset layout, log copy), window controls | `Workspace::render` |
| Status bar, far left | dock toggle: `PanelLeft` → the sidebar dock | `chrome::status_bar` (`left_toggle`) |
| Status bar left | storage chip (`local database` or the workspace path) | `chrome::status_bar` |
| Status bar middle | active environment (`Braces` icon + name) | `chrome::status_bar` |
| Status bar right | last sent URL, or the engine error message | `chrome::status_bar`, `Workspace::status` |
| Status bar, far right | dock toggle: `PanelRight` → the History + Environment dock | `chrome::status_bar` (`right_toggle`) |

The titlebar's trailing History button was **removed**: the right dock now has
its own toggle in the bottom-right corner, under the panel it opens. The window
menu's **View** section lists every `PanelName` with a checkmark on the pane its
dock is currently showing; clicking an entry reveals that pane and then focuses
it (the same pair the palette's `FocusPanel` commands run). `Reset layout` sits
at the end of that section and is the way back after a pane is closed.

Note: the status code, duration, and size are **not** here — the response pane
owns them (U43). The status bar answers "where did my data go" and "did anything
fail", nothing else.

## Commands (palette + keybindings)

Every command is a variant of `Command` (`panels/command_palette.rs`), routed by
`Workspace::run_command` (`workspace/mod.rs`). The palette is a *fixed table*,
not a registry, so this list is exhaustive.

| Command | Keybinding | Effect | POV | Source |
|---|---|---|---|---|
| Send request | `secondary-enter` | prepare → transport → store → `Document` notify | Center (Request + Response) | `run_command`, `workspace/send.rs` |
| New tab | `secondary-t` | push a `Tab` | Center (tab strip) | `Workspace::new_tab` |
| Close tab | `secondary-w` | close the active tab, empty the editor if it was the last | Center (tab strip) | `Workspace::close_tab` |
| Next tab | `ctrl-tab` / `ctrl-pagedown` | select the next tab, wrapping at the end | Center (tab strip) | `Workspace::select_relative` |
| Previous tab | `ctrl-shift-tab` / `ctrl-pageup` | select the previous tab, wrapping at the start | Center (tab strip) | `Workspace::select_relative` |
| Reopen closed tab | `secondary-shift-t` | restore the most recently closed tab | Center (tab strip) | `Workspace::reopen_closed_tab` |
| Focus URL | `secondary-l` | focus the URL field | Center | `RequestPanel::focus_url` |
| Paste cURL | `secondary-shift-v` | clipboard → `RequestDef`, adopt it, then offer to save it | Center | `RequestPanel::paste_curl` |
| Copy as cURL | `secondary-shift-c` | live `RequestDef` → `curl::to_curl` → clipboard | Window (any pane) | `Workspace::copy_curl` |
| Focus history | — (palette only) | reveal **then** focus the History pane | Right dock | `run_command` |
| Focus collections | — (palette only) | reveal **then** focus the Collections pane | Left dock | `run_command` |
| Focus environment | — (palette only) | reveal **then** focus the Environment pane | Right dock | `run_command` |
| Focus request | — (palette only) | focus the Request pane (the centre is always mounted) | Center | `Workspace::focus_panel` |
| Focus response | — (palette only) | focus the Response pane (the centre is always mounted) | Center | `Workspace::focus_panel` |
| Close history | — (palette only) | remove the History pane from its dock | Right dock | `Workspace::close_panel` |
| Close collections | — (palette only) | remove the Collections pane from its dock | Left dock | `Workspace::close_panel` |
| Close environment | — (palette only) | remove the Environment pane from its dock | Right dock | `Workspace::close_panel` |
| Close request | — (palette only) | remove the Request pane from the centre; `Reset layout` restores it | Center | `Workspace::close_panel` |
| Close response | — (palette only) | remove the Response pane from the centre; `Reset layout` restores it | Center | `Workspace::close_panel` |
| Toggle environment | — (palette only) | reveal the Environment pane | Right dock | `reveal_panel` |
| Toggle collections | — (palette only) | reveal the Collections pane | Left dock | `reveal_panel` |
| Reset layout | — (palette only) | apply the first-run arrangement and persist it | Left dock + Center | `Workspace::reset_layout` |
| Settings… | `secondary-,` | open the Settings dialog | Overlay (dialog, 560px) | `Workspace::open_settings` |
| Zoom in | `secondary-=` | scale the whole UI up, persist to `settings.json` | Chrome (all panes redraw) | `Workspace::set_zoom` |
| Zoom out | `secondary--` | scale the whole UI down | Chrome (all panes redraw) | `Workspace::set_zoom` |
| Reset zoom | `secondary-0` | return zoom to 100% | Chrome (all panes redraw) | `Workspace::set_zoom` |

`secondary` is ⌘ on macOS and Ctrl elsewhere. `FocusPanel` reveals before it
focuses because History, Environment, and Collections are docked, and a focus
request for a pane no dock shows would aim at nothing; the centre panes are
always mounted, so the reveal is a no-op for them. Closing a pane removes it
from the tree but keeps the entity, so `Reset layout` puts it back.

Every palette row also indexes its formatted shortcut, so typing `ctrl+shift+v`
finds Paste cURL instead of only matching the label.

## Panel-level triggers

These are events a pane emits and the workspace handles (in `Workspace::listen`).
This is the "intent as event" rule in action.

### Request panel → workspace

| Trigger | Event | Effect | POV | Source |
|---|---|---|---|---|
| Send button | `Event::Send` | `Workspace::send` (guard: `can_send` and no unresolved `{{var}}`) | Center | `request.rs`, `send.rs` |
| Cancel button | `Event::Cancel` | `document.cancel` | Center | `request.rs` |
| Save button / `secondary-s` | `Event::Save` | update in place if tab is bound, else open the save dialog | Center, or Overlay | `Workspace::save_active` |
| Editor tab click | `Event::SelectTab(i)` | switch tabs | Center (tab strip) | `Workspace::select_tab` |
| New-tab button | `Event::NewTab` | push a tab | Center | `Workspace::new_tab` |
| Tab close button / `secondary-w` | `Event::CloseTab(i)` | close that tab; a tab with unsaved edits asks through the close alert dialog first | Center, or Overlay (alert dialog) | `Workspace::confirm_close_tab` |
| Paste-curl imported | `Event::Imported` | flash a toast, then open the Save As dialog when a collection is selected | Overlay (bottom-right, optionally a dialog) | `Workspace::imported` |
| Paste-curl failed | `Event::Notice(_)` | flash a toast with the parse error | Overlay (bottom-right) | `Workspace::notice` |

### Sidebar → workspace

| Trigger | Event | Effect | POV | Source |
|---|---|---|---|---|
| History row click | `HistoryEvent::Picked { id }` | load the full entry, adopt the request **and** show the stored response | Center | `Workspace::listen` |
| Collections: request row click | `CollectionsEvent::Open { node, request }` | select the tab already holding `node`, else open the request in its own tab (taking over only the lone pristine placeholder), bind the tab to the node, delete any stale draft | Center | `Workspace::open_saved` |
| Collections: draft row click | `CollectionsEvent::OpenDraft { id }` | select the tab holding that draft, or open it | Center | `Workspace::open_draft` |
| Collections: any mutation | `CollectionsEvent::Notice(_)` | flash a toast (create/delete/rename/save) | Overlay (bottom-right) | `Workspace::notice` |
| Collections: delete a row | `CollectionsEvent::ConfirmDelete(target)` | open the delete alert dialog; delete on confirm | Overlay (alert dialog) | `Workspace::open_delete_dialog` |
| Environment: delete | `EnvironmentEvent::ConfirmDelete(name)` | open the delete alert dialog; delete on confirm | Overlay (alert dialog) | `Workspace::open_delete_dialog` |

### Response panel → workspace

| Trigger | Event | Effect | POV | Source |
|---|---|---|---|---|
| History clock control | `ResponseEvent::ShowHistory { hits }` | park the hits, open the history sheet from render | Overlay (right sheet, 440px) | `history_sheet::open` |
| Body / Headers / Cookies tab | (local) | switch the response view | Center (Response) | `response.rs` |

### Collections panel (in-pane)

| Trigger | Effect | POV |
|---|---|---|
| Collection switcher → a collection | select it | Left dock |
| Environment chip → an environment | set the active environment | Left dock |
| `+` menu → New Request / New Folder / New Collection | create (New Request needs a selection) | Left dock |
| Sort → Manual / Name A-Z / Name Z-A | re-sort the tree | Left dock |
| Row tools: up / down / rename / delete | move, rename (label only), or delete (opens the alert dialog) | Left dock → Overlay |

### Environment panel (in-pane)

| Trigger | Effect | POV |
|---|---|---|
| Environment picker → `(none)` / a name | set active, follow that target | Left dock |
| New / Delete / Active | create, delete (opens the alert dialog), or activate | Left dock → Overlay |
| Per-row: enabled / key / value / secret / reveal / `x` | edit the variable rows | Left dock |
| Inspector | read-only: which scope won each `{{name}}`, or `unresolved`; a `{{$generator}}` is never unresolved (it renders at send time) | Left dock |

### Request editor tabs (in-pane)

Five tabs, from the `EditorTab` enum: **Params, Headers, Body, Auth, Settings**.

| Tab | Controls |
|---|---|
| Params | key/value table (enabled toggle, remove, Add), `{{…}}` completion |
| Headers | key/value table |
| Body | kind strip: None / Raw / Form / Multipart / Binary; Raw has mime buttons + a code editor; Multipart has a parts table with Choose… |
| Auth | kind strip: None / Basic / Bearer / API key; API key has a "query string" checkbox |
| Settings | timeout, follow-redirects, insecure TLS |

The Send guard treats `{{$generator}}` differently from `{{name}}`: generators
(`$randomInt`, `$uuid`) resolve at send time in `restbook_core::template`, so
the unresolved list and the environment inspector skip every `$`-prefixed name.
Only a scope lookup can be unresolved.

## Overlays and dialogs

The workspace is the only thing that opens a window-level surface. Three of them
are opened with a deliberate two-step: an event has no `Window`, so it parks the
request (`pending_save`, `pending_history`, `pending_delete`) and `render`
consumes it with `window.defer`.

| Surface | Triggered by | Position / size | Source |
|---|---|---|---|
| Command palette | `secondary-shift-p`, titlebar menu | centered dialog, 520px | `command_palette::open` |
| Settings | palette `Settings…`, titlebar menu | centered dialog, 560px | `settings_view::open` |
| Save request | Save on an unbound tab | centered dialog, 460px | `Workspace::open_save_dialog` |
| Delete confirmation | a row's trash button (Collections) or Delete (Environment) | centered **alert** dialog, 420px | `Workspace::open_delete_dialog` |
| History sheet | response clock control | right sheet, 440px | `history_sheet::open` |
| Flash toast | any `Notice` / `flash`, including the two dock toggles | bottom-right, 2.6s, non-blocking | `chrome::flash` |

### Dock-toggle confirmations

The two status-bar toggles are the only controls that raise a flash directly
from the workspace rather than from a pane's `Notice`. Both read the dock state
*before* acting; `toggle_tools` also re-reads it after the reveal — because a
dock that opens off-screen, or under a stale saved layout, looks exactly like a
button that never fired.

| Toggle | Shortcut | Opens | Message | `FlashKind` | Source |
|---|---|---|---|---|---|
| Sidebar (far left) | `secondary-\` | `PanelName::Collections` in the left dock | `Sidebar shown` | `Success` | `toggle_sidebar` |
| Tools (far right) | `secondary-/` | `PanelName::History` in the right dock (History + Environment tabs) | `History and Environment shown` | `Success` | `toggle_tools` |
| Tools (when the reveal did not take) | — | — | `Could not show the tools dock` | `Error` | `toggle_tools` |

Shortcuts are read live from the keymap (`actions::shortcut`) for each button's
tooltip, so this table is the only place the strokes are written down.
`toggle_tools` re-reads `is_dock_open(Right)` after `reveal_panel` and flashes
`Error` when the dock stayed shut; `toggle_sidebar` does not yet re-read and so
always claims success — an inconsistency worth closing if that path ever fails.

Settings is a **dialog, not a pane** on purpose (`04-ui-guide.md`): it is
app-level, there is exactly one, and it has no per-request state to dock.

## How a send flows

```
Send button (or secondary-enter)
  RequestPanel emits Event::Send
    Workspace::listen → Workspace::send
      ├─ read the LIVE editor (current_request), not a possibly-stale copy
      ├─ prepare(&request, scopes)          // pure; Generators::now() for {{$…}}
      │    └─ Err → document.fail_without_sending, no network
      ├─ document.begin_send(cancel, progress, url)
      ├─ spawn: transport.execute_with_progress   (progress watcher ticks at 100ms)
      │    └─ on Ok → persist history off the foreground, trim to retention
      └─ document.finish_send + response
           RequestPanel + ResponsePanel redraw
```

The editor is read live at send time: raw-body and auth inputs notify their own
`InputState`, so a previously-stored copy can be one keystroke stale.

## Keeping this honest

The command table above is the part most likely to drift. `Command` in
`panels/command_palette.rs` is guarded by a test that fails if a variant is added
without appearing in this document, mirroring how `actions::bindings()` is
guarded by `every_binding_parses` (`actions.rs`). See the `mod tests` block in
`command_palette.rs`.
