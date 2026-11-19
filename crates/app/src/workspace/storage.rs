//! Where history lives on disk, and which store serves it.
//!
//! Two modes, chosen once at startup: a **workspace** (a directory with a
//! `restbook.json` marker, found by walking up like `.git`) keeps collections and
//! environments in shareable files; otherwise the per-user **application data
//! directory** holds one SQLCipher database. The marker wins; env vars override.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use restbook_core::Store;
use restbook_store::{FolderStore, JsonlStore, MemoryStore};

#[cfg(feature = "sqlcipher")]
use restbook_store::sqlcipher::SqlcipherStore;

/// The directory under the OS data root.
const APP_NAME: &str = crate::branding::NAME;
const APP_DIR: &str = crate::branding::NAME;

/// The working-directory-relative directory older builds wrote to.
const LEGACY_DIR: &str = ".restbook";
const HISTORY_FILE: &str = "history.jsonl";
#[cfg(feature = "sqlcipher")]
const HISTORY_DB: &str = "history.db";

/// The directory history and the layout live in.
pub(crate) fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RESTBOOK_DATA_DIR") {
        return PathBuf::from(dir);
    }
    platform_data_dir().unwrap_or_else(|| PathBuf::from(LEGACY_DIR))
}

/// The directory a user drops theme files into, beside `settings.json`.
pub(crate) fn themes_dir() -> PathBuf {
    data_dir().join("themes")
}

/// The theme text to load, in the order the app falls back: a user file named
/// by the setting when it exists, then the built-in embedded theme.
///
/// A name with no file is not an error: the built-in theme is always valid, and
/// the user simply loses their override, which is the same rule `settings.json`
/// follows for an unknown key.
pub(crate) fn load_theme_text(name: Option<&str>) -> String {
    if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
        let path = themes_dir().join(format!("{name}.jsonc"));
        if let Ok(text) = std::fs::read_to_string(&path) {
            return text;
        }
    }
    crate::theme::DEFAULT_THEME.to_owned()
}

/// The names of the user theme files present, without extensions, sorted. The
/// settings dialog lists these beside the built-in theme.
pub(crate) fn list_themes() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(themes_dir())
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let extension = path.extension().and_then(|ext| ext.to_str());
            if !matches!(extension, Some("jsonc") | Some("json")) {
                return None;
            }
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned)
        })
        .collect();
    names.sort();
    names
}

#[cfg(target_os = "windows")]
fn platform_data_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join(APP_DIR))
}

#[cfg(target_os = "macos")]
fn platform_data_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|dir| PathBuf::from(dir).join("Library/Application Support"))
        .map(|dir| dir.join(APP_DIR))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_data_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .map(|dir| dir.join(APP_DIR))
        .or_else(|| {
            std::env::var_os("HOME").map(|dir| {
                PathBuf::from(dir)
                    .join(".local")
                    .join("share")
                    .join(APP_DIR)
            })
        })
}

#[cfg(not(any(unix, target_os = "windows")))]
fn platform_data_dir() -> Option<PathBuf> {
    None
}

/// Which store the app is running against, and where it lives.
///
/// The workspace needs both halves: the store to hand to panes, and enough
/// context to label the window. A workspace shows its directory name; the
/// per-user database shows the app name.
pub(crate) struct StoreChoice {
    pub(crate) store: Arc<dyn Store>,
    /// What the titlebar calls this session.
    pub(crate) name: String,
    /// The workspace root, when one was found. `None` means the per-user
    /// database is in use and collections are not shareable from here.
    pub(crate) workspace_root: Option<PathBuf>,
}

/// Open the store the app should run against.
///
/// A Git-friendly workspace found around the current directory takes
/// precedence over the per-user database: the marker file is an explicit
/// invitation to use it, and treating the two as one history would silently mix
/// a shared checkout with private local data.
pub(crate) fn open_store(state_dir: &Path) -> StoreChoice {
    if let Some(root) = workspace_root() {
        match FolderStore::open(&root) {
            Ok(store) => {
                let name = workspace_name(&root);
                log::info!(
                    "workspace {} (collections and environments are files here; history stays local)",
                    root.display()
                );
                return StoreChoice {
                    store: Arc::new(store),
                    name,
                    workspace_root: Some(root),
                };
            }
            Err(error) => {
                // A workspace that cannot be opened is not a reason to lose the
                // window; fall through to the data directory and say so.
                log::warn!(
                    "workspace {} unavailable ({error}); using the application data directory",
                    root.display()
                );
            }
        }
    }

    // No workspace: history and preferences are the per-user database. The
    // path is logged because it is the answer to "where did my data go" and
    // is otherwise only visible in the status bar.
    log::info!(
        "using the application data directory {}",
        state_dir.display()
    );
    StoreChoice {
        store: open_history(state_dir),
        name: APP_NAME.to_owned(),
        workspace_root: None,
    }
}

/// The workspace to open, if any.
///
/// `RESTBOOK_WORKSPACE` names one explicitly and is authoritative; otherwise
/// the current directory is walked upward, so launching from anywhere inside a
/// workspace finds it the way `.git` is found.
fn workspace_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("RESTBOOK_WORKSPACE") {
        let dir = PathBuf::from(dir);
        // A named root that is not marked is almost certainly a mistake; opening
        // it would create collections next to unrelated files.
        if marked_workspace(&dir) {
            return Some(dir);
        }
        log::warn!(
            "RESTBOOK_WORKSPACE={} has no {}; ignoring it",
            dir.display(),
            restbook_store::MARKER_FILE
        );
    }
    let cwd = std::env::current_dir().ok()?;
    FolderStore::find_root(cwd)
}

/// Whether `dir` is a workspace root: it holds the marker file. This is the
/// whole test, and it is deliberately separate from `FolderStore::find_root`
/// because the explicit path is not allowed to walk upward — naming a
/// subdirectory of a workspace should say so, not silently open the parent.
fn marked_workspace(dir: &Path) -> bool {
    dir.join(restbook_store::MARKER_FILE).is_file()
}

/// The label for a workspace: its directory name, falling back to the app name
/// for a root with no usable final component (a drive root, say).
fn workspace_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| APP_NAME.to_owned())
}

/// Open the per-user database, preferring SQLCipher when it was compiled in.
///
/// `state_dir` is created on demand; a directory that cannot be created falls
/// through to the in-memory store, so the app still starts read-only instead of
/// losing the window.
pub(crate) fn open_history(state_dir: &Path) -> Arc<dyn Store> {
    let _ = std::fs::create_dir_all(state_dir);
    let jsonl_path = state_dir.join(HISTORY_FILE);

    #[cfg(feature = "sqlcipher")]
    {
        let db_path = state_dir.join(HISTORY_DB);
        match SqlcipherStore::open(&db_path) {
            Ok(store) => {
                let store: Arc<dyn Store> = Arc::new(store);
                import_legacy_jsonl(store.as_ref(), &jsonl_path);
                return store;
            }
            Err(error) => {
                log::warn!("encrypted history unavailable ({error}); using plaintext JSONL");
            }
        }
    }

    // A build without SQLCipher, or one whose keyring/store failed: keep the
    // JSONL file in the same data directory and carry an older history over
    // once, so moving the directory does not look like data loss.
    migrate_plain_jsonl(&jsonl_path);
    match JsonlStore::open(&jsonl_path) {
        Ok(store) => Arc::new(store),
        Err(_) => Arc::new(MemoryStore::new()),
    }
}

/// Copy the old working-directory history to the new location, once.
fn migrate_plain_jsonl(new_path: &Path) {
    if new_path.exists() {
        return;
    }
    if let Some(legacy) = legacy_files().into_iter().find(|path| path.exists()) {
        if legacy != new_path {
            let _ = std::fs::copy(&legacy, new_path);
        }
    }
}

/// Load every plaintext history not yet imported, then rename each file to mark
/// it imported so a later run does not repeat it.
///
/// Runs on every start rather than only on a fresh database: the rename is what
/// makes it once-per-file. An existing database is unharmed — requests upsert by
/// method + URL, and each imported entry appends one execution.
#[cfg(feature = "sqlcipher")]
fn import_legacy_jsonl(store: &dyn Store, new_path: &Path) {
    let mut sources = legacy_files();
    // The JSONL file in the data directory is history written by an earlier
    // build without SQLCipher; it is imported once and then handed over.
    if new_path.exists() {
        sources.push(new_path.to_path_buf());
    }

    for source in sources {
        let entries = match restbook_store::read_entries(&source) {
            Ok(entries) => entries,
            Err(error) => {
                log::warn!("skipping unreadable history {}: {error}", source.display());
                continue;
            }
        };
        if entries.is_empty() {
            continue;
        }

        let mut failure = None;
        for entry in &entries {
            if let Err(error) = store.append_history(entry) {
                failure = Some(error);
                break;
            }
        }
        if let Some(error) = failure {
            // Leave the file in place: a partial import is retried next start.
            log::warn!("history import stopped for {}: {error}", source.display());
            continue;
        }

        // Same-volume rename normally succeeds. If it does not, the import is
        // still complete, but the next run repeats it; there is no marker that
        // survives a read-only directory.
        let _ = std::fs::rename(&source, source.with_extension("jsonl.imported"));
    }
}

/// The pre-data-dir histories, in import order.
///
/// The old app wrote `.restbook/history.jsonl` relative to the working directory,
/// so a checkout run from both the root and `crates/app` has two files, each with
/// its own history. The manifest path is a development convenience: it is baked
/// in at compile time and simply does not exist on another machine.
fn legacy_files() -> Vec<PathBuf> {
    let mut files = vec![PathBuf::from(LEGACY_DIR).join(HISTORY_FILE)];
    let beside_crate = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(LEGACY_DIR)
        .join(HISTORY_FILE);
    if !files.contains(&beside_crate) {
        files.push(beside_crate);
    }
    files
}

#[cfg(test)]
mod tests {
    // Named imports rather than `use super::*`: this crate glob-imports
    // `gpui_kit::*` elsewhere, and that macro set shadows `#[test]`.
    use super::{marked_workspace, workspace_name};
    use std::path::PathBuf;

    #[test]
    fn a_workspace_is_named_for_its_directory() {
        assert_eq!(
            workspace_name(&PathBuf::from("C:/work/acme-api")),
            "acme-api"
        );
        assert_eq!(workspace_name(&PathBuf::from("/home/dev/team")), "team");
    }

    #[test]
    fn a_root_without_a_name_falls_back_to_the_app_name() {
        // A drive root and an empty path both have no final component, and a
        // titlebar showing an empty chip would look broken.
        assert_eq!(workspace_name(&PathBuf::from("C:/")), "restbook");
        assert_eq!(workspace_name(&PathBuf::from("")), "restbook");
    }

    #[test]
    fn only_a_marked_directory_is_a_workspace_root() {
        let dir = std::env::temp_dir().join("restbook-storage-marker-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A bare directory is not a workspace, however it is named.
        assert!(!marked_workspace(&dir));

        std::fs::write(dir.join(restbook_store::MARKER_FILE), b"{}").unwrap();
        assert!(marked_workspace(&dir));

        // The check does not walk upward: a nested directory is not a root even
        // though its parent is, which is what keeps an explicit path honest.
        let nested = dir.join("collections");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(!marked_workspace(&nested));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
