//! History persistence.
//!
//! The app implementation encrypts everything (SQLCipher). The trait stays
//! narrow so the UI only knows: record what happened, and look it up later.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::{RequestDef, ResponseRecord};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Assigned by the store on insert; `0` means "not yet stored".
    #[serde(default)]
    pub id: i64,
    pub request: RequestDef,
    pub response: ResponseRecord,
    /// The URL actually sent, after variable resolution.
    pub url: String,
}

impl HistoryEntry {
    pub fn hit(&self) -> HistoryHit {
        HistoryHit {
            id: self.id,
            method: self.request.method.to_string(),
            url: self.url.clone(),
            status: self.response.status,
            at: self.response.received_at.to_rfc3339(),
        }
    }
}

/// Lightweight row for lists and search results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryHit {
    pub id: i64,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub at: String,
}

pub trait Store: Send + Sync + 'static {
    fn append_history(&self, entry: &HistoryEntry) -> Result<i64>;

    /// Empty query means "most recent".
    fn search_history(&self, query: &str, limit: u32) -> Result<Vec<HistoryHit>>;
}
