//! History stores.
//!
//! `MemoryStore` is for tests, `JsonlStore` is the development harness path,
//! `FolderStore` is the collaborative path (a Git-friendly workspace on disk),
//! and `SqlcipherStore` (feature = "sqlcipher") is what the desktop app ships as
//! its local default.

mod folder;
mod jsonl;

pub mod secure;

pub use folder::{FolderStore, MARKER_FILE, STATE_DIR};
pub use jsonl::{read_entries, JsonlStore};

#[cfg(feature = "sqlcipher")]
pub mod sqlcipher;

use std::sync::Mutex;

use restbook_core::store::{trim_target, Draft};
use restbook_core::{
    Collection, Environment, Error, HistoryEntry, HistoryFilter, HistoryHit, Kv, Origin, Result,
    Store,
};

/// In-process store. Loses everything on exit; useful in tests and previews.
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<Vec<HistoryEntry>>,
    collections: Mutex<Vec<Collection>>,
    environments: Mutex<Vec<Environment>>,
    active_environment: Mutex<Option<String>>,
    globals: Mutex<Vec<Kv>>,
    drafts: Mutex<Vec<Draft>>,
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

    fn search_history(
        &self,
        query: &str,
        filter: &HistoryFilter,
        limit: u32,
    ) -> Result<Vec<HistoryHit>> {
        let entries = self.entries.lock().map_err(poisoned)?;
        Ok(matching(&entries, query, filter, limit))
    }

    fn get_history(&self, id: i64) -> Result<Option<HistoryEntry>> {
        let entries = self.entries.lock().map_err(poisoned)?;
        Ok(entries.iter().find(|entry| entry.id == id).cloned())
    }

    fn history_for_origin(
        &self,
        origin: &Origin,
        filter: &HistoryFilter,
        limit: u32,
    ) -> Result<Option<Vec<HistoryHit>>> {
        let entries = self.entries.lock().map_err(poisoned)?;
        Ok(Some(owned_by(&entries, origin, filter, limit)))
    }

    fn reassign_history_origin(&self, from: &Origin, to: &Origin) -> Result<u64> {
        let mut entries = self.entries.lock().map_err(poisoned)?;
        Ok(reassign(&mut entries, from, to))
    }

    fn trim_history(&self, keep: u32) -> Result<u64> {
        let mut entries = self.entries.lock().map_err(poisoned)?;
        if entries.len() <= keep as usize {
            return Ok(0);
        }
        let remove = entries.len() - trim_target(keep);
        entries.drain(..remove);
        Ok(remove as u64)
    }

    fn delete_history(&self, id: i64) -> Result<bool> {
        let mut entries = self.entries.lock().map_err(poisoned)?;
        let before = entries.len();
        entries.retain(|entry| entry.id != id);
        Ok(entries.len() != before)
    }

    fn clear_history(&self) -> Result<u64> {
        let mut entries = self.entries.lock().map_err(poisoned)?;
        let removed = entries.len() as u64;
        entries.clear();
        Ok(removed)
    }

    fn delete_history_for_origin(&self, origin: &Origin) -> Result<u64> {
        let mut entries = self.entries.lock().map_err(poisoned)?;
        let before = entries.len();
        entries.retain(|entry| &entry.origin != origin);
        Ok((before - entries.len()) as u64)
    }

    fn collections(&self) -> Result<Vec<Collection>> {
        Ok(self.collections.lock().map_err(poisoned)?.clone())
    }

    fn save_collection(&self, collection: &Collection) -> Result<()> {
        let mut collections = self.collections.lock().map_err(poisoned)?;
        match collections
            .iter_mut()
            .find(|stored| stored.id == collection.id)
        {
            Some(stored) => *stored = collection.clone(),
            None => collections.push(collection.clone()),
        }
        Ok(())
    }

    fn delete_collection(&self, id: &str) -> Result<()> {
        let mut collections = self.collections.lock().map_err(poisoned)?;
        collections.retain(|stored| stored.id != id);
        Ok(())
    }

    fn environments(&self) -> Result<Vec<Environment>> {
        Ok(self.environments.lock().map_err(poisoned)?.clone())
    }

    fn save_environment(&self, environment: &Environment) -> Result<()> {
        let mut environments = self.environments.lock().map_err(poisoned)?;
        match environments
            .iter_mut()
            .find(|stored| stored.name == environment.name)
        {
            Some(stored) => *stored = environment.clone(),
            None => environments.push(environment.clone()),
        }
        Ok(())
    }

    fn delete_environment(&self, name: &str) -> Result<()> {
        let mut environments = self.environments.lock().map_err(poisoned)?;
        environments.retain(|stored| stored.name != name);
        Ok(())
    }

    fn active_environment(&self) -> Result<Option<String>> {
        Ok(self.active_environment.lock().map_err(poisoned)?.clone())
    }

    fn set_active_environment(&self, name: Option<&str>) -> Result<()> {
        let mut active = self.active_environment.lock().map_err(poisoned)?;
        *active = name.map(str::to_owned);
        Ok(())
    }

    fn globals(&self) -> Result<Vec<Kv>> {
        Ok(self.globals.lock().map_err(poisoned)?.clone())
    }

    fn save_globals(&self, vars: &[Kv]) -> Result<()> {
        let mut globals = self.globals.lock().map_err(poisoned)?;
        *globals = vars.to_vec();
        Ok(())
    }

    fn drafts(&self) -> Result<Vec<Draft>> {
        let mut drafts = self.drafts.lock().map_err(poisoned)?.clone();
        drafts.sort_by_key(|draft| std::cmp::Reverse(draft.updated_at));
        Ok(drafts)
    }

    fn save_draft(&self, draft: &Draft) -> Result<i64> {
        let mut drafts = self.drafts.lock().map_err(poisoned)?;
        let mut stored = draft.clone();
        if stored.id == 0 {
            stored.id = drafts.iter().map(|draft| draft.id).max().unwrap_or(0) + 1;
        }
        let id = stored.id;
        match drafts.iter_mut().find(|existing| existing.id == id) {
            Some(existing) => *existing = stored,
            None => drafts.push(stored),
        }
        Ok(id)
    }

    fn delete_draft(&self, id: i64) -> Result<()> {
        let mut drafts = self.drafts.lock().map_err(poisoned)?;
        drafts.retain(|draft| draft.id != id);
        Ok(())
    }
}

fn poisoned<T>(_: T) -> Error {
    Error::Store("history lock poisoned".to_owned())
}

/// Rows owned by `origin`, newest first, capped at `limit`. Shared by the
/// stores that keep their history in memory or in a flat file.
///
/// Newest first matches the SQL backend's `ORDER BY received_at DESC` and what
/// the response pane reads from `history.first()` (the request's latest send).
pub(crate) fn owned_by(
    entries: &[HistoryEntry],
    origin: &Origin,
    filter: &HistoryFilter,
    limit: u32,
) -> Vec<HistoryHit> {
    entries
        .iter()
        .rev()
        .filter(|entry| &entry.origin == origin && filter.matches_entry(entry))
        .take(limit as usize)
        .map(HistoryEntry::hit)
        .collect()
}

/// Move every entry owned by `from` to `to`, returning how many changed.
pub(crate) fn reassign(entries: &mut [HistoryEntry], from: &Origin, to: &Origin) -> u64 {
    let mut moved = 0;
    for entry in entries.iter_mut() {
        if &entry.origin == from {
            entry.origin = to.clone();
            moved += 1;
        }
    }
    moved
}

/// Newest-first filter shared by the non-SQL stores. The SQL backend orders by
/// `received_at DESC`, and the response pane reads `first()` as the latest send,
/// so this must not flip the order back to oldest-first.
pub(crate) fn matching(
    entries: &[HistoryEntry],
    query: &str,
    filter: &HistoryFilter,
    limit: u32,
) -> Vec<HistoryHit> {
    let needle = query.trim().to_ascii_lowercase();
    entries
        .iter()
        .rev()
        .filter(|entry| {
            (needle.is_empty()
                || entry.url.to_ascii_lowercase().contains(&needle)
                || entry.request.name.to_ascii_lowercase().contains(&needle)
                || entry
                    .request
                    .method
                    .as_str()
                    .to_ascii_lowercase()
                    .contains(&needle))
                && filter.matches_entry(entry)
        })
        .take(limit as usize)
        .map(HistoryEntry::hit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use restbook_core::{Body, CollectionItem, Method, RequestDef, ResponseRecord};

    fn entry() -> HistoryEntry {
        let mut request = RequestDef::new(Method::Post, "https://example.com/things");
        request.headers.push(restbook_core::Kv::new("x-test", "1"));
        request.body = Body::Raw {
            mime: "application/json".to_owned(),
            text: "{\"ok\":true}".to_owned(),
        };
        request.auth = restbook_core::Auth::Bearer {
            token: "secret".to_owned(),
        };

        HistoryEntry {
            id: 0,
            url: request.url.clone(),
            request,
            response: ResponseRecord {
                status: 201,
                headers: Vec::new(),
                body: Vec::new().into(),
                duration_ms: 3,
                received_at: chrono::Utc::now(),
                final_url: None,
                redirects: Vec::new(),
            },
            origin: Origin::default(),
        }
    }

    #[test]
    fn a_hit_can_be_resolved_to_the_full_entry() {
        let store = MemoryStore::new();
        store.append_history(&entry()).unwrap();

        let hits = store
            .search_history("", &HistoryFilter::default(), 10)
            .unwrap();
        assert_eq!(hits.len(), 1);

        let loaded = store.get_history(hits[0].id).unwrap().unwrap();
        assert_eq!(loaded.request.method, Method::Post);
        assert_eq!(loaded.request.headers.len(), 1);
        assert!(matches!(loaded.request.body, Body::Raw { .. }));
        assert!(matches!(
            loaded.request.auth,
            restbook_core::Auth::Bearer { .. }
        ));
    }

    #[test]
    fn an_unknown_id_has_no_entry() {
        let store = MemoryStore::new();
        assert!(store.get_history(42).unwrap().is_none());
    }

    #[test]
    fn history_is_scoped_to_its_owner_not_its_url() {
        // Two saved requests with the same URL are different owners. A send of
        // one must not show up as a send of the other, which is what a URL
        // search would do.
        let store = MemoryStore::new();
        let mut first = entry();
        first.origin = Origin::Saved {
            node: "node-a".to_owned(),
        };
        let mut second = entry();
        second.origin = Origin::Saved {
            node: "node-b".to_owned(),
        };
        // An unsaved send of the same URL is unowned.
        let standalone = entry();
        store.append_history(&first).unwrap();
        store.append_history(&second).unwrap();
        store.append_history(&standalone).unwrap();

        let node_a = store
            .history_for_origin(
                &Origin::Saved {
                    node: "node-a".to_owned(),
                },
                &HistoryFilter::default(),
                10,
            )
            .unwrap()
            .expect("the memory store answers by owner");
        assert_eq!(node_a.len(), 1);

        let unowned = store
            .history_for_origin(&Origin::Standalone, &HistoryFilter::default(), 10)
            .unwrap();
        assert_eq!(unowned.expect("supported").len(), 1);

        // A URL search still sees all three: ownership narrows, it does not hide.
        assert_eq!(
            store
                .search_history("example.com", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn filing_a_draft_moves_its_sends_to_the_saved_node() {
        let store = MemoryStore::new();
        let mut draft = entry();
        draft.origin = Origin::Draft { draft: 7 };
        store.append_history(&draft).unwrap();

        let from = Origin::Draft { draft: 7 };
        let to = Origin::Saved {
            node: "node-a".to_owned(),
        };
        assert_eq!(store.reassign_history_origin(&from, &to).unwrap(), 1);

        let draft_rows = store
            .history_for_origin(&from, &HistoryFilter::default(), 10)
            .unwrap()
            .unwrap();
        assert!(draft_rows.is_empty());
        let saved_rows = store
            .history_for_origin(&to, &HistoryFilter::default(), 10)
            .unwrap()
            .unwrap();
        assert_eq!(saved_rows.len(), 1);
    }

    #[test]
    fn deleting_a_send_leaves_the_others_alone() {
        let store = MemoryStore::new();
        store.append_history(&entry()).unwrap();
        let doomed = store.append_history(&entry()).unwrap();
        store.append_history(&entry()).unwrap();

        assert!(store.delete_history(doomed).unwrap());
        // A second delete is a no-op, not an error (another window may have
        // removed it first).
        assert!(!store.delete_history(doomed).unwrap());

        let hits = store
            .search_history("", &HistoryFilter::default(), 10)
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| hit.id != doomed));
    }

    #[test]
    fn search_narrows_by_method_status_and_time_before_the_limit() {
        let store = MemoryStore::new();
        let now = chrono::Utc::now();
        for (method, status, hours_ago) in [
            (Method::Get, 200, 1),
            (Method::Post, 404, 2),
            (Method::Get, 500, 24 * 30),
        ] {
            let mut item = entry();
            item.request.method = method;
            item.response.status = status;
            item.url = format!("https://example.com/{}", method.as_str().to_lowercase());
            item.response.received_at = now - chrono::Duration::hours(hours_ago);
            store.append_history(&item).unwrap();
        }

        let get = HistoryFilter {
            methods: vec!["get".to_owned()],
            ..Default::default()
        };
        assert_eq!(store.search_history("", &get, 10).unwrap().len(), 2);

        let client_error = HistoryFilter {
            status_classes: vec![4],
            ..Default::default()
        };
        assert_eq!(
            store.search_history("", &client_error, 10).unwrap().len(),
            1
        );

        // The limit is applied after the filter, so a narrow list is not cut
        // short by rows it would have discarded.
        let recent = HistoryFilter {
            since: Some(now - chrono::Duration::days(7)),
            ..Default::default()
        };
        assert_eq!(store.search_history("", &recent, 2).unwrap().len(), 2);

        let successful_get = HistoryFilter {
            methods: vec!["GET".to_owned()],
            status_classes: vec![2],
            ..Default::default()
        };
        let hits = store.search_history("", &successful_get, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].status, 200);
    }

    #[test]
    fn clearing_history_removes_every_send() {
        let store = MemoryStore::new();
        for _ in 0..4 {
            store.append_history(&entry()).unwrap();
        }

        assert_eq!(store.clear_history().unwrap(), 4);
        assert!(store
            .search_history("", &HistoryFilter::default(), 10)
            .unwrap()
            .is_empty());
        // Clearing an empty history is zero, not an error.
        assert_eq!(store.clear_history().unwrap(), 0);
    }

    #[test]
    fn clearing_one_owner_keeps_the_others() {
        let store = MemoryStore::new();
        let mut saved = entry();
        saved.origin = Origin::Saved {
            node: "node-a".to_owned(),
        };
        let mut draft = entry();
        draft.origin = Origin::Draft { draft: 9 };
        store.append_history(&saved).unwrap();
        store.append_history(&draft).unwrap();

        let cleared = store
            .delete_history_for_origin(&Origin::Saved {
                node: "node-a".to_owned(),
            })
            .unwrap();
        assert_eq!(cleared, 1);
        // The draft's send survives; only the addressed owner was forgotten.
        assert_eq!(
            store
                .search_history("", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn trimming_keeps_the_newest_entries() {
        let store = MemoryStore::new();
        for _ in 0..12 {
            store.append_history(&entry()).unwrap();
        }

        // A cap of 10 trims to the low-water mark of 9, so the oldest 3 go.
        assert_eq!(store.trim_history(10).unwrap(), 3);
        assert_eq!(store.len(), 9);

        let mut ids: Vec<i64> = store
            .search_history("", &HistoryFilter::default(), 100)
            .unwrap()
            .iter()
            .map(|hit| hit.id)
            .collect();
        ids.sort();
        assert_eq!(ids, (4..=12).collect::<Vec<_>>());

        // Already within the cap: nothing changes.
        assert_eq!(store.trim_history(100).unwrap(), 0);
        assert_eq!(store.len(), 9);
    }

    #[test]
    fn a_collection_roundtrips_with_a_nested_folder() {
        let store = MemoryStore::new();
        let saved = restbook_core::SavedRequest {
            id: "req-1".to_owned(),
            name: "List things".to_owned(),
            request: RequestDef::new(Method::Get, "https://example.com/things"),
        };
        let collection = Collection {
            id: "col-1".to_owned(),
            name: "Example".to_owned(),
            items: vec![CollectionItem::Folder(restbook_core::Folder {
                id: "folder-1".to_owned(),
                name: "Group".to_owned(),
                items: vec![CollectionItem::Request(saved)],
            })],
        };
        store.save_collection(&collection).unwrap();

        let loaded = store.collections().unwrap();
        assert_eq!(loaded.len(), 1);
        let CollectionItem::Folder(folder) = &loaded[0].items[0] else {
            panic!("expected a folder");
        };
        let CollectionItem::Request(request) = &folder.items[0] else {
            panic!("expected a saved request");
        };
        assert_eq!(request.name, "List things");
        assert_eq!(request.request.url, "https://example.com/things");
    }

    #[test]
    fn saving_a_collection_again_upserts_by_id() {
        let store = MemoryStore::new();
        let mut collection = Collection {
            id: "col-1".to_owned(),
            name: "First".to_owned(),
            items: Vec::new(),
        };
        store.save_collection(&collection).unwrap();
        collection.name = "Renamed".to_owned();
        store.save_collection(&collection).unwrap();

        let loaded = store.collections().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Renamed");

        store.delete_collection("col-1").unwrap();
        assert!(store.collections().unwrap().is_empty());
    }

    #[test]
    fn environments_upsert_by_name_and_track_the_active_one() {
        let store = MemoryStore::new();
        store
            .save_environment(&Environment {
                name: "dev".to_owned(),
                vars: vec![restbook_core::Kv::new("host", "localhost")],
            })
            .unwrap();
        store
            .save_environment(&Environment {
                name: "dev".to_owned(),
                vars: vec![restbook_core::Kv::new("host", "127.0.0.1")],
            })
            .unwrap();

        let environments = store.environments().unwrap();
        assert_eq!(environments.len(), 1);
        assert_eq!(environments[0].vars[0].value, "127.0.0.1");

        assert_eq!(store.active_environment().unwrap(), None);
        store.set_active_environment(Some("dev")).unwrap();
        assert_eq!(store.active_environment().unwrap().as_deref(), Some("dev"));
        store.set_active_environment(None).unwrap();
        assert_eq!(store.active_environment().unwrap(), None);

        store.delete_environment("dev").unwrap();
        assert!(store.environments().unwrap().is_empty());
    }

    #[test]
    fn globals_are_replaced_wholesale() {
        let store = MemoryStore::new();
        store
            .save_globals(&[restbook_core::Kv::new("a", "1")])
            .unwrap();
        assert_eq!(store.globals().unwrap().len(), 1);

        store
            .save_globals(&[
                restbook_core::Kv::new("b", "2"),
                restbook_core::Kv::new("c", "3"),
            ])
            .unwrap();
        let globals = store.globals().unwrap();
        assert_eq!(globals.len(), 2);
        assert_eq!(globals[0].key, "b");
    }

    #[test]
    fn a_draft_roundtrips_and_deletes() {
        let store = MemoryStore::new();
        let mut draft = Draft {
            id: 0,
            request: RequestDef::new(Method::Post, "https://example.com/drafts"),
            updated_at: chrono::Utc::now(),
        };
        let id = store.save_draft(&draft).unwrap();
        assert!(id > 0);

        let drafts = store.drafts().unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].request.url, "https://example.com/drafts");

        // An update goes to the same row.
        draft.id = id;
        draft.request.url = "https://example.com/changed".to_owned();
        assert_eq!(store.save_draft(&draft).unwrap(), id);
        let drafts = store.drafts().unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].request.url, "https://example.com/changed");

        store.delete_draft(id).unwrap();
        assert!(store.drafts().unwrap().is_empty());
        // Deleting an unknown id is not an error.
        store.delete_draft(999).unwrap();
    }
}
