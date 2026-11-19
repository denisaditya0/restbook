//! Where the dock layout is remembered between runs.
//!
//! Only the layout is persisted, never the panels: the workspace builds the
//! panes on every start, so a stale panel name is dropped on load. The file
//! carries this app's own version, separate from `DockAreaState`'s schema
//! version, so a changed default arrangement discards layouts an older build saved.

use std::path::Path;

use gpui_kit::component::dock::DockAreaState;

/// Bump when the default arrangement changes in a way an older saved layout
/// should not override. A file written under an earlier version is discarded;
/// the user gets the new default and a fresh file on the next save.
///
/// 1 — History / Collections / Environment shared the left dock.
/// 2 — Collections alone is the left dock.
/// 3 — History and Environment moved to the right tools dock, so a saved
///     layout from 2 would put the environment back in the sidebar.
const LAYOUT_VERSION: u32 = 3;

/// The persisted file: our version plus the library's state.
#[derive(serde::Serialize, serde::Deserialize)]
struct Saved {
    layout_version: u32,
    state: DockAreaState,
}

/// Save the layout. Failing to persist is not worth interrupting the user for,
/// so the error is swallowed deliberately.
pub(crate) fn save(path: &Path, state: &DockAreaState) {
    let saved = Saved {
        layout_version: LAYOUT_VERSION,
        state: state.clone(),
    };
    let Ok(json) = serde_json::to_string_pretty(&saved) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, json);
}

/// Load a previously saved layout, or `None` when there is nothing usable.
///
/// A file from an older `LAYOUT_VERSION` is dropped rather than migrated: the
/// arrangement it holds is one this build does not want, and the default is
/// always a valid starting point.
pub(crate) fn load(path: &Path) -> Option<DockAreaState> {
    let text = std::fs::read_to_string(path).ok()?;
    let saved: Saved = serde_json::from_str(&text).ok()?;
    (saved.layout_version == LAYOUT_VERSION).then_some(saved.state)
}
