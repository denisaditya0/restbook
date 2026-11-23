//! The one place a request is sent.
//!
//! `RequestDef` -> [`prepare`] -> [`Transport`] -> [`ResponseRecord`] ->
//! [`Store`]. The panes never touch the transport; they hand the workspace
//! the request and read the document afterwards.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gpui_kit::*;

use restbook_core::{
    prepare, CancelToken, Error, HistoryEntry, OAuthConfig, OAuthGrant, OAuthTransport, Progress,
    Store, Transport,
};

use crate::workspace::Workspace;

impl Workspace {
    /// Fetch an OAuth token for the Auth tab and hand it back to the editor.
    ///
    /// Only the client-credentials grant can be completed without a browser, so
    /// the other grant reports why it stopped instead of guessing. The token
    /// lives in memory on the editor until the next send or fetch.
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
        cx.spawn(async move |this, cx| {
            let result = transport.fetch_token(request).await;
            this.update(cx, |this, cx| {
                let editor = this.panes.request.read(cx).editor();
                editor.update(cx, |editor, cx| match result {
                    Ok(token) => {
                        let status = match token.expires_in {
                            Some(expires) => format!("Token ready; expires in {expires}s."),
                            None => "Token ready.".to_owned(),
                        };
                        log::debug!(target: "restbook_app::send", "oauth: token fetched");
                        editor.set_oauth_token(Some(token.access_token), status, cx);
                    }
                    Err(error) => {
                        log::error!(target: "restbook_app::send", "oauth: {error}");
                        editor.set_oauth_token(None, error.to_string(), cx);
                    }
                });
            })
            .ok();
        })
        .detach();
    }

    /// Send the request as currently edited.
    ///
    /// Preparation is pure, so an invalid URL or an unknown `{{var}}` fails
    /// here, before anything touches the network.
    pub(crate) fn send(&mut self, cx: &mut Context<Self>) {
        // One send at a time. `sending` stays set while a transfer from a tab
        // the user has since left is still on the wire, so switching tabs
        // cannot start a second one.
        if self.document.read(cx).is_pending() || self.sending.is_some() {
            return;
        }
        // The send belongs to the tab it started from, even if the user
        // switches tabs while it is on the wire. Read the owner now, by id: a
        // closed tab shifts every index after it.
        let Some(send_id) = self.active_tab_id() else {
            return;
        };
        // Read the editor live: a body or auth keystroke notifies its own
        // input, not the editor, so the document's copy can lag by one keystroke.
        let request = self.panes.request.read(cx).current_request(cx);

        // Resolve `{{variables}}` against the active environment and globals.
        // The request pane has already refused to send while a name has no
        // value, so a failure here is a race (an environment deleted between
        // the read and the click), not the normal unknown-name path.
        let scopes = self.variables.read(cx).scoped(&request);
        let prepared = match prepare(&request, &scopes.context()) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.document.update(cx, |document, cx| {
                    document.fail_without_sending(error.to_string());
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
            sent_url,
            self.variables
                .read(cx)
                .active()
                .unwrap_or("none"),
        );
        self.document.update(cx, |document, cx| {
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

        cx.spawn(async move |this, cx| {
            // Poll the sink while the request runs so the document always holds
            // the latest byte counts; the transport writes, this reads. The
            // watcher stops when `done` is set, which happens as soon as the
            // send resolves (GPUI tasks cannot be aborted from outside).
            let done = Arc::new(AtomicBool::new(false));
            let watcher = progress.clone();
            let watcher_done = done.clone();
            let watcher_workspace = this.clone();
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
                    watcher_workspace
                        .update(cx, |this, cx| {
                            // Only the tab still on screen may show this send's
                            // progress. A switched-away tab gets the finished
                            // response when it resolves, not a live tick.
                            if this.active_tab_id() != Some(send_id) {
                                return;
                            }
                            this.document.update(cx, |document, cx| {
                                if document.is_pending() {
                                    // The elapsed clock is derived from
                                    // `started_at` at paint time, so every tick
                                    // must repaint even when no bytes arrived
                                    // and the snapshot is unchanged. Only
                                    // rewrite the snapshot when it actually
                                    // differs, to keep that value from
                                    // churning.
                                    if document.progress != Some(now) {
                                        document.progress = Some(now);
                                    }
                                    cx.notify();
                                }
                            });
                        })
                        .ok();
                }
            })
            .detach();

            let response = match transport
                .execute_with_progress(prepared, cancel, progress)
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    done.store(true, Ordering::Relaxed);
                    let canceled = matches!(error, Error::Cancelled);
                    if canceled {
                        log::debug!(target: "restbook_app::send", "cancelled: {sent_url}");
                    } else {
                        log::error!(
                            target: "restbook_app::send",
                            "failed: {}: {error}",
                            crate::logging::redact_url(&sent_url)
                        );
                    }
                    let message = if canceled {
                        None
                    } else {
                        Some(error.to_string())
                    };
                    this.update(cx, |this, cx| {
                        // The transfer is over, whichever tab is on screen.
                        if this.sending == Some(send_id) {
                            this.sending = None;
                        }
                        if this.active_tab_id() == Some(send_id) {
                            this.document.update(cx, |document, cx| {
                                // Frozen first: the summary must keep the bytes
                                // that did arrive before the request was
                                // cancelled or died.
                                document.finish_send(canceled);
                                document.error = message;
                                cx.notify();
                            });
                        } else {
                            if let Some(tab) = this.tabs.iter_mut().find(|tab| tab.id == send_id) {
                                // The document now describes the tab the user
                                // switched to, so the failure goes into the
                                // sender's snapshot; the other tab keeps its own
                                // state untouched. `last_url` still names the
                                // attempt that failed, matching what the tab
                                // would have shown had it stayed on screen.
                                tab.error = message;
                                tab.last_url = Some(sent_url.clone());
                            }
                            // The visible tab only inherited the sender's
                            // "Sending" marker: it has no summary of its own
                            // to freeze, and clearing it frees its Send button.
                            this.document.update(cx, |document, cx| {
                                document.forget_send();
                                cx.notify();
                            });
                        }
                    })
                    .ok();
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
                if this.active_tab_id() == Some(send_id) {
                    this.document.update(cx, |document, cx| {
                        document.finish_send(false);
                        document.response = Some(response);
                        cx.notify();
                    });
                } else {
                    if let Some(tab) = this.tabs.iter_mut().find(|tab| tab.id == send_id) {
                        // The user switched tabs mid-send. Adopt the response
                        // into the tab that asked for it; `select_tab` loads it
                        // back when the user returns, and the visible tab's
                        // document is left alone. `last_url` travels with the
                        // response, or the pane would show it without naming
                        // the URL it came from.
                        tab.response = Some(response);
                        tab.error = None;
                        tab.last_url = Some(sent_url.clone());
                    }
                    // The visible tab only inherited the sender's "Sending"
                    // marker: it has no summary of its own to freeze, and
                    // clearing it frees its Send button.
                    this.document.update(cx, |document, cx| {
                        document.forget_send();
                        cx.notify();
                    });
                }
            })
            .ok();

            if saved {
                history.update(cx, |history, cx| history.reload(cx));
            }
        })
        .detach();
    }
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
    use super::body_preview;

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
}
