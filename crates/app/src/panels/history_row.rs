//! One stored send, rendered the way the History pane draws it.
//!
//! The row container, its spacing, and the field order are shared by every
//! caller: identity on the first line, when it happened on the second. A caller
//! supplies only the trailing action and what a click means.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use restbook_core::{HistoryHit, Method};

use crate::panels::response::relative_time;
use crate::theme::{Metrics, Palette};

/// A hit flattened for a `'static` list closure, so a row owns its text.
#[derive(Clone)]
pub(crate) struct HistoryRow {
    pub id: i64,
    pub method: Method,
    pub url: SharedString,
    pub status: u16,
    /// RFC 3339, as the store keeps it.
    pub at: SharedString,
}

impl HistoryRow {
    pub(crate) fn from_hit(hit: &HistoryHit) -> Self {
        Self {
            id: hit.id,
            method: Method::parse(&hit.method).unwrap_or(Method::Get),
            url: hit.url.clone().into(),
            status: hit.status,
            at: hit.at.clone().into(),
        }
    }
}

/// How tall a line of text is drawn relative to its font size. Stated here, not
/// inherited from the component theme, so the row's height is a number this file
/// can account for.
const LINE_HEIGHT: f32 = 1.5;

/// The height of one two-line row: the identity line above the time line, with
/// the scale's `sm` step as padding top and bottom and its `xs` step between the
/// lines. Every state draws exactly this height.
pub(crate) fn row_height(metrics: &Metrics) -> f32 {
    let text = (metrics.font_body + metrics.font_xs) * LINE_HEIGHT;
    metrics.space_sm * 2.0 + metrics.space_xs + text
}

/// One row, complete but for its action and click. Every list that draws a send
/// uses this, so their spacing cannot drift apart again.
///
/// `selected` is the picked row's id; a list with no selection passes `false`.
pub(crate) fn row(
    id: impl Into<ElementId>,
    history: &HistoryRow,
    palette: &Palette,
    metrics: &Metrics,
    selected: bool,
    action: AnyElement,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    h_flex()
        .id(id)
        .w_full()
        .h(px(row_height(metrics)))
        .px(px(metrics.space_md))
        .gap(px(metrics.space_sm))
        .items_center()
        .cursor_pointer()
        .hover(|style| style.bg(palette.hover))
        .when(selected, |style| style.bg(palette.selected))
        .child(details(history, palette, metrics))
        .child(action)
        .on_click(on_click)
        .into_any_element()
}

/// The text block: `METHOD URL STATUS` above `TIMESTAMP · relative time`.
fn details(row: &HistoryRow, palette: &Palette, metrics: &Metrics) -> AnyElement {
    let when = when_label(&row.at);
    let ago = relative_time(&row.at);
    let mono_when = is_ascii(&row.at);
    let when = if ago.is_empty() {
        when
    } else {
        format!("{when} · {ago}")
    };

    v_flex()
        .flex_1()
        .min_w_0()
        .gap(px(metrics.space_xs))
        // A narrow pane clips here instead of pushing the status or the action
        // past the row's trailing edge.
        .overflow_hidden()
        .child(
            h_flex()
                .items_center()
                .gap(px(metrics.space_sm))
                .child(
                    div()
                        // Two `xl` steps leave room for the longest verb without
                        // letting it push the URL off the row.
                        .w(px(metrics.space_xl * 2.0))
                        .flex_none()
                        .text_size(px(metrics.font_xs))
                        .font_weight(FontWeight::BOLD)
                        .text_color(palette.method(row.method))
                        .child(row.method.as_str()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(metrics.font_body))
                        .child(row.url.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(metrics.font_xs))
                        .text_color(palette.status(row.status))
                        .child(row.status.to_string()),
                ),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(metrics.font_xs))
                .text_color(palette.subtle)
                .when(mono_when, |style| {
                    style.font_family(palette.mono_font.clone())
                })
                .child(when),
        )
        .into_any_element()
}

/// The absolute timestamp of a hit: `DD-MM-YYYY HH:MM:SS`.
///
/// The store keeps RFC 3339, normally in UTC. A history row is read against the
/// user's own day, so the stamp is converted to the machine's local zone before
/// it is formatted. Seconds are carried so two sends in the same minute are
/// distinguishable rather than merely ordered.
pub(crate) fn when_label(at: &str) -> String {
    use chrono::{DateTime, Local};

    let Ok(sent) = DateTime::parse_from_rfc3339(at) else {
        return "—".to_owned();
    };
    let sent = sent.with_timezone(&Local);
    sent.format("%d-%m-%Y %H:%M:%S").to_string()
}

/// All-ASCII detector, so a stamp with a non-Latin digit or separator falls back
/// to the body font instead of a monospace one that cannot draw it.
fn is_ascii(at: &str) -> bool {
    at.is_ascii()
}

#[cfg(test)]
mod tests {
    use super::{is_ascii, row_height, when_label};
    use crate::theme::Metrics;

    #[test]
    fn the_row_height_is_the_scale_padding_above_and_below_two_lines() {
        let metrics = Metrics::default();
        let expected = metrics.space_sm * 2.0
            + metrics.space_xs
            + (metrics.font_body + metrics.font_xs) * super::LINE_HEIGHT;
        assert_eq!(row_height(&metrics), expected);
    }

    #[test]
    fn the_label_is_the_stamp_without_the_relative_part() {
        let label = when_label("2024-01-01T00:00:00Z");
        assert_eq!(label.len(), 19, "got {label}");
        assert_eq!(&label[2..3], "-");
        assert_eq!(&label[5..6], "-");
        assert_eq!(&label[10..11], " ");
        assert_eq!(&label[13..14], ":");
        assert!(!label.contains('('), "got {label}");
    }

    #[test]
    fn an_unparseable_stamp_does_not_panic() {
        assert_eq!(when_label(""), "—");
    }

    #[test]
    fn ascii_is_recognized() {
        assert!(is_ascii("2024-01-01T00:00:00Z"));
        assert!(!is_ascii("—"));
    }
}
