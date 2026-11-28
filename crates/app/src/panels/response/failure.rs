//! The failure view: the kind-named title, one sentence of next step, and the
//! engine detail and log behind a disclosure.
//!
//! The kind is decided at the boundary, so this module maps a cause to copy
//! rather than parsing the engine's message.

use gpui_kit::component::button::*;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex, Selectable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use restbook_core::{Failure, FailureKind};

use super::{Event, ResponsePanel};
use crate::document::ResponseState;
use crate::icons::{self, IconName};
use crate::util;

impl ResponsePanel {
    /// Open the technical detail on demand. The ordinary view stays readable;
    /// the log is read once, when the disclosure first opens.
    pub(super) fn toggle_failure_details(&mut self, cx: &mut Context<Self>) {
        self.failure_details = !self.failure_details;
        if self.failure_details && self.failure_log.is_none() {
            self.failure_log = crate::logging::tail(80);
        }
        cx.notify();
    }

    pub(super) fn render_failure(
        &mut self,
        state: ResponseState,
        failure: Option<&Failure>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.failure_painted {
            self.failure_painted = true;
            log::debug!(target: "restbook_app::perf", "failure view painted");
        }
        let palette = self.palette.clone();
        let metrics = self.metrics;
        let (method, url) = {
            let document = self.document.read(cx);
            (
                document.request.method,
                document
                    .last_url
                    .clone()
                    .unwrap_or_else(|| document.request.url.clone()),
            )
        };
        let shown_url = crate::logging::redact_url(&url);
        let (elapsed, bytes) = state.stats().unwrap_or_default();
        let fallback = Failure::other("The request failed without a message.");
        let failure = failure.unwrap_or(&fallback);
        // Reqwest repeats the URL in its own message, query included. The pane
        // and the copy-for-a-report action must not show more than the log does.
        let detail = if url.is_empty() {
            failure.message.clone()
        } else {
            failure.message.replace(&url, &shown_url)
        };
        let title = failure.kind.title();
        let hint = failure_hint(failure.kind, &url);
        let stats = format!(
            "after {} · {} received",
            util::duration(elapsed.as_millis() as u64),
            util::human_bytes(bytes as usize),
        );
        let report = format!(
            "{title}\n{hint}\n\n{} {shown_url}\n{stats}\n\n{detail}",
            method
        );
        let copy_error = report.clone();
        let details_open = self.failure_details;
        let log = self.failure_log.clone();

        v_flex()
            .size_full()
            .debug_selector(|| "response-error".to_owned())
            // Same height and surface as the finished header, so a failure does
            // not move the pane's other regions.
            .child(
                h_flex()
                    .h(px(metrics.header_height))
                    .flex_none()
                    .items_center()
                    .gap(px(metrics.space_sm))
                    .px(px(metrics.space_md))
                    .bg(palette.danger_wash)
                    .border_b_1()
                    .border_color(palette.danger_border)
                    .child(
                        icons::sized(IconName::CircleAlert, icons::MEDIUM)
                            .text_color(palette.danger),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(palette.danger)
                            .child("Error"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(palette.muted)
                            .child(util::duration(elapsed.as_millis() as u64)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(palette.muted)
                            .child(util::human_bytes(bytes as usize)),
                    ),
            )
            .child(
                v_flex()
                    .id("response-error-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .px(px(metrics.space_lg))
                    .py(px(metrics.space_lg))
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(720.))
                            .mx_auto()
                            .gap(px(metrics.space_sm))
                            .child(
                                h_flex()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(metrics.space_xs))
                                    .child(
                                        icons::sized(IconName::CircleAlert, icons::MEDIUM)
                                            .text_color(palette.danger),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(palette.danger)
                                            .child(title),
                                    ),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(palette.foreground)
                                    .debug_selector(|| "response-error-hint".to_owned())
                                    .child(hint),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(palette.muted)
                                    .debug_selector(|| "response-error-attempt".to_owned())
                                    .child(format!("{method} {shown_url}")),
                            )
                            .child(div().text_xs().text_color(palette.muted).child(stats))
                            .child(
                                h_flex()
                                    .flex_wrap()
                                    .gap(px(metrics.space_xs))
                                    .child(
                                        Button::new("response-error-retry")
                                            .label("Retry")
                                            .outline()
                                            .debug_selector(|| "response-error-retry".to_owned())
                                            .tooltip("Send this request again")
                                            .on_click(
                                                cx.listener(|_, _, _, cx| cx.emit(Event::Retry)),
                                            ),
                                    )
                                    .child(
                                        Button::new("response-error-details")
                                            .icon(if details_open {
                                                IconName::ChevronUp
                                            } else {
                                                IconName::ChevronDown
                                            })
                                            .label("Details")
                                            .ghost()
                                            .compact()
                                            .selected(details_open)
                                            .debug_selector(|| "response-error-details".to_owned())
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.toggle_failure_details(cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("response-error-copy")
                                            .icon(IconName::Copy)
                                            .label("Copy error")
                                            .ghost()
                                            .compact()
                                            .tooltip("Copy this failure with its details")
                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    copy_error.clone(),
                                                ));
                                            })),
                                    )
                                    // The evidence for the failure is in the log, and
                                    // the file is not obvious. Keep both actions where
                                    // the error is.
                                    .child(
                                        Button::new("copy-log-on-error")
                                            .icon(IconName::ClipboardCopy)
                                            .ghost()
                                            .compact()
                                            .tooltip("Copy the recent log, for a bug report")
                                            .on_click(cx.listener(|_, _, window, cx| {
                                                match crate::logging::tail(200) {
                                                    Some(text) if !text.is_empty() => {
                                                        cx.write_to_clipboard(
                                                            ClipboardItem::new_string(text),
                                                        );
                                                        window.push_notification(
                                                            Notification::success(
                                                                "Copied the recent log",
                                                            ),
                                                            cx,
                                                        );
                                                    }
                                                    _ => window.push_notification(
                                                        Notification::error(
                                                            "No log file is being written",
                                                        ),
                                                        cx,
                                                    ),
                                                }
                                            })),
                                    )
                                    .child(
                                        Button::new("debug-capture-on-error")
                                            .icon(IconName::Bug)
                                            .ghost()
                                            .compact()
                                            .tooltip(
                                                "Record detailed logs for 10 minutes, for a report",
                                            )
                                            .on_click(cx.listener(|_, _, window, cx| {
                                                match crate::logging::start_debug_capture() {
                                                    Ok(path) => window.push_notification(
                                                        Notification::success(format!(
                                                            "Debug logs on for 10 minutes: {}",
                                                            path.display()
                                                        )),
                                                        cx,
                                                    ),
                                                    Err(message) => window.push_notification(
                                                        Notification::error(message),
                                                        cx,
                                                    ),
                                                }
                                            })),
                                    ),
                            )
                            .when(details_open, |column| {
                                column
                                    .child(
                                        div()
                                            .pt(px(metrics.space_xs))
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(palette.muted)
                                            .child("Engine detail"),
                                    )
                                    .child(
                                        div()
                                            .p(px(metrics.space_sm))
                                            .bg(palette.tab_bar)
                                            .border_1()
                                            .border_color(palette.border)
                                            .text_xs()
                                            .text_color(palette.muted)
                                            .debug_selector(|| "response-error-detail".to_owned())
                                            .child(detail),
                                    )
                                    .child(
                                        div()
                                            .pt(px(metrics.space_xs))
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(palette.muted)
                                            .child("Recent log"),
                                    )
                                    .child(
                                        div()
                                            .p(px(metrics.space_sm))
                                            .bg(palette.tab_bar)
                                            .border_1()
                                            .border_color(palette.border)
                                            .text_xs()
                                            .text_color(palette.muted)
                                            .debug_selector(|| "response-error-log".to_owned())
                                            .child(log.unwrap_or_else(|| {
                                                "No log file is being written.".to_owned()
                                            })),
                                    )
                            }),
                    ),
            )
            .into_any_element()
    }
}

/// The sentence under a failure's title. The kind was decided at the boundary,
/// so this maps a cause to the next step without parsing the engine's message.
pub(super) fn failure_hint(kind: FailureKind, url: &str) -> String {
    let hint = match kind {
        FailureKind::InvalidUrl => "Check the scheme and host in the URL bar.",
        FailureKind::Unresolved => {
            "A `{{…}}` reference has no value for this send. Check the environment \
             or replace the token."
        }
        FailureKind::Tls => {
            "The secure connection could not be established. Check the server \
             certificate; if this host is trusted, enable the request's insecure TLS \
             setting."
        }
        FailureKind::Proxy => {
            "The configured proxy could not be reached or rejected the connection."
        }
        FailureKind::Timeout => {
            "The server did not answer in time. Raise the timeout in the request's \
             Settings tab or Settings › Network."
        }
        FailureKind::Connect => {
            "The host could not be reached. Check the address, your connection or VPN, \
             and whether the server is listening."
        }
        FailureKind::Other => "The engine did not report a cause this view recognises.",
    };

    // A plain-HTTP host that only speaks HTTPS is the common connect failure,
    // and the fix is one character in the URL bar.
    if kind == FailureKind::Connect && url.starts_with("http://") {
        format!("{hint} This address is http://; try https:// if the server enforces TLS.")
    } else {
        hint.to_owned()
    }
}
