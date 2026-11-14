//! History stores.
//!
//! `MemoryStore` is for tests, `JsonlStore` is the development harness path, and
//! `SqlcipherStore` (feature = "sqlcipher") is what the desktop app ships.

mod jsonl;

pub use jsonl::JsonlStore;

#[cfg(feature = "sqlcipher")]
pub mod sqlcipher;

use std::sync::Mutex;

use restbook_core::{Error, HistoryEntry, HistoryHit, Result, Store};

/// In-process store. Loses everything on exit; useful in tests and previews.
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<Vec<HistoryEntry>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .map(|entries| entries.len())
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Store for MemoryStore {
    fn append_history(&self, entry: &HistoryEntry) -> Result<i64> {
        let mut entries = self.entries.lock().map_err(poisoned)?;
        let id = entries.last().map(|entry| entry.id + 1).unwrap_or(1);
        let mut stored = entry.clone();
        stored.id = id;
        entries.push(stored);
        Ok(id)
    }

    fn search_history(&self, query: &str, limit: u32) -> Result<Vec<HistoryHit>> {
        let entries = self.entries.lock().map_err(poisoned)?;
        Ok(matching(&entries, query, limit))
    }
}

fn poisoned<T>(_: T) -> Error {
    Error::Store("history lock poisoned".to_owned())
}

/// Newest-first filter shared by the non-SQL stores.
pub(crate) fn matching(entries: &[HistoryEntry], query: &str, limit: u32) -> Vec<HistoryHit> {
    let needle = query.trim().to_ascii_lowercase();
    let mut hits: Vec<HistoryHit> = entries
        .iter()
        .rev()
        .filter(|entry| {
            needle.is_empty()
                || entry.url.to_ascii_lowercase().contains(&needle)
                || entry.request.name.to_ascii_lowercase().contains(&needle)
                || entry
                    .request
                    .method
                    .as_str()
                    .to_ascii_lowercase()
                    .contains(&needle)
        })
        .take(limit as usize)
        .map(HistoryEntry::hit)
        .collect();
    hits.reverse();
    hits
}
