//! Empty states shared by the panes.
//!
//! A blank pane names what would fill it and offers the next actions with the
//! same per-OS shortcut formatting as the shortcuts sheet: a row is a label
//! with its live hint right-aligned, so the keys line up in one column.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::theme::{Metrics, Palette};

/// One next-action row: a label and its live shortcut.
pub(crate) struct Action {
    pub(crate) label: &'static str,
    pub(crate) hint: Option<String>,
}

impl Action {
    pub(crate) fn new(label: &'static str, hint: Option<String>) -> Self {
        Self { label, hint }
    }
}

/// The rows shared by both shapes: a label with its hint pushed to the right.
/// The caller owns the width, so `justify_between` has an edge to reach.
fn action_rows(actions: Vec<Action>, palette: &Palette, metrics: &Metrics) -> AnyElement {
    v_flex()
        .w_full()
        .gap(px(metrics.space_xs))
        .children(actions.into_iter().map(|action| {
            let selector = format!("empty-action-{}", action.label);
            h_flex()
                .justify_between()
                .items_center()
                .gap(px(metrics.space_md))
                .debug_selector(move || selector.clone())
                .child(
                    div()
                        .text_sm()
                        .text_color(palette.foreground)
                        .child(action.label),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(palette.muted)
                        .child(action.hint.unwrap_or_default()),
                )
        }))
        .into_any_element()
}

/// A block centered in the pane: a left-aligned title, an optional one-line
/// explanation, the next actions, and an optional footer. The block caps at a
/// reading width but shrinks with a narrow pane.
pub(crate) fn empty_state(
    title: impl Into<SharedString>,
    body: Option<impl Into<SharedString>>,
    actions: Vec<Action>,
    footer: Option<AnyElement>,
    palette: &Palette,
    metrics: &Metrics,
) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .child(
            v_flex()
                .w_full()
                .max_w(px(metrics.space_2xl * 8.0))
                .items_start()
                .gap(px(metrics.space_sm))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(palette.foreground)
                        .child(title.into()),
                )
                .when_some(body, |this, body| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(palette.subtle)
                            .child(body.into()),
                    )
                })
                .child(action_rows(actions, palette, metrics))
                .children(footer),
        )
        .into_any_element()
}

/// A top-aligned block for a narrow pane (the 260px sidebar): the same title,
/// body, and rows, but padded to the pane edge instead of centered, so it fits
/// without the fixed reading width.
pub(crate) fn empty_state_inline(
    title: impl Into<SharedString>,
    body: Option<impl Into<SharedString>>,
    actions: Vec<Action>,
    palette: &Palette,
    metrics: &Metrics,
) -> AnyElement {
    v_flex()
        .w_full()
        .items_start()
        .gap(px(metrics.space_sm))
        .px(px(metrics.space_md))
        .py(px(metrics.space_sm))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(palette.foreground)
                .child(title.into()),
        )
        .when_some(body, |this, body| {
            this.child(
                div()
                    .text_xs()
                    .text_color(palette.subtle)
                    .child(body.into()),
            )
        })
        .child(action_rows(actions, palette, metrics))
        .into_any_element()
}
