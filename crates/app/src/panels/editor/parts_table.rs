//! The multipart body editor's rows: a name, a value or file path, a mime, and
//! an enabled flag per part.
//!
//! A sibling of `kv_table`, not a reuse of it: a part carries a mime and may be
//! a file upload, so its row is a different shape. Same split though — the rows
//! own their inputs and the widget reports clicks as a [`PartAction`].

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::component::button::*;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;

use restbook_core::FormPart;

use crate::icons::IconName;
use crate::theme::{Metrics, Palette};

/// One part. `value` and `path` both live here so a row can move between a text
/// field and a file upload without losing either; only the one that applies is
/// shown, and the fold reads a row with a path as a file part.
pub(crate) struct PartRow {
    /// Stable for the row's lifetime, so removing a neighbour cannot re-key
    /// what this row keeps (focus, enabled flag, chosen file).
    id: u64,
    name: Entity<InputState>,
    value: Entity<InputState>,
    path: Entity<InputState>,
    mime: Entity<InputState>,
    /// Lives here rather than in `FormPart` because removing a row would
    /// otherwise shift every flag onto its neighbour.
    enabled: bool,
}

impl PartRow {
    fn new(window: &mut Window, cx: &mut App) -> Self {
        Self {
            id: new_id(),
            name: new_input(window, cx, "name"),
            value: new_input(window, cx, "value"),
            path: new_input(window, cx, "file path"),
            mime: new_input(window, cx, "mime"),
            enabled: true,
        }
    }

    /// Write a stored part into the inputs. Used by history restore, where the
    /// values arrive with a window available.
    fn set(&mut self, part: &FormPart, window: &mut Window, cx: &mut App) {
        self.enabled = part.enabled;
        set_input(&self.name, part.name.clone(), window, cx);
        set_input(&self.value, part.value.clone(), window, cx);
        set_input(
            &self.path,
            part.file.clone().unwrap_or_default(),
            window,
            cx,
        );
        set_input(
            &self.mime,
            part.mime.clone().unwrap_or_default(),
            window,
            cx,
        );
    }

    fn to_part(&self, cx: &App) -> Option<FormPart> {
        part_from(
            self.name.read(cx).value().as_ref(),
            self.value.read(cx).value().as_ref(),
            self.path.read(cx).value().as_ref(),
            self.mime.read(cx).value().as_ref(),
            self.enabled,
        )
    }
}

fn new_input(window: &mut Window, cx: &mut App, placeholder: &str) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_owned()))
}

/// Allocation only grows, so an id is never handed to a second row even after
/// the first is removed.
fn new_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn set_input(input: &Entity<InputState>, value: String, window: &mut Window, cx: &mut App) {
    input.update(cx, |state, cx| state.set_value(value, window, cx));
}

/// A row's inputs folded into a `FormPart`. A blank name is dropped, not sent:
/// the engine would otherwise emit a part with an empty name. A non-empty path
/// makes it a file upload; the mime is trimmed and dropped when blank.
fn part_from(name: &str, value: &str, path: &str, mime: &str, enabled: bool) -> Option<FormPart> {
    if name.trim().is_empty() {
        return None;
    }
    let mime = mime.trim();
    Some(FormPart {
        name: name.to_owned(),
        value: value.to_owned(),
        file: (!path.is_empty()).then(|| path.to_owned()),
        mime: (!mime.is_empty()).then(|| mime.to_owned()),
        enabled,
    })
}

/// What a click in the widget means. Disabled rows are kept, not dropped, so
/// the user can re-enable them.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PartAction {
    Add,
    Remove(u64),
    Toggle { id: u64, enabled: bool },
}

/// A list of multipart rows.
#[derive(Default)]
pub(crate) struct PartRows {
    rows: Vec<PartRow>,
}

impl PartRows {
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    /// Apply an action that came out of one of this list's own widgets.
    pub(crate) fn apply(&mut self, action: PartAction, window: &mut Window, cx: &mut App) {
        match action {
            PartAction::Add => self.rows.push(PartRow::new(window, cx)),
            PartAction::Remove(id) => self.rows.retain(|row| row.id != id),
            PartAction::Toggle { id, enabled } => {
                if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
                    row.enabled = enabled;
                }
            }
        }
    }

    /// Write a chosen file's path into a row. The row may be gone by the time
    /// the picker returns, so a stale id is ignored.
    pub(crate) fn set_path(&mut self, id: u64, path: &str, window: &mut Window, cx: &mut App) {
        if let Some(row) = self.rows.iter().find(|row| row.id == id) {
            set_input(&row.path, path.to_owned(), window, cx);
        }
    }

    /// Replace every row with `parts`. Used by history restore; the widgets own
    /// their text, so the swap happens here rather than in the request model.
    pub(crate) fn replace(&mut self, parts: &[FormPart], window: &mut Window, cx: &mut App) {
        self.rows.clear();
        for part in parts {
            let mut row = PartRow::new(window, cx);
            row.set(part, window, cx);
            self.rows.push(row);
        }
    }

    /// Read every row, in order, dropping blanks.
    pub(crate) fn to_parts(&self, cx: &App) -> Vec<FormPart> {
        self.rows.iter().filter_map(|row| row.to_part(cx)).collect()
    }
}

/// Render the rows plus an "+ Add" button. Every click is reported as a
/// [`PartAction`]; the widget never mutates the list itself. `cx` is needed to
/// read whether a row already holds a path, which decides which input it shows.
pub(crate) fn render(
    rows: &PartRows,
    palette: &Palette,
    cx: &App,
    on_action: impl Fn(&PartAction, &mut Window, &mut App) + 'static,
    on_choose: impl Fn(&u64, &mut Window, &mut App) + 'static,
) -> AnyElement {
    // Every button needs its own copy of the callbacks, but `cx.listener`
    // returns an opaque `Fn` that is not declared `Clone`. Wrapping them in `Rc`
    // gives the clones without a second bound.
    let on_action = Rc::new(on_action);
    let on_choose = Rc::new(on_choose);
    let metrics = Metrics::current(cx);
    let mut body = v_flex().gap(px(metrics.space_xs));

    for row in &rows.rows {
        let id = row.id;
        let has_file = !row.path.read(cx).value().is_empty();

        let toggle = Checkbox::new(("part-enabled", id))
            .checked(row.enabled)
            .on_change({
                let on_action = on_action.clone();
                move |checked: &bool, window, cx| {
                    on_action(
                        &PartAction::Toggle {
                            id,
                            enabled: *checked,
                        },
                        window,
                        cx,
                    )
                }
            });

        let choose = Button::new(format!("part-choose-{id}"))
            .icon(IconName::FileUp)
            .label("Choose…")
            .ghost()
            .compact()
            .tooltip("Pick a file to upload for this part")
            .on_click({
                let on_choose = on_choose.clone();
                move |_: &ClickEvent, window, cx| on_choose(&id, window, cx)
            });

        let remove = Button::new(format!("part-remove-{id}"))
            // A glyph, not a bare `x` label: the same "remove this row" action
            // is `Close` in every other table, and a lowercase `x` next to the
            // mime field read as part of the text.
            .icon(IconName::Close)
            .ghost()
            .compact()
            .tooltip("Remove this row")
            .on_click({
                let on_action = on_action.clone();
                move |_: &ClickEvent, window, cx| on_action(&PartAction::Remove(id), window, cx)
            });

        let target = if has_file {
            Input::new(&row.path)
                .id(format!("part-path-{id}"))
                .flex_1()
                .into_any_element()
        } else {
            Input::new(&row.value)
                .id(format!("part-value-{id}"))
                .flex_1()
                .into_any_element()
        };

        body = body.child(
            h_flex()
                .id(format!("part-row-{id}"))
                .gap(px(metrics.space_sm))
                .items_center()
                .child(toggle)
                .child(Input::new(&row.name).id(format!("part-name-{id}")).flex_1())
                .child(target)
                .child(choose)
                .child(
                    Input::new(&row.mime)
                        .id(format!("part-mime-{id}"))
                        // Twelve gutters wide, so the mime column scales with zoom.
                        .w(px(metrics.gutter * 12.)),
                )
                .child(remove),
        );
    }

    let add = Button::new("part-add")
        // `Plus` icon + "Add", matching the key/value table's Add button, so the
        // two body tables use one shape for adding a row.
        .icon(IconName::Plus)
        .label("Add")
        .ghost()
        .compact()
        .on_click({
            let on_action = on_action.clone();
            move |_: &ClickEvent, window, cx| on_action(&PartAction::Add, window, cx)
        });

    if rows.rows.is_empty() {
        return v_flex()
            .gap(px(metrics.space_sm))
            .items_start()
            .child(
                div()
                    .text_sm()
                    .text_color(palette.muted)
                    .child("No parts yet."),
            )
            .child(add)
            .into_any_element();
    }

    v_flex()
        .gap(px(metrics.space_sm))
        .child(body)
        .child(add)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Named imports, not `super::*`: the supermodule glob-imports gpui-kit,
    // which would bring its `#[test]` macro into scope and shadow the builtin.
    use super::part_from;
    use restbook_core::FormPart;

    fn part(name: &str, value: &str, path: &str, mime: &str) -> Option<FormPart> {
        part_from(name, value, path, mime, true)
    }

    #[test]
    fn an_id_is_never_reused() {
        let first = super::new_id();
        let second = super::new_id();
        assert!(second > first);
    }

    #[test]
    fn a_blank_name_is_omitted() {
        assert!(part("   ", "value", "", "").is_none());
    }

    #[test]
    fn a_value_without_a_path_is_a_text_part() {
        let part = part("field", "hello", "", "").expect("a named part");
        assert_eq!(part.name, "field");
        assert_eq!(part.value, "hello");
        assert!(part.file.is_none());
    }

    #[test]
    fn a_path_makes_it_a_file_part() {
        let part = part("file", "", "/tmp/a.txt", "").expect("a named part");
        assert_eq!(part.file.as_deref(), Some("/tmp/a.txt"));
    }

    #[test]
    fn a_blank_mime_is_dropped_and_others_are_trimmed() {
        assert!(part("f", "v", "", "  ").unwrap().mime.is_none());
        assert_eq!(
            part("f", "v", "", " text/plain ").unwrap().mime.as_deref(),
            Some("text/plain")
        );
    }
}
