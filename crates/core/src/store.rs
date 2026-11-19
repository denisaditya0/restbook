//! History persistence.
//!
//! The app implementation encrypts everything (SQLCipher). The trait stays
//! narrow so the UI only knows: record what happened, and look it up later.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::{Collection, Environment, Kv, RequestDef, ResponseRecord};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Assigned by the store on insert; `0` means "not yet stored".
    #[serde(default)]
    pub id: i64,
    pub request: RequestDef,
    pub response: ResponseRecord,
    /// The URL actually sent, after variable resolution.
    pub url: String,
    /// Who owned the request when it was sent. Older rows deserialize to
    /// [`Origin::Standalone`], which is exactly right: they predate ownership.
    #[serde(default)]
    pub origin: Origin,
}

/// The saved request or draft a send belonged to.
///
/// This is the *owner*, not the request text: `Saved` carries the tree node id,
/// so a rename, reorder, or move (all of which keep the node id) leaves the
/// history linked. Collection and folder ids are deliberately absent — they are
/// a tree fact, and duplicating them here would make every move rewrite history.
///
/// `RequestDef::id` cannot serve this purpose: the editor emits an empty id at
/// send time, so a sent request carries no usable identity of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    /// A request that was never saved, or a row from before origins existed.
    #[default]
    Standalone,
    /// A request saved in a collection: `node` is its `SavedRequest` id.
    Saved { node: String },
    /// An unsaved draft: `draft` is the drafts-table row id.
    Draft { draft: i64 },
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

/// The number of entries [`Store::trim_history`] keeps when the cap is `keep`.
///
/// Trimming to exactly the cap would touch storage on every send once the
/// history is full. A tenth of headroom means new sends fill the gap and a trim
/// happens roughly once per `keep / 10` sends instead, while the cap itself is
/// never exceeded.
pub fn trim_target(keep: u32) -> usize {
    let reserve = (keep / 10).max(1);
    keep.saturating_sub(reserve) as usize
}

/// An unsaved request kept across restarts.
///
/// A draft is the whole request, not a reference to a saved one: the UI edits
/// it in place while the user types, and it survives quitting without being
/// filed into a collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    /// Assigned by the store on insert; `0` means "not yet stored".
    #[serde(default)]
    pub id: i64,
    pub request: RequestDef,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub trait Store: Send + Sync + 'static {
    fn append_history(&self, entry: &HistoryEntry) -> Result<i64>;

    /// Empty query means "most recent".
    fn search_history(&self, query: &str, limit: u32) -> Result<Vec<HistoryHit>>;

    /// History owned by one saved request or draft, newest first.
    ///
    /// `Ok(None)` means this backend cannot answer by owner yet, and the caller
    /// should fall back to a URL search. A backend that stores the owner returns
    /// `Ok(Some(rows))`, with an empty vector meaning "this owner has no sends" —
    /// which is *not* the same thing as unsupported.
    fn history_for_origin(&self, _origin: &Origin, _limit: u32) -> Result<Option<Vec<HistoryHit>>> {
        Ok(None)
    }

    /// Re-point every row owned by `from` to `to`, returning how many moved.
    ///
    /// Used when a draft is filed into a collection: its sends are the same
    /// request, so they should follow the new owner rather than vanishing. A
    /// backend that cannot answer by owner has nothing to move and returns `0`.
    fn reassign_history_origin(&self, _from: &Origin, _to: &Origin) -> Result<u64> {
        Ok(0)
    }

    /// The full entry behind a [`HistoryHit`], so a list row can restore the
    /// request it came from (params, headers, body, auth) and be re-sent.
    fn get_history(&self, id: i64) -> Result<Option<HistoryEntry>>;

    /// Drop the oldest history entries so no more than `keep` remain, returning
    /// how many were removed. The retention cap is an app preference, so the
    /// store takes the number rather than owning it. Trimming goes slightly
    /// below `keep` (see [`trim_target`]) so a full history is not rewritten on
    /// every send.
    fn trim_history(&self, keep: u32) -> Result<u64>;

    /// Remove one history entry by id, returning whether one was removed.
    /// Removing an id that is already gone is not an error: another window may
    /// have trimmed it between the search and the click.
    fn delete_history(&self, id: i64) -> Result<bool>;

    /// Remove every history entry, returning how many were removed.
    fn clear_history(&self) -> Result<u64>;

    /// Remove every entry owned by `origin`, returning how many were removed.
    ///
    /// This is "forget this request's sends", never a cascade: deleting a
    /// collection or a request keeps its history (see [`Origin`]), so only a
    /// user clearing one request calls this.
    fn delete_history_for_origin(&self, origin: &Origin) -> Result<u64>;

    /// Every saved collection. A collection is a tree of nested folders and
    /// saved requests, persisted as one row so a reorder or rename is atomic.
    fn collections(&self) -> Result<Vec<Collection>>;

    /// Create or replace a collection. Upsert is by `collection.id`.
    fn save_collection(&self, collection: &Collection) -> Result<()>;

    /// Remove a collection by id. Deleting an unknown id is not an error.
    fn delete_collection(&self, id: &str) -> Result<()>;

    /// Every environment. Order is stable but otherwise unspecified.
    fn environments(&self) -> Result<Vec<Environment>>;

    /// Create or replace an environment. Upsert is by `environment.name`,
    /// because a name is what a `{{var}}` resolves against.
    fn save_environment(&self, environment: &Environment) -> Result<()>;

    /// Remove an environment by name. Deleting an unknown name is not an error.
    fn delete_environment(&self, name: &str) -> Result<()>;

    /// The selected environment, or `None` when no environment is active.
    fn active_environment(&self) -> Result<Option<String>>;

    /// Select an environment by name, or clear the selection with `None`.
    fn set_active_environment(&self, name: Option<&str>) -> Result<()>;

    /// Workspace-global variables, the lowest-precedence scope.
    fn globals(&self) -> Result<Vec<Kv>>;

    /// Replace the workspace-global variables wholesale.
    fn save_globals(&self, vars: &[Kv]) -> Result<()>;

    /// Every draft, newest `updated_at` first.
    fn drafts(&self) -> Result<Vec<Draft>>;

    /// Create or replace a draft. Upsert is by `draft.id`; an id of `0` inserts
    /// and returns the new id.
    fn save_draft(&self, draft: &Draft) -> Result<i64>;

    /// Remove a draft by id. Deleting an unknown id is not an error.
    fn delete_draft(&self, id: i64) -> Result<()>;
}
