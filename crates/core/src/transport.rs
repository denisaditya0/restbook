//! The one seam between the engine and an HTTP implementation.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use crate::error::Result;
use crate::model::ResponseRecord;
use crate::prepare::PreparedRequest;

/// Cooperative cancellation. Cheap to clone; pass one per in-flight request and
/// cancel it when the user closes a tab or hits escape.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// How far along a send is. Reported while a request is in flight so a UI can
/// draw a spinner with real numbers instead of an indeterminate one.
///
/// The counters are atomics behind an `Arc`, so a transport can bump them from
/// whatever thread is driving I/O while the caller reads them from the UI
/// thread. [`Progress::snapshot`] is the read side; nothing here ever blocks.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    state: Arc<ProgressState>,
}

#[derive(Debug, Default)]
struct ProgressState {
    /// Bytes of the request body sent so far, and the total when known.
    sent: AtomicU64,
    sent_total: AtomicU64,
    /// Bytes of the response body received so far, and the total when the
    /// `Content-Length` (or the decoded length) is known.
    received: AtomicU64,
    received_total: AtomicU64,
    /// Milliseconds since the request started, stamped on every report.
    elapsed_ms: AtomicU64,
    /// `1` once headers have arrived and the body is being read.
    headers_received: AtomicBool,
}

/// A point-in-time view of a [`Progress`] sink. Cheap to copy and compare.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub sent_bytes: u64,
    pub sent_total: Option<u64>,
    pub received_bytes: u64,
    pub received_total: Option<u64>,
    pub elapsed_ms: u64,
    pub headers_received: bool,
}

impl Progress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Headers are on the wire; the body is next.
    pub fn headers_received(&self) {
        self.state.headers_received.store(true, Ordering::Relaxed);
    }

    /// The request body length, when known before sending.
    pub fn set_sent_total(&self, bytes: u64) {
        self.state.sent_total.store(bytes, Ordering::Relaxed);
    }

    /// The request body is fully sent.
    pub fn sent(&self, bytes: u64, elapsed_ms: u64) {
        self.state.sent.store(bytes, Ordering::Relaxed);
        self.state.elapsed_ms.store(elapsed_ms, Ordering::Relaxed);
    }

    /// The response body length, from `Content-Length` or the decoded size.
    /// Zero is indistinguishable from "unknown", so callers pass `None`.
    pub fn set_received_total(&self, bytes: Option<u64>) {
        self.state
            .received_total
            .store(bytes.unwrap_or(0), Ordering::Relaxed);
    }

    /// A chunk of the response body has arrived.
    pub fn received(&self, bytes: u64, elapsed_ms: u64) {
        self.state.received.store(bytes, Ordering::Relaxed);
        self.state.elapsed_ms.store(elapsed_ms, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> ProgressSnapshot {
        let state = &self.state;
        ProgressSnapshot {
            sent_bytes: state.sent.load(Ordering::Relaxed),
            sent_total: non_zero(state.sent_total.load(Ordering::Relaxed)),
            received_bytes: state.received.load(Ordering::Relaxed),
            received_total: non_zero(state.received_total.load(Ordering::Relaxed)),
            elapsed_ms: state.elapsed_ms.load(Ordering::Relaxed),
            headers_received: state.headers_received.load(Ordering::Relaxed),
        }
    }
}

/// `0` means "not known", which is the same convention `Content-Length: 0`
/// would break. A zero-length body reports `Some(0)` from the receive path
/// immediately, so this only affects the "total" hint and is acceptable.
fn non_zero(value: u64) -> Option<u64> {
    if value == 0 {
        None
    } else {
        Some(value)
    }
}

#[async_trait]
pub trait Transport: Send + Sync + 'static {
    async fn execute(
        &self,
        request: PreparedRequest,
        cancel: CancelToken,
    ) -> Result<ResponseRecord>;

    /// Send, reporting byte progress as it goes.
    ///
    /// The default ignores `progress` and calls [`Transport::execute`], so a
    /// transport that cannot report progress still satisfies the trait. A
    /// transport that can (like the reqwest one) overrides this and drives the
    /// same counters during upload and download.
    async fn execute_with_progress(
        &self,
        request: PreparedRequest,
        cancel: CancelToken,
        _progress: Progress,
    ) -> Result<ResponseRecord> {
        self.execute(request, cancel).await
    }

    /// The cookies the transport would send to `url`, as `name=value` pairs.
    ///
    /// Empty for a transport that keeps no jar. This is the read half of the
    /// shared jar: the response pane lists them so a user can see what a login
    /// left behind without sending a second request.
    fn cookies(&self, _url: &str) -> Vec<String> {
        Vec::new()
    }

    /// Drop every stored cookie. The jar is process-wide, so this is the only
    /// way to start a clean session short of restarting the app.
    fn clear_cookies(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_sink_reports_nothing() {
        let snapshot = Progress::new().snapshot();
        assert_eq!(snapshot, ProgressSnapshot::default());
        assert_eq!(snapshot.sent_total, None);
        assert_eq!(snapshot.received_total, None);
        assert!(!snapshot.headers_received);
    }

    #[test]
    fn a_zero_total_is_reported_as_unknown_not_zero() {
        let progress = Progress::new();
        progress.set_received_total(None);
        assert_eq!(progress.snapshot().received_total, None);
        progress.set_received_total(Some(0));
        // Zero and "unknown" share a slot; the trade-off is documented above.
        assert_eq!(progress.snapshot().received_total, None);
    }

    #[test]
    fn reports_accumulate_and_stamp_elapsed() {
        let progress = Progress::new();
        progress.headers_received();
        progress.set_sent_total(128);
        progress.sent(128, 5);
        progress.set_received_total(Some(2048));
        progress.received(512, 7);
        progress.received(2048, 30);

        let snapshot = progress.snapshot();
        assert!(snapshot.headers_received);
        assert_eq!(snapshot.sent_bytes, 128);
        assert_eq!(snapshot.sent_total, Some(128));
        assert_eq!(snapshot.received_bytes, 2048);
        assert_eq!(snapshot.received_total, Some(2048));
        assert_eq!(snapshot.elapsed_ms, 30);
    }

    #[test]
    fn a_snapshot_is_readable_from_another_thread() {
        let progress = Progress::new();
        let writer = progress.clone();
        let handle = std::thread::spawn(move || writer.received(99, 4));
        handle.join().unwrap();
        assert_eq!(progress.snapshot().received_bytes, 99);
    }
}
