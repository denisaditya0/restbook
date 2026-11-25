//! The editor tabs share one widget: a list of enabled key/value rows.
//!
//! The rows own their inputs and report every change as a [`RowAction`] that the
//! owning tab applies. Each value field carries an [`Autocomplete`] subscribed to
//! its own input, so the tab only draws the list and forwards the keys a
//! single-line `InputState` does not claim; see [`super::RequestEditor::value_popup_key`].

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::component::button::*;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use restbook_core::Kv;

use crate::completion::{Autocomplete, Suggestion};
use crate::theme::{Metrics, Palette};
use crate::variables::Variables;

use super::RequestEditor;

/// One row: two inputs and its enabled flag.
pub(crate) struct Row {
    /// Stable for the row's lifetime, so removing a neighbour cannot re-key
    /// what this row keeps (focus, checkbox state, completion anchor).
    id: u64,
    key: Entity<InputState>,
    value: Entity<InputState>,
    /// Lives here rather than in `RequestDef` because removing a row would
    /// otherwise shift every flag onto its neighbour.
    enabled: bool,
    /// The `{{…` list for the value field. A key is a header or a parameter
    /// name, which does not interpolate, so it gets no list.
    value_completion: Autocomplete,
    /// Dropped with the row, which is what unsubscribes the input's events.
    _subscription: Subscription,
}

impl Row {
    pub(crate) fn new(
        variables: &Entity<Variables>,
        window: &mut Window,
        cx: &mut Context<RequestEditor>,
        key_placeholder: &str,
        value_placeholder: &str,
    ) -> Self {
        let key = new_input(window, cx, key_placeholder);
        let value = new_input(window, cx, value_placeholder);

        // The value field follows the URL bar's protocol: an edit re-scans for
        // an open reference, losing focus closes the list, and Enter accepts.
        // The row is found through the input's identity rather than its index,
        // because removing a row shifts every index after it.
        let subscription = cx.subscribe_in(&value, window, {
            let value = value.clone();
            let variables = variables.clone();
            move |editor, _input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let (text, cursor) = {
                        let input = value.read(cx);
                        (input.text().clone(), input.cursor())
                    };
                    if let Some(row) = editor.kv_row_mut(&value) {
                        row.value_completion.sync(&text, cursor, variables.read(cx));
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { secondary, shift } => {
                    if !secondary && !shift {
                        editor.accept_value_completion(value.clone(), None, window, cx);
                    }
                }
                InputEvent::Blur => {
                    if let Some(row) = editor.kv_row_mut(&value) {
                        row.value_completion.close();
                    }
                    cx.notify();
                }
                InputEvent::Focus => {}
            }
        });

        Self {
            id: new_id(),
            key,
            value,
            enabled: true,
            value_completion: Autocomplete::default(),
            _subscription: subscription,
        }
    }

    /// The value field's input, used to recognise the row a completion event
    /// belongs to.
    pub(crate) fn value_entity(&self) -> &Entity<InputState> {
        &self.value
    }

    pub(crate) fn completion(&self) -> &Autocomplete {
        &self.value_completion
    }

    pub(crate) fn completion_mut(&mut self) -> &mut Autocomplete {
        &mut self.value_completion
    }

    /// Secrets are not editable in the UI yet, so the flag is always false
    /// here; environment rows set it later.
    pub(crate) fn to_kv(&self, cx: &App) -> Kv {
        Kv {
            key: self.key.read(cx).value().to_string(),
            value: self.value.read(cx).value().to_string(),
            enabled: self.enabled,
            secret: false,
        }
    }

    /// Write a stored row into the inputs. Used when a history entry is
    /// restored, where the values arrive with a window available.
    fn set(&mut self, kv: &Kv, window: &mut Window, cx: &mut App) {
        self.enabled = kv.enabled;
        self.key
            .update(cx, |state, cx| state.set_value(kv.key.clone(), window, cx));
        self.value.update(cx, |state, cx| {
            state.set_value(kv.value.clone(), window, cx)
        });
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

/// What a click in the widget means. Disabled rows are kept, not dropped, so
/// the user can re-enable them.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RowAction {
    Add,
    Remove(u64),
    Toggle { id: u64, enabled: bool },
}

/// A list of rows.
#[derive(Default)]
pub(crate) struct Rows {
    rows: Vec<Row>,
}

impl Rows {
    pub(crate) fn push(
        &mut self,
        variables: &Entity<Variables>,
        window: &mut Window,
        cx: &mut Context<RequestEditor>,
    ) {
        self.rows
            .push(Row::new(variables, window, cx, "key", "value"));
    }

    pub(crate) fn remove(&mut self, id: u64) {
        self.rows.retain(|row| row.id != id);
    }

    pub(crate) fn toggle(&mut self, id: u64, enabled: bool) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
            row.enabled = enabled;
        }
    }

    /// Apply an action that came out of one of this list's own widgets.
    pub(crate) fn apply(
        &mut self,
        action: RowAction,
        variables: &Entity<Variables>,
        window: &mut Window,
        cx: &mut Context<RequestEditor>,
    ) {
        match action {
            RowAction::Add => self.push(variables, window, cx),
            RowAction::Remove(id) => self.remove(id),
            RowAction::Toggle { id, enabled } => self.toggle(id, enabled),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Row> {
        self.rows.iter_mut()
    }

    /// Replace every row with `values`, keeping their enabled flags. Used by
    /// history restore; the widgets own their text, so the swap happens here
    /// rather than in the request model.
    pub(crate) fn replace(
        &mut self,
        values: &[Kv],
        variables: &Entity<Variables>,
        window: &mut Window,
        cx: &mut Context<RequestEditor>,
    ) {
        self.rows.clear();
        for kv in values {
            let mut row = Row::new(variables, window, cx, "key", "value");
            row.set(kv, window, cx);
            self.rows.push(row);
        }
    }

    /// Read every row, in order.
    pub(crate) fn to_kv(&self, cx: &App) -> Vec<Kv> {
        self.rows.iter().map(|row| row.to_kv(cx)).collect()
    }
}

/// Render the rows plus an "+ Add" button. Every click is reported as a
/// [`RowAction`]; the widget never mutates the list itself. `cx` is needed to
/// build the per-row completion list and its key handler.
pub(crate) fn render(
    rows: &Rows,
    palette: &Palette,
    cx: &mut Context<RequestEditor>,
    on_action: impl Fn(&RowAction, &mut Window, &mut App) + 'static,
) -> AnyElement {
    // Every button needs its own copy of the callback, but `cx.listener`
    // returns an opaque `Fn` that is not declared `Clone`. Wrapping it in `Rc`
    // gives the clones without a second bound.
    let on_action = Rc::new(on_action);
    let metrics = Metrics::current(cx);
    let mut body = v_flex().gap(px(metrics.space_xs));

    for row in &rows.rows {
        let id = row.id;
        let toggle = Checkbox::new(("kv-enabled", id))
            .checked(row.enabled)
            .on_change({
                let on_action = on_action.clone();
                move |checked: &bool, window, cx| {
                    on_action(
                        &RowAction::Toggle {
                            id,
                            enabled: *checked,
                        },
                        window,
                        cx,
                    )
                }
            });

        let remove = Button::new(format!("kv-remove-{}", id))
            .icon(IconName::Close)
            .ghost()
            .compact()
            .tooltip("Remove this row")
            .on_click({
                let on_action = on_action.clone();
                move |_: &ClickEvent, window, cx| on_action(&RowAction::Remove(id), window, cx)
            });

        // The value field is wrapped so the list can hang under it without
        // changing the row's height. The wrapper is a flex parent for the input
        // and the anchor for the absolute list.
        let mut value_cell = div().relative().flex_1().min_w_0().child(
            h_flex().child(
                Input::new(&row.value)
                    .id(format!("kv-value-{}", id))
                    .flex_1(),
            ),
        );
        if let Some((suggestions, selected)) = row.value_completion.visible() {
            value_cell = value_cell.child(render_completions(
                suggestions,
                selected,
                id,
                &row.value,
                palette,
                &metrics,
                cx,
            ));
        }

        let value_for_key = row.value.clone();
        let key_handler = cx.listener(move |editor, event: &KeyDownEvent, window, cx| {
            editor.value_popup_key(value_for_key.clone(), event, window, cx);
        });

        body = body.child(
            h_flex()
                .id(format!("kv-row-{}", id))
                .gap(px(metrics.space_sm))
                .items_center()
                .child(toggle)
                .child(Input::new(&row.key).id(format!("kv-key-{}", id)).flex_1())
                .child(value_cell)
                .child(remove)
                .on_key_down(key_handler),
        );
    }

    let add = Button::new("add-row")
        .icon(IconName::Plus)
        .label("Add")
        .compact()
        .on_click({
            let on_action = on_action.clone();
            move |_: &ClickEvent, window, cx| on_action(&RowAction::Add, window, cx)
        });

    if rows.rows.is_empty() {
        return v_flex()
            .gap(px(metrics.space_sm))
            .items_start()
            .child(
                div()
                    .text_sm()
                    .text_color(palette.muted)
                    .child("No rows yet."),
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

/// The `{{…` list under one row's value field. `anchor` only has to keep the
/// overlay ids distinct if two rows ever held a list at once.
fn render_completions(
    suggestions: &[Suggestion],
    selected: usize,
    anchor: u64,
    value: &Entity<InputState>,
    palette: &Palette,
    metrics: &Metrics,
    cx: &mut Context<RequestEditor>,
) -> AnyElement {
    let mut list = v_flex().gap(px(metrics.space_xs));
    for (index, row) in suggestions.iter().enumerate() {
        let value = value.clone();
        // Key on the label, not the position: the list is rebuilt from scratch
        // on every keystroke, so an index is not an identity.
        let id = format!("kv-completion-{anchor}-{}", row.label);
        list = list.child(
            h_flex()
                .id(id)
                .items_center()
                .gap(px(metrics.space_md))
                .px(px(metrics.space_sm))
                .py(px(metrics.space_xs))
                .rounded_sm()
                .text_sm()
                .cursor_pointer()
                .when(index == selected, |this| this.bg(palette.selection))
                .hover(|this| this.bg(palette.hover))
                .child(div().flex_none().child(row.label.clone()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(palette.muted)
                        .child(row.detail.clone()),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |editor, _, window, cx| {
                        editor.accept_value_completion(value.clone(), Some(index), window, cx);
                    }),
                ),
        );
    }

    // `deferred` lifts the ink above the later siblings, the same way the URL
    // list floats over the editor below it.
    deferred(
        div()
            .absolute()
            .top_full()
            .left_0()
            .right_0()
            .pt(px(metrics.space_xs))
            .child(
                div()
                    .id(("kv-completions", anchor))
                    .occlude()
                    // Eight rows of the table's own height, so the band tracks zoom.
                    .max_h(px(metrics.row_height * 8.))
                    .overflow_y_scrollbar()
                    .rounded_md()
                    .border_1()
                    .border_color(palette.border)
                    .bg(palette.background)
                    .p(px(metrics.space_xs))
                    .child(list),
            ),
    )
    .into_any_element()
}

#[cfg(test)]
mod tests {
    // Named import, not `super::*`: the supermodule glob-imports gpui-kit,
    // whose `#[test]` macro would shadow the builtin.
    use super::new_id;

    #[test]
    fn an_id_is_never_reused() {
        let first = new_id();
        let second = new_id();
        assert!(second > first);
    }
}
