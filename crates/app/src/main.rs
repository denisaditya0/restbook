//! Desktop shell for restbook.
//!
//! Wires the engine (`restbook-core` -> `restbook-transport` ->
//! `restbook-store`) into a three-pane window: history on the left, the request
//! editor above the response viewer on the right.
//!
//! Nothing here re-implements HTTP or templating. The only responsibilities are
//! collecting user input, calling [`prepare`], handing the result to a single
//! long-lived [`ReqwestTransport`], and rendering whatever comes back.

use std::sync::Arc;

use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Disableable as _};
use gpui_kit::*;

use restbook_core::{
    prepare, CancelToken, Error, HistoryEntry, HistoryHit, Method, RequestDef, ResponseRecord,
    Store, Transport, VarContext,
};
use restbook_store::{JsonlStore, MemoryStore};
use restbook_transport::ReqwestTransport;

/// Where the JSONL history lives when running from `crates/app`.
const HISTORY_PATH: &str = ".restbook/history.jsonl";
const HISTORY_LIMIT: u32 = 50;

/// Bodies larger than this are clipped before rendering. The full bytes stay in
/// the response record; only the viewer is bounded.
const MAX_BODY_BYTES: usize = 256 * 1024;

const METHODS: [Method; 7] = [
    Method::Get,
    Method::Post,
    Method::Put,
    Method::Patch,
    Method::Delete,
    Method::Head,
    Method::Options,
];

struct AppShell {
    /// One transport for the life of the app: it owns the tokio runtime and the
    /// pooled reqwest clients, both of which are expensive to build.
    transport: Arc<ReqwestTransport>,
    store: Arc<dyn Store>,
    method: Method,
    url_input: Entity<InputState>,
    history: Vec<HistoryHit>,
    response: Option<ResponseRecord>,
    /// Engine errors are already human-readable, so they are shown verbatim.
    error: Option<String>,
    /// Present while a request is in flight; cancelling flips the token.
    pending: Option<CancelToken>,
    /// The URL actually sent, after variable resolution.
    last_url: Option<String>,
}

impl AppShell {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The encrypted store is not wired up yet; JSONL keeps the harness
        // honest, and an in-memory store covers a read-only directory.
        let store: Arc<dyn Store> = match JsonlStore::open(HISTORY_PATH) {
            Ok(store) => Arc::new(store),
            Err(_) => Arc::new(MemoryStore::new()),
        };
        let history = store.search_history("", HISTORY_LIMIT).unwrap_or_default();

        let url_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("https://httpbin.org/get")
        });

        Self {
            transport: Arc::new(
                ReqwestTransport::new().expect("failed to start the restbook http runtime"),
            ),
            store,
            method: Method::Get,
            url_input,
            history,
            response: None,
            error: None,
            pending: None,
            last_url: None,
        }
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            return;
        }

        let url = self.url_input.read(cx).value().to_string();
        let request = RequestDef::new(self.method, url);

        // Preparation is pure, so a typo or an unknown `{{var}}` fails here,
        // before anything touches the network.
        let prepared = match prepare(&request, &VarContext::new()) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.response = None;
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };

        let cancel = CancelToken::new();
        self.pending = Some(cancel.clone());
        self.response = None;
        self.error = None;
        self.last_url = Some(prepared.url.to_string());
        cx.notify();

        let transport = self.transport.clone();
        let store = self.store.clone();
        let sent_url = prepared.url.to_string();
        let request = request.clone();

        cx.spawn(async move |this, cx| {
            let response = match transport.execute(prepared, cancel).await {
                Ok(response) => response,
                Err(error) => {
                    let message = match error {
                        Error::Cancelled => None,
                        other => Some(other.to_string()),
                    };
                    this.update(cx, |state, cx| {
                        state.pending = None;
                        state.error = message;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };

            // Persisting history is blocking file I/O, so it runs off the
            // foreground executor. The store is the app's, not the engine's.
            let entry = HistoryEntry {
                id: 0,
                request,
                response: response.clone(),
                url: sent_url,
            };
            let history = cx
                .background_spawn(async move {
                    if store.append_history(&entry).is_err() {
                        return Vec::new();
                    }
                    store.search_history("", HISTORY_LIMIT).unwrap_or_default()
                })
                .await;

            this.update(cx, |state, cx| {
                state.pending = None;
                state.response = Some(response);
                state.history = history;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.pending {
            cancel.cancel();
            cx.notify();
        }
    }

    /// Reuse a history row as the next request: copy method and URL back into
    /// the editor instead of sending immediately.
    fn load_hit(&mut self, method: String, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(method) = Method::parse(&method) {
            self.method = method;
        }
        self.url_input
            .update(cx, |state, cx| state.set_value(url, window, cx));
        cx.notify();
    }

    fn render_methods(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        METHODS
            .into_iter()
            .map(|method| {
                let button = Button::new(format!("method-{}", method.as_str()))
                    .label(method.as_str())
                    .compact();
                let button = if method == self.method {
                    button.primary()
                } else {
                    button.ghost()
                };
                button
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.method = method;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn render_history(&self, cx: &mut Context<Self>, success: Hsla, danger: Hsla, muted: Hsla) -> Vec<AnyElement> {
        if self.history.is_empty() {
            return vec![div()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(muted)
                .child("Nothing sent yet.")
                .into_any_element()];
        }

        self.history
            .iter()
            .map(|hit| {
                let method = hit.method.clone();
                let url = hit.url.clone();
                let status = hit.status;
                let status_color = if (200..300).contains(&status) {
                    success
                } else if status >= 400 {
                    danger
                } else {
                    muted
                };

                h_flex()
                    .id(format!("history-{}", hit.id))
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().sidebar_accent))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(muted)
                            .child(method.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_sm()
                            .child(url.clone()),
                    )
                    .child(div().text_xs().text_color(status_color).child(status.to_string()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.load_hit(method.clone(), url.clone(), window, cx);
                    }))
                    .into_any_element()
            })
            .collect()
    }
}

impl Render for AppShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Copy the palette out first: the closures below borrow `cx`, and the
        // theme itself is borrowed from it.
        let background = cx.theme().background;
        let foreground = cx.theme().foreground;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let sidebar = cx.theme().sidebar;
        let sidebar_foreground = cx.theme().sidebar_foreground;
        let sidebar_border = cx.theme().sidebar_border;
        let success = cx.theme().success;
        let danger = cx.theme().danger;
        let mono_font = cx.theme().mono_font_family.clone();

        let pending = self.pending.is_some();

        let header = h_flex()
            .px_4()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .child("restbook"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("local-first http client"),
            );

        let error_banner = self.error.as_ref().map(|message| {
            h_flex()
                .px_4()
                .py_2()
                .gap_2()
                .items_start()
                .bg(danger.opacity(0.12))
                .border_b_1()
                .border_color(danger.opacity(0.4))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(danger)
                        .child("Request failed"),
                )
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(foreground)
                        .child(message.clone()),
                )
        });

        let sidebar_pane = v_flex()
            .h_full()
            .w(px(280.))
            .flex_shrink_0()
            .border_r_1()
            .border_color(sidebar_border)
            .bg(sidebar)
            .text_color(sidebar_foreground)
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("History"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(self.history.len().to_string()),
                    ),
            )
            .child(
                v_flex()
                    .id("history-list")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .py_1()
                    .children(self.render_history(cx, success, danger, muted)),
            );

        let send_button = Button::new("send")
            .label(if pending { "Sending" } else { "Send" })
            .primary()
            .loading(pending)
            .disabled(pending)
            .on_click(cx.listener(|this, _, _, cx| this.send(cx)));

        let cancel_button = Button::new("cancel")
            .label("Cancel")
            .danger()
            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx)));

        let request_editor = v_flex()
            .gap_2()
            .p_4()
            .border_b_1()
            .border_color(border)
            .child(h_flex().gap_1().children(self.render_methods(cx)))
            .child(
                h_flex()
                    .gap_2()
                    .child(Input::new(&self.url_input).id("url-bar").flex_1())
                    .child(send_button)
                    .children(pending.then(|| cancel_button.into_any_element())),
            );

        let response_view = match &self.response {
            Some(response) => {
                let (body, truncated) = response_body_text(response);
                let status_color = if response.is_success() {
                    success
                } else if response.status >= 400 {
                    danger
                } else {
                    muted
                };

                v_flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        h_flex()
                            .px_4()
                            .py_2()
                            .gap_3()
                            .border_b_1()
                            .border_color(border)
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(status_color)
                                    .child(response.status.to_string()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(format!("{} ms", response.duration_ms)),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(human_bytes(response.size_bytes())),
                            )
                            .children(
                                response
                                    .header("content-type")
                                    .map(|value| div().text_sm().text_color(muted).child(value.to_owned())),
                            )
                            .children(truncated.then(|| {
                                div().text_xs().text_color(danger).child("truncated for display")
                            }))
                            .children(self.last_url.as_ref().map(|url| {
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(url.clone())
                            })),
                    )
                    .child(
                        div()
                            .id("response-body")
                            .flex_1()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .px_4()
                            .py_3()
                            .font_family(mono_font.clone())
                            .text_size(px(12.))
                            .child(body),
                    )
                    .into_any_element()
            }
            None => v_flex()
                .flex_1()
                .min_h(px(0.))
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child("Send a request to see the response."),
                )
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .bg(background)
            .text_color(foreground)
            .child(header)
            .children(error_banner)
            .child(
                h_flex()
                    .flex_1()
                    .min_h(px(0.))
                    .items_stretch()
                    .child(sidebar_pane)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h(px(0.))
                            .h_full()
                            .child(request_editor)
                            .child(response_view),
                    ),
            )
    }
}

/// Decode and, when the payload is JSON, pretty-print it. Returns the text to
/// render plus whether the display was clipped.
fn response_body_text(response: &ResponseRecord) -> (String, bool) {
    let truncated = response.body.len() > MAX_BODY_BYTES;
    let bytes = &response.body[..response.body.len().min(MAX_BODY_BYTES)];
    let text = String::from_utf8_lossy(bytes).into_owned();

    let is_json = response
        .header("content-type")
        .is_some_and(|value| value.contains("json"));
    let maybe_json = is_json || {
        let trimmed = text.trim_start();
        trimmed.starts_with('{') || trimmed.starts_with('[')
    };

    if maybe_json {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Ok(pretty) = serde_json::to_string_pretty(&value) {
                return (pretty, truncated);
            }
        }
    }

    (text, truncated)
}

fn human_bytes(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn main() {
    gpui_kit::application().run(move |cx| {
        // Required before any GPUI Component feature is used.
        gpui_kit::init(cx);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(80.), px(80.)),
                size: size(px(1280.), px(820.)),
            })),
            ..Default::default()
        };

        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| AppShell::new(window, cx)))
            .expect("failed to open window");
    });
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: the `gpui_kit::*` glob re-exports GPUI's
    // `#[test]` macro, which would shadow the built-in attribute.
    use restbook_core::ResponseRecord;

    use super::{human_bytes, response_body_text, MAX_BODY_BYTES};

    fn response(status: u16, content_type: &str, body: &[u8]) -> ResponseRecord {
        serde_json::from_value(serde_json::json!({
            "status": status,
            "headers": [["content-type", content_type]],
            "body": body,
            "duration_ms": 12u64,
            "received_at": "2026-01-01T00:00:00Z",
        }))
        .expect("response record should deserialize")
    }

    #[test]
    fn pretty_prints_json_bodies() {
        let response = response(200, "application/json", br#"{"b":1,"a":[1,2]}"#);
        let (text, truncated) = response_body_text(&response);
        assert!(!truncated);
        // `preserve_order` is on somewhere in the graph, so keys keep their
        // input order rather than sorting.
        assert_eq!(text, "{\n  \"b\": 1,\n  \"a\": [\n    1,\n    2\n  ]\n}");
    }

    #[test]
    fn leaves_non_json_bodies_alone() {
        let response = response(200, "text/plain", b"hello");
        assert_eq!(response_body_text(&response), ("hello".to_owned(), false));
    }

    #[test]
    fn clips_large_bodies_for_display() {
        let body = vec![b'a'; MAX_BODY_BYTES + 1];
        let response = response(200, "text/plain", &body);
        let (text, truncated) = response_body_text(&response);
        assert!(truncated);
        assert_eq!(text.len(), MAX_BODY_BYTES);
    }

    #[test]
    fn formats_byte_counts() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}
