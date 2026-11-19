//! `{{name}}` completion for the request editor, over two surfaces. The raw
//! body is an `EditorState`, the only input with a completion hook:
//! [`CompletionProvider`]/[`VariableCompletions`] feeds the built-in popover.
//! The URL bar and kv value fields are plain `InputState`s, so [`Autocomplete`]
//! tracks the reference and row. Both read the same [`suggestions`], with
//! secrets masked as in the inspector. An accept replaces the whole reference,
//! including a `}}` the editor's auto-close has already inserted after the
//! caret — otherwise the accepted row would be followed by an orphan pair.

use std::ops::Range as ByteRange;
use std::rc::Rc;

use anyhow::Result;
use gpui_kit::base::input::{CompletionProvider, EditorState, RopeExt};
use gpui_kit::*;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Range, TextEdit,
};
use ropey::Rope;

use restbook_core::{Generators, ResolvedVar};

use crate::variables::{secret_of, Variables};

/// Stands in for a secret's value: the same intent as the inspector's mask.
const SECRET_MASK: &str = "••••••";

/// Offers the variables an open `{{…` can resolve to.
pub(crate) struct VariableCompletions {
    /// Read at completion time rather than snapshotted, so editing an
    /// environment updates the next popup with nothing to re-install.
    variables: Entity<Variables>,
}

impl VariableCompletions {
    pub(crate) fn new(variables: Entity<Variables>) -> Rc<Self> {
        Rc::new(Self { variables })
    }
}

/// Accept the raw body's highlighted completion from a plain Enter key.
///
/// The kit routes `enter` through its overlay handler, and that is the path a
/// normal key press takes. This is the same accept for the frame in which the
/// binding would do something else — the body is multi-line, so its own `enter`
/// inserts a newline — which is why the editor consumes the keystroke before
/// dispatch and calls this. Returns whether a row was accepted; an accepted row
/// closes the menu, so a second call cannot accept twice.
///
/// Routing can have nowhere to go: the handler is installed by the kit's
/// render pass and looks the menu up in a per-window registry, so a key that
/// arrives before that pass ran (or after the registry dropped the editor) is
/// told `false` even though the menu is visibly open. The direct insert is the
/// same call the click path makes; index 0 is the row `show` selects, and with
/// no handler the user cannot have moved the highlight, so it is also the row
/// on screen.
pub(crate) fn accept_body_completion(
    state: &mut EditorState,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) -> bool {
    if !state.completion_menu_state().open {
        return false;
    }
    let enter = Box::new(gpui_kit::base::input::Enter {
        secondary: false,
        shift: false,
    });
    if state.route_overlay_action(enter, window, cx) {
        return true;
    }

    let cursor = state.cursor();
    let (item, start) = {
        let menu = state.completion_menu_state();
        let Some(item) = menu.items.first().cloned() else {
            return false;
        };
        (item, menu.trigger_start_offset.unwrap_or(cursor))
    };
    state.insert_completion(&item, start..cursor, window, cx);
    state.dismiss_completion_overlay(cx);
    true
}

impl CompletionProvider for VariableCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        // Outside `{{…` there is nothing to offer, and an empty answer closes
        // the popover — which is how a character that cannot continue a name
        // dismisses it.
        let Some(open) = open_reference(text, offset) else {
            return Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        };

        let range = Range::new(
            text.offset_to_position(open.start),
            text.offset_to_position(open.end),
        );
        let items: Vec<CompletionItem> = suggestions(self.variables.read(cx), &open.query)
            .iter()
            .map(|row| item(row, &open.query, range))
            .collect();

        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    /// Every edit is a candidate, deletions included: a latch the kit keeps
    /// would go stale on a backspace, and the scan in [`Self::completions`]
    /// reads the caret itself, so it is never wrong about the reference. An
    /// empty answer is what closes the popover.
    fn is_completion_trigger(&self, _offset: usize, _new_text: &str, _cx: &mut App) -> bool {
        true
    }
}

/// One row a popup can offer: the token to show, the text accepting it writes,
/// and the one-line explanation beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Suggestion {
    /// What the row is called, `{{`/`}}` excluded — it is what the user is
    /// typing, so it is also what the filter matches on.
    pub(crate) label: String,
    /// The whole reference, braces included: accepting replaces the partial
    /// text with this.
    pub(crate) insert: String,
    pub(crate) detail: String,
}

/// Everything `{{query` could resolve to: the variables a send would see, then
/// the send-time generators.
///
/// A query that begins with `$` can only name a generator, so the variables are
/// skipped rather than filtered down to nothing — the `$` prefix is reserved
/// for generators in [`restbook_core::template`].
pub(crate) fn suggestions(variables: &Variables, query: &str) -> Vec<Suggestion> {
    let mut rows = Vec::new();

    if !query.starts_with('$') {
        rows.extend(
            variables
                .scoped_names()
                .iter()
                .filter(|var| matches_query(&var.name, query))
                .map(|var| Suggestion {
                    label: var.name.clone(),
                    insert: format!("{{{{{}}}}}", var.name),
                    detail: detail(var, variables),
                }),
        );
    }

    rows.extend(
        Generators::names()
            .iter()
            .filter(|name| matches_query(name, query))
            .map(|name| Suggestion {
                label: (*name).to_owned(),
                insert: format!("{{{{{name}}}}}"),
                detail: format!("Generator · {}", Generators::describe(name)),
            }),
    );

    rows
}

/// The popup state for one plain `InputState`.
///
/// `EditorState` hosts the kit's completion popover; `InputState` has none, so
/// the owning pane keeps this and draws the list itself. What cannot be
/// borrowed from the kit is the part that remembers where the reference under
/// the caret starts and which row is highlighted.
#[derive(Default)]
pub(crate) struct Autocomplete {
    open: Option<Reference>,
}

struct Reference {
    /// Byte range from the first `{` through the caret: exactly the text an
    /// accept replaces.
    span: ByteRange<usize>,
    rows: Vec<Suggestion>,
    selected: usize,
}

impl Autocomplete {
    /// Recompute from the input's text and caret after an edit. The highlight
    /// resets to the first row, because the rows behind it may have changed.
    pub(crate) fn sync(&mut self, text: &Rope, cursor: usize, variables: &Variables) {
        self.open = open_reference(text, cursor).and_then(|open| {
            let rows = suggestions(variables, &open.query);
            // An empty list is the same as closed: the pane draws nothing and
            // the keys stay with the input.
            (!rows.is_empty()).then_some(Reference {
                span: open.start..open.end,
                rows,
                selected: 0,
            })
        });
    }

    pub(crate) fn close(&mut self) {
        self.open = None;
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The rows to draw and the index of the highlighted one.
    pub(crate) fn visible(&self) -> Option<(&[Suggestion], usize)> {
        self.open
            .as_ref()
            .map(|reference| (reference.rows.as_slice(), reference.selected))
    }

    /// Move the highlight by `delta`, wrapping at both ends. Returns whether
    /// there was anything to move.
    pub(crate) fn move_selection(&mut self, delta: isize) -> bool {
        let Some(reference) = self.open.as_mut() else {
            return false;
        };
        let len = reference.rows.len() as isize;
        reference.selected = (reference.selected as isize + delta).rem_euclid(len) as usize;
        true
    }

    /// Highlight `index`: a click names its row directly instead of moving the
    /// highlight first.
    pub(crate) fn select(&mut self, index: usize) {
        if let Some(reference) = self.open.as_mut() {
            reference.selected = index.min(reference.rows.len().saturating_sub(1));
        }
    }

    /// The span to replace and the text to put there. Closes the popup, so an
    /// accept can only happen once.
    pub(crate) fn accept(&mut self) -> Option<(ByteRange<usize>, String)> {
        let reference = self.open.take()?;
        let row = reference.rows.get(reference.selected)?;
        Some((reference.span, row.insert.clone()))
    }
}

/// Where the `{{` being typed begins, and the partial name after it.
struct OpenReference {
    /// Byte offset of the first `{`.
    start: usize,
    /// One past the last byte accepting a row replaces: the caret, extended
    /// over a `}}` the editor inserted ahead of it. Stopping at the caret
    /// would leave that closer behind, so a reference would end `}}}}`.
    end: usize,
    /// What has been typed between the braces, without them.
    query: String,
}

/// The reference the caret is inside, if any.
///
/// The scan stops at the first character that cannot appear in a name, so a
/// closed `{{name}}` and a plain line of JSON both answer `None` without
/// needing a second parser. A byte that is not a character boundary ends the
/// scan too: it cannot be part of a name.
fn open_reference(text: &Rope, offset: usize) -> Option<OpenReference> {
    let mut cursor = offset;
    let mut query = String::new();

    while cursor > 0 {
        let ch = text.char_at(cursor - 1)?;
        if ch == '{' {
            if cursor >= 2 && text.char_at(cursor - 2) == Some('{') {
                return Some(OpenReference {
                    start: cursor - 2,
                    end: closer_end(text, offset),
                    query: query.chars().rev().collect(),
                });
            }
            return None;
        }
        if !name_char(ch) {
            return None;
        }
        query.push(ch);
        cursor -= 1;
    }

    None
}

/// One past the last byte an accept replaces. The editor's auto-close inserts
/// `}}` the moment the second `{` is typed and leaves the caret between them,
/// so the replacement has to take that pair with it. A lone `}` is left alone:
/// swallowing it would eat text the user may still be typing into.
fn closer_end(text: &Rope, offset: usize) -> usize {
    if text.char_at(offset) == Some('}') && text.char_at(offset + 1) == Some('}') {
        offset + 2
    } else {
        offset
    }
}

/// The characters a `{{name}}` may be typed from. A leading `$` marks a
/// generator, and the rest match what [`restbook_core::ScopedVars::preview`]
/// accepts as a name. Characters outside this set end the scan, so a brace
/// pattern such as `$timestamp('%Y')` closes the popup at the `(` — the whole
/// example is offered as a single accepted row instead.
fn name_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '-' | '$')
}

/// Case-insensitive substring, which is the same rule the popover highlight
/// implies: the typed text must appear in the label it highlights.
fn matches_query(name: &str, query: &str) -> bool {
    let query = query.trim();
    query.is_empty() || name.to_lowercase().contains(&query.to_lowercase())
}

fn item(row: &Suggestion, partial: &str, range: Range) -> CompletionItem {
    let mut item = CompletionItem::new_simple(row.label.clone(), row.detail.clone());
    item.kind = Some(CompletionItemKind::VARIABLE);
    // Drives the highlight: exactly the characters the user has typed.
    item.filter_text = Some(partial.to_owned());
    // The braces are part of the text that gets replaced, so the insert has to
    // put them back. `text_edit` (rather than `insert_text`) is what makes the
    // kit replace the partial instead of appending to it.
    item.text_edit = Some(CompletionTextEdit::Edit(TextEdit::new(
        range,
        row.insert.clone(),
    )));
    item
}

/// `Environment · value`, or `Environment · ••••` for a secret.
fn detail(var: &ResolvedVar, variables: &Variables) -> String {
    let shown = if secret_of(variables, &var.name, Some(var.scope)) {
        SECRET_MASK.to_owned()
    } else {
        one_line(&var.value)
    };
    format!("{} · {shown}", var.scope.label())
}

/// A value on one line, cut at a word-safe length. A body-valued variable
/// would otherwise stretch the popover across the pane.
fn one_line(value: &str) -> String {
    const MAX_CHARS: usize = 40;

    let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "\"\"".to_owned();
    }
    match flat.char_indices().nth(MAX_CHARS) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

#[cfg(test)]
mod enter_tests {
    // Named imports only: `use super::*` would drag in GPUI's `test` macro and
    // shadow the built-in `#[test]`.
    use std::sync::Arc;

    use gpui_kit::component::input::{Editor, EditorState};
    use gpui_kit::{
        div, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
        Styled as _, TestAppContext, Window,
    };
    use restbook_store::MemoryStore;

    use crate::completion::{accept_body_completion, VariableCompletions};
    use crate::variables::Variables;

    /// Press Enter the way the platform delivers it: a `KeyDownEvent` with the
    /// key binding that carries no text. The simulated IME supplies `"\n"`,
    /// which inserts a newline before the editor can see the binding.
    fn press_enter(cx: &mut gpui_kit::VisualTestContext) {
        cx.update(|window, cx| {
            window.dispatch_event(
                gpui_kit::PlatformInput::KeyDown(gpui_kit::KeyDownEvent {
                    keystroke: gpui_kit::Keystroke {
                        modifiers: gpui_kit::Modifiers::default(),
                        key: "enter".to_owned(),
                        key_char: None,
                    },
                    is_held: false,
                    prefer_character_input: false,
                }),
                cx,
            );
        });
        // The menu inserts on a spawned task, the same one a click schedules,
        // so the accept only lands once the executor runs.
        cx.run_until_parked();
    }

    struct Body {
        state: Entity<EditorState>,
    }

    impl Render for Body {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(Editor::new(&self.state))
        }
    }

    /// Build the raw body as the Body tab has it: a code editor with the
    /// variable completion provider installed, focused and ready to type.
    fn body<'a>(
        cx: &'a mut TestAppContext,
        variables: &Entity<Variables>,
    ) -> (Entity<EditorState>, &'a mut gpui_kit::VisualTestContext) {
        let mut editor = None;
        let (_view, cx) = cx.add_window_view(|window, cx| {
            let state = cx.new(|cx| {
                let mut state = EditorState::new(window, cx);
                state.lsp_mut().completion_provider =
                    Some(VariableCompletions::new(variables.clone()));
                state
            });
            editor = Some(state.clone());
            Body { state }
        });
        let editor = editor.expect("the view built the editor");
        cx.update(|window, cx| editor.update(cx, |state, cx| state.focus(window, cx)));
        (editor, cx)
    }

    /// Enter has to accept the highlighted row, the same edit a click makes.
    /// With no environment configured the first row is the first generator, so
    /// the accepted text is exactly `{{$uuid}}`.
    #[gpui_kit::test]
    fn a_real_enter_key_accepts_a_completion_in_the_raw_body(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let variables =
            cx.update(|cx| cx.new(|cx| Variables::new(Arc::new(MemoryStore::new()), cx)));
        let (editor, cx) = body(cx, &variables);

        cx.simulate_input("{{");
        press_enter(cx);

        let text = cx.update(|_, cx| editor.read(cx).value().to_string());
        assert_eq!(text, "{{$uuid}}");
    }

    /// The routed accept writes the highlighted row. This is the path the
    /// editor's Enter interceptor takes when the kit's overlay handler is
    /// installed; the real key above covers the binding as well.
    #[gpui_kit::test]
    fn accepting_through_the_popover_writes_the_highlighted_row(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let variables =
            cx.update(|cx| cx.new(|cx| Variables::new(Arc::new(MemoryStore::new()), cx)));
        let (editor, cx) = body(cx, &variables);

        cx.simulate_input("{{");
        let accepted = cx.update(|window, cx| {
            editor.update(cx, |state, cx| accept_body_completion(state, window, cx))
        });
        // The write is spawned, exactly like the one a click schedules.
        cx.run_until_parked();

        assert!(accepted);
        let text = cx.update(|_, cx| editor.read(cx).value().to_string());
        assert_eq!(text, "{{$uuid}}");
    }

    /// The fallback does not depend on the kit's overlay handler.
    ///
    /// Routing is only possible once a render pass has installed the handler
    /// and the menu is in the registry. A key that arrives before that pass
    /// still has a visibly open menu, so the fallback writes the first row
    /// itself — the same `insert_completion` the click path calls.
    #[gpui_kit::test]
    fn the_fallback_accepts_while_the_overlay_handler_is_unavailable(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let variables =
            cx.update(|cx| cx.new(|cx| Variables::new(Arc::new(MemoryStore::new()), cx)));
        let (editor, cx) = body(cx, &variables);

        cx.simulate_input("{{");
        let accepted = cx.update(|window, cx| {
            editor.update(cx, |state, cx| {
                // Exactly what the registry lookup does when the editor has no
                // host: an installed handler that refuses the action.
                state.set_overlay_action_handler(|_, _, _, _| false);
                accept_body_completion(state, window, cx)
            })
        });
        cx.run_until_parked();

        assert!(accepted);
        let text = cx.update(|_, cx| editor.read(cx).value().to_string());
        assert_eq!(text, "{{$uuid}}");
    }
}

#[cfg(test)]
mod tests {
    use super::{matches_query, one_line, open_reference};
    use ropey::Rope;

    fn open(text: &str) -> Option<(usize, String)> {
        open_at(text, text.len())
    }

    fn open_at(text: &str, offset: usize) -> Option<(usize, String)> {
        let rope = Rope::from_str(text);
        open_reference(&rope, offset).map(|found| (found.start, found.query))
    }

    fn open_before_close(text: &str, offset: usize) -> Option<(usize, usize, String)> {
        let rope = Rope::from_str(text);
        open_reference(&rope, offset).map(|found| (found.start, found.end, found.query))
    }

    #[test]
    fn a_partial_name_after_two_braces_opens_a_reference() {
        assert_eq!(open("{{na"), Some((0, "na".to_owned())));
        assert_eq!(open("{{"), Some((0, String::new())));
    }

    #[test]
    fn the_reference_starts_where_the_braces_do() {
        // The offset is the position of the first `{` in the whole text, not
        // the position within the line, because the edit range is absolute.
        assert_eq!(open("{\n  \"id\": \"{{us"), Some((11, "us".to_owned())));
    }

    #[test]
    fn a_closed_reference_offers_nothing() {
        assert_eq!(open("{{name}}"), None);
        assert_eq!(open("{{name}} and {{"), Some((13, String::new())));
    }

    #[test]
    fn an_auto_closed_pair_is_part_of_the_edit() {
        // The editor inserts `}}` the moment `{{` is typed, with the caret
        // between them, so accepting must replace the pair too.
        assert_eq!(open_before_close("{{}}", 2), Some((0, 4, String::new())));
        assert_eq!(
            open_before_close("{\"t\": \"{{}}\"}", 9),
            Some((7, 11, String::new()))
        );
        assert_eq!(
            open_before_close("{{$ra}}", 5),
            Some((0, 7, "$ra".to_owned()))
        );
    }

    #[test]
    fn a_lone_closer_is_left_in_place() {
        assert_eq!(open_before_close("{{na}", 4), Some((0, 4, "na".to_owned())));
    }

    #[test]
    fn prose_and_single_braces_are_not_references() {
        assert_eq!(open("plain text"), None);
        assert_eq!(open("{"), None);
        assert_eq!(open("{\"a\": 1"), None);
    }

    #[test]
    fn a_generator_is_scanned_like_a_name() {
        // The `$` prefix marks a generator, so it has to keep the reference
        // open the same way a variable's characters do.
        assert_eq!(open("{{$ti"), Some((0, "$ti".to_owned())));
        assert_eq!(open("{{"), Some((0, String::new())));
    }

    #[test]
    fn a_value_is_flattened_and_cut() {
        assert_eq!(one_line(""), "\"\"");
        assert_eq!(
            one_line("https://api.example.com"),
            "https://api.example.com"
        );
        assert_eq!(one_line("a\nb\tc"), "a b c");

        let long = "x".repeat(80);
        let cut = one_line(&long);
        assert!(cut.ends_with('…'));
        assert_eq!(cut.chars().count(), 41);
    }

    #[test]
    fn a_query_is_matched_case_insensitively_and_partially() {
        assert!(matches_query("base_url", ""));
        assert!(matches_query("base_url", "BASE"));
        assert!(matches_query("base_url", "url"));
        assert!(!matches_query("base_url", "host"));
    }
}
