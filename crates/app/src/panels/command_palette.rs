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
    shortcut, CloseTab, CopyCurl, ExportFile, FindInResponse, FocusUrl, ImportFile, NewTab,
    NextTab, OpenSettings, PasteCurl, PreviousTab, ReopenClosedTab, ResetZoom, SendRequest,
    ShowShortcuts, ZoomIn, ZoomOut,
};
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
}

/// One command, and what the palette shows for it.
///
/// The order of [`ROWS`] is the palette's order and therefore its confirm
/// index, so it stays a fixed list rather than a registry. The key hint does
/// not live here: it is read from the live keymap at render time by [`hint`],
/// so it formats itself per platform and cannot drift from the binding.
pub(crate) struct Row {
    pub(crate) command: Command,
    pub(crate) label: &'static str,
    /// The typed action behind the command, when it has a keystroke. `None` is
    /// a command only the palette can reach.
    pub(crate) action: Option<&'static dyn Action>,
}

impl Row {
    const fn bound(command: Command, label: &'static str, action: &'static dyn Action) -> Self {
        Self {
            command,
            label,
            action: Some(action),
        }
    }

    const fn plain(command: Command, label: &'static str) -> Self {
        Self {
            command,
            label,
            action: None,
        }
    }
}

/// Every command, one row each. The palette lists them all in this order; the
/// shortcuts sheet lists the bound ones.
fn rows() -> Vec<Row> {
    vec![
        Row::bound(Command::Send, "Send request", &SendRequest),
        Row::bound(Command::NewTab, "New tab", &NewTab),
        Row::bound(Command::CloseTab, "Close tab", &CloseTab),
        Row::bound(Command::NextTab, "Next tab", &NextTab),
        Row::bound(Command::PreviousTab, "Previous tab", &PreviousTab),
        Row::bound(
            Command::ReopenClosedTab,
            "Reopen closed tab",
            &ReopenClosedTab,
        ),
        Row::bound(Command::FocusUrl, "Focus URL", &FocusUrl),
        Row::bound(Command::FindInResponse, "Find in response", &FindInResponse),
        Row::bound(Command::PasteCurl, "Paste cURL", &PasteCurl),
        Row::bound(Command::ImportFile, "Import…", &ImportFile),
        Row::bound(Command::ExportFile, "Export…", &ExportFile),
        Row::bound(Command::CopyCurl, "Copy as cURL", &CopyCurl),
        Row::plain(Command::Snapshot, "Snapshot…"),
        // One focus and one close per pane, so the fixed table stays flat and the
        // confirm index remains a plain position in this list.
        Row::plain(Command::FocusPanel(PanelName::History), "Focus history"),
        Row::plain(
            Command::FocusPanel(PanelName::Collections),
            "Focus collections",
        ),
        Row::plain(
            Command::FocusPanel(PanelName::Environment),
            "Focus environment",
        ),
        Row::plain(Command::FocusPanel(PanelName::Request), "Focus request"),
        Row::plain(Command::FocusPanel(PanelName::Response), "Focus response"),
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
        Row::plain(Command::ResetLayout, "Reset layout"),
        Row::bound(Command::OpenSettings, "Settings…", &OpenSettings),
        Row::bound(Command::ShowShortcuts, "Keyboard shortcuts", &ShowShortcuts),
        Row::bound(Command::ZoomIn, "Zoom in", &ZoomIn),
        Row::bound(Command::ZoomOut, "Zoom out", &ZoomOut),
        Row::bound(Command::ResetZoom, "Reset zoom", &ResetZoom),
    ]
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
    let table = rows();
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
    fn every_command_has_exactly_one_palette_row() {
        let rows = super::rows();
        for row in rows.iter() {
            let count = rows
                .iter()
                .filter(|each| each.command == row.command)
                .count();
            assert_eq!(
                count, 1,
                "expected exactly one palette row for {:?}",
                row.command
            );
        }

        let mut labels: Vec<&str> = rows.iter().map(|row| row.label).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), rows.len(), "two commands share a label");
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
