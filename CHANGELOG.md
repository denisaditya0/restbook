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
