//! The Cookies tab: the session's cookies for the current URL, plus the ones a
//! stored response set, with in-place edit and delete.
//!
//! The transport owns the jar; this module only renders rows and emits the
//! intent, so a cookie change goes through the workspace.

use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex, Disableable as _};
use gpui_kit::*;

use restbook_core::ResponseRecord;

use super::{empty_state, Event, ResponsePanel};
use crate::icons::IconName;
use crate::util;

/// One row of the Cookies tab: a cookie the session would send to the URL, or,
/// when the session holds none for it, one the stored response set.
struct CookieRow {
    /// Element identity: index plus name, because two cookies can share a name
    /// across paths.
    key: String,
    name: String,
    value: String,
    attributes: String,
    /// Whether the row is in the live jar, which is what makes it editable.
    live: bool,
}

/// An in-place edit of one cookie's value. The field is dropped with the edit,
/// which also drops its subscription.
pub(super) struct CookieEdit {
    name: String,
    value: Entity<InputState>,
    _subscription: Subscription,
}

impl ResponsePanel {
    /// The Cookies tab: what the session would send to this URL, one row
    /// editable and removable at a time. A response restored from history has
    /// no jar entries, so its cookies are listed read-only rather than offering
    /// an edit that would change nothing.
    pub(super) fn render_cookies(
        &self,
        response: &ResponseRecord,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = &self.palette;
        let stored = util::cookies(&response.headers);
        let session = self.transport.cookies(&self.document.read(cx).url());
        let live = !session.is_empty();

        let rows = if live {
            session
                .iter()
                .enumerate()
                .filter_map(|(index, pair)| {
                    let (name, value) = pair.split_once('=')?;
                    Some(CookieRow {
                        key: format!("{index}-{name}"),
                        attributes: stored
                            .iter()
                            .find(|cookie| cookie.name == name)
                            .map(|cookie| cookie.attributes.clone())
                            .unwrap_or_default(),
                        name: name.to_owned(),
                        value: value.to_owned(),
                        live: true,
                    })
                })
                .collect::<Vec<_>>()
        } else {
            stored
                .into_iter()
                .enumerate()
                .map(|(index, cookie)| CookieRow {
                    key: format!("{index}-{}", cookie.name),
                    name: cookie.name,
                    value: cookie.value,
                    attributes: cookie.attributes,
                    live: false,
                })
                .collect()
        };

        if rows.is_empty() {
            return empty_state("No cookies received.", palette);
        }

        let note = if live {
            "The session sends these to this URL."
        } else {
            "This stored response is not in the session, so its cookies cannot be changed."
        };

        v_flex()
            .id("response-cookies")
            .debug_selector(|| "response-cookies".to_owned())
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .px(px(self.metrics.space_lg))
            .py(px(self.metrics.space_sm))
            .font_family(palette.mono_font.clone())
            .text_size(px(self.metrics.font_small))
            .child(
                div()
                    .pb(px(self.metrics.space_sm))
                    .text_xs()
                    .text_color(palette.muted)
                    .child(note),
            )
            .children(rows.into_iter().map(|row| self.render_cookie_row(row, cx)))
            .into_any_element()
    }

    fn render_cookie_row(&self, row: CookieRow, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;
        let metrics = &self.metrics;
        let editing = self
            .cookie_edit
            .as_ref()
            .is_some_and(|edit| edit.name == row.name);

        let value: AnyElement = match (editing, self.cookie_edit.as_ref()) {
            (true, Some(edit)) => div()
                .flex_1()
                .min_w_0()
                .child(Input::new(&edit.value).id(format!("cookie-value-{}", row.key)))
                .into_any_element(),
            _ => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(row.value.clone())
                .into_any_element(),
        };

        let actions: AnyElement = if editing {
            h_flex()
                .flex_none()
                .gap(px(metrics.space_xs))
                .items_center()
                .child(
                    Button::new(format!("cookie-save-{}", row.key))
                        .icon(IconName::Check)
                        .ghost()
                        .compact()
                        .tooltip("Save cookie value")
                        .on_click(cx.listener(|this, _, _, cx| this.commit_cookie_edit(cx))),
                )
                .child(
                    Button::new(format!("cookie-cancel-{}", row.key))
                        .icon(IconName::X)
                        .ghost()
                        .compact()
                        .tooltip("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_cookie_edit(cx))),
                )
                .into_any_element()
        } else {
            let live = row.live;
            let edit_name = row.name.clone();
            let edit_value = row.value.clone();
            let delete_name = row.name.clone();
            let why = if live {
                "Edit this cookie's value"
            } else {
                "This cookie is not in the session"
            };
            h_flex()
                .flex_none()
                .gap(px(metrics.space_xs))
                .items_center()
                .child(
                    Button::new(format!("cookie-edit-{}", row.key))
                        .icon(IconName::Pencil)
                        .ghost()
                        .compact()
                        .disabled(!live)
                        .tooltip(why)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_cookie_edit(
                                edit_name.clone(),
                                edit_value.clone(),
                                window,
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new(format!("cookie-delete-{}", row.key))
                        .icon(IconName::Trash)
                        .ghost()
                        .compact()
                        .disabled(!live)
                        .tooltip("Delete this cookie")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.cookie_edit = None;
                            cx.emit(Event::DeleteCookie {
                                name: delete_name.clone(),
                            });
                        })),
                )
                .into_any_element()
        };

        h_flex()
            .id(format!("response-cookie-{}", row.key))
            .debug_selector({
                let key = row.key.clone();
                move || format!("response-cookie-{key}")
            })
            .gap(px(metrics.space_md))
            .py(px(metrics.space_xs))
            .items_center()
            .border_b_1()
            .border_color(palette.border)
            .child(
                div()
                    .w(px(160.))
                    .flex_none()
                    .truncate()
                    .text_color(palette.info)
                    .child(row.name),
            )
            .child(value)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(palette.muted)
                    .child(row.attributes),
            )
            .child(actions)
            .into_any_element()
    }

    /// Begin editing one row in place. The field is created here, once, so the
    /// caret survives a repaint; Enter commits through the subscription.
    fn start_cookie_edit(
        &mut self,
        name: String,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("cookie value"));
        input.update(cx, |state, cx| {
            state.set_value(value, window, cx);
            state.focus(window, cx);
        });
        let subscription = cx.subscribe_in(&input, window, {
            move |this, _input, event: &InputEvent, _window, cx| {
                if matches!(
                    event,
                    InputEvent::PressEnter {
                        secondary: false,
                        shift: false
                    }
                ) {
                    this.commit_cookie_edit(cx);
                }
            }
        });
        self.cookie_edit = Some(CookieEdit {
            name,
            value: input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Hand the edited value to the workspace, which owns the jar.
    fn commit_cookie_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.cookie_edit.take() else {
            return;
        };
        let value = edit.value.read(cx).value().to_string();
        cx.emit(Event::UpdateCookie {
            name: edit.name,
            value,
        });
        cx.notify();
    }

    fn cancel_cookie_edit(&mut self, cx: &mut Context<Self>) {
        self.cookie_edit = None;
        cx.notify();
    }
}
