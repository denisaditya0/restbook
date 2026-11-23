//! The request line and the outcome of sending it, as a value the panes observe.
//!
//! The editor owns the widgets and mirrors method and URL here so the Send
//! button reads one shared place; a history pick goes the other way. At send
//! time the workspace re-folds the editor into a full request, then writes the
//! response, the error, and the in-flight flag back here. No second copies.

use std::time::{Duration, Instant};

use gpui_kit::Context;

use restbook_core::Origin;
use restbook_core::{CancelToken, Method, ProgressSnapshot, RequestDef, ResponseRecord};

pub(crate) struct Document {
    /// The request line mirrored from the editor, with the rest of the request
    /// as of the last editor change. Sending re-reads the editor live, so a
    /// body or auth keystroke that has not been mirrored yet is still included.
    pub(crate) request: RequestDef,
    pub(crate) response: Option<ResponseRecord>,
    /// Engine errors are already human-readable, so they are shown verbatim.
    pub(crate) error: Option<String>,
    /// Present while a request is in flight; cancelling flips the token.
    pub(crate) pending: Option<CancelToken>,
    /// Live byte progress while a request is in flight, cleared on completion.
    /// The response pane can render it without reaching into the transport.
    pub(crate) progress: Option<ProgressSnapshot>,
    /// When the in-flight request started, for the summary's ticking clock.
    ///
    /// `ProgressSnapshot::elapsed_ms` only advances when the transport reports
    /// bytes, so a server that is slow to answer would leave the clock frozen.
    /// Wall time from this `Instant` moves every frame regardless.
    pub(crate) started_at: Option<Instant>,
    /// The elapsed time and byte count of a send that left no response (a
    /// cancel or an error). `progress` is cleared when a send resolves, so the
    /// numbers are frozen here first or the summary would read zero.
    pub(crate) finished: Option<SendStats>,
    /// Whether the last send ended because it was cancelled. A cleared
    /// `pending` alone cannot tell a deliberate cancel from a fresh state.
    pub(crate) canceled: bool,
    /// The URL actually sent, after variable resolution.
    pub(crate) last_url: Option<String>,
    /// The saved request or draft this document belongs to, mirrored from the
    /// active tab. The response pane scopes its history on this, so two saved
    /// requests with the same URL do not share a send log.
    pub(crate) origin: Origin,
    /// Bumped whenever history is removed behind the response pane. That pane
    /// otherwise reloads only on a new query or send, so a delete made in the
    /// sidebar or the history sheet would leave its "Earlier sends" count stale.
    pub(crate) history_revision: u64,
}

/// The final numbers of a send, kept for the states that have no
/// [`ResponseRecord`] to read them from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SendStats {
    pub(crate) elapsed: Duration,
    pub(crate) bytes_received: u64,
}

/// What the response pane is showing right now.
///
/// This is derived from the fields on [`Document`] rather than stored: an
/// in-flight clock has to keep moving, and a stored state would freeze it.
/// The variant still makes the five states explicit so the pane matches on
/// them instead of guessing from a pile of `Option`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResponseState {
    /// Nothing sent yet, or the response was wiped. The pane shows its hint.
    Idle,
    /// A request is on the wire. `elapsed` is re-derived on every repaint.
    Fetching {
        bytes_received: u64,
        elapsed: Duration,
    },
    /// The user cancelled, or the request was abandoned before an answer.
    Canceled {
        bytes_received: u64,
        elapsed: Duration,
    },
    /// A response arrived. The pane renders the full header and tabs.
    Completed {
        status: u16,
        bytes_received: u64,
        elapsed: Duration,
    },
    /// The send failed before producing a response.
    Error {
        bytes_received: u64,
        elapsed: Duration,
    },
}

impl ResponseState {
    /// The word shown in the summary strip. `Completed` normally renders the
    /// full header instead, but naming it here keeps every state in one place.
    pub(crate) fn label(self) -> &'static str {
        match self {
            ResponseState::Idle => "Ready",
            ResponseState::Fetching { .. } => "Fetching",
            ResponseState::Canceled { .. } => "Canceled",
            ResponseState::Completed { .. } => "Completed",
            ResponseState::Error { .. } => "Error",
        }
    }

    /// The elapsed time and bytes received, for the summary strip.
    pub(crate) fn stats(self) -> Option<(Duration, u64)> {
        match self {
            ResponseState::Idle => None,
            ResponseState::Fetching {
                bytes_received,
                elapsed,
            }
            | ResponseState::Canceled {
                bytes_received,
                elapsed,
            }
            | ResponseState::Completed {
                bytes_received,
                elapsed,
                ..
            }
            | ResponseState::Error {
                bytes_received,
                elapsed,
            } => Some((elapsed, bytes_received)),
        }
    }
}

impl Document {
    pub(crate) fn new() -> Self {
        Self {
            request: RequestDef::new(Method::Get, ""),
            response: None,
            error: None,
            pending: None,
            progress: None,
            started_at: None,
            finished: None,
            canceled: false,
            last_url: None,
            origin: Origin::default(),
            history_revision: 0,
        }
    }

    /// Signal that history was removed elsewhere, so every view that caches a
    /// list of it re-reads the store instead of showing a deleted row.
    pub(crate) fn touch_history(&mut self) {
        self.history_revision = self.history_revision.wrapping_add(1);
    }

    /// The URL this document is about: the one last sent, else the one in the
    /// editor. The cookie jar and the history scope both read it, so they agree
    /// about which request is on screen.
    pub(crate) fn url(&self) -> String {
        self.last_url
            .clone()
            .unwrap_or_else(|| self.request.url.clone())
    }

    /// The state the response pane renders, derived from the fields above. One
    /// source of truth: the pane never caches its own copy.
    pub(crate) fn response_state(&self) -> ResponseState {
        if self.pending.is_some() {
            return ResponseState::Fetching {
                bytes_received: self
                    .progress
                    .map(|progress| progress.received_bytes)
                    .unwrap_or(0),
                elapsed: self
                    .started_at
                    .map(|started| started.elapsed())
                    .unwrap_or_default(),
            };
        }

        if let Some(response) = &self.response {
            return ResponseState::Completed {
                status: response.status,
                bytes_received: response.size_bytes() as u64,
                elapsed: Duration::from_millis(response.duration_ms),
            };
        }

        let stats = self.finished.unwrap_or_default();
        // An error outranks a stale cancel flag: only a resolved
        // `Error::Cancelled` sets `canceled`, and a later failure must win.
        if self.error.is_some() {
            return ResponseState::Error {
                bytes_received: stats.bytes_received,
                elapsed: stats.elapsed,
            };
        }
        if self.canceled {
            return ResponseState::Canceled {
                bytes_received: stats.bytes_received,
                elapsed: stats.elapsed,
            };
        }

        ResponseState::Idle
    }

    /// Mark a send as in flight. The workspace calls this before the transport
    /// runs, so the summary can tick from the first frame.
    pub(crate) fn begin_send(
        &mut self,
        cancel: CancelToken,
        progress: ProgressSnapshot,
        url: String,
    ) {
        self.pending = Some(cancel);
        self.response = None;
        self.error = None;
        self.canceled = false;
        self.finished = None;
        self.started_at = Some(Instant::now());
        self.progress = Some(progress);
        self.last_url = Some(url);
    }

    /// Close out an in-flight send. The clock and byte count are frozen before
    /// `progress` is cleared, so a cancel or failure still has a summary.
    pub(crate) fn finish_send(&mut self, canceled: bool) {
        self.finished = Some(SendStats {
            elapsed: self
                .started_at
                .map(|started| started.elapsed())
                .unwrap_or_default(),
            bytes_received: self
                .progress
                .map(|progress| progress.received_bytes)
                .unwrap_or(0),
        });
        self.canceled = canceled;
        self.pending = None;
        self.progress = None;
        self.started_at = None;
    }

    /// Drop an in-flight send's live state without recording a summary.
    ///
    /// Used when the document is showing a tab that only inherited another
    /// tab's "Sending" marker: there is nothing to freeze, and the frozen
    /// stats and outcome already in the document belong to this tab and must
    /// survive.
    pub(crate) fn forget_send(&mut self) {
        self.pending = None;
        self.progress = None;
        self.started_at = None;
    }

    /// A send that never reached the wire (an unresolved variable, a bad URL).
    /// Any numbers from a previous send must go with it.
    pub(crate) fn fail_without_sending(&mut self, message: String) {
        self.response = None;
        self.error = Some(message);
        self.canceled = false;
        self.pending = None;
        self.progress = None;
        self.started_at = None;
        self.finished = None;
    }

    /// Forget the previous send's cancel/error summary without touching a
    /// response that may still be worth showing. Called when a different
    /// request becomes active (a tab switch, a saved row): that request has not
    /// been sent, so a stale `Canceled` strip must not hang over it.
    pub(crate) fn forget_cancel(&mut self) {
        self.canceled = false;
        self.finished = None;
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Whether the Send button should be inert: no URL yet, or a request
    /// already in flight.
    pub(crate) fn is_sendable(&self) -> bool {
        !self.is_pending() && !self.request.url.trim().is_empty()
    }

    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.pending {
            cancel.cancel();
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_needs_a_url_and_no_request_in_flight() {
        let document = Document::new();
        assert!(!document.is_sendable(), "an empty URL is not sendable");

        let document = Document {
            request: RequestDef::new(Method::Get, "https://httpbin.org/get"),
            ..Document::new()
        };
        assert!(document.is_sendable());
    }

    #[test]
    fn an_adopted_request_is_sendable_without_a_second_click() {
        // Opening a saved row or a history pick writes the request into the
        // document directly; the editor catches up on its next render. If the
        // document were left stale, Send would stay inert until a second click
        // re-fired the editor's observer.
        let mut document = Document::new();
        assert!(!document.is_sendable());
        document.request = RequestDef::new(Method::Post, "https://httpbin.org/post");
        assert!(document.is_sendable());
    }

    #[test]
    fn touching_history_advances_the_revision() {
        let mut document = Document::new();
        assert_eq!(document.history_revision, 0);
        document.touch_history();
        assert_eq!(document.history_revision, 1);
    }

    #[test]
    fn a_fresh_document_is_idle() {
        assert_eq!(Document::new().response_state(), ResponseState::Idle);
    }

    #[test]
    fn an_in_flight_request_reports_the_bytes_received_so_far() {
        let mut document = Document::new();
        document.begin_send(
            CancelToken::new(),
            ProgressSnapshot {
                received_bytes: 42,
                ..ProgressSnapshot::default()
            },
            "https://httpbin.org/get".to_owned(),
        );

        match document.response_state() {
            ResponseState::Fetching {
                bytes_received,
                elapsed,
            } => {
                assert_eq!(bytes_received, 42);
                // Wall time has barely moved, and certainly not backwards.
                assert!(elapsed < Duration::from_secs(5));
            }
            other => panic!("expected Fetching, got {other:?}"),
        }
    }

    #[test]
    fn a_canceled_send_keeps_the_numbers_it_had_when_it_stopped() {
        let mut document = Document::new();
        document.begin_send(
            CancelToken::new(),
            ProgressSnapshot {
                received_bytes: 17,
                ..ProgressSnapshot::default()
            },
            "https://httpbin.org/get".to_owned(),
        );
        document.finish_send(true);

        // `progress` is cleared at the finish, so these can only come from the
        // frozen copy. A zero here would mean the summary lost them.
        match document.response_state() {
            ResponseState::Canceled { bytes_received, .. } => assert_eq!(bytes_received, 17),
            other => panic!("expected Canceled, got {other:?}"),
        }
        assert!(document.error.is_none(), "a cancel is not an error");
    }

    #[test]
    fn a_failure_outranks_a_stale_cancel_flag() {
        let mut document = Document::new();
        document.begin_send(
            CancelToken::new(),
            ProgressSnapshot::default(),
            "https://httpbin.org/get".to_owned(),
        );
        document.finish_send(true);
        // A later, different failure must not be reported as a cancel.
        document.error = Some("boom".to_owned());
        assert!(matches!(
            document.response_state(),
            ResponseState::Error { .. }
        ));
    }

    #[test]
    fn a_response_is_completed_with_its_status_and_size() {
        let mut document = Document::new();
        // Deserialise instead of constructing, so the test does not need a
        // clock just to fill `received_at`.
        document.response = Some(
            serde_json::from_str(
                r#"{
                    "status": 201,
                    "headers": [],
                    "body": [104, 105],
                    "duration_ms": 12,
                    "received_at": "2024-01-01T00:00:00Z"
                }"#,
            )
            .expect("a response record should deserialise"),
        );

        match document.response_state() {
            ResponseState::Completed {
                status,
                bytes_received,
                elapsed,
            } => {
                assert_eq!(status, 201);
                assert_eq!(bytes_received, 2);
                assert_eq!(elapsed, Duration::from_millis(12));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn every_state_names_itself_and_carries_its_numbers() {
        let fetching = ResponseState::Fetching {
            bytes_received: 3,
            elapsed: Duration::from_millis(7),
        };
        assert_eq!(fetching.label(), "Fetching");
        assert_eq!(fetching.stats(), Some((Duration::from_millis(7), 3)));
        assert_eq!(ResponseState::Idle.stats(), None);
        assert_eq!(
            ResponseState::Error {
                bytes_received: 0,
                elapsed: Duration::ZERO
            }
            .label(),
            "Error"
        );
    }
}
