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

use crate::actions::shortcut;
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
    PasteCurl,
    ImportFile,
    ExportFile,
    CopyCurl,
    FocusPanel(PanelName),
    ClosePanel(PanelName),
    ToggleEnvironment,
    ToggleCollections,
    ResetLayout,
    OpenSettings,
    ShowShortcuts,
    ZoomIn,
    ZoomOut,
    ResetZoom,
}

/// A label for every command, in display order. The key hint is not stored
/// here: it is read from the live keymap at render time by [`command_hint`], so
/// it formats itself per platform and cannot drift from the binding.
fn commands() -> Vec<(Command, &'static str)> {
    vec![
        (Command::Send, "Send request"),
        (Command::NewTab, "New tab"),
        (Command::CloseTab, "Close tab"),
        (Command::NextTab, "Next tab"),
        (Command::PreviousTab, "Previous tab"),
        (Command::ReopenClosedTab, "Reopen closed tab"),
        (Command::FocusUrl, "Focus URL"),
        (Command::PasteCurl, "Paste cURL"),
        (Command::ImportFile, "Import…"),
        (Command::ExportFile, "Export…"),
        (Command::CopyCurl, "Copy as cURL"),
        // One focus and one close per pane, so the fixed table stays flat and
        // the confirm index remains a plain position in this list.
        (Command::FocusPanel(PanelName::History), "Focus history"),
        (
            Command::FocusPanel(PanelName::Collections),
            "Focus collections",
        ),
        (
            Command::FocusPanel(PanelName::Environment),
            "Focus environment",
        ),
        (Command::FocusPanel(PanelName::Request), "Focus request"),
        (Command::FocusPanel(PanelName::Response), "Focus response"),
        (Command::ClosePanel(PanelName::History), "Close history"),
        (
            Command::ClosePanel(PanelName::Collections),
            "Close collections",
        ),
        (
            Command::ClosePanel(PanelName::Environment),
            "Close environment",
        ),
        (Command::ClosePanel(PanelName::Request), "Close request"),
        (Command::ClosePanel(PanelName::Response), "Close response"),
        (Command::ToggleEnvironment, "Toggle environment"),
        (Command::ToggleCollections, "Toggle collections"),
        (Command::ResetLayout, "Reset layout"),
        (Command::OpenSettings, "Settings…"),
        (Command::ShowShortcuts, "Keyboard shortcuts"),
        (Command::ZoomIn, "Zoom in"),
        (Command::ZoomOut, "Zoom out"),
        (Command::ResetZoom, "Reset zoom"),
    ]
}

/// The shortcut a command shows beside its label, or `None` for the commands
/// that only live in the palette and have no keybinding of their own.
fn command_hint(command: Command, window: &Window) -> Option<String> {
    use crate::actions::{
        CloseTab, CopyCurl, ExportFile, FocusUrl, ImportFile, NewTab, NextTab, OpenSettings,
        PasteCurl, PreviousTab, ReopenClosedTab, ResetZoom, SendRequest, ShowShortcuts, ZoomIn,
        ZoomOut,
    };

    match command {
        Command::Send => shortcut(&SendRequest, window),
        Command::NewTab => shortcut(&NewTab, window),
        Command::CloseTab => shortcut(&CloseTab, window),
        Command::NextTab => shortcut(&NextTab, window),
        Command::PreviousTab => shortcut(&PreviousTab, window),
        Command::ReopenClosedTab => shortcut(&ReopenClosedTab, window),
        Command::FocusUrl => shortcut(&FocusUrl, window),
        Command::PasteCurl => shortcut(&PasteCurl, window),
        Command::ImportFile => shortcut(&ImportFile, window),
        Command::ExportFile => shortcut(&ExportFile, window),
        Command::CopyCurl => shortcut(&CopyCurl, window),
        Command::OpenSettings => shortcut(&OpenSettings, window),
        Command::ShowShortcuts => shortcut(&ShowShortcuts, window),
        Command::ZoomIn => shortcut(&ZoomIn, window),
        Command::ZoomOut => shortcut(&ZoomOut, window),
        Command::ResetZoom => shortcut(&ResetZoom, window),
        Command::FocusPanel(_)
        | Command::ClosePanel(_)
        | Command::ToggleEnvironment
        | Command::ToggleCollections
        | Command::ResetLayout => None,
    }
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
    commands()
        .into_iter()
        .filter_map(|(command, label)| {
            command_hint(command, window).map(|hint| (label.to_owned(), hint))
        })
        .collect()
}

/// Open the palette, and report the chosen command through `on_run`.
pub(crate) fn open<F>(window: &mut Window, cx: &mut App, on_run: F)
where
    F: Fn(Command, &mut Window, &mut App) + 'static,
{
    let state = cx.new(|cx| CommandState::new(window, cx));
    let commands = commands();
    let on_run = Rc::new(on_run);
    // `open_dialog` focuses the dialog shell, so the first build claims focus
    // for the query field. Later builds must not steal it back.
    let initial_focus = Cell::new(true);

    window.open_dialog(cx, move |dialog, window, cx| {
        let on_run = on_run.clone();
        // A shortcut hint is quieter than the command it sits beside, so it
        // takes the palette's subtle role rather than the muted value color.
        let hint_color = crate::theme::Palette::new(cx).subtle;
        let items = commands
            .iter()
            .map(|&(command, label)| {
                // Resolve the hint once per build: the row shows it, and it
                // also rides along as a search term so a typed shortcut finds
                // its command.
                let hint = command_hint(command, window);
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
        let by_index = commands
            .iter()
            .map(|&(command, _)| command)
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
        for command in super::commands() {
            let label = command.1.to_lowercase();
            assert!(
                doc.contains(&label),
                "docs/07-flows.md is missing the command {label:?}",
            );
        }
    }

    /// Every variant has exactly one row and one label. This is the drift guard
    /// for the palette: a new command that forgets its row, or a copy-pasted
    /// label that hides two rows from search, fails here. The variant list is
    /// hand-written because `Command` has no `ALL`; `0.2.4` replaces this with
    /// the one command table.
    #[test]
    fn every_command_has_exactly_one_palette_row() {
        use Command::*;

        let mut all = vec![
            Send,
            NewTab,
            CloseTab,
            NextTab,
            PreviousTab,
            ReopenClosedTab,
            FocusUrl,
            PasteCurl,
            ImportFile,
            ExportFile,
            CopyCurl,
        ];
        for name in crate::workspace::PanelName::all() {
            all.push(FocusPanel(name));
            all.push(ClosePanel(name));
        }
        all.extend([
            ToggleEnvironment,
            ToggleCollections,
            ResetLayout,
            OpenSettings,
            ShowShortcuts,
            ZoomIn,
            ZoomOut,
            ResetZoom,
        ]);

        let rows = super::commands();
        assert_eq!(
            rows.len(),
            all.len(),
            "the palette and the variant list disagree"
        );
        for command in all {
            assert_eq!(
                rows.iter().filter(|(each, _)| *each == command).count(),
                1,
                "expected exactly one palette row for {command:?}",
            );
        }

        let mut labels: Vec<&str> = rows.iter().map(|(_, label)| *label).collect();
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
        let commands = super::commands();
        let has = |wanted: Command| commands.iter().any(|&(command, _)| command == wanted);
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
