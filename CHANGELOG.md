# Changelog

What changed for the person using the app. `docs/01-milestones.md` owns which
lane an item belongs to; this file only records what shipped.

The section headed by the current `Cargo.toml` version is what `release.yml`
publishes as the release body, so the same text is the manifest's `notes` and
the update offer. A version with no section fails CI rather than shipping a
release with no description.

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

- Settings › Updates downloads, verifies, and opens the update, published from a
  single stable channel.
- Settings › Storage picks a folder as a workspace instead of asking you to add
  a `restbook.json` marker by hand.
- The environment chip moved to the titlebar, and stops truncating to
  `No enviro…`.
- The response copy controls moved from the header strip into the body.
- The titlebar menu was trimmed to its commands; the View rows duplicated the
  edge toggles in the status bar.
