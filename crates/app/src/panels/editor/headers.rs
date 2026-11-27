//! The Headers tab: request headers, the collection's inherited rows, and the
//! generated defaults.
//!
//! Rows the user writes are the request's own headers. Headers the client adds on
//! every send (and the body's `Content-Type`) are not part of the request, so they
//! live in a collapsed "Default headers" section beneath the table. An enabled row
//! with the same name overrides the default and drops it from the section.

use gpui_kit::component::button::*;
use gpui_kit::component::{h_flex, v_flex, IconName};
use gpui_kit::*;

use crate::theme::{Metrics, Palette};

use super::kv_table::{self, RowAction};
use super::{BodyKind, InheritedDefaults, RequestEditor, MIME_TYPES};

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
        let inherited = self
            .inherited
            .as_ref()
            .filter(|each| each.headers.iter().any(|header| header.enabled))
            .map(|each| self.render_collection_headers(each, cx));

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
            ))
            .children(inherited);

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
            let rows = defaults
                .iter()
                .map(|(key, value)| (key.clone(), value.clone(), "auto"))
                .collect::<Vec<_>>();
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
                .child(read_only_rows(&rows, &self.palette, &metrics));
        }

        page.child(section).into_any_element()
    }

    /// The collection's headers, read-only: a request row with the same name
    /// wins, and this is where that precedence is visible.
    fn render_collection_headers(
        &self,
        inherited: &InheritedDefaults,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let metrics = self.metrics;
        let expanded = self.show_collection_headers;
        let rows = collection_header_rows(inherited, &self.headers.to_kv(cx));

        let toggle = Button::new("headers-collection-toggle")
            .icon(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .label(format!("Collection headers ({})", rows.len()))
            .ghost()
            .compact()
            .tooltip("Headers inherited from the request's collection")
            .debug_selector(|| "headers-collection-toggle".to_owned())
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_collection_headers = !this.show_collection_headers;
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
                        .child(format!(
                            "From {}. Edit them in the Collections pane; a request row with the same name wins.",
                            inherited.collection
                        )),
                )
                .child(read_only_rows(&rows, &self.palette, &metrics));
        }

        section.into_any_element()
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

/// The collection's enabled headers, tagged with whether an enabled request row
/// has taken the name over. `prepare` merges the same way: the request wins.
fn collection_header_rows(
    inherited: &InheritedDefaults,
    request_rows: &[restbook_core::Kv],
) -> Vec<(String, String, &'static str)> {
    inherited
        .headers
        .iter()
        .filter(|header| header.enabled)
        .map(|header| {
            let overridden = request_rows
                .iter()
                .any(|row| row.enabled && row.key.eq_ignore_ascii_case(&header.key));
            (
                header.key.clone(),
                header.value.clone(),
                if overridden {
                    "overridden"
                } else {
                    "collection"
                },
            )
        })
        .collect()
}

/// The read-only rows of an inherited or generated section. They are text, not
/// inputs: the values come from a collection or the client, and editing one here
/// would name a place that does not hold it.
fn read_only_rows(
    rows: &[(String, String, &'static str)],
    palette: &Palette,
    metrics: &Metrics,
) -> AnyElement {
    let mut list = v_flex().gap(px(metrics.space_xs));
    for (key, value, tag) in rows {
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
                        .child(*tag),
                ),
        );
    }
    list.into_any_element()
}

#[cfg(test)]
mod tests {
    // Named import, not `super::*`: the supermodule glob-imports gpui-kit, whose
    // `test` macro would shadow the built-in `#[test]`.
    use super::collection_header_rows;
    use crate::panels::editor::InheritedDefaults;
    use restbook_core::{Auth, Kv};

    /// A request row with the same name wins, so the inherited row is tagged as
    /// overridden rather than dropped: the user still sees what the collection
    /// would have sent, and a disabled collection row is not listed at all.
    #[test]
    fn an_overridden_collection_header_is_marked_not_dropped() {
        let inherited = InheritedDefaults {
            collection: "Sample API".to_owned(),
            auth: Auth::None,
            headers: vec![
                Kv::new("x-client", "theirs"),
                Kv {
                    enabled: false,
                    ..Kv::new("x-off", "no")
                },
                Kv::new("accept", "*/*"),
            ],
        };
        let request = [Kv::new("X-Client", "mine")];

        assert_eq!(
            collection_header_rows(&inherited, &request),
            vec![
                ("x-client".to_owned(), "theirs".to_owned(), "overridden"),
                ("accept".to_owned(), "*/*".to_owned(), "collection"),
            ]
        );
    }
}
