//! The app's command vocabulary.
//!
//! A keybinding is only a name plus a keystroke, so the names live apart from
//! the widgets that handle them. The keystroke for a command is not written
//! here, though: it rides on the command's row in
//! [`crate::panels::command_palette`], and [`bindings`] turns those rows into
//! the table `main` installs. A pane listens for the action it owns. An action's
//! identity is its type, so a new command is a name here plus a listener, never
//! a closure in a panel.

use gpui_kit::base::actions::{SelectDown, SelectUp};
use gpui_kit::base::input::ShowCompletions;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::AsKeystroke as _;
use gpui_kit::*;

use crate::panels::command_palette;

actions!(
    restbook,
    [
        /// Send the request in the focused request pane (`secondary-enter`).
        SendRequest,
        /// Save the current request into a collection (`secondary-s`).
        SaveRequest,
        /// Open another request tab (`secondary-t`).
        NewTab,
        /// Close the active request tab (`secondary-w`).
        CloseTab,
        /// Move to the next request tab, wrapping at the end
        /// (`ctrl-tab`).
        NextTab,
        /// Move to the previous request tab, wrapping at the start
        /// (`ctrl-shift-tab`).
        PreviousTab,
        /// Bring back the most recently closed tab (`secondary-shift-t`).
        ReopenClosedTab,
        /// Save the active request as a new collection entry, even when its tab
        /// came from one. The dialog opens with the current name prefilled.
        SaveRequestAs,
        /// Focus the URL field of the request pane (`secondary-l`).
        FocusUrl,
        /// Open find in the response body (`secondary-f`).
        FindInResponse,
        /// Show or hide the command palette (`secondary-shift-p`).
        ToggleCommandPalette,
        /// Show the keyboard shortcuts sheet (`?`).
        ShowShortcuts,
        /// Import a request from a cURL command on the clipboard
        /// (`secondary-shift-v`).
        PasteCurl,
        /// Import requests from a file (`secondary-shift-i`).
        ImportFile,
        /// Export a collection or the whole workspace (`secondary-shift-e`).
        ExportFile,
        /// Copy the active request as a cURL command (`secondary-shift-c`).
        CopyCurl,
        /// Open the Settings dialog (`secondary-,`).
        OpenSettings,
        /// Scale the whole UI up one step (`secondary-=`).
        ZoomIn,
        /// Scale the whole UI down one step (`secondary--`).
        ZoomOut,
        /// Return zoom to 100% (`secondary-0`).
        ResetZoom,
        /// Show or hide the sidebar dock (`secondary-\`).
        ToggleSidebar,
        /// Show or hide the History and Environment dock (`secondary-/`).
        ToggleTools,
    ]
);

/// Build one binding through the App, so `secondary-` resolves against the
/// host OS and the context predicate parses eagerly.
fn load(cx: &App, keystrokes: &str, action: Box<dyn Action>, context: Option<&str>) -> KeyBinding {
    let context = context.map(|source| {
        KeyBindingContextPredicate::parse(source)
            .unwrap_or_else(|error| panic!("invalid binding context {source}: {error}"))
            .into()
    });
    KeyBinding::load(keystrokes, action, context, false, None, cx)
        .unwrap_or_else(|error| panic!("invalid binding {keystrokes}: {error}"))
}

/// The same, for an action this module names.
fn bound(cx: &App, keystrokes: &str, action: impl Action, context: Option<&str>) -> KeyBinding {
    load(cx, keystrokes, Box::new(action), context)
}

/// The whole keybinding table: which keystroke runs which action, in which key
/// context; `main` installs it with one `cx.bind_keys` call.
///
/// The restbook commands come from their rows in [`command_palette::rows`], so a
/// command's key is written once — beside the command — and the palette, the
/// menu, and this table cannot disagree. Each binding is loaded with the App, so
/// `secondary` is cmd/ctrl for this host; a bad keystroke or context panics here,
/// and the tests below catch typos at `cargo test` time.
pub(crate) fn bindings(cx: &App) -> Vec<KeyBinding> {
    let mut bindings: Vec<KeyBinding> = command_palette::rows()
        .into_iter()
        .filter_map(|row| Some((row.action?, row.keys, row.context)))
        .flat_map(|(action, keys, context)| {
            keys.iter()
                .map(move |keys| load(cx, keys, action.boxed_clone(), context))
        })
        .collect();

    // Actions the table does not own: a panel-level trigger, a library action
    // whose handler ships with the widget, and the two dock toggles, which the
    // palette reaches as plain commands. None of these is a `Command` row, so
    // each is written here.
    bindings.extend([
        // Save is the request pane's own button; it has no palette row, so the
        // key is declared beside the action it runs.
        bound(cx, "secondary-s", SaveRequest, None),
        // Emacs-style next/previous inside the command palette. The palette
        // already binds Up/Down; these aliases let a keyboard-only user stay
        // on the home row. Scoped to the palette's own key context, so Ctrl+N
        // elsewhere keeps meaning nothing.
        bound(cx, "ctrl-n", SelectDown, Some("Command")),
        bound(cx, "ctrl-p", SelectUp, Some("Command")),
        // The raw body's completion popover. The kit ships the action but binds
        // no key for it, and the editor is the only element that carries the
        // `mode` key, so this stays out of every other input's way.
        bound(
            cx,
            "ctrl-space",
            ShowCompletions,
            Some("Input && mode == editor"),
        ),
        // The two dock toggles. Spelled *unshifted* on purpose: `\` and `/` are
        // shifted-glyph keys on the US layout, and GPUI folds the produced
        // character (`|`, `?`) back to the physical key and clears `shift`, so
        // `secondary-/` matches the Ctrl+Shift press while `secondary-shift-/`
        // matches no stroke any layout emits.
        bound(cx, "secondary-\\", ToggleSidebar, None),
        bound(cx, "secondary-/", ToggleTools, None),
    ]);
    bindings
}

/// The platform-formatted shortcut for `action`, read from the live keymap so a
/// hint can never drift from its binding: `⌘⏎` on macOS, `Ctrl+Enter` on
/// Windows/Linux. `None` when the action has no binding, as the palette's
/// navigation-only commands do.
pub(crate) fn shortcut(action: &dyn Action, window: &Window) -> Option<String> {
    let binding = window.highest_precedence_binding_for_action(action)?;
    let stroke = binding.keystrokes().first()?.as_keystroke();
    Some(Kbd::format(stroke))
}

#[cfg(test)]
mod tests {
    // Named imports rather than `use super::*`: the glob would shadow the test
    // attribute with GPUI's own.
    use super::{bindings, CopyCurl, PasteCurl};

    /// Build the table through an App so secondary- resolves for this host.
    fn app_bindings() -> Vec<super::KeyBinding> {
        let cx = gpui_kit::TestAppContext::single();
        cx.update(gpui_kit::init);
        cx.update(|cx| bindings(cx))
    }

    /// The semantic secondary modifier depends on the platform, which the
    /// modifier value now asks an App for.
    fn secondary(stroke: &gpui_kit::Keystroke) -> bool {
        let cx = gpui_kit::TestAppContext::single();
        cx.update(|cx| stroke.modifiers.secondary(cx))
    }

    /// Building the table is the assertion: each binding parses both the
    /// keystroke and the context predicate and panics on either.
    #[test]
    fn every_binding_parses() {
        let bindings = app_bindings();
        assert!(!bindings.is_empty());
        assert!(bindings.iter().all(|b| !b.keystrokes().is_empty()));
    }

    /// A command's key lives on its row, and this is what makes the table the
    /// single source: every row that names an action has to be reachable from a
    /// keystroke, so a row cannot be added with a listener but no key.
    #[test]
    fn every_action_row_is_bound() {
        let bindings = app_bindings();
        for row in crate::panels::command_palette::rows() {
            let Some(action) = row.action else { continue };
            assert!(
                bindings.iter().any(|b| b.action().name() == action.name()),
                "no keystroke is bound to {} ({})",
                row.label,
                action.name(),
            );
        }
    }

    /// Two rows may not claim the same keystroke in the same context; the later
    /// binding would silently shadow the earlier one. Comparing the raw strings
    /// keeps this independent of how a keystroke parses.
    #[test]
    fn no_two_rows_claim_the_same_keystroke() {
        use std::collections::HashSet;

        let mut seen = HashSet::new();
        for row in crate::panels::command_palette::rows() {
            for keys in row.keys {
                assert!(
                    seen.insert((keys, row.context)),
                    "{keys} is claimed twice in context {:?}",
                    row.context,
                );
            }
        }
    }

    /// The zoom and dock actions are the ones a reader is most likely to
    /// assume work. A binding with no listener is silent, so assert each is at
    /// least reachable from a keystroke; the listener side is covered by the
    /// workspace render tree.
    #[test]
    fn every_zoom_and_dock_action_has_a_keystroke() {
        use gpui_kit::Action as _;

        use super::{ResetZoom, ToggleSidebar, ToggleTools, ZoomIn, ZoomOut};

        let bindings = app_bindings();
        for name in [
            ZoomIn::name_for_type(),
            ZoomOut::name_for_type(),
            ResetZoom::name_for_type(),
            ToggleSidebar::name_for_type(),
            ToggleTools::name_for_type(),
        ] {
            assert!(
                bindings.iter().any(|b| b.action().name() == name),
                "no keystroke is bound to {name}",
            );
        }
    }

    /// The cURL pair is the app's clipboard contract, and the file pair is the
    /// same contract on disk. All four are workspace-wide because the clipboard
    /// and the pickers are window resources, and they can run while a URL or
    /// body input has focus.
    #[test]
    fn the_curl_commands_are_bound() {
        use gpui_kit::Action as _;

        use super::{ExportFile, ImportFile};

        let bindings = app_bindings();
        for name in [
            PasteCurl::name_for_type(),
            CopyCurl::name_for_type(),
            ImportFile::name_for_type(),
            ExportFile::name_for_type(),
        ] {
            assert!(
                bindings.iter().any(|b| b.action().name() == name),
                "no keystroke is bound to {name}",
            );
        }
    }

    /// Settings has to be reachable by keystroke, not only from the menu and
    /// the palette, and on the conventional `secondary-,` stroke.
    #[test]
    fn the_settings_shortcut_is_bound() {
        use gpui_kit::Action as _;
        use gpui_kit::AsKeystroke as _;

        use super::OpenSettings;

        let bindings = app_bindings();
        let binding = bindings
            .iter()
            .find(|b| b.action().name() == OpenSettings::name_for_type())
            .expect("the settings action is bound");
        let stroke = binding
            .keystrokes()
            .first()
            .expect("the settings action has a keystroke")
            .as_keystroke();

        assert_eq!(stroke.key, ",", "settings opens on cmd/ctrl + comma");
        assert!(secondary(stroke), "settings keeps its cmd/ctrl modifier");
    }

    /// The shortcuts sheet opens on `?`, but `?` is printable, so the binding
    /// must carry a context predicate that keeps it out of text inputs. The
    /// palette row and the window menu are the routes from inside an editor.
    #[test]
    fn the_shortcuts_sheet_is_bound_outside_inputs() {
        use gpui_kit::Action as _;
        use gpui_kit::AsKeystroke as _;

        use super::ShowShortcuts;

        let bindings = app_bindings();
        let binding = bindings
            .iter()
            .find(|b| b.action().name() == ShowShortcuts::name_for_type())
            .expect("the shortcuts action is bound");
        let stroke = binding
            .keystrokes()
            .first()
            .expect("the shortcuts action has a keystroke")
            .as_keystroke();

        assert_eq!(stroke.key, "?", "the sheet opens on the question mark");
        assert!(
            binding.predicate().is_some(),
            "a printable key needs a context predicate, or it would eat text input"
        );
    }

    /// Find in the response body opens on the conventional `secondary-f`, from
    /// any pane, so a keyboard user does not have to click into the response
    /// first.
    #[test]
    fn find_in_response_uses_the_conventional_find_key() {
        use gpui_kit::Action as _;
        use gpui_kit::AsKeystroke as _;

        use super::FindInResponse;

        let bindings = app_bindings();
        let binding = bindings
            .iter()
            .find(|b| b.action().name() == FindInResponse::name_for_type())
            .expect("the find action is bound");
        let stroke = binding
            .keystrokes()
            .first()
            .expect("the find action has a keystroke")
            .as_keystroke();

        assert_eq!(stroke.key, "f");
        assert!(secondary(stroke), "find keeps its cmd/ctrl modifier");
    }

    /// Tab rotation has to stay reachable from any pane, and both spellings —
    /// the browser `ctrl-tab` pair and the page-key aliases — have to survive
    /// the parser.
    #[test]
    fn the_tab_rotation_commands_are_bound() {
        use gpui_kit::Action as _;
        use gpui_kit::AsKeystroke as _;

        use super::{NextTab, PreviousTab};

        let bindings = app_bindings();
        let strokes = |action: &str| -> Vec<String> {
            bindings
                .iter()
                .filter(|b| b.action().name() == action)
                .filter_map(|b| b.keystrokes().first())
                .map(|stroke| stroke.as_keystroke().key.clone())
                .collect()
        };

        assert_eq!(strokes(NextTab::name_for_type()), ["tab", "pagedown"]);
        assert_eq!(strokes(PreviousTab::name_for_type()), ["tab", "pageup"]);
    }

    /// The dock shortcuts have to be the keys their documentation promises, and
    /// they have to be *unshifted* spellings: a shifted-glyph key like `/`
    /// arrives from the OS as its produced character, so `secondary-shift-/`
    /// would never match a real press.
    #[test]
    fn the_dock_toggles_use_the_bracket_keys() {
        use gpui_kit::Action as _;
        use gpui_kit::AsKeystroke as _;

        use super::{ToggleSidebar, ToggleTools};

        let bindings = app_bindings();
        let stroke = |name: &str| -> gpui_kit::Keystroke {
            let binding = bindings
                .iter()
                .find(|b| b.action().name() == name)
                .expect("both dock actions are bound");
            binding
                .keystrokes()
                .first()
                .expect("both dock actions have a keystroke")
                .as_keystroke()
                .clone()
        };

        let sidebar = stroke(ToggleSidebar::name_for_type());
        let tools = stroke(ToggleTools::name_for_type());
        assert_eq!(
            sidebar.key, "\\",
            "the sidebar toggle is cmd/ctrl + backslash"
        );
        assert_eq!(tools.key, "/", "the tools toggle is cmd/ctrl + slash");
        // `secondary` is cmd/ctrl on both platforms; assert it survived the
        // parser rather than being dropped.
        for stroke in [&sidebar, &tools] {
            assert!(
                secondary(stroke),
                "the toggle must keep its cmd/ctrl modifier"
            );
            // Not `shift`, for the reason in the test's own doc comment: a
            // shifted spelling is the exact bug this pins shut.
            assert!(
                !stroke.modifiers.shift,
                "the toggle must not require shift — the OS reports the shifted glyph"
            );
        }
    }
}
