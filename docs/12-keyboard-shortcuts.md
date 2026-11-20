# restbook — keyboard shortcuts

Every shortcut is a GPUI `KeyBinding` from `crates/app/src/actions.rs`. The table
is installed once in `main.rs` (`cx.bind_keys(actions::bindings())`), so the
binding and the tooltip that shows it always come from the same place. Tooltips
read the live keymap through `actions::shortcut`, which is why they print
`⌘⏎` on macOS and `Ctrl+Enter` on Windows/Linux.

`secondary` means **Cmd** on macOS and **Ctrl** on Windows and Linux.

## Sending and saving

| Shortcut | Action | Where it works |
| --- | --- | --- |
| `secondary-enter` | Send the request | Anywhere in the request pane, including the URL bar and the body editor |
| `secondary-s` | Save / Save As | Anywhere in the window |
| `secondary-l` | Focus the URL field | Anywhere in the window |

`secondary-enter` is handled by the request pane, so the pane must be open and
must have a tab. A request with an unresolved `{{variable}}` does not send; the
red band above the editor names what is missing and the Send tooltip repeats it.

`secondary-s` on a tab that came from a collection overwrites that node. On a
tab that belongs nowhere yet it opens the Save As dialog, so the request gets a
name and a destination.

## Tabs

| Shortcut | Action |
| --- | --- |
| `secondary-t` | New request tab |
| `secondary-w` | Close the active tab, asking first when it has unsaved work |
| `ctrl-tab` / `ctrl-pagedown` | Next tab, wrapping at the end |
| `ctrl-shift-tab` / `ctrl-pageup` | Previous tab, wrapping at the start |
| `secondary-shift-t` | Reopen the most recently closed tab |

Next, Previous, and Reopen are also in the command palette when a keybinding is
not at hand.

## Clipboard: cURL

| Shortcut | Action |
| --- | --- |
| `secondary-shift-v` | Paste cURL: import the command on the clipboard into the active request, then offer to name and save it |
| `secondary-shift-c` | Copy as cURL: copy the request currently in the editor |

The same two commands are in the window menu (**Menu → Paste cURL / Copy as
cURL**), in the command palette (`secondary-shift-p`), and as buttons beside the
`+` in the request-tab strip, so a pointer user has the same route as a keyboard
user.

Paste cURL reads clipboard text only. A malformed command is reported in the
toast and leaves the current request untouched. When the sidebar has a save
destination selected, a successful import opens the Save As dialog; without a
destination it imports the request as an unsaved tab and says so.

Copy as cURL needs a URL. It emits a shell command with single-quoted arguments,
so a space, `&`, `$`, or an embedded quote survives a paste into a terminal.
`{{variables}}` are written as-is.

## Panes and window

| Shortcut | Action |
| --- | --- |
| `secondary-\` | Show or hide the sidebar dock |
| `secondary-/` | Show or hide the History + Environment dock |
| `secondary-shift-p` | Command palette |
| `secondary-,` | Settings |
| `secondary-=` / `secondary-+` | Zoom in |
| `secondary--` | Zoom out |
| `secondary-0` | Reset zoom to 100% |

The two dock keys are spelled unshifted on purpose: `/` and `\` are shifted
glyphs on a US layout, and the OS reports the produced character. Binding
`secondary-shift-/` would look for a stroke no layout emits.

The palette searches the formatted shortcut as well as the label, so typing
`ctrl+shift+v` finds **Paste cURL**. Every shortcut in this page is also a
palette row (focusing and closing a pane, and resetting the layout, are
palette-only). The window menu's **View** section lists every pane with a check
for the one its dock is showing.

## Completion

| Shortcut | Action |
| --- | --- |
| `ctrl-space` | Open the raw body's `{{…}}` completion popover |
| `up` / `down`, `ctrl-p` / `ctrl-n` | Move through a completion list |
| `enter` | Accept the highlighted completion |

The URL bar and key/value rows draw their own completion lists; they open as soon
as `{{` is typed and do not need `ctrl-space`.

## When a shortcut does not fire

- **Focus context.** Sending and URL focus are request-pane actions. If the
  response pane or the sidebar owns focus, click into the request pane first, or
  use the Send button.
- **A closed dock.** The request pane lives in the center split, not in a dock,
  so closing a sidebar does not disable its shortcuts. A tab must still be open;
  after closing the last tab the empty state's **New request** button is the way
  back.
- **Platform spelling.** On macOS use Cmd where this page says `secondary`; on
  Windows/Linux use Ctrl. If a key does nothing, `RUST_LOG=restbook_app=info`
  makes each command log a line (`event: …` / `command: …`) in the app log.
- **The bindings parse.** `cargo test -p restbook-app` runs
  `actions::tests::every_binding_parses`; a mistyped keystroke or context panics
  there rather than at launch.
