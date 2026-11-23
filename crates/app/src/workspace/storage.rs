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
    /// How the history this session writes is protected. The window names it,
    /// because a plaintext fallback is otherwise only ever said on stderr.
    pub(crate) history: HistoryMode,
}

/// How the history at rest is protected for this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryMode {
    /// SQLCipher under a key from the OS keyring.
    #[cfg_attr(not(feature = "sqlcipher"), allow(dead_code))]
    Encrypted,
    /// A plain JSONL file, with the reason encryption did not happen.
    Plaintext(PlaintextReason),
    /// Nothing on disk: this window's history lives in memory.
    Ephemeral(EphemeralReason),
}

/// Why this session's history is not written to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EphemeralReason {
    /// The encrypted store could not be opened, and neither could a plaintext
    /// file in its place.
    OpenFailed,
    /// A workspace refuses to write plain text, and encryption was unavailable,
    /// so the session's history stays in memory rather than in the shared tree.
    WorkspaceRefusedPlaintext,
}

/// Why history is written in plain text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaintextReason {
    /// The build was compiled without the `sqlcipher` feature. Only that build
    /// constructs it, so the default build's variant has no producer.
    #[cfg_attr(feature = "sqlcipher", allow(dead_code))]
    NoEncryptionSupport,
    /// The keyring or the encrypted database could not be opened.
    #[cfg_attr(not(feature = "sqlcipher"), allow(dead_code))]
    EncryptionUnavailable,
}

/// Whether plain text is an acceptable place to put this session's history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryPolicy {
    /// The per-user data directory: fall back to a JSONL file when encryption
    /// is unavailable, so a window always has somewhere to write.
    AllowPlaintext,
    /// A workspace: refuse the fallback. A workspace is the tree the user
    /// shares, so history there is either encrypted or not saved at all.
    NoPlaintext,
}

/// The store history is served from, and how it is protected.
pub(crate) struct History {
    pub(crate) store: Arc<dyn Store>,
    pub(crate) mode: HistoryMode,
}

/// Open the store the app should run against.
///
/// A Git-friendly workspace found around the current directory takes
/// precedence over the per-user database: the marker file is an explicit
/// invitation to use it, and treating the two as one history would silently mix
/// a shared checkout with private local data.
pub(crate) fn open_store(state_dir: &Path) -> StoreChoice {
    if let Some(root) = workspace_root() {
        // A workspace's history is scoped to its gitignored state directory and
        // is never allowed to fall back to plain text: this is the tree the
        // user shares, so a readable body under `.restbook/` would undo the
        // encryption the rest of the app is built on.
        let history = open_history(
            &root.join(restbook_store::STATE_DIR),
            HistoryPolicy::NoPlaintext,
        );
        match FolderStore::with_history(&root, history.store.clone()) {
            Ok(store) => {
                let name = workspace_name(&root);
                log::info!(
                    "workspace {} (collections and environments are files here; history is kept under {} and never in plain text)",
                    root.display(),
                    restbook_store::STATE_DIR
                );
                return StoreChoice {
                    store: Arc::new(store),
                    name,
                    workspace_root: Some(root),
                    history: history.mode,
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
    let history = open_history(state_dir, HistoryPolicy::AllowPlaintext);
    StoreChoice {
        store: history.store,
        name: APP_NAME.to_owned(),
        workspace_root: None,
        history: history.mode,
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

/// Why a folder cannot be turned into a workspace root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnableWorkspaceError {
    /// The folder is not there, or is not a folder.
    NotADirectory,
    /// It already holds a marker. Writing one would be a no-op at best and a
    /// silent rewrite of someone else's workspace at worst.
    AlreadyAWorkspace,
    /// The marker could not be written.
    WriteFailed,
}

/// Turn `dir` into a workspace root by writing the marker file.
///
/// The marker is what [`workspace_root`] looks for, so writing it is the whole
/// operation; the caller decides when the change takes effect. It refuses a
/// directory that is already marked rather than rewriting the file, because an
/// existing `restbook.json` may carry settings this app does not own.
pub(crate) fn enable_workspace(dir: &Path) -> Result<(), EnableWorkspaceError> {
    if !dir.is_dir() {
        return Err(EnableWorkspaceError::NotADirectory);
    }
    if marked_workspace(dir) {
        return Err(EnableWorkspaceError::AlreadyAWorkspace);
    }
    // `create_new` so a folder that gained a marker between the check above and
    // this call is reported rather than overwritten.
    let marker = dir.join(restbook_store::MARKER_FILE);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => EnableWorkspaceError::AlreadyAWorkspace,
            _ => EnableWorkspaceError::WriteFailed,
        })?;
    use std::io::Write as _;
    file.write_all(b"{}\n")
        .map_err(|_| EnableWorkspaceError::WriteFailed)?;
    log::info!("workspace enabled at {}", dir.display());
    Ok(())
}

/// Start a second copy of this build with `root` as `RESTBOOK_WORKSPACE`.
///
/// Switching in place would rebuild the store, the panes, and every tab from a
/// different root; a relaunch is the smaller seam, and it matches the rule the
/// storage picker already states: the choice takes effect the next time restbook
/// starts. The child inherits the current directory, so a relative root still
/// resolves the way the current process resolved it.
pub(crate) fn relaunch_in_workspace(root: &Path) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    relaunch_command(&exe, root).spawn()?;
    Ok(())
}

fn relaunch_command(exe: &Path, root: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(exe);
    command.env("RESTBOOK_WORKSPACE", root);
    command
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

/// Open a history store at `state_dir`, preferring SQLCipher when it was
/// compiled in.
///
/// `state_dir` is created on demand; a directory that cannot be created falls
/// through to the in-memory store, so the app still starts read-only instead of
/// losing the window. `policy` decides what happens when encryption is
/// unavailable: a JSONL file in the data directory, or nothing on disk in a
/// workspace.
pub(crate) fn open_history(state_dir: &Path, policy: HistoryPolicy) -> History {
    let _ = restbook_store::secure::create_dir(state_dir);
    let jsonl_path = state_dir.join(HISTORY_FILE);

    #[cfg(feature = "sqlcipher")]
    {
        let db_path = state_dir.join(HISTORY_DB);
        match SqlcipherStore::open(&db_path) {
            Ok(store) => {
                let store: Arc<dyn Store> = Arc::new(store);
                match policy {
                    HistoryPolicy::AllowPlaintext => {
                        import_legacy_jsonl(store.as_ref(), &jsonl_path)
                    }
                    HistoryPolicy::NoPlaintext => {
                        import_workspace_jsonl(store.as_ref(), &jsonl_path)
                    }
                }
                return History {
                    store,
                    mode: HistoryMode::Encrypted,
                };
            }
            Err(error) => {
                log::warn!("encrypted history unavailable ({error}); using plaintext JSONL");
            }
        }
    }

    without_encryption(&jsonl_path, policy)
}

/// What a session gets when no encrypted store could be opened.
///
/// Split out from [`open_history`] so the strict-mode rule can be tested without
/// a keyring: the decision is about the fallback, not about SQLCipher.
fn without_encryption(jsonl_path: &Path, policy: HistoryPolicy) -> History {
    if policy == HistoryPolicy::NoPlaintext {
        // Strict mode: the build cannot encrypt or the keyring refused, and a
        // workspace must not gain a plaintext file. The window still opens; the
        // status bar says this session's history will not survive it.
        log::warn!(
            "workspace history needs encryption, which is unavailable; history is not saved"
        );
        return History {
            store: Arc::new(MemoryStore::new()),
            mode: HistoryMode::Ephemeral(EphemeralReason::WorkspaceRefusedPlaintext),
        };
    }

    // A build without SQLCipher, or one whose keyring/store failed: keep the
    // JSONL file in the same data directory and carry an older history over
    // once, so moving the directory does not look like data loss.
    migrate_plain_jsonl(jsonl_path);
    match JsonlStore::open(jsonl_path) {
        Ok(store) => History {
            store: Arc::new(store),
            mode: HistoryMode::Plaintext(plaintext_reason()),
        },
        // The file exists but cannot be opened; the window still works, and the
        // status bar says this session's history will not survive it.
        Err(_) => History {
            store: Arc::new(MemoryStore::new()),
            mode: HistoryMode::Ephemeral(EphemeralReason::OpenFailed),
        },
    }
}

/// Why the fallback path is plaintext: the build has no encryption, or it has
/// some and refused to open the database.
#[cfg(feature = "sqlcipher")]
fn plaintext_reason() -> PlaintextReason {
    PlaintextReason::EncryptionUnavailable
}

#[cfg(not(feature = "sqlcipher"))]
fn plaintext_reason() -> PlaintextReason {
    PlaintextReason::NoEncryptionSupport
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

/// Hand a workspace's plaintext history to the encrypted store once, then
/// delete the file.
///
/// Unlike [`import_legacy_jsonl`], which renames the source to mark it imported,
/// this removes it: a `.jsonl.imported` beside the database would leave a
/// readable copy of every body in the tree the user shares, which is the leak
/// this whole mode exists to close. A failed import leaves the file in place so
/// the next start retries.
#[cfg(feature = "sqlcipher")]
fn import_workspace_jsonl(store: &dyn Store, path: &Path) {
    if !path.exists() {
        return;
    }
    let entries = match restbook_store::read_entries(path) {
        Ok(entries) => entries,
        Err(error) => {
            log::warn!("skipping unreadable history {}: {error}", path.display());
            return;
        }
    };
    for entry in &entries {
        if let Err(error) = store.append_history(entry) {
            log::warn!("history import stopped for {}: {error}", path.display());
            return;
        }
    }
    let _ = std::fs::remove_file(path);
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
    use super::{
        enable_workspace, marked_workspace, relaunch_command, workspace_name, EnableWorkspaceError,
    };
    use std::path::PathBuf;

    /// A relaunch names the workspace explicitly; otherwise the child would walk
    /// up from the inherited directory and could open a different tree.
    #[test]
    fn a_relaunch_names_the_workspace_root() {
        let command = relaunch_command(
            std::path::Path::new("restbook"),
            std::path::Path::new("C:/work/tree"),
        );
        let envs: Vec<_> = command.get_envs().collect();
        assert!(
            envs.iter().any(|(key, value)| {
                *key == "RESTBOOK_WORKSPACE" && *value == Some(std::ffi::OsStr::new("C:/work/tree"))
            }),
            "the child must be pinned to the chosen workspace: {envs:?}",
        );
    }

    /// The plaintext warning names its reason. A build without encryption and
    /// one whose keyring was refused are different problems, and the tooltip
    /// must not tell the user to fix the wrong one.
    #[test]
    fn a_plaintext_fallback_keeps_its_reason() {
        use super::{plaintext_reason, PlaintextReason};

        #[cfg(feature = "sqlcipher")]
        assert_eq!(plaintext_reason(), PlaintextReason::EncryptionUnavailable);
        #[cfg(not(feature = "sqlcipher"))]
        assert_eq!(plaintext_reason(), PlaintextReason::NoEncryptionSupport);
    }

    /// A workspace is the tree the user shares, so a session without an
    /// encrypted store keeps its history in memory instead of creating a file
    /// the next commit could pick up.
    #[test]
    fn a_workspace_without_encryption_writes_no_history_file() {
        use super::{without_encryption, EphemeralReason, HistoryMode, HistoryPolicy};

        let dir = std::env::temp_dir().join("restbook-storage-strict-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jsonl_path = dir.join("history.jsonl");

        let history = without_encryption(&jsonl_path, HistoryPolicy::NoPlaintext);

        assert_eq!(
            history.mode,
            HistoryMode::Ephemeral(EphemeralReason::WorkspaceRefusedPlaintext)
        );
        assert!(
            !jsonl_path.exists(),
            "the strict fallback must write nothing to the workspace"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The per-user data directory keeps the old fallback: a window always has
    /// somewhere to write, and the status bar names the reason.
    #[test]
    fn a_data_directory_without_encryption_falls_back_to_a_plain_file() {
        use super::{without_encryption, HistoryMode, HistoryPolicy};

        let dir = std::env::temp_dir().join("restbook-storage-plain-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jsonl_path = dir.join("history.jsonl");

        let history = without_encryption(&jsonl_path, HistoryPolicy::AllowPlaintext);

        assert!(matches!(history.mode, HistoryMode::Plaintext(_)));
        assert!(jsonl_path.exists(), "the data directory keeps its fallback");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A send big enough to be found if it were written in the clear.
    #[cfg(feature = "sqlcipher")]
    fn entry(url: &str) -> restbook_core::HistoryEntry {
        restbook_core::HistoryEntry {
            id: 0,
            request: restbook_core::RequestDef::new(restbook_core::Method::Get, url),
            response: restbook_core::ResponseRecord {
                status: 200,
                headers: Vec::new(),
                body: b"{}".to_vec().into(),
                duration_ms: 1,
                received_at: chrono::Utc::now(),
                final_url: None,
                redirects: Vec::new(),
            },
            url: url.to_owned(),
            origin: restbook_core::Origin::default(),
        }
    }

    /// A plaintext history left in a workspace is handed to the encrypted store
    /// once, then removed: leaving a readable copy beside it would keep the leak
    /// the import closes.
    #[cfg(feature = "sqlcipher")]
    #[test]
    fn an_existing_workspace_history_is_imported_once_and_removed() {
        use super::import_workspace_jsonl;
        use restbook_core::{HistoryFilter, Store as _};

        let dir = std::env::temp_dir().join("restbook-storage-import-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jsonl_path = dir.join("history.jsonl");

        {
            let jsonl = restbook_store::JsonlStore::open(&jsonl_path).unwrap();
            jsonl
                .append_history(&entry("https://example.com/1"))
                .unwrap();
            jsonl
                .append_history(&entry("https://example.com/2"))
                .unwrap();
        }

        let store = restbook_store::MemoryStore::new();
        let hits = |store: &restbook_store::MemoryStore| {
            store
                .search_history("", &HistoryFilter::default(), 10)
                .unwrap()
                .len()
        };

        import_workspace_jsonl(&store, &jsonl_path);
        assert!(!jsonl_path.exists(), "the plaintext file is gone");
        assert_eq!(hits(&store), 2, "both sends were imported");

        // The deleted file is the marker: a later start has nothing to repeat.
        import_workspace_jsonl(&store, &jsonl_path);
        assert_eq!(hits(&store), 2, "the import is not repeated");

        let _ = std::fs::remove_dir_all(&dir);
    }

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
        // A filesystem root and an empty path both have no final component, and
        // a titlebar showing an empty chip would look broken. `/` is a root on
        // every platform, so it is the portable half of this test.
        assert_eq!(workspace_name(&PathBuf::from("/")), "restbook");
        assert_eq!(workspace_name(&PathBuf::from("")), "restbook");

        // A drive root is a Windows idea: on Unix `C:/` is one component whose
        // name is `C:`, which is a perfectly good workspace name.
        if cfg!(windows) {
            assert_eq!(workspace_name(&PathBuf::from("C:/")), "restbook");
        }
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

    /// Enabling a workspace writes the marker and nothing else, and refuses the
    /// two cases that would be destructive or pointless.
    #[test]
    fn enabling_a_workspace_writes_the_marker_once() {
        let dir = std::env::temp_dir().join("restbook-storage-enable-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(!marked_workspace(&dir));
        enable_workspace(&dir).expect("a bare directory can become a workspace");
        assert!(marked_workspace(&dir));

        // A second call must not rewrite a marker this app does not own.
        assert_eq!(
            enable_workspace(&dir),
            Err(EnableWorkspaceError::AlreadyAWorkspace)
        );

        // A path that is not a directory is refused rather than created.
        let missing = dir.join("not-there");
        assert_eq!(
            enable_workspace(&missing),
            Err(EnableWorkspaceError::NotADirectory)
        );
        assert!(!missing.exists(), "a refused path must not be created");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
