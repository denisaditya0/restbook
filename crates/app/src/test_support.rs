//! Shared setup for the app's UI tests.
//!
//! Every `#[gpui_kit::test]` needs a data directory it owns (via [`DataDir`],
//! which points `RESTBOOK_DATA_DIR` at a temp dir) and a theme installed before
//! any pane snapshots its palette. A test that raises a notification also needs
//! the window root: `gpui_kit::open_window` wraps the app in `base::Root`, which
//! installs the component state `push_notification` and `open_dialog` need; a
//! bare `add_window_view` panics on the first notice.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::TestAppContext;

/// The variable [`crate::workspace::storage::data_dir`] reads first.
pub(crate) const DATA_DIR_VAR: &str = "RESTBOOK_DATA_DIR";

/// A directory of this test's own, removed when the guard drops.
///
/// `RESTBOOK_DATA_DIR` is process-wide, so the guard also holds a lock: two
/// tests that both opened a `Workspace` would otherwise point the app at each
/// other's store.
pub(crate) struct DataDir {
    path: PathBuf,
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl DataDir {
    /// Point the app at a fresh temporary directory for the life of the guard.
    pub(crate) fn new(label: &str) -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        static NEXT: AtomicU64 = AtomicU64::new(0);

        let lock = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("restbook-test-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create the test data directory");

        let previous = std::env::var_os(DATA_DIR_VAR);
        // SAFETY: the lock above is held until this guard drops, so no other
        // test in this process reads or writes the variable meanwhile.
        unsafe { std::env::set_var(DATA_DIR_VAR, &path) };
        Self {
            path,
            previous,
            _lock: lock,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DataDir {
    fn drop(&mut self) {
        // SAFETY: the lock is still held; it drops after this body.
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var(DATA_DIR_VAR, value),
                None => std::env::remove_var(DATA_DIR_VAR),
            }
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// What every UI test does before it builds a view: install the theme and
/// reduce motion. The theme goes first because a pane snapshots its palette in
/// its constructor (`Palette::new(cx)`). Reduced motion keeps a dialog's
/// entrance animation from moving the button between a click's aim and its
/// arrival; call once, before `cx.new(...)`.
pub(crate) fn init_ui(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::theme::ThemeRegistry::install(cx, "", 1.0, crate::theme::Appearance::Dark);
        cx.set_reduce_motion(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard is what lets a test open a real `Workspace`: while it is held,
    /// `data_dir` answers with the temporary path, and afterwards the previous
    /// value is back.
    #[test]
    fn the_data_dir_guard_points_the_app_at_its_own_directory_and_restores() {
        let before = std::env::var_os(DATA_DIR_VAR);
        {
            let dir = DataDir::new("guard");
            assert_eq!(
                crate::workspace::storage::data_dir(),
                dir.path(),
                "the app reads the guard's directory"
            );
            assert!(dir.path().exists());
        }
        assert_eq!(std::env::var_os(DATA_DIR_VAR), before, "restored");
    }

    /// Every UI test needs the theme in place before a pane is built; a test
    /// that skipped it would assert against component defaults.
    #[gpui_kit::test]
    fn init_ui_installs_a_theme_and_settles_motion(cx: &mut TestAppContext) {
        init_ui(cx);
        cx.update(|cx| {
            assert!(cx.try_global::<crate::theme::ThemeRegistry>().is_some());
            assert!(cx.reduce_motion());
        });
    }
}
