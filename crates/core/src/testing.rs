//! An in-memory [`Transport`] for tests and previews.
//!
//! This answers from a scripted handler instead of a socket, so the whole
//! pipeline ([`crate::prepare`] -> [`Transport`] -> [`ResponseRecord`] ->
//! [`crate::Store`]) runs offline and deterministically. It depends only on
//! `core`, so the app can render a request without sending one.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::model::ResponseRecord;
use crate::prepare::PreparedRequest;
use crate::transport::{CancelToken, Progress, Transport};

/// What a [`FakeTransport`] answers.
pub enum Reply {
    /// A response with this status, empty body, and no headers.
    Status(u16),
    /// A response with a JSON body (`application/json`).
    Json(String),
    /// A fully specified response record.
    Record(ResponseRecord),
    /// Fail the send with this message.
    Error(String),
}

impl Reply {
    fn into_record(self) -> Result<ResponseRecord> {
        Ok(match self {
            Reply::Status(status) => ResponseRecord {
                status,
                headers: Vec::new(),
                body: Vec::new().into(),
                duration_ms: 0,
                received_at: chrono::Utc::now(),
                final_url: None,
                redirects: Vec::new(),
            },
            Reply::Json(body) => ResponseRecord {
                status: 200,
                headers: vec![("content-type".to_owned(), "application/json".to_owned())],
                body: body.into_bytes().into(),
                duration_ms: 0,
                received_at: chrono::Utc::now(),
                final_url: None,
                redirects: Vec::new(),
            },
            Reply::Record(record) => record,
            Reply::Error(message) => return Err(Error::Transport(message)),
        })
    }
}

type Handler = Arc<dyn Fn(&PreparedRequest) -> Reply + Send + Sync>;

/// A [`Transport`] that answers from a scripted handler and records what it was
/// asked to send.
pub struct FakeTransport {
    handler: Handler,
    /// Every request passed to `execute`, in order. A test asserts on these to
    /// check what `prepare()` actually handed to the wire.
    sent: Mutex<Vec<PreparedRequest>>,
}

impl FakeTransport {
    /// Answer every request with `reply`.
    pub fn always(reply: Reply) -> Arc<Self> {
        Self::with_handler(move |_| match &reply {
            Reply::Status(status) => Reply::Status(*status),
            Reply::Json(body) => Reply::Json(body.clone()),
            Reply::Record(record) => Reply::Record(record.clone()),
            Reply::Error(message) => Reply::Error(message.clone()),
        })
    }

    /// Answer from a handler that sees each prepared request.
    pub fn with_handler<F>(handler: F) -> Arc<Self>
    where
        F: Fn(&PreparedRequest) -> Reply + Send + Sync + 'static,
    {
        Arc::new(Self {
            handler: Arc::new(handler),
            sent: Mutex::new(Vec::new()),
        })
    }

    /// Everything this transport was asked to send.
    pub fn sent(&self) -> Vec<PreparedRequest> {
        self.sent
            .lock()
            .map(|sent| sent.clone())
            .unwrap_or_default()
    }

    /// How many requests were sent.
    pub fn send_count(&self) -> usize {
        self.sent.lock().map(|sent| sent.len()).unwrap_or(0)
    }
}

#[async_trait]
impl Transport for FakeTransport {
    async fn execute(
        &self,
        request: PreparedRequest,
        cancel: CancelToken,
    ) -> Result<ResponseRecord> {
        self.execute_with_progress(request, cancel, Progress::new())
            .await
    }

    async fn execute_with_progress(
        &self,
        request: PreparedRequest,
        cancel: CancelToken,
        progress: Progress,
    ) -> Result<ResponseRecord> {
        // Honor cancellation before recording or answering, so a test can drive
        // the cancel path without a socket. A cancelled request is never "sent".
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }

        if let Ok(mut sent) = self.sent.lock() {
            sent.push(request.clone());
        }

        let record = (self.handler)(&request).into_record()?;
        progress.headers_received();
        let len = record.body.len() as u64;
        progress.set_received_total(Some(len));
        progress.received(len, record.duration_ms);
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Body, Method, RequestDef};
    use crate::prepare::prepare;
    use crate::template::VarContext;

    fn prepared() -> PreparedRequest {
        prepare(
            &RequestDef::new(Method::Get, "https://example.com/things"),
            &VarContext::new(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn answers_the_scripted_reply_and_records_the_send() {
        let transport = FakeTransport::always(Reply::Json("{\"ok\":true}".to_owned()));
        let record = transport
            .execute(prepared(), CancelToken::new())
            .await
            .unwrap();

        assert_eq!(record.status, 200);
        assert_eq!(record.header("content-type"), Some("application/json"));
        assert_eq!(record.text(), "{\"ok\":true}");
        assert_eq!(transport.send_count(), 1);
    }

    #[tokio::test]
    async fn a_handler_can_vary_per_request() {
        let transport = FakeTransport::with_handler(|request| {
            if request.url.path() == "/missing" {
                Reply::Status(404)
            } else {
                Reply::Status(200)
            }
        });

        let mut missing = RequestDef::new(Method::Get, "https://example.com/missing");
        missing.body = Body::None;
        let record = transport
            .execute(
                prepare(&missing, &VarContext::new()).unwrap(),
                CancelToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(record.status, 404);
    }

    #[tokio::test]
    async fn cancellation_short_circuits() {
        let transport = FakeTransport::always(Reply::Status(200));
        let cancel = CancelToken::new();
        cancel.cancel();
        let error = transport.execute(prepared(), cancel).await.unwrap_err();
        assert!(matches!(error, Error::Cancelled));
        assert_eq!(transport.send_count(), 0);
    }

    #[tokio::test]
    async fn progress_reports_the_whole_body() {
        let transport = FakeTransport::always(Reply::Json("12345".to_owned()));
        let progress = Progress::new();
        transport
            .execute_with_progress(prepared(), CancelToken::new(), progress.clone())
            .await
            .unwrap();

        let snapshot = progress.snapshot();
        assert!(snapshot.headers_received);
        assert_eq!(snapshot.received_bytes, 5);
        assert_eq!(snapshot.received_total, Some(5));
    }

    #[tokio::test]
    async fn an_error_reply_becomes_a_transport_error() {
        let transport = FakeTransport::always(Reply::Error("boom".to_owned()));
        let error = transport
            .execute(prepared(), CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Transport(message) if message == "boom"));
    }
}
