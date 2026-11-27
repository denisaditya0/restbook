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
| **Center** | Request and Response, stacked or side by side | `workspace/mod.rs` `install_layout`, `Workspace::toggle_center_orientation` |
| **Chrome** | the titlebar (34px) and status bar (24px), app-drawn | `workspace/chrome.rs` |
| **Overlay** | a window-level dialog, sheet, palette, or notification | `window.open_dialog` / `open_sheet_at`, `window.push_notification` |

The layering rule that makes this predictable is in
[`02-ui-plan.md`](02-ui-plan.md): *the canvas decides placement, panes decide
content.* A pane never names a dock region; it emits an event and the workspace
chooses. So "where does this appear" always has one answer: look at
`Workspace::run_command` and the panel subscriptions in `Workspace::listen`.

## Default window layout

Set once in `Workspace::install_layout` (`workspace/mod.rs`) on first run:

| Region | Panes | Source |
|---|---|---|
| Left dock | `Collections` only, 260px wide | `install_layout` |
| Center | `Request` over `Response` (a `v_split`; the response header's split control flips it to an `h_split`) | `install_layout` |

History and Environment are **constructed but not docked** by default. They are
revealed on demand, which is the deliberate simplification noted in
`05-ui-guide.md`: the sidebar is what you work *from*, not a log of what you
sent. A saved `layout.json` restores the user's own arrangement by name.

The same file records the open tabs and which one was active
(`Workspace::restore_tabs`): a restart reopens them in the same order, on the
same tab. Only a tab with an owner is recorded — a bound request, or a draft
whose row exists — so a request typed or pasted and never sent is not restored.
A tab whose collection node was deleted while the app was closed, or a draft
whose row is gone, is dropped rather than reopened against nothing, and one
message names how many were dropped.

Switching tabs swaps the whole `Document`, not a snapshot of one: each tab owns
its request and its response, and `select_tab` hands the new entity to every pane
through `set_document`. Per-tab view state is the request editor tab, the response
Body/Headers/Cookies tab, and the body scroll position. Shared across tabs: the
pretty/raw toggle, the find query and whether its row is open, the failure
Details disclosure, the in-progress cookie edit (cleared when the tab changes),
the center orientation, and zoom. A response that arrives after the user has
switched away still lands in the tab that sent it, so the tab on screen never
shows another tab's result. An OAuth fetch is bound the same way to the tab that
asked for it, so a token that answers after a switch does not appear on the tab
now on screen.

Only open tabs hold a document: closing a tab drops its response, and a restart
reopens the tabs without their responses — History is the durable copy. One send
runs at a time (`Workspace::sending`); while it is on the wire, its own tab shows
a spinner in the strip's status slot and every other tab's **Send** is disabled
with a tooltip naming the reason, so a second send is refused rather than
silently dropped. Closing the sending tab cancels its transfer
(`document.cancel`) before the slot frees, so an abandoned send cannot keep the
wire while a new one starts.

A history pick opens the stored request and response in its own tab, reusing the
active tab only while it is an untouched page, so a pick never replaces edits or
a binding. It also resolves the row's owner: a saved send jumps to the tab
holding that node (or opens one bound to it), a draft jumps to its tab, and an
orphan or a legacy `Standalone` row opens as an unfiled request. `last_url`
follows the picked request, so the cookie jar and the history scope name what is
on screen.

A request that skips certificate verification says so on the center surface, in
`palette.danger`: a `TLS off` chip beside the URL field (glyph only in a narrow
row, hazard in the tooltip) and a `TriangleAlert` on its tab. The mark follows
the flag, so the tab a send would weaken is visible before it is selected; the
Settings editor tab carries the same words beside the checkbox that clears it.

The center opens as **Request over Response**, but it is no longer fixed. The
control is at the right edge of the **response header**, beside the status
summary: `Columns2` flips the centre to a horizontal split and `Rows3` stacks it
again, so the glyph and tooltip always name the *next* action. It lives with the
response because the split is Request-over-Response, and it moves with the pane
rather than sitting in a global bar. The palette's **Toggle side-by-side**
command does the same and is the only route while no response is on screen (the
idle state is the empty-state screen, which draws no header). The choice is
persisted: the dock stores the split's axis in `layout.json`, so a restart opens
the same way, and **Reset layout** puts it back to stacked. Panes can be focused
and closed from the palette (`Focus <pane>` / `Close <pane>`) and each pane's own
dock menu; Request and Response are also in the titlebar's **View** menu, which
leaves out the three panes that already have a status-bar toggle.

### Scroll and size

A pane never clips: when the window or a split narrows past what a pane needs,
its body scrolls (the request editor, the response body, headers, and cookies,
the environment list), and a custom region shows a scrollbar once its content
overflows. The two exceptions are the response body's horizontal scroll — it
keeps the plain scrollport so a long line keeps its full width — and the request
tab strip, which scrolls to the selected tab through a handle the pane owns.

A menu whose rows grow with the user's data scrolls inside the popup instead of
clipping at the window edge: the collection switcher (one row per collection) and
the environment switcher (one row per environment). A fixed menu never scrolls.

The window's own floor is computed, not typed: `workspace::layout_policy`
gives every region its usable width — a request or response pane 340 px
(`CENTRE_MIN`), the Collections dock and the tools dock 400 px each — and
derives the window floor from the centre alone. The side docks collapse before
a pane is squeezed, so the floor is one centre pane stacked (340 px) or two side
by side (680 px); `MIN_WINDOW_WIDTH` takes the wider 680 because it is fixed at
launch and the split can flip. A dock that is shown adds its 400 px back:
`340 + 400 + 400 = 1140` stacked, `680 + 400 + 400 = 1480` side by side. A
resize collapses the tools dock first, then the sidebar (`sync_dock_fit`), and
an auto collapse is not written to `layout.json` — the file keeps the user's
intent, and the fit is re-derived on the next launch. The kit's 100 px split
floor is unchanged: the handle can still narrow a pane to 100 px by hand, and
the panes stay responsive below their breakpoints.

## When a request is kept

A request is kept only once it is **sent**. Typing or pasting fills a tab in
memory; pressing **Send** on a tab with a URL mints a draft row before `prepare`,
so the click — not a successful response — is what persists it. The **Drafts**
pane lists those sent-but-unsaved requests, and every write (debounce, switch,
quit) is update-only: it never inserts a row for a tab that has no owner.

States: **P** pristine (blank page) · **U** unfiled (typed or pasted, never sent)
· **D** draft · **S** saved to a collection node.

| Interaction | P | U | D | S |
|---|---|---|---|---|
| Type / edit | → U, not persisted | stays U, not persisted | updates the row, same id | in memory only; the node is untouched until Save |
| Paste cURL | fill → U | new tab U | new tab U | new tab U |
| History pick | fill; state ← row owner | new tab; ← owner | new tab; ← owner | new tab; ← owner |
| Send | nothing (empty URL) | mint → D, then history `Draft{id}` | history `Draft{id}` | history `Saved{node}` |
| Save | → S, new node | → S, new node | → S: reassign history to the new node, delete the row | same node id |
| Save As | → S, new node | → S, new node | → S, new node, reassign | → S, new node; the old node keeps its history |
| Close tab | gone | gone | row deleted; its history keeps `Draft{id}` and surfaces under *All* only | node stays |
| Switch | — | in-memory snapshot | snapshot + row | snapshot; node unchanged |
| Quit | gone | gone | row flushed | prompt if edited |
| Restart | not restored | not restored | restored from the row | restored from the node |

An edit never changes the id; **U → D** mints one at Send; **D → S** replaces the
draft id with the node id (`reassign_history_origin`), so a draft's sends follow
it into the collection. Deleting an open draft or node drops its tab to **U**, so
it cannot save back to a dead id.

## Quitting the window

Closing the window is the only user-reachable quit; the app binds no Quit
command, so a close is also a quit. The close is cancellable: `save_on_close`
registers `on_window_should_close`, and `Workspace::close_window` returns
`false` to keep the window open while a question is unanswered.

| State | What close does | Source |
|---|---|---|
| No dirty collection-bound tab | flush layout and drafts, close | `Workspace::flush` |
| `save_on_quit = Always save` | write every dirty bound tab into its node, then flush, close | `Workspace::save_dirty_bound_tabs` |
| `save_on_quit = Don't save` | flush, close; the bound edits are dropped | `Workspace::flush` |
| `save_on_quit = Ask` (default) | show the prompt, return `false`; its buttons close the window | `Workspace::open_quit_dialog` |

The prompt lists the dirty bound tabs and offers *Save and quit*, *Discard and
quit*, and *Cancel*. A *Remember my choice* tick writes *Always save* or *Don't
save* to Settings (`Settings › General › Quitting`), where it can be set back to
*Ask every time*. An unfiled request is never in the prompt: a request that was
never sent has no row and is not restored, and a draft was written when Send
minted it, so closing loses nothing the user kept.

Quitting cancels the request in flight on whichever tab owns it: `flush` cancels
the document `Workspace::sending` names, so a send on a background tab closes
deliberately instead of dying with the process.

**Cancel means cancel here.** Every other alert in the app performs the action
when its Cancel/confirm pair is dismissed the other way; this dialog's Cancel
aborts the whole close, because the close was cancelled to show it. A quit that
closes no window (the update installer's `App::quit`) cannot prompt — gpui-pre
awaits those quit observers and they cannot be cancelled — so it flushes drafts
and closes.

## Global chrome (always visible)

| Element | Shows | Source |
|---|---|---|
| Titlebar chip | workspace name, initial in a rounded square | `chrome::titlebar` |
| Titlebar environment chip | active environment; opens the environment menu | `chrome::environment_chip` |
| Titlebar trailing | window-menu dropdown (palette, settings, clipboard, View panes, reset layout, log copy), window controls | `Workspace::render` |
| Status bar, far left | dock toggle: `PanelLeft` → the sidebar dock | `chrome::status_bar` (`left_toggle`) |
| Status bar left | storage chip (`local database` or the workspace path) | `chrome::status_bar` |
| Status bar left | history chip (`Encrypted` / `Not encrypted` / `Not saved`), icon + word | `chrome::history_mode_chip` |
| Status bar right | last sent URL, or the engine error message | `chrome::status_bar`, `Workspace::status` |
| Status bar, far right | dock toggle: `PanelRight` → the History + Environment dock | `chrome::status_bar` (`right_toggle`) |

The titlebar's trailing History button was **removed**: the right dock now has
its own toggle in the bottom-right corner, under the panel it opens. The window
menu's **View** section lists the panes that have no corner toggle — Request and
Response — with a checkmark on the pane its dock is currently showing; clicking
an entry reveals that pane and then focuses it (the same pair the palette's
`FocusPanel` commands run). Collections, History, and Environment are left out
because each already has a corner control, and a second route one menu deep
would be the same command twice. `Reset layout` sits at the end of that section
and is the way back after a pane is closed, whichever route closed it.

Note: the status code, duration, and size are **not** here — the response pane
owns them (U43). The status bar answers "where did my data go" and "did anything
fail", nothing else.

Below `layout_policy::STATUS_COMPACT_WIDTH` (900 px, zoom-scaled) the storage and
history chips drop their words and keep the icon, the word moving into the
tooltip; only the URL ellipsises. This follows the guide's "collapse a secondary
label before clipping" rule, and keeps the corner toggles from being shoved off
the bar.

## Commands (palette + keybindings)

Every command is a variant of `Command` (`panels/command_palette.rs`), routed by
`Workspace::run_command` (`workspace/mod.rs`). One fixed table holds every row,
not a registry, so this list is exhaustive: the palette lists every row except
the menu-only ones, the titlebar menu lists the rows that carry a menu slot, and
the shortcuts sheet lists the rows with a binding.

| Command | Keybinding | Effect | POV | Source |
|---|---|---|---|---|
| Send request | `secondary-enter` | prepare → transport → store → `Document` notify | Center (Request + Response) | `run_command`, `workspace/send.rs` |
| New tab | `secondary-t` | push a `Tab` | Center (tab strip) | `Workspace::new_tab` |
| Close tab | `secondary-w` | close the active tab, empty the editor if it was the last | Center (tab strip) | `Workspace::close_tab` |
| Next tab | `ctrl-tab` / `ctrl-pagedown` | select the next tab, wrapping at the end | Center (tab strip) | `Workspace::select_relative` |
| Previous tab | `ctrl-shift-tab` / `ctrl-pageup` | select the previous tab, wrapping at the start | Center (tab strip) | `Workspace::select_relative` |
| Reopen closed tab | `secondary-shift-t` | restore the most recently closed tab | Center (tab strip) | `Workspace::reopen_closed_tab` |
| Focus URL | `secondary-l` | focus the URL field | Center | `RequestPanel::focus_url` |
| Focus next pane | `f6` | move focus to the next pane in `PanelName::all` order, revealing a closed dock before it takes focus, and wrapping at the end | Window (any pane) | `Workspace::focus_relative_pane` |
| Focus previous pane | `shift-f6` | the same step backwards, wrapping at the start | Window (any pane) | `Workspace::focus_relative_pane` |
| Find in response | `secondary-f` | open the find row and focus it; Enter steps forward, Shift+Enter back, Escape closes | Center (Response) | `ResponsePanel::open_find` |
| Paste cURL | `secondary-shift-v` | clipboard → `RequestDef`, fill the tab if pristine else open a new one, then offer to save it | Center | `RequestPanel::paste_curl` → `Workspace::imported` |
| Import… | `secondary-shift-i` | file picker → a busy notification naming the file (read, guess, parse; closing it cancels before the parse) → destination dialog (new collections, the default, or an existing collection or folder) → save, busy again under the same notification → result dialog | Overlay (dialog) + Chrome (notification) + Left dock | `Workspace::import_file` |
| Export… | `secondary-shift-e` | picker → `import::export` → save prompt → write the file | Overlay (dialog) + Chrome (notification) | `Workspace::open_export_dialog` |
| Copy as cURL | `secondary-shift-c` | live `RequestDef` → `curl::to_curl` → clipboard | Window (any pane) | `Workspace::copy_curl` |
| Snapshot… | — (menu and palette) | draw the current request and response as a PNG, then copy it or save it | Overlay (dialog) | `Workspace::open_snapshot_dialog` |
| Focus history | — (palette only) | reveal **then** focus the History pane | Right dock | `run_command` |
| Focus collections | — (palette only) | reveal **then** focus the Collections pane | Left dock | `run_command` |
| Focus environment | — (palette only) | reveal **then** focus the Environment pane | Right dock | `run_command` |
| Focus request | — (menu and palette) | focus the Request pane (the centre is always mounted) | Center | `Workspace::focus_panel` |
| Focus response | — (menu and palette) | focus the Response pane (the centre is always mounted) | Center | `Workspace::focus_panel` |
| Close history | — (palette only) | remove the History pane from its dock | Right dock | `Workspace::close_panel` |
| Close collections | — (palette only) | remove the Collections pane from its dock | Left dock | `Workspace::close_panel` |
| Close environment | — (palette only) | remove the Environment pane from its dock | Right dock | `Workspace::close_panel` |
| Close request | — (palette only) | remove the Request pane from the centre; `Reset layout` restores it | Center | `Workspace::close_panel` |
| Close response | — (palette only) | remove the Response pane from the centre; `Reset layout` restores it | Center | `Workspace::close_panel` |
| Toggle environment | — (palette only) | reveal the Environment pane | Right dock | `reveal_panel` |
| Toggle collections | — (palette only) | reveal the Collections pane | Left dock | `reveal_panel` |
| Toggle side-by-side | — (palette only) | flip the centre between stacked and side by side; persisted in `layout.json` | Center | `Workspace::toggle_center_orientation` |
| Reset layout | — (menu and palette) | apply the first-run arrangement and persist it | Left dock + Center | `Workspace::reset_layout` |
| Settings… | `secondary-,` | open the Settings dialog | Overlay (dialog, 880px) | `Workspace::open_settings` |
| Keyboard shortcuts | `?` | open the shortcuts sheet, generated from the palette table and the live keymap | Overlay (dialog) | `Workspace::open_shortcuts` |
| Zoom in | `secondary-=` | scale the whole UI up, persist to `settings.json` | Chrome (all panes redraw) | `Workspace::set_zoom` |
| Zoom out | `secondary--` | scale the whole UI down | Chrome (all panes redraw) | `Workspace::set_zoom` |
| Reset zoom | `secondary-0` | return zoom to 100% | Chrome (all panes redraw) | `Workspace::set_zoom` |
| Command palette… | `secondary-shift-p` | open the command palette | Overlay (dialog) | `Workspace::open_palette` |
| Copy log path | — (menu only) | write the log file's path to the clipboard, or say there is none | Chrome (flash) | `Workspace::copy_log_path` |
| Copy recent log | — (menu only) | write the last 200 log lines to the clipboard, or say there is none | Chrome (flash) | `Workspace::copy_recent_log` |

`secondary` is ⌘ on macOS and Ctrl elsewhere. `FocusPanel` reveals before it
focuses because History, Environment, and Collections are docked, and a focus
request for a pane no dock shows would aim at nothing; the centre panes are
always mounted, so the reveal is a no-op for them. Closing a pane removes it
from the tree but keeps the entity, so `Reset layout` puts it back.

The titlebar menu reads the table's rows and groups them by menu slot — palette
opener, app commands, View panes, reset layout, log utilities — drawing a
separator where the slot changes, so its order and its separators are the table's
and not a second list. A `FocusPanel` row is drawn only for a pane with no
status-bar toggle, which is why Request and Response are the two the View
section shows.

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
| Paste-curl imported | `Event::Imported` | fill the tab if pristine else open a new one, report a notification, then open the Save As dialog when a collection is selected | Overlay (notification, optionally a dialog) | `Workspace::imported` |
| Paste-curl failed | `Event::Notice(_)` | report a notification with the parse error | Overlay (notification) | `Workspace::notice` |
| Empty-state Import… button | `Event::Import` | open the file picker, choose a destination, land the file, show the result | Overlay (dialog) + Left dock | `Workspace::import_file` |
| Empty-state Paste URL button | `Event::PasteUrl` | clipboard text (first line) → a new tab whose URL is that text | Center | `Workspace::paste_url` |
| Editor menu: Copy as cURL | `Event::CopyCurl` | live `RequestDef` → `curl::to_curl` → clipboard | Window (any pane) | `Workspace::copy_curl` |

### Sidebar → workspace

| Trigger | Event | Effect | POV | Source |
|---|---|---|---|---|
| Collections: request row click | `CollectionsEvent::Open { node, request }` | select the tab already holding `node`, else open the request in its own tab (taking over only the lone pristine placeholder), bind the tab to the node, delete any stale draft | Center | `Workspace::open_saved` |
| Collections: draft row click | `CollectionsEvent::OpenDraft { id }` | select the tab holding that draft, or open it | Center | `Workspace::open_draft` |
| Collections: any mutation | `CollectionsEvent::Notice(_)` | report a notification (create/delete/rename/save/move) | Overlay (notification) | `Workspace::notice` |
| Collections: delete a row | `CollectionsEvent::ConfirmDelete(target)` | open the delete alert dialog; delete on confirm | Overlay (alert dialog) | `Workspace::open_delete_dialog` |
| Collections: Move to… | `CollectionsEvent::Move { node }` | open the destination picker; its confirm rewrites the target collection, then the source | Overlay (dialog) | `Workspace::open_move_dialog` |
| Collections: Save request… | `CollectionsEvent::SaveRequest` | save the active request as a new entry through the save dialog | Overlay (dialog) | `Workspace::save_active_as` |
| Collections: Import… | `CollectionsEvent::Import` | open the file picker from the next render, choose a destination, land the file, show the result | Overlay (dialog) + Left dock | `Workspace::import_file` |
| Collections: Export… | `CollectionsEvent::Export` | open the export picker from the next render | Overlay (dialog) | `Workspace::open_export_dialog` |
| Environment: delete | `EnvironmentEvent::ConfirmDelete(name)` | open the delete alert dialog; delete on confirm | Overlay (alert dialog) | `Workspace::open_delete_dialog` |

### Response panel → workspace

| Trigger | Event | Effect | POV | Source |
|---|---|---|---|---|
| History clock control | `ResponseEvent::ShowHistory` | scope the History pane to this request, then reveal and focus it | Right dock | `Workspace::show_history_pane` |
| Cookies: edit a value | `ResponseEvent::UpdateCookie { name, value }` | write the value into the shared jar, then redraw the tab | Center (Response, Cookies tab) | `Workspace::update_cookie` |
| Cookies: delete a row | `ResponseEvent::DeleteCookie { name }` | remove that cookie from the shared jar, leaving the rest | Center (Response, Cookies tab) | `Workspace::delete_cookie` |
| Body / Headers / Cookies tab | (local) | switch the response view | Center (Response) | `response.rs` |
| Body toolbar: snapshot | `ResponseEvent::Snapshot` | open the snapshot dialog for this request and response | Overlay (dialog) | `Workspace::open_snapshot_dialog` |
| Body toolbar: find | (local) | open or close the find row, then focus it; the row reports the match count and next/previous | Center (Response) | `ResponsePanel::toggle_find` |
| Body toolbar: format switch, copy body, copy summary | (local) | pretty/raw, or write the body or the status line to the clipboard | Center (Response) | `ResponsePanel::render_body_toolbar` |
| Error view: capture debug | (local) | start a ten-minute `restbook-debug-<timestamp>.log` capture and name its path in a notification | Overlay (notification) | `logging::start_debug_capture` |
| Error view: Retry | `ResponseEvent::Retry` | send the same request again through `Workspace::send`, so the unresolved-variable and pending guards still apply | Center → wire | `ResponsePanel::render_failure` |
| Error view: Details | (local) | disclose the engine message and read the recent log for the first time; a later success closes it again | Center (Response) | `ResponsePanel::toggle_failure_details` |

The Cookies tab draws the session jar for the request's URL, because those are
the cookies a send would carry. A response restored from history has no jar
entries, so it lists its own `Set-Cookie` rows instead, read-only.

The copy controls sit in the body's toolbar rather than in the header strip: the
header also draws for the Headers and Cookies tabs, so a copy button there
offered a copy of payload the current tab does not show. The centre-split toggle
is the opposite case and does live in the header, at its right edge: it divides
Request from Response, not the body, so it means the same thing on every tab and
should not disappear when the user switches to Headers or Cookies.

### Collections panel (in-pane)

| Trigger | Effect | POV |
|---|---|---|
| Collection switcher → a collection | select it | Left dock |
| `+` menu → Save request here / New Folder / New Collection | save the open request into the selection, or create a folder or collection there | Left dock |
| `+` menu → Save request… / Import… / Export… | open the save dialog, the import picker, or the export picker | Left dock → Overlay |
| Sort → Manual / Name A-Z / Name Z-A | re-sort the tree | Left dock |
| Row tools: up / down / rename / move / delete | move within the siblings, rename (label only), pick a new home, or delete (opens the alert dialog) | Left dock → Overlay |
| `f2` while the pane has focus | rename the selected row in place, the keyboard twin of the row menu's Rename | Left dock |

### Environment panel (in-pane)

| Trigger | Effect | POV |
|---|---|---|
| Environment picker → `(none)` / a name | set active, follow that target | Left dock |
| New / Delete / Active | create, delete (opens the alert dialog), or activate | Left dock → Overlay |
| Per-row: enabled / key / value / secret / reveal / `x` | edit the variable rows | Left dock |
| Inspector | read-only: which scope won each `{{name}}`, or `unresolved`; a `{{$generator}}` is never unresolved (it renders at send time) | Left dock |

### History pane (in-pane)

| Trigger | Effect | POV |
|---|---|---|
| `All` / `This request` | scope the list to every send, or to the owner of the request on screen (URL fallback for a `Standalone` one); the request scope follows a tab switch | Right dock |
| Search box | filter the scoped list by URL text | Right dock |
| Filter button | one menu with method, status class, and a sent-within preset; checked items are the state, and *Clear filters* resets all three | Right dock |
| Row click | `HistoryEvent::Picked { id }`: load the full entry, open the request and show the stored response in its own tab (reusing the active one only when pristine), bound to the row's owner when it still exists | Right dock → Center |
| Row trash | `HistoryEvent::Delete { id }`: delete the send (alert dialog) | Right dock |

### Request editor tabs (in-pane)

Five tabs, from the `EditorTab` enum: **Params, Headers, Body, Auth, Settings**.

| Tab | Controls |
|---|---|
| Params | key/value table (enabled toggle, remove, Add), `{{…}}` completion; a Table/Raw switch turns the rows into wire-form text |
| Headers | key/value table |
| Body | kind strip: None / Raw / Form / Multipart / Binary; Raw has mime buttons + a code editor; Form has a Table/Raw switch; Multipart has a parts table with Choose… |
| Auth | kind strip: None / Basic / Bearer / API key; API key has a "query string" checkbox |
| Settings | timeout, follow-redirects, insecure TLS |

The table shows `RequestDef.params` only. A query string typed into the URL row
is sent as-is and does not appear here; the two are not reconciled (3.12).

The Send guard treats `{{$generator}}` differently from `{{name}}`: generators
(`$randomInt`, `$uuid`) resolve at send time in `restbook_core::template`, so
the unresolved list and the environment inspector skip every `$`-prefixed name.
Only a scope lookup can be unresolved.

## Overlays and dialogs

The workspace is the only thing that opens a window-level surface. Five of them
are opened with a deliberate two-step: an event or a background task has no
`Window`, so it parks the request (`pending_save`, `pending_import`,
`pending_history`, `pending_delete`, `pending_update`) and `render` consumes it
with `window.defer`.

| Surface | Triggered by | Position / size | Source |
|---|---|---|---|
| Command palette | `secondary-shift-p`, titlebar menu | centered dialog, 520px | `command_palette::open` |
| Settings | palette `Settings…`, titlebar menu | centered dialog, 880px, category rail | `settings_view::open` |
| Keyboard shortcuts | `?`, palette `Keyboard shortcuts`, titlebar menu | centered dialog, 448px; one row per bound command | `Workspace::open_shortcuts` |
| Save request | Save on an unbound tab, or Save As | centered dialog, 448px; a name and a searchable destination list of every collection and its folders, led by *New collection…*. The sidebar selection only pre-fills the picker, so a save needs no selection; with no collection at all the dialog opens ready to create one, and that confirm leaves both behind | `Workspace::open_save_dialog` |
| Import destination | an import whose file holds at least one collection | centered dialog, 448px; a searchable destination list whose first entry is *New collection(s)* — the default — followed by every collection and folder. Choosing an existing place appends into it, and a request whose name is already a direct child there is reported, not overwritten | `Workspace::open_import_dialog` |
| Import result | an import that landed something | centered dialog, 448px; counts, saved names, duplicates, every skipped item with its reason, and any notes (an imported row that needs attention, such as inherited auth) | `Workspace::open_import_report` |
| Export picker | `secondary-shift-e`, palette `Export…`, titlebar menu | centered dialog, 448px; scope, format, *Include secrets* | `Workspace::open_export_dialog` |
| Snapshot | palette `Snapshot…`, titlebar menu, Response body toolbar | centered dialog, 448px; *Include secrets* off by default, *Copy image* and *Save PNG…* | `Workspace::open_snapshot_dialog` |
| Delete confirmation | a row's trash button (Collections) or Delete (Environment) | centered **alert** dialog, 420px | `Workspace::open_delete_dialog` |
| Move request | a saved request row's *Move to…* | centered dialog, 448px; the same searchable destination list the save dialog uses, with *Move* disabled while the choice is the request's current home | `Workspace::open_move_dialog` |
| Update offer | the launch check finding a newer release that is not skipped | centered dialog, 448px; *Install now* / *Later* / *Skip this version* | `Workspace::open_update_offer_dialog` |
| Flash toast | any `Notice` / `flash`, including the two dock toggles | park in `pending_notice`, then a kit notification above dialogs | `Workspace::issue_notice` |

The update offer is a plain dialog, not an alert: an alert's confirm/cancel pair
has nowhere to put a third choice, and nothing here is destructive. *Install
now* hands the version and asset to `Updates::install_offered`, which is
`Updates::install` with the status seeded; *Skip this version* writes
`settings.skipped_update` so the same version is not offered again (`0.1.5`
still is). Settings › Updates keeps its own check, so nothing is lost if the
offer is dismissed.

Settings › Storage lists the workspace roots this install has opened, newest
first and capped at eight. **Open** starts a fresh process pinned with
`RESTBOOK_WORKSPACE`, because the store and every pane are built from the root
at startup; the root this session is already using is labeled **Current** rather
than offered as a relaunch.

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

Settings is a **dialog, not a pane** on purpose (`05-ui-guide.md`): it is
app-level, there is exactly one, and it has no per-request state to dock.

## How a send flows

```
Send button (or secondary-enter)
  RequestPanel emits Event::Send
    Workspace::listen → Workspace::send
      ├─ read the LIVE editor (current_request), not a possibly-stale copy
      ├─ mint a draft when the tab has no owner (Send is the checkpoint)
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

Send is the checkpoint: a tab with no owner mints a draft row before `prepare`,
so a send that fails to prepare still keeps the request the user asked to send.

Each stage above is a type in `crates/core`; the stage-to-directory map is in
[`03-backend-plan.md`](03-backend-plan.md#where-each-stage-lives).

## Keeping this honest

The command table above is the part most likely to drift. `Command` in
`panels/command_palette.rs` is guarded by a test that fails if a row is added
without appearing in this document. The **Keybinding** column is not kept by
hand: a row carries its keys, and `actions::bindings()` turns them into the
keymap, so this column restates the rows rather than duplicating them (the
`every_action_row_is_bound` test pins it). See the `mod tests` block in
`command_palette.rs`.
