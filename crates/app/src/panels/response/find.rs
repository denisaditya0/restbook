//! Find in the response body: the query field, its matches, and stepping
//! between them.
//!
//! The panel owns the state; these methods are the only place the query is
//! scanned, and they run from a change observer rather than from paint.

use gpui_kit::component::button::*;
use gpui_kit::component::h_flex;
use gpui_kit::component::input::Input;
use gpui_kit::*;

use std::rc::Rc;

use super::display::{find_rows, Finding};
use super::{ResponsePanel, ResponseTab};
use crate::icons::IconName;

impl ResponsePanel {
    /// Recompute the matches when the query or the decoded body changed.
    ///
    /// Called from the input's own change observer and from `body_for_display`,
    /// never from paint: a scroll repaints continuously, and rescanning a
    /// quarter-megabyte body on every frame is the cost this exists to avoid.
    pub(super) fn refresh_finding(&mut self, cx: &mut Context<Self>) {
        let query = self.find.read(cx).value().to_string();
        let Some(cache) = self.display.as_ref() else {
            self.finding = None;
            return;
        };

        // Same shape rule as the body view, so a match never points at a row
        // the user cannot see.
        let hits = if self.numbered() {
            find_rows(&cache.lines, &query)
        } else {
            find_rows(std::slice::from_ref(&cache.text), &query)
        };

        // Keep the current position only while the query is the same, so
        // typing narrows the set from the top rather than jumping to an index
        // that no longer means anything.
        let current = self
            .finding
            .as_ref()
            .filter(|old| old.query == query)
            .and_then(|old| old.current)
            .filter(|index| *index < hits.len());
        self.finding = Some(Finding {
            query,
            hits: Rc::new(hits),
            current,
        });
    }

    /// Open the find row and focus it. Used by the toolbar button, the palette,
    /// and `secondary-f`; unlike the toggle it never closes an already-open row,
    /// so pressing the shortcut again just returns the caret to the field.
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Nothing to search without a body. Checked before focus so the shortcut
        // cannot send the caret to a row that is not in the tree.
        if self.document.read(cx).response.is_none() {
            return;
        }
        // The find lives on the body tab; a shortcut from Headers or Cookies
        // should land where the matches are, not focus a hidden field.
        self.tab = ResponseTab::Body;
        self.find_open = true;
        // A body decoded after the query was typed may not have been scanned, so
        // refresh before the row draws; an empty query is a cheap no-op.
        self.refresh_finding(cx);
        let find = self.find.clone();
        cx.notify();
        window.defer(cx, move |window, cx| {
            find.update(cx, |state, cx| state.focus_handle(cx).focus(window, cx));
        });
    }

    /// Open the find row, or close it and clear the query. The row used to be
    /// rendered only once the query was non-empty, so the button focused an
    /// input that was not in the tree and nothing visibly happened.
    pub(super) fn toggle_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.find_open || !self.find.read(cx).value().is_empty();
        if open {
            self.close_find(window, cx);
        } else {
            self.open_find(window, cx);
        }
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = false;
        self.finding = None;
        self.find
            .update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    /// Move to the next or previous match and return it, wrapping at the ends.
    /// The first press after typing picks the first match rather than the
    /// second, which is what makes Enter behave like the field's own "go".
    pub(super) fn step_match(
        &mut self,
        forward: bool,
        cx: &mut Context<Self>,
    ) -> Option<(usize, usize, usize)> {
        self.refresh_finding(cx);
        let numbered = self.numbered();
        let finding = self.finding.as_mut()?;
        if finding.hits.is_empty() {
            return None;
        }
        let total = finding.hits.len();
        let next = match (finding.current, forward) {
            (None, _) => {
                if forward {
                    0
                } else {
                    total - 1
                }
            }
            (Some(current), true) => (current + 1) % total,
            (Some(current), false) => (current + total - 1) % total,
        };
        let hit = finding.hits[next];
        finding.current = Some(next);
        if numbered {
            // Stepping through a long body has to move the viewport, not only
            // the count; Centre keeps context above and below the hit.
            self.body_scroll
                .scroll_to_item(hit.0, ScrollStrategy::Center);
        }
        cx.notify();
        Some(hit)
    }

    /// The find row under the body toolbar: the query field, the count, and
    /// next/previous. Absent until the query is non-empty, so the toolbar is not
    /// permanently wider than it needs to be.
    pub(super) fn render_find_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = &self.palette;
        let metrics = &self.metrics;
        let (hits, current) = self
            .finding
            .as_ref()
            .map(|finding| (finding.hits.len(), finding.current))
            .unwrap_or((0, None));
        let count = match (hits, current) {
            (0, _) => "No matches".to_owned(),
            (total, Some(index)) => format!("{} of {total}", index + 1),
            (total, None) => format!("{total} matches"),
        };

        h_flex()
            .flex_none()
            .items_center()
            .gap(px(metrics.space_xs))
            .px(px(metrics.space_md))
            .py(px(metrics.space_xs))
            .border_b_1()
            .border_color(palette.border)
            .debug_selector(|| "response-find-row".to_owned())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close_find(window, cx);
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.find).id("response-find")),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(palette.muted)
                    .debug_selector(|| "response-find-count".to_owned())
                    .child(count),
            )
            .child(
                Button::new("find-prev")
                    .icon(IconName::ChevronUp)
                    .ghost()
                    .compact()
                    .debug_selector(|| "response-find-prev".to_owned())
                    .tooltip("Previous match")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.step_match(false, cx);
                    })),
            )
            .child(
                Button::new("find-next")
                    .icon(IconName::ChevronDown)
                    .ghost()
                    .compact()
                    .debug_selector(|| "response-find-next".to_owned())
                    .tooltip("Next match")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.step_match(true, cx);
                    })),
            )
            .child(
                Button::new("find-close")
                    .icon(IconName::Close)
                    .ghost()
                    .compact()
                    .debug_selector(|| "response-find-close".to_owned())
                    .tooltip("Close find")
                    .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            )
    }
}
