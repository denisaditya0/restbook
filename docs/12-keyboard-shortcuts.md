# restbook — keyboard shortcuts

Every shortcut is a GPUI `KeyBinding` produced by `actions::bindings()`. For a
palette command the keystroke lives on the command's row in
`panels/command_palette.rs`; for a pane-only action (`F2` rename, `secondary-s`
save, the dock toggles) it is declared in `crates/app/src/actions.rs` beside the
action. Either way the table is installed once in `main.rs`
(`cx.bind_keys(actions::bindings())`), so the binding and the tooltip that shows
it always come from the same place. Tooltips read the live keymap through
`actions::shortcut`, which is why they print `⌘⏎` on macOS and `Ctrl+Enter` on
Windows/Linux.

`secondary` means **Cmd** on macOS and **Ctrl** on Windows and Linux.

## Sending and saving

| Shortcut | Action | Where it works |
| --- | --- | --- |
| `secondary-enter` | Send the request | Anywhere in the request pane, including the URL bar and the body editor |
| `secondary-s` | Save / Save As | Anywhere in the window |
| `secondary-l` | Focus the URL field | Anywhere in the window |
| `secondary-f` | Find in the response body | Anywhere in the window |

`secondary-enter` is handled by the request pane, so the pane must be open and
must have a tab. A request with an unresolved `{{variable}}` does not send; the
red band above the editor names what is missing and the Send tooltip repeats it.

`secondary-s` on a tab that came from a collection overwrites that node. On a
tab that belongs nowhere yet it opens the Save As dialog, so the request gets a
name and a destination.

`secondary-f` opens the response body's find row. With the field focused, Enter
steps to the next match, Shift+Enter to the previous one, and Escape closes the
row. The arrows beside the count do the same step.

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
| `f6` / `shift-f6` | Focus the next / previous pane, wrapping and revealing a closed pane's dock |
| `secondary-shift-p` | Command palette |
| `?` | Show the keyboard shortcuts sheet when focus is not in a text field |
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

`f6` walks the panes in the View menu's order — Collections, History,
Environment, Request, Response — and wraps at both ends. A pane whose dock is
closed is opened before it takes focus, so the cycle never lands on something
hidden.

`?` is a printable key, so its binding is scoped to `!Input`: it opens the sheet
from the canvas, but a question mark typed into a URL or body stays text. Use
the palette row or the window menu's **Keyboard shortcuts** entry from inside an
editor.

## Collections

| Shortcut | Action | Where it works |
| --- | --- | --- |
| `f2` | Rename the selected collection, folder, or request in place | While the Collections pane has focus and a row is selected |

`f2` is the keyboard twin of the row menu's **Rename**. It is scoped to the
Collections pane's key context, so a press while typing in the request body does
nothing. Clicking a row both selects it and gives the pane focus, which is what
makes `f2` reach the tree after a click.

## Completion

| Shortcut | Action |
| --- | --- |
| `ctrl-space` | Open the raw body's `{{…}}` completion popover |
| `up` / `down`, `ctrl-p` / `ctrl-n` | Move through a completion list |
| `enter` / `tab` | Accept the highlighted completion |

The URL bar and key/value rows draw their own completion lists; they open as soon
as `{{` is typed and do not need `ctrl-space`. `tab` accepts there too: the list
stops the key so the input does not also indent.

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
