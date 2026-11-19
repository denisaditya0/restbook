//! The Settings tab: request-level options that are not headers, body, or auth.
//!
//! All three options are optional by design — an untouched request stores
//! `timeout_ms: None`, `follow_redirects: None`, and `insecure: false`, so a
//! shared collection stays clean and a future default reaches anyone who never
//! overrode it. A global preference would make them un-shareable.

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::Input;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;

use super::RequestEditor;

impl RequestEditor {
    pub(crate) fn render_settings_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = self.palette.muted;
        // The three option rows share this label column, in rem so it tracks
        // the UI scale instead of freezing at one pixel width.
        let label_width = rems(6.);

        let timeout = h_flex()
            .gap(px(self.metrics.space_md))
            .items_center()
            .child(
                div()
                    .w(label_width)
                    .text_sm()
                    .text_color(muted)
                    .child("Timeout"),
            )
            .child(
                div()
                    .w(rems(7.5))
                    .child(Input::new(&self.timeout_ms).id("request-timeout")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("milliseconds; blank uses the transport default"),
            );

        let redirects = h_flex()
            .gap(px(self.metrics.space_md))
            .items_center()
            .child(
                div()
                    .w(label_width)
                    .text_sm()
                    .text_color(muted)
                    .child("Redirects"),
            )
            .child(
                Checkbox::new("request-follow-redirects")
                    .label("follow up to 10")
                    .checked(self.follow_redirects)
                    .on_change(cx.listener(|this, checked, _, cx| {
                        this.follow_redirects = *checked;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("off means a 3xx is returned as-is"),
            );

        let insecure = h_flex()
            .gap(px(self.metrics.space_md))
            .items_center()
            .child(
                div()
                    .w(label_width)
                    .text_sm()
                    .text_color(muted)
                    .child("TLS"),
            )
            .child(
                Checkbox::new("request-insecure")
                    .label("skip certificate verification")
                    .checked(self.insecure)
                    .on_change(cx.listener(|this, checked, _, cx| {
                        this.insecure = *checked;
                        cx.notify();
                    })),
            );

        // The warning is not decoration: turning this on makes the request
        // interceptable, and a user who did it once and forgot would otherwise
        // have no signal at all.
        let warning = self.insecure.then(|| {
            div().text_xs().text_color(self.palette.danger).child(
                "Certificate verification is off for this request; \
                     anyone on the network can read and change it.",
            )
        });

        v_flex()
            .gap(px(self.metrics.space_md))
            .child(timeout)
            .child(redirects)
            .child(insecure)
            .children(warning)
            .into_any_element()
    }
}
