# Changelog

What changed for the person using the app. `docs/01-milestones.md` owns which
lane an item belongs to; this file only records what shipped.

The section headed by the current `Cargo.toml` version is the manifest's `notes`
and the update offer — what the app shows when a newer version exists. The
release page itself is generated from the merged pull requests; this file stays
the record written in the user's words. A version with no section fails CI
rather than shipping an update with no description.

<!--
Template — every version reads the same way:

Heading: `## <version>`

- **<What changed, in the user's words.>** <What the user now sees or can do,
  in one to three plain sentences.>

Newest version first. The heading is the bare version (`0.1.7`, no `v`). One
bullet per user-visible change. Plain Markdown prose only: no code fences,
tables, images, or HTML comments, because the update dialog renders this text.
Contributors and pull requests are added to the release page by `release.yml`;
do not write them here.
-->

## 0.2.6

- **A collection can share variables, auth, and headers with its requests.**
  Requests saved in a collection resolve {{variables}} from it and inherit its
  auth and default headers, so setup written once applies to every request in
  that collection.
- **The Auth tab has an explicit Inherit state.** A request inside a collection
  shows Inherit instead of silently falling back, and a request created there
  starts on Inherit.
- **A request overrides what it inherits.** Its own headers are applied first
  and win over the collection's by name, and its own auth replaces the
  collection's.
- **Collection defaults are edited in the sidebar.** Select a collection to set
  its variables, auth, and default headers without leaving the app.
- **A collection secret stays masked.** A variable marked secret is hidden in
  the inspector and in exports, and an imported request that inherits its
  collection's auth keeps doing so.

## 0.2.5

- **A response stays with the tab that sent it.** Send a request, switch to
  another tab while it is on the wire, and the result now lands in the tab that
  asked for it instead of the one you are looking at.
- **A fetched OAuth token stays with the tab that asked.** Start a token fetch,
  switch tabs before it finishes, and the token now lands in the tab that
  requested it instead of the one on screen.
- **Each tab remembers where you were.** Switching back to a tab restores its
  response view, its body scroll, and the editor tab it was left on, instead of
  resetting to the body at the top.
- **The tab strip shows which tab is sending.** While a request is on the wire
  its tab shows a spinner, and every other tab's Send is disabled with a reason
  instead of doing nothing.
- **Quitting or closing the sending tab cancels its transfer.** Close the window,
  or the tab a request is on, while a send is running — even a tab you switched
  away from — and its transfer is cancelled instead of being left to die with
  the process.
- **Picking a request from history names it.** The cookie jar and the history
  scope follow the restored request rather than the one the tab sent before, and
  a cookie you started editing is dropped when you switch tabs.
- **A request is kept once you send it.** Typing or pasting into a tab no longer
  stores anything for a request you have not sent; pressing Send is what keeps
  it, and a send that fails before it leaves still keeps it. A pasted cURL
  command or a pick from History opens its own tab instead of overwriting the
  request you were working on.
- **A request that skips certificate checks says so.** Turning off TLS
  verification marks the URL row and the tab, so a request that can be
  intercepted is visible without opening its Settings tab.
- **Imports are bounded.** A file over 32 MB is refused before it is read, and a
  malformed file changes nothing in the store.

## 0.2.4

- **The window menu has a View section.** The titlebar's View menu lists every
  pane with a check beside the one that is open, and a pane can be focused or
  closed from it and from the command palette.
- **F2 renames a row in place.** Press F2 on a selected collection, folder, or
  request to edit its name directly: the whole name is selected, Enter saves it,
  and Escape cancels.
- **F6 moves focus between panes.** F6 and Shift+F6 cycle forward and backward
  through the panes, wrapping at the ends and opening a closed dock so focus
  never lands somewhere hidden.

## 0.2.3

- **A new window starts on Collections alone.** A first run opens with your
  collections in the sidebar and Request over Response in the centre; the tools
  dock opens when you ask for it. Reset Layout returns to this arrangement.
- **Empty panes tell you what to do.** The blank sidebar, the idle response, and
  a history search with no hits each name the next action with its shortcut
  instead of sitting empty.
- **Quitting asks before it drops edits.** A close can prompt, always save, or
  never save, and the answer is remembered. The decision covers a saved tab you
  edited as well as an unsaved draft.
- **The centre can sit side by side.** Flip Request and Response from stacked to
  side by side with the response header's toggle, and the choice survives a
  restart.
- **Narrow windows stay usable.** The window has a real minimum width now, and
  shrinking it drops the tools dock first, then the sidebar, collapsing the
  status chips to icons before any pane is squeezed.
- **Long menus and panes scroll instead of clipping.** Every popup that can
  outgrow the window scrolls, and a pane too small to show everything scrolls
  rather than cutting content off.
- **Open a request from a pasted URL.** Paste a URL to start a request from it.
- **The controls follow your theme.** Buttons, inputs, menus, dialogs,
  scrollbars, and the tab bar draw from the theme file's colours instead of the
  library defaults.
- **A two-finger scroll holds one direction.** A diagonal scroll no longer
  drifts sideways first when you meant to go down.

## 0.2.2

- **A big import no longer makes the app crawl.** Importing a large OpenAPI
  document used to leave the window sluggish long after the report appeared;
  the Collections tree now draws only the rows you can see, so a collection with
  thousands of requests scrolls like any other.
- **An import shows what it is doing.** Picking a file now shows the file name
  and the step it is on until the result appears, instead of an unchanged
  window. Closing that notice cancels the import until it reaches the store.
- **Reading a large file costs about half the memory.** Guessing a file's format
  no longer builds a throwaway copy of the whole document, which is what pushed
  a 29 MB import to a few hundred megabytes of heap before it even parsed.

## 0.2.1

- **Your tabs come back after a restart.** The app reopens the tabs you had open,
  with the same one active, and says so when a tab had to be dropped because its
  request no longer exists.
- **Import into a collection you already have.** Import offers a destination the
  way Save does: a new collection, or any collection or folder in your tree.
  Arriving requests are appended with fresh ids, and one whose name is already
  taken is reported instead of replacing the request that was there.
- **Large OpenAPI documents import completely.** Parameters declared by reference
  inside the document are resolved instead of silently dropped, and a failed
  import names the format it tried and why the file did not match.

## 0.2.0

- **Choose where a request is saved.** Saving opens a destination picker that
lists every collection and folder, plus *New collection…*, so a save no longer
needs something selected in the sidebar first. Creating a collection and saving
into it now happens in one step.
- **Move a saved request.** A saved request's row offers *Move to…*, which
re-homes it in another collection or folder with the same picker.
- **Collection commands moved to the Collections pane.** Save, Import, and Export
left the window menu for the Collections pane's header and row menus, leaving
the window menu with window-level entries only.
- **Snapshot a request and response as an image.** A palette command, the window
menu, and the Response toolbar draw the current send as a PNG — the request as a
cURL command and the response body as it reads — to copy or save. Secrets are
redacted unless you tick *Include secrets*. Long responses stay on one wide
column, and a *Compact* option clips them into a small image.
- **A quick tab switch keeps its own body.** Switching between tabs faster than
the editor redraws no longer copies one tab's raw request body into another.

## 0.1.10

- **Every shortcut in one place.** Press `?` outside a text field, or choose
*Keyboard shortcuts* from the palette or the window menu, to see every binding
built from the live keymap. A question mark typed into a URL or body still
inserts a question mark.
- **Find the response body.** `Ctrl+F` opens a find row over the body: every
match is marked, the current one is stronger, and Enter, Shift+Enter, or the
arrows step through them. Escape closes the row, and the match set refreshes
when a response finishes decoding.
- **Return to a recent workspace.** Settings › Storage lists the workspace
folders this install has opened, marks the current one, and reopens another in a
fresh window.
- **Long menus stay reachable.** The history filter, the window menu, and a
collection row menu now scroll instead of losing their last entries when the
window is at its minimum height.
- **A current window toolkit.** The app now builds on GPUI Kit 0.11, with no
intended change to how the window looks or behaves.

## 0.1.9

- **A folder workspace encrypts its history.** History in a workspace now lives
in the same encrypted database the per-user data directory uses, under
`.restbook/`, instead of a plain text file — so no request or response body sits
readable in the tree you share. A plain text history left there by an older
build is imported once and then removed, and a machine without the keyring keeps
that session's history in memory rather than writing it to disk.
- **The window says how your history is protected.** A chip in the status bar
reads *Encrypted*, *Not encrypted*, or *Not saved* beside the storage name, and
its tooltip explains why. A plain text fallback used to be mentioned only in a
log a window user never sees.
- **The files the app creates are private.** The data directory, history
database, log, settings, layout, and a workspace's local file are now readable
only by you, and their directories only by you, on macOS and Linux.
- **URLs stay out of the log.** A failed send's details, and the log lines that
name a request, leave out the query string and any user name or password, so a
token cannot end up in a log attached to a bug report.
- **A failed send shows its result at once.** The response pane leaves its
*Fetching* state as soon as the send fails instead of waiting for an unrelated
repaint.
- **A cleaner log after an upgrade.** Installing a new version starts a fresh
`restbook.log` for the version you are running and keeps the old one beside it,
so an attached log describes one build rather than a week of them.

## 0.1.8

- **Filter the history.** A filter button beside the History search box narrows
the list by method, status class, or how recently it was sent. The menu shows
which choices are active and offers *Clear filters*; the filter works in both
the *All* and *This request* scopes.
- **Failures say what failed.** A failed send now fills the response pane with
the cause instead of a generic sentence: a title such as *Certificate not
trusted*, *Request timed out*, or *Couldn't connect*, one sentence about what to
try, the request that was attempted, and a Retry button. The engine message and
the recent log sit behind **Details**, and an `http://` connect failure suggests
trying `https://`.
- **Find in the response works again.** Clicking the Find button opens and
focuses the find row; it has a close control and keeps the match count and
next/previous arrows.
- **Import Bruno request files.** A single-request `.yml` exported from Bruno
imports as one request, including its headers, query rows (disabled rows stay
disabled), body, timeout, and redirect choice. An inherited login is reported
so it can be set again rather than silently dropped.
- **A quieter log, with a capture when it is needed.** `restbook.log` now keeps
only startup facts, warnings and errors, and one line per send, and it rotates
at 1 MiB. Settings › Storage, or the button on a failed send, records ten
minutes of detailed logs in a separate timestamped file for a bug report; that
file is removed on the next launch.

## 0.1.7

- **Import a file.** **Import…** — in the titlebar menu, the command palette, and
  beside *New request* when no tab is open — reads a Postman v2.1 collection, a
  HAR, an OpenAPI 3 document, a cURL command, or a restbook bundle. A HAR with
  several hosts lands one collection per host, a file that only partly reads
  reports every item it skipped and why, and nothing is written until the whole
  file parses.
- **Export to a file.** **Export…** writes the selected collection as Postman
  v2.1, HAR, or a one-collection bundle, or the whole workspace as a bundle —
  the one format that carries environments. Secrets of every kind are replaced
  with a visible placeholder unless *Include secrets* is ticked, and that choice
  starts off every time.
- **A bundle imports back.** The app's own export, environments included, can
  now be read by **Import…**; before, it fell through to the cURL parser.
- **Form and multipart requests save again.** A request with either body kind
  failed to serialize, so it could not be saved to a collection, kept as a
  draft, or recorded in history. It now persists.

## 0.1.6

- **Query parameters and form bodies edit as text.** A **Table / Raw** switch
  on the Params tab and on a Form body shows the same rows as `key=value`
  lines, so a query string can be pasted in and edited as a whole. Repeated
  keys, percent-encoding, and a disabled row (written `# key=value`) survive
  switching back.
- **One cookie at a time.** The Cookies tab lists the cookies the session would
  send to the request's URL, with an inline value edit and a per-row delete,
  instead of replacing the whole session from Settings. A response restored
  from history keeps its own rows, read-only.
- **Earlier sends moved into the History pane.** The slide-in sheet is gone:
  the clock control reveals the right-dock History pane already scoped to the
  request, and the pane gains an **All / This request** switch beside its
  search box.
- **Redirects no longer carry credentials to another host.** A hop that changes
  host or scheme drops `Authorization`, `Cookie`, and `Proxy-Authorization`,
  and an `https → http` downgrade is refused unless the request allows
  insecure TLS.

## 0.1.5

- **Nothing looks different.** This release splits the window's one large
  module into five, so the next feature can land without touching tabs, send,
  layout and dialogs at once. Behaviour is unchanged.

## 0.1.4

- **An update offers itself on launch.** When a newer release exists, a dialog
  asks — *Install now*, *Later*, or *Skip this version*. A skipped version is
  remembered and not offered again. Installing from Settings is unchanged.
- **Find in the response body.** A query field beside the body toolbar, with
  the match count and next/previous, over both the pretty rows and the raw
  block.
- **The request panel is the redesign.** Default headers the engine supplies
  are listed read-only under **Headers → Default headers**; raw bodies choose a
  language preset instead of a `Content-Type` header; **Paste cURL** sits in the
  tab strip; and **Copy as cURL** exports the live request.
- **Messages no longer hide behind dialogs.** Save, create, and delete outcomes
  use the window's notification layer, so opening a dialog cannot bury one.
- **The response pane costs less per frame.** The cookie count is computed once
  per response instead of on every frame, and the body rows are no longer
  cloned to draw them.
- **The environment chip lives in the titlebar** and no longer truncates.
- **`Tab` accepts a completion** in the suggestion lists, alongside the arrows.

## 0.1.3

- **Settings › Updates installs from one stable channel.** It downloads,
  verifies, and opens the update.
- **Settings › Storage picks a folder as a workspace**, instead of asking you to
  add a `restbook.json` marker by hand.
- **The environment chip moved to the titlebar** and stops truncating to
  `No enviro…`.
- **The response copy controls moved into the body**, out of the header strip.
- **The titlebar menu is shorter.** The View rows duplicated the edge toggles in
  the status bar, so they are gone.
