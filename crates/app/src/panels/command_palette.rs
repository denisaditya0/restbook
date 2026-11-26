//! The command palette: a searchable list of app commands.
//!
//! The workspace owns the palette; a pane never opens it, since placement is a
//! canvas decision, and every command returns through one callback. The list is
//! a fixed table rather than a registry, which keeps the order — and so the
//! confirm index — deterministic.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::command::{Command as Palette, CommandItem, CommandState};
use gpui_kit::component::{h_flex, WindowExt as _};
use gpui_kit::*;

use crate::actions::{
    shortcut, CloseTab, CopyCurl, ExportFile, FindInResponse, FocusNextPane, FocusPreviousPane,
    FocusUrl, ImportFile, NewTab, NextTab, OpenSettings, PasteCurl, PreviousTab, ReopenClosedTab,
    ResetZoom, SendRequest, ShowShortcuts, ToggleCommandPalette, ZoomIn, ZoomOut,
};
use crate::icons::IconName;
use crate::workspace::PanelName;

/// A command the palette can run. The workspace maps each variant to an
/// effect; the palette only names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    Send,
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    ReopenClosedTab,
    FocusUrl,
    /// Move focus to the next pane in the View order, wrapping at the end.
    FocusNextPane,
    /// Move focus to the previous pane, wrapping at the start.
    FocusPreviousPane,
    FindInResponse,
    PasteCurl,
    ImportFile,
    ExportFile,
    CopyCurl,
    Snapshot,
    FocusPanel(PanelName),
    ClosePanel(PanelName),
    ToggleEnvironment,
    ToggleCollections,
    ToggleSideBySide,
    ResetLayout,
    OpenSettings,
    ShowShortcuts,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    /// Open the command palette (`secondary-shift-p`). Menu-only: the palette
    /// cannot list the command that opens it.
    ToggleCommandPalette,
    /// Write the log file's path to the clipboard. Menu-only; a diagnostic.
    CopyLogPath,
    /// Write the most recent log lines to the clipboard. Menu-only.
    CopyRecentLog,
}

/// Where a row sits in the titlebar menu. The variants are in menu order; a
/// slot change draws a separator, and rows keep the table's order inside one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MenuSlot {
    /// The palette opener, alone at the top.
    Palette,
    /// App-level commands a pointer user has no other route to.
    App,
    /// The View panes: the ones with no status-bar toggle.
    View,
    /// Reset layout, below the panes it restores.
    Layout,
    /// The diagnostics run at the bottom.
    Diagnostics,
}

/// One command, and what the palette shows for it.
///
/// The order of [`rows`] is the palette's order and therefore its confirm
/// index, so it stays a fixed list rather than a registry. The key hint does
/// not live here: it is read from the live keymap at render time by [`hint`],
/// so it formats itself per platform and cannot drift from the binding.
pub(crate) struct Row {
    pub(crate) command: Command,
    pub(crate) label: &'static str,
    /// The typed action behind the command, when it has a keystroke. `None` is
    /// a command only the palette can reach.
    pub(crate) action: Option<&'static dyn Action>,
    /// The titlebar menu slot, when the menu lists the row.
    pub(crate) menu: Option<MenuSlot>,
    /// The glyph the menu draws. The palette shows a shortcut instead.
    pub(crate) icon: Option<IconName>,
    /// Whether the palette lists the row. A menu-only command is off.
    pub(crate) palette: bool,
    /// The keystrokes [`Row::action`] is bound to. This is the only place a
    /// command's key is written down; `actions::bindings` reads it. Empty for a
    /// plain command.
    pub(crate) keys: &'static [&'static str],
    /// The key context the bindings run in. `None` is window-wide.
    pub(crate) context: Option<&'static str>,
}

impl Row {
    /// A row a key reaches. `keys` is required, so a bound command cannot end up
    /// unreachable by forgetting it.
    const fn bound(
        command: Command,
        label: &'static str,
        action: &'static dyn Action,
        keys: &'static [&'static str],
    ) -> Self {
        Self {
            command,
            label,
            action: Some(action),
            menu: None,
            icon: None,
            palette: true,
            keys,
            context: None,
        }
    }

    const fn plain(command: Command, label: &'static str) -> Self {
        Self {
            command,
            label,
            action: None,
            menu: None,
            icon: None,
            palette: true,
            keys: &[],
            context: None,
        }
    }

    /// Scope the row's bindings to a key context. A printable key or a key an
    /// input already owns needs this, or it fires everywhere.
    const fn in_context(mut self, context: &'static str) -> Self {
        self.context = Some(context);
        self
    }

    /// List the row in the titlebar menu under `slot`.
    const fn in_menu(mut self, slot: MenuSlot) -> Self {
        self.menu = Some(slot);
        self
    }

    /// Give the menu item a glyph. The View rows have none: the label already
    /// names the pane.
    const fn with_icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// List the row in the menu only: it already has a pointer route, or the
    /// palette must not offer it.
    const fn menu_only(mut self, slot: MenuSlot) -> Self {
        self.menu = Some(slot);
        self.palette = false;
        self
    }
}

/// Every command, one row each. The palette lists the rows that are not
/// menu-only; the shortcuts sheet lists the bound ones; the titlebar menu lists
/// the rows with a slot, in slot order; `actions::bindings` turns each row's
/// `keys` into a keybinding.
pub(crate) fn rows() -> Vec<Row> {
    vec![
        Row::bound(
            Command::Send,
            "Send request",
            &SendRequest,
            &["secondary-enter"],
        ),
        Row::bound(Command::NewTab, "New tab", &NewTab, &["secondary-t"]),
        Row::bound(Command::CloseTab, "Close tab", &CloseTab, &["secondary-w"]),
        // The tab rotation pair. `ctrl-tab` is the browser convention, and the
        // page-key aliases cover layouts and habits that reach for them.
        Row::bound(
            Command::NextTab,
            "Next tab",
            &NextTab,
            &["ctrl-tab", "ctrl-pagedown"],
        ),
        Row::bound(
            Command::PreviousTab,
            "Previous tab",
            &PreviousTab,
            &["ctrl-shift-tab", "ctrl-pageup"],
        ),
        Row::bound(
            Command::ReopenClosedTab,
            "Reopen closed tab",
            &ReopenClosedTab,
            &["secondary-shift-t"],
        ),
        Row::bound(Command::FocusUrl, "Focus URL", &FocusUrl, &["secondary-l"]),
        // The cycle follows `PanelName::all`, so focus-next visits the panes in
        // the same order the View menu lists them.
        Row::bound(
            Command::FocusNextPane,
            "Focus next pane",
            &FocusNextPane,
            &["f6"],
        ),
        Row::bound(
            Command::FocusPreviousPane,
            "Focus previous pane",
            &FocusPreviousPane,
            &["shift-f6"],
        ),
        Row::bound(
            Command::FindInResponse,
            "Find in response",
            &FindInResponse,
            &["secondary-f"],
        ),
        Row::bound(
            Command::PasteCurl,
            "Paste cURL",
            &PasteCurl,
            &["secondary-shift-v"],
        )
        .in_menu(MenuSlot::App)
        .with_icon(IconName::ClipboardPaste),
        Row::bound(
            Command::ImportFile,
            "Import…",
            &ImportFile,
            &["secondary-shift-i"],
        ),
        Row::bound(
            Command::ExportFile,
            "Export…",
            &ExportFile,
            &["secondary-shift-e"],
        ),
        Row::bound(
            Command::CopyCurl,
            "Copy as cURL",
            &CopyCurl,
            &["secondary-shift-c"],
        )
        .in_menu(MenuSlot::App)
        .with_icon(IconName::Copy),
        Row::plain(Command::Snapshot, "Snapshot…")
            .in_menu(MenuSlot::App)
            .with_icon(IconName::Image),
        // One focus and one close per pane, so the fixed table stays flat and the
        // confirm index remains a plain position in this list.
        Row::plain(Command::FocusPanel(PanelName::History), "Focus history")
            .in_menu(MenuSlot::View),
        Row::plain(
            Command::FocusPanel(PanelName::Collections),
            "Focus collections",
        )
        .in_menu(MenuSlot::View),
        Row::plain(
            Command::FocusPanel(PanelName::Environment),
            "Focus environment",
        )
        .in_menu(MenuSlot::View),
        Row::plain(Command::FocusPanel(PanelName::Request), "Focus request")
            .in_menu(MenuSlot::View),
        Row::plain(Command::FocusPanel(PanelName::Response), "Focus response")
            .in_menu(MenuSlot::View),
        Row::plain(Command::ClosePanel(PanelName::History), "Close history"),
        Row::plain(
            Command::ClosePanel(PanelName::Collections),
            "Close collections",
        ),
        Row::plain(
            Command::ClosePanel(PanelName::Environment),
            "Close environment",
        ),
        Row::plain(Command::ClosePanel(PanelName::Request), "Close request"),
        Row::plain(Command::ClosePanel(PanelName::Response), "Close response"),
        Row::plain(Command::ToggleEnvironment, "Toggle environment"),
        Row::plain(Command::ToggleCollections, "Toggle collections"),
        Row::plain(Command::ToggleSideBySide, "Toggle side-by-side"),
        Row::plain(Command::ResetLayout, "Reset layout")
            .in_menu(MenuSlot::Layout)
            .with_icon(IconName::RefreshCcw),
        Row::bound(
            Command::OpenSettings,
            "Settings…",
            &OpenSettings,
            &["secondary-,"],
        )
        .in_menu(MenuSlot::App)
        .with_icon(IconName::Settings),
        // `?` is printable, so it only opens the sheet when focus is not inside
        // an input; otherwise typing a question mark in a body would be stolen.
        Row::bound(
            Command::ShowShortcuts,
            "Keyboard shortcuts",
            &ShowShortcuts,
            &["?"],
        )
        .in_context("!Input")
        .in_menu(MenuSlot::App)
        .with_icon(IconName::Keyboard),
        Row::bound(
            Command::ZoomIn,
            "Zoom in",
            &ZoomIn,
            &["secondary-=", "secondary-+"],
        ),
        Row::bound(Command::ZoomOut, "Zoom out", &ZoomOut, &["secondary--"]),
        Row::bound(
            Command::ResetZoom,
            "Reset zoom",
            &ResetZoom,
            &["secondary-0"],
        ),
        Row::bound(
            Command::ToggleCommandPalette,
            "Command palette…",
            &ToggleCommandPalette,
            &["secondary-shift-p"],
        )
        .menu_only(MenuSlot::Palette)
        .with_icon(IconName::Search),
        Row::plain(Command::CopyLogPath, "Copy log path")
            .menu_only(MenuSlot::Diagnostics)
            .with_icon(IconName::ClipboardCopy),
        Row::plain(Command::CopyRecentLog, "Copy recent log")
            .menu_only(MenuSlot::Diagnostics)
            .with_icon(IconName::ClipboardCopy),
    ]
}

/// The rows the titlebar menu lists, in menu order: slot order first, then the
/// table's order inside a slot. The caller draws a separator where the slot
/// changes.
pub(crate) fn menu_rows() -> Vec<Row> {
    let mut rows: Vec<Row> = rows()
        .into_iter()
        .filter(|row| row.menu.is_some())
        .collect();
    // Stable, so rows inside one slot keep the table's order.
    rows.sort_by_key(|row| row.menu);
    rows
}

/// The shortcut a row shows beside its label, or `None` for a command that has
/// no keybinding of its own.
fn hint(row: &Row, window: &Window) -> Option<String> {
    row.action.and_then(|action| shortcut(action, window))
}

/// Extra search terms for a row, beyond the label the kit already searches.
/// The formatted shortcut belongs here: it is what the user sees beside the
/// command, so typing `ctrl+shift+v` should find Paste cURL just as its name
/// does. The kit lowercases both sides before comparing.
fn search_terms(hint: Option<String>) -> Vec<SharedString> {
    hint.into_iter().map(SharedString::from).collect()
}

/// The rows the shortcuts sheet shows: a label and the live shortcut for every
/// command that has a binding. Palette-only commands are left out — the sheet is
/// about keys, and an empty hint would only take a line.
pub(crate) fn shortcut_rows(window: &Window) -> Vec<(String, String)> {
    rows()
        .iter()
        .filter_map(|row| hint(row, window).map(|hint| (row.label.to_owned(), hint)))
        .collect()
}

/// Open the palette, and report the chosen command through `on_run`.
pub(crate) fn open<F>(window: &mut Window, cx: &mut App, on_run: F)
where
    F: Fn(Command, &mut Window, &mut App) + 'static,
{
    let state = cx.new(|cx| CommandState::new(window, cx));
    // A menu-only command is the menu's business; the palette lists the rest, in
    // table order, because the confirm index is a position in this list.
    let table: Vec<Row> = rows().into_iter().filter(|row| row.palette).collect();
    let on_run = Rc::new(on_run);
    // `open_dialog` focuses the dialog shell, so the first build claims focus
    // for the query field. Later builds must not steal it back.
    let initial_focus = Cell::new(true);

    window.open_dialog(cx, move |dialog, window, cx| {
        let on_run = on_run.clone();
        // A shortcut hint is quieter than the command it sits beside, so it
        // takes the palette's subtle role rather than the muted value color.
        let hint_color = crate::theme::Palette::new(cx).subtle;
        let items = table
            .iter()
            .map(|row| {
                // Resolve the hint once per build: the row shows it, and it
                // also rides along as a search term so a typed shortcut finds
                // its command.
                let hint = hint(row, window);
                let label = row.label;
                let item = CommandItem::new()
                    .label(label)
                    .keywords(search_terms(hint.clone()));
                item.child(move |_window, _cx| {
                    let hint = hint.clone().unwrap_or_default();
                    h_flex()
                        .w_full()
                        .justify_between()
                        .child(div().child(label))
                        .child(div().text_xs().text_color(hint_color).child(hint))
                        .into_any_element()
                })
            })
            .collect::<Vec<_>>();
        let by_index = table
            .iter()
            .map(|row| row.command)
            .collect::<Vec<Command>>();

        if initial_focus.replace(false) {
            state.update(cx, |state, cx| state.focus(window, cx));
        }

        dialog.title("Command palette").w_128().child(
            Palette::new(&state)
                .placeholder("Type a command…")
                .items(items)
                .on_confirm(move |index_path, window, cx| {
                    let Some(command) = by_index.get(index_path.row).copied() else {
                        return;
                    };
                    (*on_run)(command, window, cx);
                    window.close_dialog(cx);
                })
                .on_cancel(|window, cx| window.close_dialog(cx)),
        )
    });
}

#[cfg(test)]
mod tests {
    //! The command table is documented in `docs/07-flows.md`, which is the map a
    //! reader consults for "where does clicking this land?". A new command that
    //! skips the doc would quietly make it wrong, so the doc is asserted to name
    //! every label, the same way `actions::bindings()` is guarded by
    //! `every_binding_parses`.
    use super::{search_terms, Command};

    /// The flows doc, resolved from the crate root so the test does not care
    /// what directory `cargo test` runs in.
    fn flows_doc() -> String {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/07-flows.md");
        std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("cannot read {path}: {error}"))
            .to_lowercase()
    }

    /// Every command's label appears in the flows doc. If this fails, add the
    /// command to the table in `docs/07-flows.md` before adding it here.
    #[test]
    fn every_command_is_in_the_flows_doc() {
        let doc = flows_doc();
        for row in super::rows() {
            let label = row.label.to_lowercase();
            assert!(
                doc.contains(&label),
                "docs/07-flows.md is missing the command {label:?}",
            );
        }
    }

    /// One row per command and one label per row. The table is the command list
    /// now, so a duplicated variant hides a command from search and a
    /// copy-pasted label hides a second one; both fail here.
    #[test]
    fn every_command_has_exactly_one_row() {
        let rows = super::rows();
        for row in rows.iter() {
            let count = rows
                .iter()
                .filter(|each| each.command == row.command)
                .count();
            assert_eq!(count, 1, "expected exactly one row for {:?}", row.command);
        }

        let mut labels: Vec<&str> = rows.iter().map(|row| row.label).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), rows.len(), "two commands share a label");
    }

    /// The three menu-only commands stay out of the palette: the palette cannot
    /// list the command that opens it, and the log utilities are diagnostics a
    /// pointer user reaches from the window menu.
    #[test]
    fn the_palette_leaves_out_the_menu_only_rows() {
        let palette: Vec<Command> = super::rows()
            .into_iter()
            .filter(|row| row.palette)
            .map(|row| row.command)
            .collect();
        for command in [
            Command::ToggleCommandPalette,
            Command::CopyLogPath,
            Command::CopyRecentLog,
        ] {
            assert!(
                !palette.contains(&command),
                "{command:?} must not be a palette row"
            );
            assert!(
                super::menu_rows().iter().any(|row| row.command == command),
                "{command:?} must be a menu row"
            );
        }
    }

    /// The menu groups by slot in declaration order and keeps the table's order
    /// inside one, so the caller can draw a separator wherever the slot changes.
    #[test]
    fn the_menu_is_ordered_by_slot() {
        let rows = super::menu_rows();
        assert!(
            rows.windows(2).all(|pair| pair[0].menu <= pair[1].menu),
            "the menu rows are not grouped by slot"
        );
    }

    /// The palette searches the hint as well as the label. This pins the
    /// contract `5.1` depends on: the formatted stroke is one of the search
    /// terms, and the kit lowercases both sides before comparing.
    #[test]
    fn a_typed_shortcut_matches_its_command() {
        let terms = search_terms(Some("Ctrl+Shift+V".to_owned()));
        let query = "ctrl+shift+v";
        assert!(
            terms.iter().any(|term| term.to_lowercase().contains(query)),
            "the formatted shortcut is a search term: {terms:?}",
        );
        // A command with no keybinding contributes no terms, so its row is not
        // matched by an empty keyword either.
        assert!(search_terms(None).is_empty());
    }

    /// The panel commands are generated from the same list the View menu
    /// uses, so every pane has both a focus and a close entry.
    #[test]
    fn every_panel_has_a_focus_and_a_close_command() {
        let rows = super::rows();
        let has = |wanted: Command| rows.iter().any(|row| row.command == wanted);
        for name in crate::workspace::PanelName::all() {
            assert!(
                has(Command::FocusPanel(name)),
                "no focus command for {name:?}"
            );
            assert!(
                has(Command::ClosePanel(name)),
                "no close command for {name:?}"
            );
        }
    }
}
