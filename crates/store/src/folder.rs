//! A Git-friendly workspace on disk: one `collection.json` per collection under
//! `collections/<slug>/`, environments under `environments/`, gitignored local
//! state in `restbook.local.json`, and history in `.restbook/history.jsonl`.
//!
//! Saves are VCS-stable: keys are sorted, secrets become placeholders (real
//! values stay local), and every write is temp-file plus rename.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use restbook_core::secrets::{self, SecretSlot, REDACTED};
use restbook_core::store::Draft;
use restbook_core::{
    Collection, Environment, Error, HistoryEntry, HistoryHit, Kv, Origin, Result, Store,
};
use serde::{Deserialize, Serialize};

use crate::jsonl::JsonlStore;

/// The marker file at a workspace root. Its presence is what makes a directory
/// a workspace rather than just a directory.
pub const MARKER_FILE: &str = "restbook.json";

/// Where collections and environments live, relative to the root.
const COLLECTIONS_DIR: &str = "collections";
const ENVIRONMENTS_DIR: &str = "environments";

/// Local-only state, expected to be gitignored.
const LOCAL_FILE: &str = "restbook.local.json";

/// The per-workspace, gitignored state directory.
const STATE_DIR: &str = ".restbook";
const HISTORY_FILE: &str = "history.jsonl";

/// The file name every collection is stored under, inside its own directory.
/// A directory rather than `collections/foo.json` so a future format can put a
/// request-per-file layout beside `collection.json` without moving anything.
const COLLECTION_FILE: &str = "collection.json";

/// A workspace's `restbook.json`. Deliberately tiny: it carries the format
/// version and nothing that changes on every edit, so it is not a merge
/// hot-spot.
#[derive(Debug, Serialize, Deserialize)]
struct Marker {
    /// Bumped only for a breaking layout change, so an older app can refuse
    /// rather than silently rewrite in the wrong shape.
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

const FORMAT_VERSION: u32 = 1;

/// The gitignored half of a workspace: things that are true for *this* machine
/// or *this* person, not for everyone who clones it.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Local {
    #[serde(default)]
    active_environment: Option<String>,
    /// Workspace-global variables. Kept local because a global is usually a
    /// token or a host, and either way it is the one scope with no name to
    /// hint that it is safe to share.
    #[serde(default)]
    globals: Vec<Kv>,
    /// Unsaved requests. Local for the same reason as globals: a draft may
    /// hold a token the user pasted in and has not decided to save yet.
    #[serde(default)]
    drafts: Vec<Draft>,
    /// The values behind secrets that appear in tracked files: a tracked file
    /// keeps the row (so `{{TOKEN}}` still resolves and the shape is diffable)
    /// but writes a placeholder in place of the value, and the real value lives
    /// here and is spliced back in when the file is read. Keys are
    /// `"<kind>:<owner>:<row>"`, e.g. `env:dev:TOKEN`. See `secret_key`.
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

impl Local {
    /// Sort the variable rows by key so a re-save is byte-identical. Only the
    /// order changes; duplicates are left alone, because the resolve rules
    /// already decide which one wins and reordering must not change that.
    fn normalize(&mut self) {
        self.globals.sort_by(|a, b| a.key.cmp(&b.key));
    }
}

pub struct FolderStore {
    root: PathBuf,
    state: PathBuf,
    local: Mutex<Local>,
    /// History is not part of the repository: it holds every response body,
    /// which is exactly the data nobody wants in a shared checkout. It is
    /// served by the same JSONL store the harness uses, under `.restbook/`.
    history: JsonlStore,
    /// Serializes the read-modify-write of `restbook.local.json`.
    local_lock: Mutex<()>,
}

impl FolderStore {
    /// Open (or create) a workspace at `root`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join(COLLECTIONS_DIR))?;
        fs::create_dir_all(root.join(ENVIRONMENTS_DIR))?;

        let state = root.join(STATE_DIR);
        fs::create_dir_all(&state)?;

        // `JsonlStore` renames sources into place and is happy with an empty
        // directory; history is plaintext here on purpose, because the
        // workspace root is the user's own checkout, not the app's data dir.
        let history = JsonlStore::open(state.join(HISTORY_FILE))?;

        let local_path = root.join(LOCAL_FILE);
        let local = read_local(&local_path)?;

        write_marker(&root, FORMAT_VERSION)?;
        write_gitignore(&root)?;

        Ok(Self {
            root,
            state,
            local: Mutex::new(local),
            history,
            local_lock: Mutex::new(()),
        })
    }

    /// The directory that holds the workspace.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where collections are read from and written to.
    pub fn collections_dir(&self) -> PathBuf {
        self.root.join(COLLECTIONS_DIR)
    }

    /// Whether `dir` (or any ancestor up to the filesystem root) is a
    /// workspace. This is what makes "open this folder" work from a nested
    /// directory, the way `.git` does.
    pub fn find_root(start: impl AsRef<Path>) -> Option<PathBuf> {
        let start = start.as_ref();
        let mut dir = if start.is_file() {
            start.parent()?.to_path_buf()
        } else {
            start.to_path_buf()
        };
        loop {
            if dir.join(MARKER_FILE).is_file() {
                return Some(dir);
            }
            if !dir.pop() {
                return None;
            }
        }
    }

    fn collection_dir(&self, collection: &Collection) -> PathBuf {
        self.collections_dir()
            .join(slug(&collection.name, &collection.id))
    }

    /// The slug a collection already has on disk, if it has one.
    ///
    /// A rename must not lose the directory, so the directory is found by the
    /// id recorded inside each `collection.json` rather than by re-deriving the
    /// slug from the name. Deriving it would orphan the old directory whenever
    /// a name changed, and two renames would leave two directories.
    fn existing_collection_dir(&self, id: &str) -> Result<Option<PathBuf>> {
        for dir in self.collection_dirs()? {
            let path = dir.join(COLLECTION_FILE);
            let Ok(bytes) = fs::read(&path) else { continue };
            let Ok(collection) = serde_json::from_slice::<Collection>(&bytes) else {
                continue;
            };
            if collection.id == id {
                return Ok(Some(dir));
            }
        }
        Ok(None)
    }

    fn collection_dirs(&self) -> Result<Vec<PathBuf>> {
        let dir = self.collections_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.path())
            .collect();
        dirs.sort();
        Ok(dirs)
    }

    fn environment_path(&self, name: &str) -> PathBuf {
        self.root
            .join(ENVIRONMENTS_DIR)
            .join(format!("{}.json", slug(name, name)))
    }

    fn edit_local<T>(&self, edit: impl FnOnce(&mut Local) -> T) -> Result<T> {
        let _guard = self
            .local_lock
            .lock()
            .map_err(|_| Error::Store("local workspace lock poisoned".to_owned()))?;
        let mut local = self
            .local
            .lock()
            .map_err(|_| Error::Store("local workspace lock poisoned".to_owned()))?;
        let outcome = edit(&mut local);
        local.normalize();
        write_json(&self.root.join(LOCAL_FILE), &*local)?;
        Ok(outcome)
    }
}

impl Store for FolderStore {
    fn append_history(&self, entry: &HistoryEntry) -> Result<i64> {
        self.history.append_history(entry)
    }

    fn search_history(&self, query: &str, limit: u32) -> Result<Vec<HistoryHit>> {
        self.history.search_history(query, limit)
    }

    fn get_history(&self, id: i64) -> Result<Option<HistoryEntry>> {
        self.history.get_history(id)
    }

    fn trim_history(&self, keep: u32) -> Result<u64> {
        self.history.trim_history(keep)
    }

    fn delete_history(&self, id: i64) -> Result<bool> {
        self.history.delete_history(id)
    }

    fn clear_history(&self) -> Result<u64> {
        self.history.clear_history()
    }

    fn delete_history_for_origin(&self, origin: &Origin) -> Result<u64> {
        self.history.delete_history_for_origin(origin)
    }

    fn history_for_origin(&self, origin: &Origin, limit: u32) -> Result<Option<Vec<HistoryHit>>> {
        self.history.history_for_origin(origin, limit)
    }

    fn reassign_history_origin(&self, from: &Origin, to: &Origin) -> Result<u64> {
        self.history.reassign_history_origin(from, to)
    }

    fn collections(&self) -> Result<Vec<Collection>> {
        let mut collections = Vec::new();
        for dir in self.collection_dirs()? {
            let path = dir.join(COLLECTION_FILE);
            // A missing file is a directory that is not a collection (a stray
            // folder a user made); a parse failure is a hand-edit gone wrong.
            // Neither should take the whole workspace down, so both are named
            // and skipped rather than propagated.
            let Ok(bytes) = fs::read(&path) else { continue };
            match serde_json::from_slice::<Collection>(&bytes) {
                Ok(mut collection) => {
                    // A tracked file holds placeholders where a credential was;
                    // splice this machine's real values back before the
                    // collection leaves the store.
                    let local = self
                        .local
                        .lock()
                        .map_err(|_| Error::Store("local workspace lock poisoned".to_owned()))?;
                    secrets::visit_collection_secrets(&mut collection, &mut |slot| {
                        restore_from(slot, &local.secrets)
                    });
                    collections.push(collection);
                }
                Err(error) => {
                    log::warn!("skipping unreadable collection {}: {error}", path.display())
                }
            }
        }
        // Directory order is the on-disk order, which the filesystem does not
        // promise to keep stable; the name is what a human expects.
        collections.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(collections)
    }

    fn save_collection(&self, collection: &Collection) -> Result<()> {
        // A rename moves the directory so the layout keeps matching the names
        // a person reads in a file browser, and the id inside the file is what
        // makes that safe to do.
        let dir = match self.existing_collection_dir(&collection.id)? {
            Some(existing) => {
                let wanted = self.collection_dir(collection);
                if existing != wanted {
                    fs::create_dir_all(&wanted)?;
                    fs::rename(&existing, &wanted)?;
                }
                wanted
            }
            None => {
                let wanted = self.collection_dir(collection);
                fs::create_dir_all(&wanted)?;
                wanted
            }
        };
        // A tracked file is read by everyone who clones the workspace, so the
        // credential is taken out and the placeholder is left in its place. The
        // real value goes to `restbook.local.json`, which `.gitignore` excludes.
        let mut redacted = collection.clone();
        let _guard = self
            .local_lock
            .lock()
            .map_err(|_| Error::Store("local workspace lock poisoned".to_owned()))?;
        {
            let mut local = self
                .local
                .lock()
                .map_err(|_| Error::Store("local workspace lock poisoned".to_owned()))?;
            secrets::visit_collection_secrets(&mut redacted, &mut |slot| {
                redact_into(slot, &mut local.secrets)
            });
            local.normalize();
            write_json(&self.root.join(LOCAL_FILE), &*local)?;
        }
        write_json(&dir.join(COLLECTION_FILE), &redacted)
    }

    fn delete_collection(&self, id: &str) -> Result<()> {
        if let Some(dir) = self.existing_collection_dir(id)? {
            // Remove the whole directory: it exists only for this collection.
            match fs::remove_dir_all(&dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn environments(&self) -> Result<Vec<Environment>> {
        let dir = self.root.join(ENVIRONMENTS_DIR);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };

        let mut environments = Vec::new();
        for entry in entries.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            // A read failure is the workspace's problem (permissions, a
            // vanished file) and is reported; a *parse* failure is one
            // hand-edited file's problem and is skipped, so one bad environment
            // does not hide all the others.
            let bytes = fs::read(&path)?;
            match serde_json::from_slice::<Environment>(&bytes) {
                Ok(mut environment) => {
                    let local = self
                        .local
                        .lock()
                        .map_err(|_| Error::Store("local workspace lock poisoned".to_owned()))?;
                    secrets::visit_environment_secrets(&mut environment, &mut |slot| {
                        restore_from(slot, &local.secrets)
                    });
                    environments.push(environment);
                }
                Err(error) => log::warn!(
                    "skipping unreadable environment {}: {error}",
                    path.display()
                ),
            }
        }
        environments.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(environments)
    }

    fn save_environment(&self, environment: &Environment) -> Result<()> {
        // An environment is keyed by name (`{{var}}` resolves against it), so a
        // rename is a delete plus an add and needs no id lookup. The stale file
        // is removed by matching the `name` *inside* each file, not the
        // filename, for the same reason collections use ids.
        let wanted = self.environment_path(&environment.name);
        for existing in self.environment_files()? {
            if existing == wanted {
                continue;
            }
            let Ok(bytes) = fs::read(&existing) else {
                continue;
            };
            let Ok(stored) = serde_json::from_slice::<Environment>(&bytes) else {
                continue;
            };
            if stored.name == environment.name {
                let _ = fs::remove_file(&existing);
            }
        }
        fs::create_dir_all(self.root.join(ENVIRONMENTS_DIR))?;
        // Same rule as a collection: the tracked file keeps the row, so
        // `{{TOKEN}}` still resolves and the variable list still diffs, but a
        // `secret` row ships as a placeholder and the value stays local.
        let mut redacted = environment.clone();
        let _guard = self
            .local_lock
            .lock()
            .map_err(|_| Error::Store("local workspace lock poisoned".to_owned()))?;
        {
            let mut local = self
                .local
                .lock()
                .map_err(|_| Error::Store("local workspace lock poisoned".to_owned()))?;
            secrets::visit_environment_secrets(&mut redacted, &mut |slot| {
                redact_into(slot, &mut local.secrets)
            });
            local.normalize();
            write_json(&self.root.join(LOCAL_FILE), &*local)?;
        }
        write_json(&wanted, &redacted)
    }

    fn delete_environment(&self, name: &str) -> Result<()> {
        for existing in self.environment_files()? {
            let Ok(bytes) = fs::read(&existing) else {
                continue;
            };
            let Ok(stored) = serde_json::from_slice::<Environment>(&bytes) else {
                continue;
            };
            if stored.name == name {
                match fs::remove_file(&existing) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(())
    }

    fn active_environment(&self) -> Result<Option<String>> {
        let local = self
            .local
            .lock()
            .map_err(|_| Error::Store("local workspace lock poisoned".to_owned()))?;
        Ok(local.active_environment.clone())
    }

    fn set_active_environment(&self, name: Option<&str>) -> Result<()> {
        self.edit_local(|local| {
            local.active_environment = name.map(str::to_owned);
        })
    }

    fn globals(&self) -> Result<Vec<Kv>> {
        let local = self
            .local
            .lock()
            .map_err(|_| Error::Store("local workspace lock poisoned".to_owned()))?;
        Ok(local.globals.clone())
    }

    fn save_globals(&self, vars: &[Kv]) -> Result<()> {
        self.edit_local(|local| local.globals = vars.to_vec())
    }

    fn drafts(&self) -> Result<Vec<Draft>> {
        let mut drafts = self
            .local
            .lock()
            .map_err(|_| Error::Store("local workspace lock poisoned".to_owned()))?
            .drafts
            .clone();
        drafts.sort_by_key(|draft| std::cmp::Reverse(draft.updated_at));
        Ok(drafts)
    }

    fn save_draft(&self, draft: &Draft) -> Result<i64> {
        self.edit_local(|local| {
            let mut stored = draft.clone();
            if stored.id == 0 {
                stored.id = local.drafts.iter().map(|draft| draft.id).max().unwrap_or(0) + 1;
            }
            let id = stored.id;
            match local.drafts.iter_mut().find(|existing| existing.id == id) {
                Some(existing) => *existing = stored,
                None => local.drafts.push(stored),
            }
            id
        })
    }

    fn delete_draft(&self, id: i64) -> Result<()> {
        self.edit_local(|local| local.drafts.retain(|draft| draft.id != id))
    }
}

impl FolderStore {
    fn environment_files(&self) -> Result<Vec<PathBuf>> {
        let dir = self.root.join(ENVIRONMENTS_DIR);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .collect();
        files.sort();
        Ok(files)
    }

    /// The gitignored state directory, for a caller that wants to show it.
    pub fn state_dir(&self) -> &Path {
        &self.state
    }
}

/// A filename-safe form of `name`, with the first 8 characters of `id` appended
/// so two collections sharing a name get distinct directories.
///
/// The suffix is always present, even for a unique name: that is the price of
/// never having to move a directory when a different collection is renamed in.
fn slug(name: &str, id: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true; // start true so a leading separator is dropped
    for ch in name.chars() {
        // Every character is lowercased, including non-ASCII: Windows and macOS
        // compare filenames case-insensitively, so keeping `Ünïcode` distinct
        // from `ünïcode` would let two collections collide on those systems and
        // not on Linux.
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch.is_alphanumeric() {
            // Keep non-ASCII letters and digits; the filesystem takes them and
            // a reader recognises them.
            ch.to_lowercase().next()
        } else if !last_dash {
            Some('-')
        } else {
            None
        };
        match mapped {
            Some(ch) => {
                last_dash = ch == '-';
                out.push(ch);
            }
            None => last_dash = true,
        }
    }
    let trimmed = out.trim_matches('-');
    let stem = if trimmed.is_empty() {
        "untitled"
    } else {
        trimmed
    };
    let suffix: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    if suffix.is_empty() {
        stem.to_owned()
    } else {
        format!("{stem}-{suffix}")
    }
}

fn read_local(path: &Path) -> Result<Local> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| Error::Store(format!("local workspace file: {error}"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Local::default()),
        Err(error) => Err(error.into()),
    }
}

/// The `Local::secrets` key for one secret value.
///
/// The tuple is only used to look a value back up, so it must stay stable
/// across a rename of the workspace path — hence a collection id, an
/// environment name, or a row's key rather than a path.
fn secret_key(kind: &str, owner: &str, row: &str) -> String {
    format!("{kind}:{owner}:{row}")
}

/// Take one credential out of the tracked file and into `secrets`.
fn redact_into(slot: SecretSlot<'_>, secrets: &mut BTreeMap<String, String>) {
    let value = std::mem::take(slot.value);
    secrets.insert(secret_key(slot.kind, slot.owner, slot.row), value);
    *slot.value = REDACTED.to_owned();
}

/// The inverse of [`redact_into`]: put back any value this machine knows for a
/// slot the tracked file left as [`REDACTED`].
fn restore_from(slot: SecretSlot<'_>, secrets: &BTreeMap<String, String>) {
    if *slot.value != REDACTED {
        return;
    }
    if let Some(value) = secrets.get(&secret_key(slot.kind, slot.owner, slot.row)) {
        *slot.value = value.clone();
    }
}

fn write_marker(root: &Path, version: u32) -> Result<()> {
    let path = root.join(MARKER_FILE);
    if path.exists() {
        return Ok(());
    }
    write_json(
        &path,
        &Marker {
            version,
            name: None,
        },
    )
}

/// A `.gitignore` that keeps the secrets and the response bodies out of a
/// repository. Written only when absent, so a user who edits it keeps their
/// edit.
fn write_gitignore(root: &Path) -> Result<()> {
    let path = root.join(".gitignore");
    if path.exists() {
        return Ok(());
    }
    let body = "\
# Written by restbook. Everything here is local to this machine.
# The active environment and the global variables can hold secrets.
restbook.local.json
# Every send, with its response body.
.restbook/

# Build output, in case a workspace is its own repository.
target/
";
    let mut file = File::create(&path)?;
    file.write_all(body.as_bytes())?;
    file.flush()?;
    Ok(())
}

/// Write JSON the way a person and a VCS want to read it: keys sorted, two-space
/// indent, one trailing newline. Sorting makes even *hand-edited* files stable;
/// a `struct` alone is already deterministic, but going through
/// `serde_json::Value` costs one allocation on a path that runs at most once per
/// save.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let value = serde_json::to_value(value)?;
    let sorted = sort_keys(value);
    let mut bytes = serde_json::to_vec_pretty(&sorted)?;
    bytes.push(b'\n');

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    // Atomic: a crash mid-write leaves the previous file, not a truncated one.
    let temp = temp_path(path);
    let mut file = File::create(&temp)?;
    file.write_all(&bytes)?;
    file.flush()?;
    drop(file);
    fs::rename(&temp, path)?;
    Ok(())
}

/// Recursively sort object keys so serialization is deterministic.
fn sort_keys(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<String, serde_json::Value> = map
                .into_iter()
                .map(|(key, value)| (key, sort_keys(value)))
                .collect();
            // `serde_json` preserves insertion order only with the
            // `preserve_order` feature; without it a `Map` is already a
            // `BTreeMap`, and building from one makes the order explicit either
            // way. Serializing the map itself (not a `Value`) collects it back
            // into the same type, so this is a sort, not a re-shape.
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sort_keys).collect())
        }
        other => other,
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    PathBuf::from(temp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use restbook_core::{CollectionItem, Folder, Method, RequestDef, SavedRequest};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("restbook-folder-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_collection(id: &str, name: &str) -> Collection {
        Collection {
            id: id.to_owned(),
            name: name.to_owned(),
            items: vec![CollectionItem::Folder(Folder {
                id: "folder-1".to_owned(),
                name: "Group".to_owned(),
                items: vec![CollectionItem::Request(SavedRequest {
                    id: "req-1".to_owned(),
                    name: "List things".to_owned(),
                    request: RequestDef::new(Method::Get, "https://example.com/things"),
                })],
            })],
        }
    }

    #[test]
    fn a_workspace_roundtrips_collections_through_files() {
        let dir = temp_dir("roundtrip");
        {
            let store = FolderStore::open(&dir).unwrap();
            store
                .save_collection(&sample_collection("col-1", "Example"))
                .unwrap();
        }

        // The file is where a human would look for it.
        let file = dir
            .join(COLLECTIONS_DIR)
            .join("example-col1")
            .join(COLLECTION_FILE);
        assert!(file.is_file(), "expected {}", file.display());

        let reopened = FolderStore::open(&dir).unwrap();
        let collections = reopened.collections().unwrap();
        assert_eq!(collections.len(), 1);
        assert_eq!(collections[0].name, "Example");
        let CollectionItem::Folder(folder) = &collections[0].items[0] else {
            panic!("expected a folder");
        };
        let CollectionItem::Request(request) = &folder.items[0] else {
            panic!("expected a saved request");
        };
        assert_eq!(request.request.url, "https://example.com/things");
    }

    #[test]
    fn a_marker_and_gitignore_are_written_once() {
        let dir = temp_dir("marker");
        let _store = FolderStore::open(&dir).unwrap();
        assert!(dir.join(MARKER_FILE).is_file());
        let gitignore = fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(gitignore.contains(LOCAL_FILE));
        assert!(gitignore.contains(".restbook/"));

        // A user edit to .gitignore survives a reopen.
        fs::write(dir.join(".gitignore"), "custom\n").unwrap();
        let _store = FolderStore::open(&dir).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join(".gitignore")).unwrap(),
            "custom\n"
        );
    }

    #[test]
    fn saving_twice_produces_identical_bytes() {
        let dir = temp_dir("stable");
        let store = FolderStore::open(&dir).unwrap();
        let collection = sample_collection("col-1", "Example");
        store.save_collection(&collection).unwrap();
        let file = dir
            .join(COLLECTIONS_DIR)
            .join("example-col1")
            .join(COLLECTION_FILE);
        let first = fs::read(&file).unwrap();

        store.save_collection(&collection).unwrap();
        let second = fs::read(&file).unwrap();

        assert_eq!(first, second, "a no-op save must not change the file");
        assert!(first.ends_with(b"\n"), "the file ends in a newline");
    }

    #[test]
    fn a_rename_moves_the_directory_and_keeps_the_id() {
        let dir = temp_dir("rename");
        let store = FolderStore::open(&dir).unwrap();
        let mut collection = sample_collection("col-1", "Example");
        store.save_collection(&collection).unwrap();

        collection.name = "Renamed".to_owned();
        store.save_collection(&collection).unwrap();

        // One directory, under the new name, and no orphan left behind.
        let dirs: Vec<_> = fs::read_dir(dir.join(COLLECTIONS_DIR))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(dirs, vec!["renamed-col1".to_owned()]);

        let collections = store.collections().unwrap();
        assert_eq!(collections.len(), 1);
        assert_eq!(collections[0].id, "col-1");
    }

    #[test]
    fn two_collections_may_share_a_name() {
        let dir = temp_dir("collision");
        let store = FolderStore::open(&dir).unwrap();
        store
            .save_collection(&sample_collection("aaaa1111", "API"))
            .unwrap();
        store
            .save_collection(&sample_collection("bbbb2222", "API"))
            .unwrap();

        let names = store.collections().unwrap();
        assert_eq!(names.len(), 2, "both survive");

        let dirs = fs::read_dir(dir.join(COLLECTIONS_DIR)).unwrap().count();
        assert_eq!(dirs, 2, "and they do not overwrite each other");
    }

    #[test]
    fn deleting_a_collection_removes_its_directory() {
        let dir = temp_dir("delete");
        let store = FolderStore::open(&dir).unwrap();
        store
            .save_collection(&sample_collection("col-1", "Example"))
            .unwrap();
        store.delete_collection("col-1").unwrap();

        assert!(store.collections().unwrap().is_empty());
        assert_eq!(fs::read_dir(dir.join(COLLECTIONS_DIR)).unwrap().count(), 0);

        // Deleting an unknown id is not an error.
        store.delete_collection("missing").unwrap();
    }

    #[test]
    fn environments_roundtrip_and_rename_without_leaving_a_stale_file() {
        let dir = temp_dir("env");
        let store = FolderStore::open(&dir).unwrap();
        store
            .save_environment(&Environment {
                name: "dev".to_owned(),
                vars: vec![Kv::new("host", "localhost")],
            })
            .unwrap();

        assert!(dir.join(ENVIRONMENTS_DIR).join("dev-dev.json").is_file());

        // A rename is a delete plus an add: the old file must go.
        store
            .save_environment(&Environment {
                name: "stage".to_owned(),
                vars: vec![Kv::new("host", "staging")],
            })
            .unwrap();
        let files = fs::read_dir(dir.join(ENVIRONMENTS_DIR)).unwrap().count();
        assert_eq!(files, 2, "dev and stage are different environments");

        // Renaming dev to dev replaces in place rather than adding a third.
        store
            .save_environment(&Environment {
                name: "dev".to_owned(),
                vars: vec![Kv::new("host", "127.0.0.1")],
            })
            .unwrap();
        assert_eq!(fs::read_dir(dir.join(ENVIRONMENTS_DIR)).unwrap().count(), 2);

        let environments = store.environments().unwrap();
        let dev = environments.iter().find(|e| e.name == "dev").unwrap();
        assert_eq!(dev.vars[0].value, "127.0.0.1");

        store.delete_environment("dev").unwrap();
        assert_eq!(store.environments().unwrap().len(), 1);
    }

    #[test]
    fn local_state_is_kept_out_of_the_tracked_files() {
        let dir = temp_dir("local");
        {
            let store = FolderStore::open(&dir).unwrap();
            store.set_active_environment(Some("dev")).unwrap();
            store.save_globals(&[Kv::new("token", "secret")]).unwrap();
        }

        // It lives in restbook.local.json, which .gitignore names.
        let local = fs::read_to_string(dir.join(LOCAL_FILE)).unwrap();
        assert!(local.contains("secret"));

        let reopened = FolderStore::open(&dir).unwrap();
        assert_eq!(
            reopened.active_environment().unwrap().as_deref(),
            Some("dev")
        );
        assert_eq!(reopened.globals().unwrap()[0].key, "token");

        reopened.set_active_environment(None).unwrap();
        assert_eq!(reopened.active_environment().unwrap(), None);
    }

    #[test]
    fn history_lives_under_the_gitignored_state_directory() {
        let dir = temp_dir("history");
        let store = FolderStore::open(&dir).unwrap();
        assert_eq!(store.state_dir(), dir.join(STATE_DIR));

        let entry = HistoryEntry {
            id: 0,
            request: RequestDef::new(Method::Get, "https://example.com"),
            response: restbook_core::ResponseRecord {
                status: 200,
                headers: Vec::new(),
                body: b"{}".to_vec().into(),
                duration_ms: 1,
                received_at: chrono::Utc::now(),
                final_url: None,
                redirects: Vec::new(),
            },
            url: "https://example.com".to_owned(),
            origin: Origin::default(),
        };
        store.append_history(&entry).unwrap();

        let hits = store.search_history("", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(dir.join(STATE_DIR).join(HISTORY_FILE).is_file());
        // The history file is not at the root, so it cannot be committed by
        // accident.
        assert!(!dir.join(HISTORY_FILE).exists());
    }

    #[test]
    fn drafts_live_in_the_local_file_and_roundtrip() {
        let dir = temp_dir("drafts");
        let id = {
            let store = FolderStore::open(&dir).unwrap();
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

        // Drafts are local state, so they land in the gitignored file.
        let local = fs::read_to_string(dir.join(LOCAL_FILE)).unwrap();
        assert!(local.contains("https://example.com/drafts"));

        let reopened = FolderStore::open(&dir).unwrap();
        let drafts = reopened.drafts().unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].request.method, Method::Post);

        reopened.delete_draft(id).unwrap();
        assert!(reopened.drafts().unwrap().is_empty());
    }

    #[test]
    fn find_root_walks_up_from_a_nested_directory() {
        let dir = temp_dir("findroot");
        let store = FolderStore::open(&dir).unwrap();
        let nested = dir.join("collections").join("example-col1");
        fs::create_dir_all(&nested).unwrap();

        let found = FolderStore::find_root(&nested).unwrap();
        assert_eq!(found.canonicalize().unwrap(), dir.canonicalize().unwrap());

        // A path that does not exist yet still resolves against the workspace
        // above it: `find_root` reads the string, not the filesystem, for the
        // starting point. That is what makes "open this folder" work for a
        // directory the app is about to create.
        let missing = FolderStore::find_root(dir.join("nowhere")).unwrap();
        assert_eq!(missing.canonicalize().unwrap(), dir.canonicalize().unwrap());

        // Above the workspace there is nothing, so a path outside it is not a
        // workspace root.
        let outside = temp_dir("findroot-outside");
        assert!(FolderStore::find_root(&outside).is_none());

        drop(store);
    }

    #[test]
    fn slugs_are_filename_safe_and_never_empty() {
        assert_eq!(slug("Example", "col-1"), "example-col1");
        assert_eq!(slug("My API v2", "id"), "my-api-v2-id");
        assert_eq!(slug("  trailing  ", "abc"), "trailing-abc");
        assert_eq!(slug("///", "abc"), "untitled-abc");
        assert_eq!(slug("Ünïcode", ""), "ünïcode");
        assert_eq!(slug("", ""), "untitled");
    }

    /// The tracked file must never hold a literal credential, and the value must
    /// come back when the same machine reads the workspace.
    #[test]
    fn a_literal_auth_token_is_redacted_in_the_tracked_file() {
        let dir = temp_dir("auth-redact");
        let mut collection = sample_collection("col-1", "Example");
        let CollectionItem::Folder(folder) = &mut collection.items[0] else {
            unreachable!();
        };
        let CollectionItem::Request(saved) = &mut folder.items[0] else {
            unreachable!();
        };
        saved.request.auth = restbook_core::Auth::Bearer {
            token: "super-secret-token".to_owned(),
        };

        {
            let store = FolderStore::open(&dir).unwrap();
            store.save_collection(&collection).unwrap();
        }

        let file = dir
            .join(COLLECTIONS_DIR)
            .join("example-col1")
            .join(COLLECTION_FILE);
        let on_disk = fs::read_to_string(&file).unwrap();
        assert!(
            !on_disk.contains("super-secret-token"),
            "the tracked file leaked the token:\n{on_disk}"
        );
        assert!(on_disk.contains(REDACTED), "the placeholder is missing");
        // The shape is preserved: a reviewer still sees that it is a bearer.
        assert!(on_disk.contains("bearer"));

        // The local file holds it, and it is gitignored.
        let local = fs::read_to_string(dir.join(LOCAL_FILE)).unwrap();
        assert!(local.contains("super-secret-token"));
        let gitignore = fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(gitignore.contains(LOCAL_FILE));

        // Reading the workspace back on this machine restores the real value.
        let reopened = FolderStore::open(&dir).unwrap();
        let collections = reopened.collections().unwrap();
        let CollectionItem::Folder(folder) = &collections[0].items[0] else {
            panic!("expected a folder");
        };
        let CollectionItem::Request(saved) = &folder.items[0] else {
            panic!("expected a request");
        };
        assert!(matches!(
            &saved.request.auth,
            restbook_core::Auth::Bearer { token } if token == "super-secret-token"
        ));
    }

    /// A `{{var}}` is indirection, not a credential, so it must survive as-is.
    #[test]
    fn a_templated_auth_value_is_not_redacted() {
        let dir = temp_dir("auth-template");
        let mut collection = sample_collection("col-1", "Example");
        let CollectionItem::Folder(folder) = &mut collection.items[0] else {
            unreachable!();
        };
        let CollectionItem::Request(saved) = &mut folder.items[0] else {
            unreachable!();
        };
        saved.request.auth = restbook_core::Auth::Bearer {
            token: "{{TOKEN}}".to_owned(),
        };

        let store = FolderStore::open(&dir).unwrap();
        store.save_collection(&collection).unwrap();

        let file = dir
            .join(COLLECTIONS_DIR)
            .join("example-col1")
            .join(COLLECTION_FILE);
        let on_disk = fs::read_to_string(&file).unwrap();
        assert!(on_disk.contains("{{TOKEN}}"), "templating was clobbered");
    }

    #[test]
    fn a_secret_header_is_redacted_and_restored() {
        let dir = temp_dir("header-secret");
        let mut collection = sample_collection("col-1", "Example");
        let CollectionItem::Folder(folder) = &mut collection.items[0] else {
            unreachable!();
        };
        let CollectionItem::Request(saved) = &mut folder.items[0] else {
            unreachable!();
        };
        saved.request.headers = vec![
            Kv::new("Accept", "application/json"),
            Kv {
                key: "X-Api-Key".to_owned(),
                value: "header-secret".to_owned(),
                enabled: true,
                secret: true,
            },
        ];
        saved.request.params = vec![Kv {
            key: "token".to_owned(),
            value: "param-secret".to_owned(),
            enabled: true,
            secret: true,
        }];
        saved.request.body = restbook_core::Body::Form(vec![Kv {
            key: "password".to_owned(),
            value: "form-secret".to_owned(),
            enabled: true,
            secret: true,
        }]);

        {
            let store = FolderStore::open(&dir).unwrap();
            store.save_collection(&collection).unwrap();
        }

        let file = dir
            .join(COLLECTIONS_DIR)
            .join("example-col1")
            .join(COLLECTION_FILE);
        let on_disk = fs::read_to_string(&file).unwrap();
        assert!(!on_disk.contains("header-secret"), "leaked:\n{on_disk}");
        assert!(!on_disk.contains("param-secret"), "leaked:\n{on_disk}");
        assert!(!on_disk.contains("form-secret"), "leaked:\n{on_disk}");
        assert!(on_disk.contains("application/json"), "a plain row is kept");

        let reopened = FolderStore::open(&dir).unwrap();
        let collections = reopened.collections().unwrap();
        let CollectionItem::Folder(folder) = &collections[0].items[0] else {
            unreachable!();
        };
        let CollectionItem::Request(saved) = &folder.items[0] else {
            unreachable!();
        };
        assert_eq!(saved.request.headers[1].value, "header-secret");
        assert_eq!(saved.request.params[0].value, "param-secret");
        let restbook_core::Body::Form(rows) = &saved.request.body else {
            unreachable!();
        };
        assert_eq!(rows[0].value, "form-secret");
    }

    #[test]
    fn a_secret_environment_variable_is_redacted_and_restored() {
        let dir = temp_dir("env-secret");
        let environment = Environment {
            name: "dev".to_owned(),
            vars: vec![
                Kv::new("HOST", "https://example.com"),
                Kv {
                    key: "TOKEN".to_owned(),
                    value: "hunter2".to_owned(),
                    enabled: true,
                    secret: true,
                },
            ],
        };

        {
            let store = FolderStore::open(&dir).unwrap();
            store.save_environment(&environment).unwrap();
        }

        let tracked = fs::read_to_string(dir.join(ENVIRONMENTS_DIR).join("dev-dev.json"))
            .unwrap_or_else(|_| {
                // The exact slug is incidental; find whatever was written.
                let entry = fs::read_dir(dir.join(ENVIRONMENTS_DIR))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap();
                fs::read_to_string(entry.path()).unwrap()
            });
        assert!(
            !tracked.contains("hunter2"),
            "the tracked environment leaked the secret:\n{tracked}"
        );
        assert!(tracked.contains("HOST"), "non-secret rows are kept");
        assert!(tracked.contains(REDACTED));

        let reopened = FolderStore::open(&dir).unwrap();
        let environments = reopened.environments().unwrap();
        let token = environments[0]
            .vars
            .iter()
            .find(|kv| kv.key == "TOKEN")
            .unwrap();
        assert_eq!(token.value, "hunter2");
        assert!(token.secret);
    }

    /// Saving the same collection twice must still produce identical bytes, now
    /// that redaction runs on the way out.
    #[test]
    fn redaction_keeps_saves_byte_stable() {
        let dir = temp_dir("redact-stable");
        let mut collection = sample_collection("col-1", "Example");
        let CollectionItem::Folder(folder) = &mut collection.items[0] else {
            unreachable!();
        };
        let CollectionItem::Request(saved) = &mut folder.items[0] else {
            unreachable!();
        };
        saved.request.auth = restbook_core::Auth::ApiKey {
            name: "x-api-key".to_owned(),
            value: "abc123".to_owned(),
            location: restbook_core::ApiKeyLocation::Header,
        };

        let store = FolderStore::open(&dir).unwrap();
        store.save_collection(&collection).unwrap();
        let file = dir
            .join(COLLECTIONS_DIR)
            .join("example-col1")
            .join(COLLECTION_FILE);
        let first = fs::read(&file).unwrap();
        store.save_collection(&collection).unwrap();
        let second = fs::read(&file).unwrap();
        assert_eq!(first, second, "a re-save produced a diff");
    }
}
