//! The one place a request is sent.
//!
//! `RequestDef` -> [`prepare`] -> [`Transport`] -> [`ResponseRecord`] ->
//! [`Store`]. The panes never touch the transport; they hand the workspace
//! the request and read the document afterwards.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gpui_kit::*;

use restbook_core::{
    prepare, Auth, CancelToken, Error, Failure, HistoryEntry, OAuthConfig, OAuthGrant,
    OAuthTransport, Origin, Progress, Store, Transport,
};

use crate::workspace::{TabId, Workspace};

impl Workspace {
    /// Fetch an OAuth token for the Auth tab and hand it back to the tab that
    /// asked, never the one on screen.
    ///
    /// Only the client-credentials grant can be completed without a browser, so
    /// the other grant reports why it stopped instead of guessing. The editor is
    /// shared across tabs, so the completion is bound to the originating
    /// [`TabId`]: while that tab is still active the editor takes the token, and
    /// otherwise it is written into that tab's request so returning to it
    /// re-adopts the token.
    pub(crate) fn fetch_token(&mut self, config: OAuthConfig, cx: &mut Context<Self>) {
        let editor = self.panes.request.read(cx).editor();

        if !matches!(config.grant, OAuthGrant::ClientCredentials) {
            editor.update(cx, |editor, cx| {
                editor.set_oauth_token(
                    None,
                    "Authorization-code tokens need a browser redirect; use client credentials for now."
                        .to_owned(),
                    cx,
                );
            });
            return;
        }

        let request = match config.client_credentials_request() {
            Ok(request) => request,
            Err(error) => {
                editor.update(cx, |editor, cx| {
                    editor.set_oauth_token(None, error.to_string(), cx);
                });
                return;
            }
        };

        editor.update(cx, |editor, cx| {
            editor.set_oauth_token(None, "Fetching token…".to_owned(), cx);
        });

        let transport = self.transport.clone();
        // The tab that asked for the token. The transfer outlives a switch, so
        // the result cannot be written to whichever editor is current when it
        // lands — that is how one tab's token would appear in another.
        let origin = self.active_tab_id();
        cx.spawn(async move |this, cx| {
            let result = transport.fetch_token(request).await;
            this.update(cx, |this, cx| this.apply_token_result(origin, result, cx))
                .ok();
        })
        .detach();
    }

    /// Deliver a fetched token to the tab that asked for it.
    pub(super) fn apply_token_result(
        &mut self,
        origin: Option<TabId>,
        result: restbook_core::Result<restbook_core::TokenResponse>,
        cx: &mut Context<Self>,
    ) {
        let (token, status) = match result {
            Ok(token) => {
                let status = match token.expires_in {
                    Some(expires) => format!("Token ready; expires in {expires}s."),
                    None => "Token ready.".to_owned(),
                };
                log::debug!(target: "restbook_app::send", "oauth: token fetched");
                (Some(token.access_token), status)
            }
            Err(error) => {
                log::error!(target: "restbook_app::send", "oauth: {error}");
                (None, error.to_string())
            }
        };

        if self.active_tab_id() == origin {
            let editor = self.panes.request.read(cx).editor();
            editor.update(cx, |editor, cx| editor.set_oauth_token(token, status, cx));
            return;
        }

        // The tab that asked was closed mid-fetch: there is no request to hand
        // the token to, and the active tab must not take it.
        let Some(index) = origin.and_then(|id| self.tabs.position(id)) else {
            return;
        };
        // A failure belongs to the tab that asked; do not surface it on another.
        let Some(token) = token else {
            return;
        };
        if let Auth::OAuth2 { access_token, .. } = &mut self.tabs[index].request.auth {
            *access_token = Some(token);
        }
        self.push_tabs(cx);
    }

    /// Send the request as currently edited.
    ///
    /// Preparation is pure, so an invalid URL or an unknown `{{var}}` fails
    /// here, before anything touches the network.
    pub(crate) fn send(&mut self, cx: &mut Context<Self>) {
        // One send at a time. `sending` stays set while a transfer from a tab
        // the user has since left is still on the wire, so switching tabs
        // cannot start a second one.
        if self.sending.is_some() {
            return;
        }
        // The send belongs to the tab it started from, even if the user
        // switches tabs while it is on the wire. Read the owner now, by id: a
        // closed tab shifts every index after it, and the tab's own document is
        // where all of this send's outcome lands.
        let Some(send_id) = self.active_tab_id() else {
            return;
        };
        let Some(document) = self.documents.get(&send_id).cloned() else {
            return;
        };
        // Read the editor live: a body or auth keystroke notifies its own
        // input, not the editor, so the document's copy can lag by one keystroke.
        let request = self.panes.request.read(cx).current_request(cx);

        // Pressing Send is the checkpoint: an unfiled tab with a URL becomes a
        // draft now, before `prepare`, so the click — not a successful response —
        // is what keeps the request. A failed send keeps this row.
        if let Some(index) = self.tabs.position(send_id) {
            let unfiled = self.tabs[index].node.is_none() && self.tabs[index].draft.is_none();
            if unfiled && !request.url.trim().is_empty() {
                self.tabs[index].request = request.clone();
                if let Some(draft) = self.save_draft(index, cx) {
                    let origin = Origin::Draft { draft };
                    // Mirror the owner into the active document now, or the
                    // response pane's "This request" scope lags until a switch.
                    document.update(cx, |document, cx| {
                        document.origin = origin;
                        cx.notify();
                    });
                    self.persist_tabs(cx);
                }
            }
        }

        // Resolve `{{variables}}` against the active environment and globals.
        // The request pane has already refused to send while a name has no
        // value, so a failure here is a race (an environment deleted between
        // the read and the click), not the normal unknown-name path.
        let scopes = self.variables.read(cx).scoped(&request);
        let prepared = match prepare(&request, &scopes.context()) {
            Ok(prepared) => prepared,
            Err(error) => {
                document.update(cx, |document, cx| {
                    document.fail_without_sending(scrub_failure(error.failure()));
                    cx.notify();
                });
                return;
            }
        };

        let cancel = CancelToken::new();
        let progress = Progress::new();
        let sent_url = prepared.url.to_string();
        log::debug!(
            target: "restbook_app::send",
            "{} {} (environment: {})",
            request.method,
            crate::logging::redact_url(&sent_url),
            self.variables
                .read(cx)
                .active()
                .unwrap_or("none"),
        );
        document.update(cx, |document, cx| {
            document.begin_send(cancel.clone(), progress.snapshot(), sent_url.clone());
            cx.notify();
        });

        let transport = self.transport.clone();
        let store: Arc<dyn Store> = self.store.clone();
        // The sidebar keeps its own handle on the store, so refreshing it from
        // the task needs nothing to travel back out but the reload call.
        let history = self.panes.history.clone();
        // Read retention now, on the foreground: the background insert below must
        // not touch the settings entity, and the cap cannot change mid-send in a
        // way that matters.
        let retention = self.settings.read(cx).current().history_max_entries;
        // Who the send belongs to for history scoping, by the same reasoning.
        let origin = self
            .tabs
            .get(self.tabs.active())
            .map(|tab| tab.origin())
            .unwrap_or_default();
        self.sending = Some(send_id);
        // Tell the strip which tab is now sending, so its spinner and the other
        // tabs' disabled Send appear before the first tick.
        self.push_tabs(cx);
        let watcher_document = document.clone();

        cx.spawn(async move |this, cx| {
            // Poll the sink while the request runs so the document always holds
            // the latest byte counts; the transport writes, this reads. The
            // watcher stops when `done` is set, which happens as soon as the
            // send resolves (GPUI tasks cannot be aborted from outside).
            let done = Arc::new(AtomicBool::new(false));
            let watcher = progress.clone();
            let watcher_done = done.clone();
            cx.spawn(async move |cx| {
                while !watcher_done.load(Ordering::Relaxed) {
                    // This watcher is the single heartbeat for the response
                    // pane: 100 ms is fast enough for a ticking clock while
                    // the request runs, and nothing else repaints the pane on
                    // a timer.
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(100))
                        .await;
                    let now = watcher.snapshot();
                    // Tick the sending tab's own document, whichever tab is on
                    // screen: the elapsed clock derives from `started_at` at
                    // paint time, so every tick must repaint even when no bytes
                    // arrived. Rewrite the snapshot only when it differs.
                    watcher_document.update(cx, |document, cx| {
                        if document.is_pending() {
                            if document.progress != Some(now) {
                                document.progress = Some(now);
                            }
                            cx.notify();
                        }
                    });
                }
            })
            .detach();

            let request_started = std::time::Instant::now();
            let response = match transport
                .execute_with_progress(prepared, cancel, progress)
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    done.store(true, Ordering::Relaxed);
                    let canceled = matches!(error, Error::Cancelled);
                    let failure = if canceled {
                        None
                    } else {
                        Some(scrub_failure(error.failure()))
                    };
                    // The failure is moved into the document below, so keep the
                    // text the log needs now. It is the same scrubbed message
                    // the Details disclosure shows.
                    let failure_detail = failure
                        .as_ref()
                        .map(|failure| failure.message.clone())
                        .unwrap_or_default();
                    // Paint first. The log sink writes synchronously to stderr
                    // and a file, and doing that before the document update
                    // puts a slow console between the failure and the view.
                    let queued_at = std::time::Instant::now();
                    this.update(cx, |this, cx| {
                        log::debug!(
                            target: "restbook_app::perf",
                            "failure update entered {}ms after transport returned",
                            queued_at.elapsed().as_millis(),
                        );
                        // The transfer is over, whichever tab is on screen.
                        if this.sending == Some(send_id) {
                            this.sending = None;
                        }
                        // The failure lands in the tab that sent it, whichever
                        // tab is on screen now.
                        document.update(cx, |document, cx| {
                            // Frozen first: the summary must keep the bytes that
                            // did arrive before the request was cancelled or died.
                            document.finish_send(canceled);
                            document.error = failure;
                            cx.notify();
                        });
                        this.push_tabs(cx);
                    })
                    .ok();
                    log::debug!(
                        target: "restbook_app::perf",
                        "failure update returned after {}ms",
                        queued_at.elapsed().as_millis(),
                    );

                    // The timestamp separates "the wire took 2.5 s" from "the
                    // pane took 2.5 s"; the log write itself is off the
                    // foreground so it cannot be the latter.
                    let elapsed = request_started.elapsed().as_millis();
                    let shown_url = crate::logging::redact_url(&sent_url);
                    cx.background_spawn(async move {
                        if canceled {
                            log::debug!(
                                target: "restbook_app::send",
                                "cancelled after {elapsed}ms: {shown_url}"
                            );
                        } else {
                            log::error!(
                                target: "restbook_app::send",
                                "failed after {elapsed}ms: {shown_url}: {failure_detail}"
                            );
                        }
                    })
                    .detach();
                    return;
                }
            };
            done.store(true, Ordering::Relaxed);

            log::info!(
                target: "restbook_app::send",
                "{} {} -> {} in {}ms, {} bytes",
                request.method,
                crate::logging::redact_url(&sent_url),
                response.status,
                response.duration_ms,
                response.body.len(),
            );
            // The outcome above is not enough to see *what* came back when a
            // user attaches a capture, so a bounded single-line body preview
            // follows it. Binary is named, never dumped.
            log::debug!(
                target: "restbook_app::send",
                "  response body: {}",
                body_preview(response.body.as_bytes(), 400),
            );
            // Full headers are opt-in (`RUST_LOG=restbook_app=debug`): they can
            // carry cookies and tokens, so they do not belong in the default
            // file a user is asked to attach.
            log::debug!(
                target: "restbook_app::send",
                "response headers: {:?}",
                response.headers,
            );

            // Persisting history is blocking file I/O, so it runs off the
            // foreground executor. The store is the app's, not the engine's.
            let stored_response = response.clone();
            let stored_url = sent_url.clone();
            let saved = cx
                .background_spawn(async move {
                    let entry = HistoryEntry {
                        id: 0,
                        request,
                        response: stored_response,
                        url: stored_url,
                        origin,
                    };
                    if store.append_history(&entry).is_ok() {
                        // Retention runs with the insert so the store cannot grow
                        // without bound. A failed trim is not worth reporting: the
                        // row is already saved and the next send retries.
                        let _ = store.trim_history(retention);
                        true
                    } else {
                        false
                    }
                })
                .await;

            this.update(cx, |this, cx| {
                // The transfer is over, whichever tab is on screen.
                if this.sending == Some(send_id) {
                    this.sending = None;
                }
                // The response lands in the tab that sent it, whichever tab is
                // on screen now; `last_url` travels with it so the pane names
                // the URL it came from.
                document.update(cx, |document, cx| {
                    document.finish_send(false);
                    document.response = Some(response);
                    document.error = None;
                    document.last_url = Some(sent_url.clone());
                    cx.notify();
                });
                this.push_tabs(cx);
            })
            .ok();

            if saved {
                history.update(cx, |history, cx| history.reload(cx));
            }
        })
        .detach();
    }
}

/// A failure's message can quote the URL `reqwest` was given, so its URLs are
/// reduced before the text reaches the log or the Details disclosure.
fn scrub_failure(mut failure: Failure) -> Failure {
    failure.message = crate::logging::redact_urls(&failure.message);
    failure
}

/// A single-line, bounded preview of a response body for the log. Binary
/// payloads are named rather than mangled, and newlines/tabs collapse to spaces
/// so the preview cannot turn one log line into a hundred.
fn body_preview(body: &[u8], max: usize) -> String {
    if body.is_empty() {
        return "<empty>".to_owned();
    }
    if body.iter().take(1024).any(|byte| *byte == 0) {
        return format!("<binary, {} bytes>", body.len());
    }
    let text = String::from_utf8_lossy(body);
    let mut preview = String::with_capacity(max.min(text.len()) + 8);
    for (count, ch) in text.chars().enumerate() {
        if count >= max {
            preview.push('…');
            break;
        }
        match ch {
            '\n' | '\r' | '\t' => preview.push(' '),
            ch if ch.is_control() => preview.push('.'),
            ch => preview.push(ch),
        }
    }
    preview
}

#[cfg(test)]
mod tests {
    // Named import: `use super::*` would pull in GPUI's `#[test]` macro.
    use super::{body_preview, scrub_failure};
    use restbook_core::{Failure, FailureKind};

    #[test]
    fn an_empty_body_is_named() {
        assert_eq!(body_preview(b"", 10), "<empty>");
    }

    #[test]
    fn a_text_body_is_collapsed_to_one_line() {
        assert_eq!(body_preview(b"{\n  \"a\": 1\n}", 100), "{   \"a\": 1 }");
    }

    #[test]
    fn a_long_body_is_truncated() {
        assert_eq!(body_preview(b"abcdefghij", 4), "abcd…");
    }

    #[test]
    fn a_binary_body_is_named_not_dumped() {
        assert_eq!(body_preview(b"a\0b", 100), "<binary, 3 bytes>");
    }

    #[test]
    fn a_failure_message_loses_its_url_secrets() {
        let failure = scrub_failure(Failure::new(
            FailureKind::Connect,
            "error sending request for url (https://alice:secret@api.test/v1/items?token=abc): \
             tcp connect error: refused",
        ));

        assert!(!failure.message.contains("alice"));
        assert!(!failure.message.contains("secret"));
        assert!(!failure.message.contains("token=abc"));
        assert!(failure.message.contains("https://api.test/v1/items"));
        assert!(failure.message.contains("refused"));
        assert_eq!(failure.kind, FailureKind::Connect);
    }
}
