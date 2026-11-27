//! Unencrypted, append-only JSONL history plus a JSON workspace snapshot: the
//! development harness path and the desktop fallback when SQLCipher cannot open.
//!
//! History stays append-only JSONL. Collections, environments, globals, and
//! drafts are low-frequency and edited as a whole, so they live in a single
//! `workspace.json` beside the history file and are rewritten atomically.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use restbook_core::store::{trim_target, Draft};
use restbook_core::{
    Collection, Environment, Error, HistoryEntry, HistoryFilter, HistoryHit, Kv, Origin, Result,
    Store,
};
use serde::{Deserialize, Serialize};

pub struct JsonlStore {
    path: PathBuf,
    workspace_path: PathBuf,
    write_lock: Mutex<()>,
    workspace: Mutex<Workspace>,
}

/// The whole non-history state, persisted as one file. `#[serde(default)]` on
/// every field keeps an older snapshot loadable.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Workspace {
    #[serde(default)]
    collections: Vec<Collection>,
    #[serde(default)]
    environments: Vec<Environment>,
    #[serde(default)]
    active_environment: Option<String>,
    #[serde(default)]
    globals: Vec<Kv>,
    #[serde(default)]
    drafts: Vec<Draft>,
}

impl JsonlStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                crate::secure::create_dir(parent)?;
            }
        }
        crate::secure::open_append(&path)?;

        let workspace_path = workspace_path_for(&path);
        let workspace = read_workspace(&workspace_path)?;

        Ok(Self {
            path,
            workspace_path,
            write_lock: Mutex::new(()),
            workspace: Mutex::new(workspace),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where the collections/environments snapshot lives, beside the history.
    pub fn workspace_path(&self) -> &Path {
        &self.workspace_path
    }

    fn read_all(&self) -> Result<Vec<HistoryEntry>> {
        read_entries(&self.path)
    }

    /// Mutate the snapshot and rewrite it atomically, under the lock that guards
    /// it, so a crash leaves the previous file intact rather than a half-write.
    fn edit_workspace<T>(&self, edit: impl FnOnce(&mut Workspace) -> T) -> Result<T> {
        let mut workspace = self
            .workspace
            .lock()
            .map_err(|_| Error::Store("workspace lock poisoned".to_owned()))?;
        let outcome = edit(&mut workspace);
        write_workspace(&self.workspace_path, &workspace)?;
        Ok(outcome)
    }
}

fn workspace_path_for(history_path: &Path) -> PathBuf {
    match history_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join("workspace.json"),
        _ => PathBuf::from("workspace.json"),
    }
}

fn read_workspace(path: &Path) -> Result<Workspace> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| Error::Store(format!("workspace file: {error}"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Workspace::default()),
        Err(error) => Err(error.into()),
    }
}

fn write_workspace(path: &Path, workspace: &Workspace) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            crate::secure::create_dir(parent)?;
        }
    }

    let bytes = serde_json::to_vec_pretty(workspace)?;
    let temp = temp_path(path);

    let mut file = crate::secure::create(&temp)?;
    file.write_all(&bytes)?;
    file.flush()?;
    drop(file);

    fs::rename(&temp, path)?;
    Ok(())
}

/// Replace the history file with `entries`, atomically. Appending is the normal
/// path; only retention rewrites, and it must not leave a half-written file if
/// it is interrupted.
fn write_history(path: &Path, entries: &[HistoryEntry]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            crate::secure::create_dir(parent)?;
        }
    }

    let temp = temp_path(path);
    let mut file = crate::secure::create(&temp)?;
    for entry in entries {
        writeln!(file, "{}", serde_json::to_string(entry)?)?;
    }
    file.flush()?;
    drop(file);

    fs::rename(&temp, path)?;
    Ok(())
}

fn temp_path(path: &Path) -> PathBuf {
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    PathBuf::from(temp)
}

/// Every entry in a JSONL history file, oldest first. A missing file is an
/// empty history, not an error.
pub fn read_entries(path: impl AsRef<Path>) -> Result<Vec<HistoryEntry>> {
    let file = match File::open(path.as_ref()) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };

    let mut entries = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        entries.push(serde_json::from_str(&line)?);
    }
    Ok(entries)
}

impl Store for JsonlStore {
    fn append_history(&self, entry: &HistoryEntry) -> Result<i64> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let next_id = self
            .read_all()?
            .last()
            .map(|entry| entry.id + 1)
            .unwrap_or(1);

        let mut stored = entry.clone();
        stored.id = next_id;

        let mut file = crate::secure::open_append(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(&stored)?)?;
        file.flush()?;
        Ok(next_id)
    }

    fn search_history(
        &self,
        query: &str,
        filter: &HistoryFilter,
        limit: u32,
    ) -> Result<Vec<HistoryHit>> {
        Ok(crate::matching(&self.read_all()?, query, filter, limit))
    }

    fn get_history(&self, id: i64) -> Result<Option<HistoryEntry>> {
        Ok(self.read_all()?.into_iter().find(|entry| entry.id == id))
    }

    fn history_for_origin(
        &self,
        origin: &Origin,
        filter: &HistoryFilter,
        limit: u32,
    ) -> Result<Option<Vec<HistoryHit>>> {
        Ok(Some(crate::owned_by(
            &self.read_all()?,
            origin,
            filter,
            limit,
        )))
    }

    fn reassign_history_origin(&self, from: &Origin, to: &Origin) -> Result<u64> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let mut entries = self.read_all()?;
        let moved = crate::reassign(&mut entries, from, to);
        if moved > 0 {
            write_history(&self.path, &entries)?;
        }
        Ok(moved)
    }

    fn trim_history(&self, keep: u32) -> Result<u64> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let mut entries = self.read_all()?;
        if entries.len() <= keep as usize {
            return Ok(0);
        }
        let remove = entries.len() - trim_target(keep);
        entries.drain(..remove);
        write_history(&self.path, &entries)?;
        Ok(remove as u64)
    }

    fn delete_history(&self, id: i64) -> Result<bool> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let mut entries = self.read_all()?;
        let before = entries.len();
        entries.retain(|entry| entry.id != id);
        if entries.len() == before {
            return Ok(false);
        }
        write_history(&self.path, &entries)?;
        Ok(true)
    }

    fn clear_history(&self) -> Result<u64> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let entries = self.read_all()?;
        let removed = entries.len() as u64;
        if removed > 0 {
            write_history(&self.path, &[])?;
        }
        Ok(removed)
    }

    fn delete_history_for_origin(&self, origin: &Origin) -> Result<u64> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let mut entries = self.read_all()?;
        let before = entries.len();
        entries.retain(|entry| &entry.origin != origin);
        let removed = (before - entries.len()) as u64;
        if removed > 0 {
            write_history(&self.path, &entries)?;
        }
        Ok(removed)
    }

    fn collections(&self) -> Result<Vec<Collection>> {
        let workspace = self
            .workspace
            .lock()
            .map_err(|_| Error::Store("workspace lock poisoned".to_owned()))?;
        Ok(workspace.collections.clone())
    }

    fn save_collection(&self, collection: &Collection) -> Result<()> {
        self.edit_workspace(|workspace| {
            match workspace
                .collections
                .iter_mut()
                .find(|stored| stored.id == collection.id)
            {
                Some(stored) => *stored = collection.clone(),
                None => workspace.collections.push(collection.clone()),
            }
        })
    }

    fn delete_collection(&self, id: &str) -> Result<()> {
        self.edit_workspace(|workspace| workspace.collections.retain(|stored| stored.id != id))
    }

    fn environments(&self) -> Result<Vec<Environment>> {
        let workspace = self
            .workspace
            .lock()
            .map_err(|_| Error::Store("workspace lock poisoned".to_owned()))?;
        Ok(workspace.environments.clone())
    }

    fn save_environment(&self, environment: &Environment) -> Result<()> {
        self.edit_workspace(|workspace| {
            match workspace
                .environments
                .iter_mut()
                .find(|stored| stored.name == environment.name)
            {
                Some(stored) => *stored = environment.clone(),
                None => workspace.environments.push(environment.clone()),
            }
        })
    }

    fn delete_environment(&self, name: &str) -> Result<()> {
        self.edit_workspace(|workspace| workspace.environments.retain(|stored| stored.name != name))
    }

    fn active_environment(&self) -> Result<Option<String>> {
        let workspace = self
            .workspace
            .lock()
            .map_err(|_| Error::Store("workspace lock poisoned".to_owned()))?;
        Ok(workspace.active_environment.clone())
    }

    fn set_active_environment(&self, name: Option<&str>) -> Result<()> {
        self.edit_workspace(|workspace| {
            workspace.active_environment = name.map(str::to_owned);
        })
    }

    fn globals(&self) -> Result<Vec<Kv>> {
        let workspace = self
            .workspace
            .lock()
            .map_err(|_| Error::Store("workspace lock poisoned".to_owned()))?;
        Ok(workspace.globals.clone())
    }

    fn save_globals(&self, vars: &[Kv]) -> Result<()> {
        self.edit_workspace(|workspace| workspace.globals = vars.to_vec())
    }

    fn drafts(&self) -> Result<Vec<Draft>> {
        let mut drafts = self
            .workspace
            .lock()
            .map_err(|_| Error::Store("workspace lock poisoned".to_owned()))?
            .drafts
            .clone();
        drafts.sort_by_key(|draft| std::cmp::Reverse(draft.updated_at));
        Ok(drafts)
    }

    fn save_draft(&self, draft: &Draft) -> Result<i64> {
        self.edit_workspace(|workspace| {
            let mut stored = draft.clone();
            if stored.id == 0 {
                stored.id = workspace
                    .drafts
                    .iter()
                    .map(|draft| draft.id)
                    .max()
                    .unwrap_or(0)
                    + 1;
            }
            let id = stored.id;
            match workspace
                .drafts
                .iter_mut()
                .find(|existing| existing.id == id)
            {
                Some(existing) => *existing = stored,
                None => workspace.drafts.push(stored),
            }
            id
        })
    }

    fn delete_draft(&self, id: i64) -> Result<()> {
        self.edit_workspace(|workspace| workspace.drafts.retain(|draft| draft.id != id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use restbook_core::{CollectionItem, Folder, Method, RequestDef, SavedRequest};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_collection() -> Collection {
        Collection {
            items: vec![CollectionItem::Folder(Folder {
                id: "folder-1".to_owned(),
                name: "Group".to_owned(),
                items: vec![CollectionItem::Request(SavedRequest {
                    id: "req-1".to_owned(),
                    name: "List".to_owned(),
                    request: RequestDef::new(Method::Get, "https://example.com/things"),
                })],
            })],
            ..Collection::new("col-1", "Example")
        }
    }

    #[test]
    fn trimming_rewrites_the_file_to_the_newest_entries() {
        let dir = temp_dir("restbook-jsonl-trim-test");
        let history = dir.join("history.jsonl");
        let store = JsonlStore::open(&history).unwrap();

        for _ in 0..20 {
            store.append_history(&sample_entry()).unwrap();
        }

        // Cap 10 trims to 9, so the oldest 11 are dropped and the file is
        // rewritten rather than left append-only.
        assert_eq!(store.trim_history(10).unwrap(), 11);
        assert_eq!(
            store
                .search_history("", &HistoryFilter::default(), 100)
                .unwrap()
                .len(),
            9
        );

        // The rewrite is what survives a reopen.
        let reopened = JsonlStore::open(&history).unwrap();
        assert_eq!(
            reopened
                .search_history("", &HistoryFilter::default(), 100)
                .unwrap()
                .len(),
            9
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn filters_apply_to_the_file_and_survive_a_reopen() {
        let dir = temp_dir("restbook-jsonl-filter-test");
        let history = dir.join("history.jsonl");

        let doomed;
        {
            let store = JsonlStore::open(&history).unwrap();
            let mut ids = Vec::new();
            for (method, status) in [(Method::Get, 200), (Method::Post, 404), (Method::Get, 500)] {
                let mut item = sample_entry();
                item.request.method = method;
                item.response.status = status;
                ids.push(store.append_history(&item).unwrap());
            }
            doomed = ids[2];
            store.delete_history(doomed).unwrap();
        }

        let reopened = JsonlStore::open(&history).unwrap();
        let get = HistoryFilter {
            methods: vec!["GET".to_owned()],
            ..Default::default()
        };
        let rows = reopened.search_history("", &get, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, 200);

        let all = reopened
            .search_history("", &HistoryFilter::default(), 10)
            .unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|hit| hit.id != doomed));

        let _ = fs::remove_dir_all(&dir);
    }

    fn sample_entry() -> HistoryEntry {
        let request = RequestDef::new(Method::Get, "https://example.com/things");
        HistoryEntry {
            id: 0,
            url: request.url.clone(),
            request,
            response: restbook_core::ResponseRecord {
                status: 200,
                headers: Vec::new(),
                body: Vec::new().into(),
                duration_ms: 1,
                received_at: chrono::Utc::now(),
                final_url: None,
                redirects: Vec::new(),
            },
            origin: Origin::default(),
        }
    }

    #[test]
    fn origins_survive_a_reopen_and_can_be_moved() {
        let dir = temp_dir("restbook-jsonl-origin-test");
        let history = dir.join("history.jsonl");

        {
            let store = JsonlStore::open(&history).unwrap();
            let mut entry = sample_entry();
            entry.origin = Origin::Draft { draft: 3 };
            store.append_history(&entry).unwrap();

            let from = Origin::Draft { draft: 3 };
            let to = Origin::Saved {
                node: "node-9".to_owned(),
            };
            assert_eq!(store.reassign_history_origin(&from, &to).unwrap(), 1);
        }

        // The rewrite, not just the in-memory view, carries the new owner.
        let reopened = JsonlStore::open(&history).unwrap();
        let rows = reopened
            .history_for_origin(
                &Origin::Saved {
                    node: "node-9".to_owned(),
                },
                &HistoryFilter::default(),
                10,
            )
            .unwrap()
            .unwrap();
        assert_eq!(rows.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_and_clearing_rewrite_the_file() {
        let dir = temp_dir("restbook-jsonl-delete-test");
        let history = dir.join("history.jsonl");

        let doomed;
        {
            let store = JsonlStore::open(&history).unwrap();
            let kept = store.append_history(&sample_entry()).unwrap();
            doomed = store.append_history(&sample_entry()).unwrap();
            let mut owned = sample_entry();
            owned.origin = Origin::Saved {
                node: "node-a".to_owned(),
            };
            store.append_history(&owned).unwrap();
            let _ = kept;

            assert!(store.delete_history(doomed).unwrap());
            assert!(!store.delete_history(doomed).unwrap());
        }

        // The removal is on disk, not just in the in-memory view.
        let reopened = JsonlStore::open(&history).unwrap();
        assert_eq!(
            reopened
                .search_history("", &HistoryFilter::default(), 100)
                .unwrap()
                .len(),
            2
        );

        assert_eq!(
            reopened
                .delete_history_for_origin(&Origin::Saved {
                    node: "node-a".to_owned(),
                })
                .unwrap(),
            1
        );
        assert_eq!(reopened.clear_history().unwrap(), 1);

        let emptied = JsonlStore::open(&history).unwrap();
        assert!(emptied
            .search_history("", &HistoryFilter::default(), 100)
            .unwrap()
            .is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_persists_across_reopen() {
        let dir = temp_dir("restbook-jsonl-workspace-test");
        let history = dir.join("history.jsonl");

        {
            let store = JsonlStore::open(&history).unwrap();
            store.save_collection(&sample_collection()).unwrap();
            store
                .save_environment(&Environment {
                    name: "dev".to_owned(),
                    vars: vec![Kv::new("host", "localhost")],
                })
                .unwrap();
            store.set_active_environment(Some("dev")).unwrap();
            store.save_globals(&[Kv::new("token", "secret")]).unwrap();

            // The snapshot lives beside the history file, not inside it.
            assert_eq!(store.workspace_path(), dir.join("workspace.json"));
        }

        let reopened = JsonlStore::open(&history).unwrap();
        let collections = reopened.collections().unwrap();
        assert_eq!(collections.len(), 1);
        assert_eq!(collections[0].name, "Example");
        assert!(matches!(
            &collections[0].items[0],
            CollectionItem::Folder(folder) if folder.name == "Group"
        ));
        assert_eq!(reopened.environments().unwrap().len(), 1);
        assert_eq!(
            reopened.active_environment().unwrap().as_deref(),
            Some("dev")
        );
        assert_eq!(reopened.globals().unwrap()[0].key, "token");

        // History is untouched by the workspace file.
        assert!(reopened
            .search_history("", &HistoryFilter::default(), 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn drafts_roundtrip_and_delete_across_reopen() {
        let dir = temp_dir("restbook-jsonl-drafts-test");
        let history = dir.join("history.jsonl");

        let id = {
            let store = JsonlStore::open(&history).unwrap();
            let id = store
                .save_draft(&Draft {
                    id: 0,
                    request: RequestDef::new(Method::Post, "https://example.com/drafts"),
                    updated_at: chrono::Utc::now(),
                })
                .unwrap();
            assert!(id > 0);
            id
        };

        let reopened = JsonlStore::open(&history).unwrap();
        let drafts = reopened.drafts().unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].request.method, Method::Post);
        assert_eq!(drafts[0].request.url, "https://example.com/drafts");

        reopened.delete_draft(id).unwrap();
        assert!(reopened.drafts().unwrap().is_empty());
    }

    /// History holds response bodies and `workspace.json` holds drafts and
    /// secrets, so both are `0600` where the platform has a mode.
    #[cfg(unix)]
    #[test]
    fn the_history_and_snapshot_are_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = temp_dir("restbook-jsonl-private-test");
        let history = dir.join("history.jsonl");
        let store = JsonlStore::open(&history).unwrap();
        store.append_history(&sample_entry()).unwrap();
        store
            .save_globals(&[restbook_core::Kv::new("token", "x")])
            .unwrap();

        for path in [history, store.workspace_path().to_path_buf()] {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{} should be private", path.display());
        }
    }
}
