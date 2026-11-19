//! The Headers tab: request headers, plus the generated defaults.
//!
//! Rows the user writes are the request's own headers. Headers the client adds
//! on every send (and the body's `Content-Type`) are not part of the request,
//! so they live in a collapsed "Default headers" section beneath the table,
//! the way a web client hides its generated headers. An enabled row above with
//! the same name overrides the default and drops it from the section.

use gpui_kit::component::button::*;
use gpui_kit::component::{h_flex, v_flex, IconName};
use gpui_kit::*;

use crate::theme::{Metrics, Palette};

use super::kv_table::{self, RowAction};
use super::{BodyKind, RequestEditor, MIME_TYPES};

impl RequestEditor {
    pub(crate) fn render_headers_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let metrics = self.metrics;
        let on_action = cx.listener(|this, action: &RowAction, window, cx| {
            let variables = this.variables.clone();
            this.headers.apply(*action, &variables, window, cx);
            cx.notify();
        });

        let defaults = self.default_header_rows(cx);
        let expanded = self.show_default_headers;

        let page = v_flex()
            .child(
                div()
                    .pb(px(metrics.space_sm))
                    .text_xs()
                    .text_color(self.palette.muted)
                    .child("Request headers. Auth adds its own header on top of these."),
            )
            .child(kv_table::render(
                &self.headers,
                &self.palette,
                cx,
                on_action,
            ));

        // Nothing to hide once every default is overridden, so the section
        // disappears rather than showing an empty disclosure.
        if defaults.is_empty() {
            return page.into_any_element();
        }

        let count = defaults.len();
        let toggle = Button::new("headers-default-toggle")
            .icon(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .label(format!("Default headers ({count})"))
            .ghost()
            .compact()
            .tooltip("Headers the client adds without storing them in the request")
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_default_headers = !this.show_default_headers;
                cx.notify();
            }));

        let mut section = v_flex()
            .mt(px(metrics.space_lg))
            .gap(px(metrics.space_xs))
            .child(toggle);
        if expanded {
            section = section
                .child(
                    div()
                        .px(px(metrics.space_xs))
                        .text_xs()
                        .text_color(self.palette.muted)
                        .child(
                            "Sent automatically. Add a header above with the same name to override one.",
                        ),
                )
                .child(default_rows(&defaults, &self.palette, &metrics));
        }

        page.child(section).into_any_element()
    }

    /// The headers that will be added at send time because the user's rows do
    /// not already name them. `Content-Type` leads because the body produces it
    /// and it is the one default a user is most likely to want to change.
    fn default_header_rows(&self, cx: &App) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        if let Some(mime) = self.auto_content_type() {
            rows.push(("Content-Type".to_owned(), mime));
        }
        rows.extend(restbook_core::default_headers());

        // An enabled row with the same name replaces the default; a disabled
        // one does not, because it is not sent.
        let user = self.headers.to_kv(cx);
        rows.retain(|(key, _)| {
            !user
                .iter()
                .any(|header| header.enabled && header.key.eq_ignore_ascii_case(key))
        });
        rows
    }

    /// The `Content-Type` the body will produce, or `None` when it sends none.
    fn auto_content_type(&self) -> Option<String> {
        match self.body_kind {
            BodyKind::None => None,
            BodyKind::Raw => {
                let mime = self.raw_mime.trim();
                Some(if mime.is_empty() {
                    MIME_TYPES[0].to_owned()
                } else {
                    mime.to_owned()
                })
            }
            BodyKind::Form => Some("application/x-www-form-urlencoded".to_owned()),
            BodyKind::Multipart => Some("multipart/form-data".to_owned()),
            BodyKind::Binary => None,
        }
    }
}

/// The read-only rows of the default section. They are text, not inputs: the
/// values are generated, and editing one here would mean editing the body or
/// the client, not a request header.
fn default_rows(rows: &[(String, String)], palette: &Palette, metrics: &Metrics) -> AnyElement {
    let mut list = v_flex().gap(px(metrics.space_xs));
    for (key, value) in rows {
        list = list.child(
            h_flex()
                .gap(px(metrics.space_md))
                .items_center()
                .px(px(metrics.space_xs))
                .min_h(px(metrics.row_height))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(palette.muted)
                        .child(key.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(palette.subtle)
                        .child(value.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(palette.subtle)
                        .child("auto"),
                ),
        );
    }
    list.into_any_element()
}
