//! Where the dock layout and the open tabs are remembered between runs.
//!
//! Only the arrangement and the open tabs' names are persisted, never the panels:
//! the workspace rebuilds the panes on every start, so a stale name is dropped on
//! load. The file carries this app's own version, so a changed default arrangement
//! discards layouts an older build saved.

use std::path::Path;
use std::time::{Duration, SystemTime};

use gpui_kit::component::dock::{DockAreaState, DockPlacement, PanelState};
use gpui_kit::*;

use restbook_core::Draft;

use crate::theme;

use super::{CenterOrientation, PanelName, Workspace};

/// Bump when the default arrangement changes in a way an older saved layout
/// should not override: a file from an earlier version is discarded, the user gets
/// the new default, and a fresh file is written on the next save.
///
/// 1 — History / Collections / Environment shared the left dock.
/// 2 — Collections alone is the left dock.
/// 3 — History and Environment moved to the right tools dock.
///
/// Open tabs are a defaulted field, not a version concern: a file from any version
/// parses, and a launch without `tabs` restores none.
const LAYOUT_VERSION: u32 = 3;

/// The persisted file: our version plus the library's state.
#[derive(serde::Serialize, serde::Deserialize)]
struct Saved {
    layout_version: u32,
    state: DockAreaState,
    #[serde(default)]
    tabs: SavedTabs,
}

/// The open tabs, in strip order, and which one was on screen.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedTabs {
    #[serde(default)]
    pub entries: Vec<SavedTab>,
    #[serde(default)]
    pub active: usize,
}

/// One restorable tab. A bound tab names its collection node; a draft names its
/// store row. Anything else — a pristine blank page — has nothing to restore
/// and is not recorded.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum SavedTab {
    Bound { node: String },
    Draft { id: i64 },
}

/// Save the layout and the open tabs. Failing to persist is not worth
/// interrupting the user for, so the error is swallowed deliberately.
pub(crate) fn save(path: &Path, state: &DockAreaState, tabs: &SavedTabs) {
    let saved = Saved {
        layout_version: LAYOUT_VERSION,
        state: state.clone(),
        tabs: SavedTabs {
            entries: tabs.entries.clone(),
            active: tabs.active,
        },
    };
    let Ok(json) = serde_json::to_string_pretty(&saved) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = restbook_store::secure::create_dir(parent);
    }
    let _ = restbook_store::secure::write(path, json);
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

/// The open tabs a usable layout file recorded, or `None` when the file is
/// missing, unreadable, or from an older version.
pub(crate) fn load_tabs(path: &Path) -> Option<SavedTabs> {
    let text = std::fs::read_to_string(path).ok()?;
    let saved: Saved = serde_json::from_str(&text).ok()?;
    (saved.layout_version == LAYOUT_VERSION).then_some(saved.tabs)
}

impl Workspace {
    /// The last layout edit and the live draft are written when the window closes.
    ///
    /// `on_window_should_close` runs before the window is destroyed, the last point
    /// where the store can be written without a task being dropped at process exit.
    /// gpui-pre's teardown on Windows logs target-less errors after this point;
    /// `flush` marks the process as closing so the log sink drops them.
    pub(crate) fn save_on_close(&self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            workspace
                .update(cx, |workspace, cx| workspace.flush(cx))
                .ok();
            true
        });
    }

    /// A quit that does not close a window first (Cmd+Q) still writes the
    /// layout and the live draft. [`Self::flush`] is idempotent, so the quit
    /// observers running after a normal close cost nothing.
    pub(crate) fn flush_on_quit(&self, cx: &mut Context<Self>) {
        cx.on_app_quit(|workspace, cx| {
            workspace.flush(cx);
            async {}
        })
        .detach();
    }

    /// Install the panes from the saved arrangement when there is one, the default
    /// otherwise.
    ///
    /// The saved state is read for its names and slot sizes only: `DockArea::load`
    /// would rebuild panels from the registry, which this app deliberately does not
    /// use. Re-adding the entities survives a restart, and sizes keep user drags.
    pub(crate) fn install_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = self::load(&self.layout_path) {
            let left = panel_names(state.left_dock.as_ref().map(|dock| dock.panel()));
            let center = panel_names(Some(&state.center));
            let right = panel_names(state.right_dock.as_ref().map(|dock| dock.panel()));
            if !left.is_empty() || !center.is_empty() || !right.is_empty() {
                let left_size = state.left_dock.as_ref().map(|dock| dock.size());
                let right_size = state.right_dock.as_ref().map(|dock| dock.size());
                let right_open = state.right_dock.as_ref().is_none_or(|dock| dock.open());
                // The centre's direction is the split's axis in the saved tree;
                // rebuild it the same way so a side-by-side choice survives a
                // restart. Its slot sizes are the request/response proportion the
                // user dragged. `dump` writes both; without passing them back the
                // split would start 50/50 (and vertical) on every launch.
                self.center_orientation = CenterOrientation::from_axis(state.center.info.axis());
                let center_sizes = state.center.info.sizes().map(Vec::as_slice);
                self.apply_layout(
                    &left,
                    &center,
                    &right,
                    center_sizes,
                    left_size,
                    right_size,
                    right_open,
                    window,
                    cx,
                );
                return;
            }
        }

        // First run: Collections alone is the sidebar, because the saved-things
        // explorer is what the window opens on. Environment and History live in
        // a wider right-hand dock and are opened on demand, so they never share
        // the tree's narrow width. Request over response in the centre.
        self.default_layout(window, cx);
    }

    /// The arrangement a first run installs, and what Reset Layout restores.
    pub(crate) fn default_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Reset layout also restores the default direction, so a user who wants
        // Request over Response again does not have to toggle after a reset.
        self.center_orientation = CenterOrientation::Stacked;
        self.apply_layout(
            &[PanelName::Collections],
            &[PanelName::Request, PanelName::Response],
            &[],
            None,
            Some(px(HISTORY_WIDTH * theme::zoom(cx))),
            None,
            true,
            window,
            cx,
        );
    }

    /// Put the window back in its first-run arrangement and write it to disk.
    ///
    /// `install_layout` reads the saved file first, so it is not what a reset
    /// wants: a reset has to ignore that file and replace it. A drag would
    /// otherwise schedule the same write, but this runs once, synchronously,
    /// so a reset followed by a quit still leaves the default arrangement.
    pub(crate) fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        log::debug!(target: "restbook_app::ui", "reset layout");
        self.default_layout(window, cx);
        self::save(
            &self.layout_path,
            &self.dock.read(cx).dump(cx),
            &self.tabs_snapshot(),
        );
        self.flash(super::chrome::FlashKind::Success, "Layout reset", cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_layout(
        &mut self,
        left: &[PanelName],
        center: &[PanelName],
        right: &[PanelName],
        center_sizes: Option<&[Pixels]>,
        left_size: Option<Pixels>,
        right_size: Option<Pixels>,
        right_open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let left_layout = (!left.is_empty()).then(|| self.tabs_layout(left, cx));
        let center_layout = self.stack_layout(center, center_sizes, cx);
        let right_layout = (!right.is_empty()).then(|| self.tabs_layout(right, cx));
        let left_size = left_size.unwrap_or(px(HISTORY_WIDTH * theme::zoom(cx)));
        let right_size = right_size.unwrap_or(px(TOOL_WIDTH * theme::zoom(cx)));

        self.dock.update(cx, |dock, cx| {
            if let Some(left_layout) = left_layout {
                dock.set_dock(DockPlacement::Left, left_layout, window, cx);
                dock.set_dock_size(DockPlacement::Left, left_size, window, cx);
            } else if dock.has_dock(DockPlacement::Left) {
                // An empty list means "no such dock", which is what Reset
                // Layout asks for when the first-run arrangement has no left
                // dock. A fresh window never reaches this arm.
                dock.remove_dock(DockPlacement::Left, window, cx);
            }
            if let Some(center_layout) = center_layout {
                dock.set_center(center_layout, window, cx);
            }
            if let Some(right_layout) = right_layout {
                dock.set_dock(DockPlacement::Right, right_layout, window, cx);
                dock.set_dock_size(DockPlacement::Right, right_size, window, cx);
                // A restored layout knows whether the tools dock was open, but
                // not that it is collapsible — that flag is not persisted, and
                // `Dock::new` defaults it on. Restoring a *closed* dock is
                // where it would otherwise be lost, and with it the only way
                // to close it again after the icons reopen it.
                dock.set_dock_collapsible(DockPlacement::Right, true, window, cx);
                if dock.is_dock_open(DockPlacement::Right) != right_open {
                    dock.toggle_dock(DockPlacement::Right, window, cx);
                }
            } else if dock.has_dock(DockPlacement::Right) {
                // The first-run arrangement has no tools dock at all; Reset
                // Layout has to remove one that was opened since.
                dock.remove_dock(DockPlacement::Right, window, cx);
            }
        });

        // The header's toggle glyph names the orientation that is now installed,
        // so it follows every path that rebuilds the centre — the saved layout,
        // the first-run default, and a reset.
        self.push_center_orientation(cx);
    }

    /// layout debounce. A burst of typing costs one write; only the active tab is
    /// live, the rest were snapshotted when they were left.
    ///
    /// The document notifies on every edit and every progress tick during a send, so
    /// this only bumps `draft_gen`; one live task re-reads it until things go quiet.
    pub(crate) fn schedule_draft_save(&mut self, cx: &mut Context<Self>) {
        self.draft_gen = self.draft_gen.wrapping_add(1);
        if self.draft_task.is_some() {
            // The live task will notice the new generation on its next wake.
            return;
        }
        // The generation this task has already accounted for. Starting at the
        // current one means a single quiet debounce writes once; any later
        // notify makes the comparison differ and re-arms.
        let mut seen = self.draft_gen;
        self.draft_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SAVE_DEBOUNCE).await;
                let edited = this
                    .update(cx, |workspace, _cx| {
                        let edited = workspace.draft_gen != seen;
                        seen = workspace.draft_gen;
                        edited
                    })
                    .unwrap_or(false);
                if !edited {
                    break;
                }
            }
            // The task is done, so the next edit may spawn a fresh one. The
            // write reads live state, so there is nothing to pass along.
            this.update(cx, |workspace, cx| {
                workspace.draft_task = None;
                workspace.persist_draft(cx);
            })
            .ok();
        }));
    }

    /// Write the active tab to the drafts table, or clear its row once filed into a
    /// collection. A blank URL is not worth restoring.
    ///
    /// Runs only at the end of a quiet debounce, never on a keystroke. A new draft's
    /// first insert is synchronous because it assigns the row id every later write
    /// needs; once bound, updates stay on the background executor.
    fn persist_draft(&mut self, cx: &mut Context<Self>) {
        // Nothing to persist when the last tab was closed.
        if self.tabs.is_empty() {
            return;
        }
        self.snapshot_active(cx);
        let index = self.tabs.active();
        let existing = self.tabs[index].draft;

        if self.tabs[index].node.is_some() {
            if let Some(id) = existing {
                let store = self.store.clone();
                cx.spawn(async move |this, cx| {
                    let deleted = cx
                        .background_spawn(async move { store.delete_draft(id).is_ok() })
                        .await;
                    if deleted {
                        this.update(cx, |workspace, cx| {
                            // The tabs can have changed while the write ran; only
                            // clear the row that was actually deleted.
                            if workspace
                                .tabs
                                .get(index)
                                .is_some_and(|tab| tab.draft == Some(id))
                            {
                                workspace.tabs[index].draft = None;
                            }
                            workspace.refresh_drafts(cx);
                        })
                        .ok();
                    }
                })
                .detach();
            }
            return;
        }

        if self.tabs[index].request.url.trim().is_empty() {
            return;
        }

        // `SystemTime` converts straight into the store's timestamp type, so
        // the app never needs to name it.
        let draft = Draft {
            id: existing.unwrap_or(0),
            request: self.tabs[index].request.clone(),
            updated_at: SystemTime::now().into(),
        };

        // The first write allocates the id, so it stays on the foreground. A
        // close before the background had bound it would otherwise insert the
        // same draft a second time.
        if existing.is_none() {
            let started = std::time::Instant::now();
            let result = self.store.save_draft(&draft);
            log::debug!(
                target: "restbook_app::perf",
                "first draft save took {}ms",
                started.elapsed().as_millis(),
            );
            match result {
                Ok(id) => {
                    self.tabs[index].draft = Some(id);
                    self.refresh_drafts(cx);
                }
                Err(error) => log::error!(
                    target: "restbook_app::ui",
                    "failed to save the draft: {error}"
                ),
            }
            return;
        }

        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let id = cx
                .background_spawn(async move { store.save_draft(&draft).ok() })
                .await;
            let Some(id) = id else {
                return;
            };
            this.update(cx, |workspace, cx| {
                // A tab can have been closed or switched under the write, so
                // bind the id only to the slot that is still the same draft.
                if workspace
                    .tabs
                    .get(index)
                    .is_some_and(|tab| tab.draft == existing)
                {
                    workspace.tabs[index].draft = Some(id);
                }
                workspace.refresh_drafts(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Write one tab's draft synchronously. Used when the tab is leaving the screen
    /// (a switch or a quit): the background writer reads whichever tab is active
    /// when it wakes, and a task spawned here would be dropped with the process.
    ///
    /// `dirty` says a debounce was armed. Without it an already-stored row is left
    /// untouched so its `updated_at` does not reorder the Unsaved list.
    pub(crate) fn persist_tab_draft(&mut self, index: usize, dirty: bool, cx: &mut Context<Self>) {
        let (existing, has_node) = match self.tabs.get(index) {
            Some(tab) => (tab.draft, tab.node.is_some()),
            None => return,
        };

        // A tab filed into a collection keeps no draft row. Clean it up even
        // when no debounce is armed, or the stale row comes back on launch.
        if has_node {
            if let Some(id) = existing {
                if let Err(error) = self.store.delete_draft(id) {
                    log::error!(
                        target: "restbook_app::ui",
                        "failed to delete the filed draft: {error}"
                    );
                }
                self.tabs[index].draft = None;
                self.refresh_drafts(cx);
            }
            return;
        }

        if !dirty && existing.is_some() {
            return;
        }
        // A blank tab is not worth restoring, the same rule the debounce uses.
        if self.tabs[index].request.url.trim().is_empty() {
            return;
        }

        let draft = Draft {
            id: existing.unwrap_or(0),
            request: self.tabs[index].request.clone(),
            updated_at: SystemTime::now().into(),
        };
        match self.store.save_draft(&draft) {
            Ok(id) => {
                self.tabs[index].draft = Some(id);
                self.refresh_drafts(cx);
            }
            Err(error) => log::error!(
                target: "restbook_app::ui",
                "failed to save the draft before switching tabs: {error}"
            ),
        }
    }

    /// Re-read the stored drafts so the sidebar's Unsaved list follows a write
    /// or delete made here.
    pub(crate) fn refresh_drafts(&mut self, cx: &mut Context<Self>) {
        self.panes
            .collections
            .update(cx, |panel, cx| panel.refresh_drafts(cx));
    }

    /// The open tabs as the layout file records them. A tab bound to neither a node
    /// nor a draft row has nothing to restore and is skipped, keeping a blank page
    /// out of the file.
    ///
    /// `active` indexes `entries`, not the live strip: the two differ by the skipped
    /// tabs, and a restore must land on the same request, not a surviving slot.
    pub(crate) fn tabs_snapshot(&self) -> SavedTabs {
        let mut entries = Vec::new();
        let mut active = None;
        for (index, tab) in self.tabs.iter().enumerate() {
            let entry = match (&tab.node, tab.draft) {
                (Some(node), _) => SavedTab::Bound { node: node.clone() },
                (None, Some(id)) => SavedTab::Draft { id },
                (None, None) => continue,
            };
            if index == self.tabs.active() {
                active = Some(entries.len());
            }
            entries.push(entry);
        }
        SavedTabs {
            entries,
            active: active.unwrap_or(0),
        }
    }

    /// Write the layout and the open tabs together, synchronously. Tab changes
    /// are discrete, unlike a drag, so there is no burst to debounce away.
    pub(crate) fn persist_tabs(&mut self, cx: &App) {
        save(
            &self.layout_path,
            &self.dock.read(cx).dump(cx),
            &self.tabs_snapshot(),
        );
    }
}

/// A drag emits a layout change per frame, so the file is written on a pause.
pub(super) const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);
pub(super) const CLOSED_TAB_LIMIT: usize = 20;

/// The width the sidebar starts at, before the user drags its edge.
pub(crate) const HISTORY_WIDTH: f32 = 260.;

/// The width the right-hand tools dock starts at. Environment rows carry a
/// key, a value, and three controls, so the pane cannot use the sidebar's
/// width without clipping the controls that create an environment.
pub(crate) const TOOL_WIDTH: f32 = 400.;
/// The pane names in a saved tree, drops unknown ones, in layout order.
pub(super) fn panel_names(state: Option<&PanelState>) -> Vec<PanelName> {
    let mut names = Vec::new();
    if let Some(state) = state {
        collect_names(state, &mut names);
    }
    names
}

fn collect_names(state: &PanelState, names: &mut Vec<PanelName>) {
    if state.children.is_empty() {
        if let Some(name) = PanelName::parse(&state.panel_name) {
            names.push(name);
        }
    } else {
        for child in &state.children {
            collect_names(child, names);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{load, save};
    use gpui_kit::component::dock::DockAreaState;
    use std::os::unix::fs::PermissionsExt as _;

    /// `layout.json` is app-owned; it is `0600` where the platform has a mode.
    #[test]
    fn a_saved_layout_file_is_private() {
        let dir = std::env::temp_dir().join("restbook-layout-private-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("layout.json");

        save(&path, &DockAreaState::default(), &Default::default());

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "layout.json should be private");
        assert!(load(&path).is_some(), "the saved file still parses");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tab_tests {
    use gpui_kit::component::dock::DockAreaState;

    use super::{load_tabs, save, SavedTab, SavedTabs, LAYOUT_VERSION};

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("layout.json")
    }

    /// 0.2.0 wrote no `tabs` key; a file from it must still parse and restore
    /// no tabs rather than discard the layout.
    #[test]
    fn a_file_without_tabs_parses_as_no_tabs() {
        let path = temp_path("restbook-tabs-missing-key-test");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let state = serde_json::to_value(DockAreaState::default()).unwrap();
        let doc = serde_json::json!({
            "layout_version": LAYOUT_VERSION,
            "state": state,
        });
        std::fs::write(&path, doc.to_string()).unwrap();

        let tabs = load_tabs(&path).expect("a file from the previous build still parses");

        assert!(tabs.entries.is_empty());
        assert_eq!(tabs.active, 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A bound tab and a draft keep their keys across a write and a read.
    #[test]
    fn bound_and_draft_tabs_round_trip() {
        let path = temp_path("restbook-tabs-roundtrip-test");
        let saved = SavedTabs {
            entries: vec![
                SavedTab::Bound {
                    node: "node-1".into(),
                },
                SavedTab::Draft { id: 7 },
            ],
            active: 1,
        };

        save(&path, &DockAreaState::default(), &saved);
        let tabs = load_tabs(&path).expect("the file just written parses");

        assert_eq!(tabs.active, 1);
        assert_eq!(tabs.entries.len(), 2);
        assert!(matches!(&tabs.entries[0], SavedTab::Bound { node } if node == "node-1"));
        assert!(matches!(tabs.entries[1], SavedTab::Draft { id: 7 }));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
