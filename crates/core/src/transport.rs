//! The one seam between the engine and an HTTP implementation.

use std::sync::atomic::{AtomicBool, Ordering};
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

#[async_trait]
pub trait Transport: Send + Sync + 'static {
    async fn execute(
        &self,
        request: PreparedRequest,
        cancel: CancelToken,
    ) -> Result<ResponseRecord>;
}
