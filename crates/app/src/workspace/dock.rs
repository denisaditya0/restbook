//! Where panels go, and what the user is told about it.
//!
//! A pane names itself; this module decides which dock holds it, whether that
//! dock is open, and how it is brought to the front. Every outcome says so
//! through [`Workspace::flash`], which parks the message for a `Window`-less caller.

use std::cell::Cell;
use std::sync::Arc;

use gpui_kit::component::button::*;
pub(crate) use gpui_kit::component::dock::panel_handle;
use gpui_kit::component::dock::{DockLayout, DockPlacement, PaneRef, PanelId};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

use crate::notice::Notice;
use crate::theme;

use super::delete::Deleted;
use super::layout_policy;
use super::persistence::{HISTORY_WIDTH, TOOL_WIDTH};
use super::{chrome, wrap_index, CenterOrientation, PanelName, Workspace, WorkspaceNotice};

/// The usable width of the display the window sits on, for capping a grow.
fn display_width(window: &Window, cx: &App) -> Option<f32> {
    let centre = window.bounds().center();
    cx.displays()
        .into_iter()
        .find(|display| display.bounds().contains(&centre))
        .map(|display| f32::from(display.visible_bounds().size.width))
}

impl Workspace {
    /// A tab group of the named panes, in order.
    ///
    /// A pane goes in through [`panel_handle`], not as a bare entity: the
    /// handle carries the presentation half of the `Panel` trait with it, which
    /// is how a pane can say `title_bar(false)` and draw its own chrome.
    pub(crate) fn tabs_layout(&self, names: &[PanelName], cx: &App) -> DockLayout {
        names.iter().fold(DockLayout::tabs(), |tabs, name| {
            tabs.panel_view(self.panel_view(*name), cx)
        })
    }

    /// One pane as a dock handle, by name.
    pub(crate) fn panel_view(
        &self,
        name: PanelName,
    ) -> Arc<dyn gpui_kit::component::dock::BasePanelView> {
        match name {
            PanelName::History => panel_handle(self.panes.history.clone()),
            PanelName::Request => panel_handle(self.panes.request.clone()),
            PanelName::Response => panel_handle(self.panes.response.clone()),
            PanelName::Environment => panel_handle(self.panes.environment.clone()),
            PanelName::Collections => panel_handle(self.panes.collections.clone()),
        }
    }

    /// The named panes in one split, in order.
    ///
    /// The axis follows `center_orientation`. `sizes` is a restored split's slot
    /// sizes, in the same order as `names`, and is only a starting point: an
    /// absent or short list leaves that slot to share the space evenly.
    pub(crate) fn stack_layout(
        &self,
        names: &[PanelName],
        sizes: Option<&[Pixels]>,
        cx: &App,
    ) -> Option<DockLayout> {
        let first = names.first()?;
        let size_at = |ix: usize| sizes.and_then(|sizes| sizes.get(ix).copied());
        let split = match self.center_orientation {
            CenterOrientation::Stacked => DockLayout::v_split(),
            CenterOrientation::SideBySide => DockLayout::h_split(),
        };
        let mut split = split.child(self.tabs_layout(&[*first], cx), size_at(0));
        for (ix, name) in names[1..].iter().enumerate() {
            split = split.child(self.tabs_layout(&[*name], cx), size_at(ix + 1));
        }
        Some(split)
    }

    /// Flip the centre between stacked and side by side, then persist it.
    ///
    /// The dock already stores the split's axis, so this rebuilds the centre with
    /// the new direction (even slots: a height proportion means nothing as a
    /// width) and writes the same layout file every other layout change does.
    /// A flipped orientation that is not saved would be lost on the next launch.
    pub(crate) fn toggle_center_orientation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.center_orientation = self.center_orientation.flipped();
        log::debug!(
            target: "restbook_app::ui",
            "centre split: {:?}",
            self.center_orientation
        );
        let center = super::persistence::panel_names(Some(&self.dock.read(cx).dump(cx).center));
        if let Some(layout) = self.stack_layout(&center, None, cx) {
            self.dock
                .update(cx, |dock, cx| dock.set_center(layout, window, cx));
        }
        self.layout_cache = None;
        self.persist_tabs(cx);
        self.push_center_orientation(cx);
        // The flip changes how much centre room the docks leave, so the fit is
        // re-derived before the message names the new orientation.
        self.sync_dock_fit(window, cx);
        let message = match self.center_orientation {
            CenterOrientation::Stacked => "Stacked",
            CenterOrientation::SideBySide => "Side by side",
        };
        self.flash(chrome::FlashKind::Success, message, cx);
    }

    /// Hand the centre's orientation to the response pane, which draws the
    /// split toggle's glyph. The canvas owns the split; the pane only mirrors the
    /// next action, so this is called after every install, reset, and flip.
    pub(crate) fn push_center_orientation(&mut self, cx: &mut Context<Self>) {
        let side = self.center_orientation == CenterOrientation::SideBySide;
        self.panes
            .response
            .update(cx, |panel, cx| panel.set_center_side_by_side(side, cx));
    }
    /// Show a confirmation in the bottom-right for a few seconds.
    ///
    /// Actions routed through an event arrive without a `Window`, so
    /// `push_notification` is unavailable. A new flash replaces the pending clear,
    /// so two quick saves do not leave the first one's timer to wipe the second.
    pub(crate) fn flash(
        &mut self,
        kind: chrome::FlashKind,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        // Replacing the whole flash is the point: the new message and its kind
        // must land together, never one from each action.
        self.set_flash(kind, message.into());
        self.pending_notice = self.flash.clone();
        cx.notify();
    }

    /// Hand the parked flash to the kit's notification layer.
    ///
    /// A notification is window-level, and most paths that report an outcome have
    /// no `Window`, so [`Self::flash`] parks the message and this issues it from
    /// render. The kit stacks these above dialogs.
    pub(crate) fn issue_notice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(flash) = self.pending_notice.take() else {
            return;
        };
        let mut notification = match flash.kind {
            chrome::FlashKind::Success => Notification::success(flash.message.clone()),
            chrome::FlashKind::Error => Notification::error(flash.message.clone()),
        };
        // One id for every workspace notice, so a second message replaces the
        // first instead of stacking: that is the rule the hand-drawn toast had
        // and the one a user expects from a confirmation.
        notification = notification.id::<WorkspaceNotice>();
        if let Some(action) = &flash.action {
            let on_click = action.on_click.clone();
            let label = action.label.clone();
            notification = notification.action(move |_, _, _| {
                let on_click = on_click.clone();
                Button::new("workspace-flash-action")
                    .label(label.clone())
                    .ghost()
                    .compact()
                    .on_click(move |_, window, cx| on_click(window, cx))
            });
            // An undo offer must not vanish before it can be clicked.
            notification = notification.autohide(false);
        }
        window.push_notification(notification, cx);
    }

    /// [`Self::flash`] for a dialog that holds only a weak handle here.
    ///
    /// Settings reports the outcome of a folder choice from inside its own
    /// callback, where the workspace is reached as `WeakEntity`; without this
    /// the message would have to be dropped or routed through a second action.
    pub(crate) fn notice_flash(
        &mut self,
        kind: chrome::FlashKind,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.flash(kind, message, cx);
    }

    /// Offer a one-step undo for the delete that just landed.
    ///
    /// The captured value rides in the action's callback: when the toast
    /// expires, or a later flash replaces it, the callback and its snapshot go
    /// with it. That is the whole pending-undo lifecycle.
    pub(crate) fn flash_undo(&mut self, deleted: Deleted, cx: &mut Context<Self>) {
        let action = {
            let deleted = deleted.clone();
            let workspace = cx.entity().downgrade();
            // A fast second click must not apply the same snapshot twice; an
            // item insert would land a duplicate where a collection upsert
            // would merely be redundant.
            let used = Cell::new(false);
            chrome::FlashAction::new("Undo", move |window, cx| {
                if used.replace(true) {
                    return;
                }
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.undo_delete(deleted.clone(), window, cx)
                    })
                    .ok();
            })
        };
        self.set_flash(chrome::FlashKind::Success, deleted.deleted_message().into());
        if let Some(flash) = &mut self.flash {
            flash.action = Some(action);
        }
        self.pending_notice = self.flash.clone();
        cx.notify();
    }

    /// The state half of [`Self::flash`], split out so the replace-in-place
    /// rule can be tested without a window to push a notification to.
    pub(crate) fn set_flash(&mut self, kind: chrome::FlashKind, message: SharedString) {
        self.flash = Some(chrome::Flash {
            kind,
            message,
            action: None,
        });
    }

    /// Dismiss the current toast and retire whatever undo it offered. Every
    /// destructive act does this first, so a new delete never leaves the
    /// previous one's Undo looking like it belongs to the new message.
    pub(crate) fn dismiss_flash(&mut self) {
        self.flash = None;
        self.pending_notice = None;
    }

    /// Put a captured value back after the toast's Undo was clicked, then
    /// refresh the pane that shows it.
    pub(crate) fn undo_delete(
        &mut self,
        deleted: Deleted,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_flash();
        log::info!(
            target: "restbook_app::action",
            "undo: restore {}",
            deleted.noun()
        );

        let outcome = match &deleted {
            // `Variables` owns the environment list and the active scope, so
            // the restore goes through it rather than behind its cache.
            Deleted::Environment {
                environment,
                was_active,
            } => {
                self.variables.update(cx, |variables, cx| {
                    variables.save_environment(environment.clone(), cx);
                    if *was_active {
                        variables.set_active(Some(environment.name.clone()), cx);
                    }
                });
                if self
                    .store
                    .environments()
                    .map(|environments| {
                        environments
                            .iter()
                            .any(|each| each.name == environment.name)
                    })
                    .unwrap_or(false)
                {
                    Ok(())
                } else {
                    Err(restbook_core::Error::Store(
                        "the environment was not written".to_owned(),
                    ))
                }
            }
            _ => deleted.restore(self.store.as_ref()),
        };

        match outcome {
            Ok(()) => {
                if !matches!(deleted, Deleted::Environment { .. }) {
                    self.reload_collections(window, cx);
                }
                self.flash(chrome::FlashKind::Success, deleted.restored_message(), cx);
            }
            Err(error) => {
                log::error!(
                    target: "restbook_app::action",
                    "undo restore failed: {error}"
                );
                self.flash(
                    chrome::FlashKind::Error,
                    format!(
                        "Couldn't restore the {}. {error}",
                        deleted.noun().to_lowercase()
                    ),
                    cx,
                );
            }
        }
    }

    /// Make the Collections pane re-read the tree after an undo restored a
    /// collection or an item.
    ///
    /// There is no public reload hook, but `delete_collection` always re-reads the
    /// store and deleting an unknown id is a documented no-op. `REFRESH_ID` is
    /// generated-safe, and its own notice is replaced by the caller's undo result.
    pub(crate) fn reload_collections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const REFRESH_ID: &str = "restbook-refresh-only";
        self.panes.collections.update(cx, |panel, cx| {
            panel.delete_collection(REFRESH_ID, window, cx)
        });
    }

    /// Route a pane's confirmation to the toast. The panes and the workspace share
    /// one `Notice` type so every mutation reports the same way.
    ///
    /// This is also the one place every user action is logged: every pane emits a
    /// `Notice` when it finishes, so one line here covers creating, saving,
    /// deleting, renaming, and any store failure a bug report needs.
    pub(crate) fn notice(&mut self, notice: &Notice, cx: &mut Context<Self>) {
        match notice {
            Notice::Done(message) => log::debug!(target: "restbook_app::action", "{message}"),
            Notice::Failed(message) => log::error!(target: "restbook_app::action", "{message}"),
        }
        let flash = chrome::Flash::from_notice(notice);
        self.flash(flash.kind, flash.message, cx);
    }

    /// Step the zoom by a multiplicative `factor`, persist it, and tell every
    /// pane to rebuild its cached palette and metrics.
    pub(crate) fn zoom_by(&mut self, factor: f32, window: &mut Window, cx: &mut Context<Self>) {
        let next = theme::zoom(cx) * factor;
        self.set_zoom(next, window, cx);
    }

    /// Apply a zoom level, persist it to `settings.json`, and fan the change out
    /// to every pane. Zoom is a multiplier over the theme's metric block, so a
    /// pane redraws from the same code path as a theme switch.
    pub(crate) fn set_zoom(&mut self, zoom: f32, window: &mut Window, cx: &mut Context<Self>) {
        let applied = theme::set_zoom(cx, zoom);
        let mut next = self.settings.read(cx).current().clone();
        if next.zoom != applied {
            next.zoom = applied;
            self.settings
                .update(cx, |settings, cx| settings.update(next, cx));
        }
        self.refresh_theme(window, cx);
    }

    /// Re-read the theme in every pane after a theme, zoom, or appearance
    /// change. The workspace is the only thing that holds every pane, so it is
    /// the only thing that can redraw them together.
    pub(crate) fn focus_panel(&self, name: PanelName, window: &mut Window, cx: &mut App) {
        self.pane_focus_handle(name, cx).focus(window, cx);
    }

    /// The focus handle a pane's root carries. Kept in one place so focusing and
    /// asking which pane is focused cannot disagree.
    pub(crate) fn pane_focus_handle(&self, name: PanelName, cx: &App) -> FocusHandle {
        match name {
            PanelName::History => self.panes.history.read(cx).focus_handle(cx),
            PanelName::Environment => self.panes.environment.read(cx).focus_handle(cx),
            PanelName::Collections => self.panes.collections.read(cx).focus_handle(cx),
            PanelName::Request => self.panes.request.read(cx).focus_handle(cx),
            PanelName::Response => self.panes.response.read(cx).focus_handle(cx),
        }
    }

    /// Move focus `step` panes along [`PanelName::all`], wrapping at both ends.
    ///
    /// A pane no dock is showing is revealed before it takes focus, the same
    /// rule `FocusPanel` follows: History and Environment are closed by default,
    /// and focusing one that is not painted would aim at nothing.
    pub(crate) fn focus_relative_pane(
        &mut self,
        step: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let order = PanelName::all();
        let current = order
            .iter()
            .position(|name| self.pane_focus_handle(*name, cx).is_focused(window));
        let index = match current {
            Some(index) => wrap_index(index, order.len(), step),
            // Nothing focused yet: forward starts at the first pane, backward at
            // the last, so the first press always lands somewhere visible.
            None if step >= 0 => 0,
            None => order.len() - 1,
        };
        let name = order[index];
        self.reveal_panel(name, window, cx);
        self.focus_panel(name, window, cx);
    }

    /// Close a pane wherever it is docked.
    ///
    /// This removes the panel from the tree; the entity itself stays alive, so
    /// Reset Layout can put it back. Closing a centre pane is allowed for the
    /// same reason, and Reset Layout is the way back.
    pub(crate) fn close_panel(
        &mut self,
        name: PanelName,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        log::debug!(target: "restbook_app::ui", "close panel: {}", name.as_str());
        match name {
            PanelName::History => {
                let pane = self.panes.history.clone();
                self.dock
                    .update(cx, |dock, cx| dock.remove_panel(pane, window, cx));
            }
            PanelName::Environment => {
                let pane = self.panes.environment.clone();
                self.dock
                    .update(cx, |dock, cx| dock.remove_panel(pane, window, cx));
            }
            PanelName::Collections => {
                let pane = self.panes.collections.clone();
                self.dock
                    .update(cx, |dock, cx| dock.remove_panel(pane, window, cx));
            }
            PanelName::Request => {
                let pane = self.panes.request.clone();
                self.dock
                    .update(cx, |dock, cx| dock.remove_panel(pane, window, cx));
            }
            PanelName::Response => {
                let pane = self.panes.response.clone();
                self.dock
                    .update(cx, |dock, cx| dock.remove_panel(pane, window, cx));
            }
        }
    }

    /// Whether `name` already has a dock toggle in the status bar's corners.
    ///
    /// The View menu lists only the panes this returns `false` for; a pane with
    /// a corner control does not need a second route one menu deep. The two
    /// toggles are `toggle_sidebar` (Collections) and `toggle_tools` (History
    /// and Environment), each sitting under the dock it opens.
    pub(crate) fn has_corner_toggle(name: PanelName) -> bool {
        matches!(
            name,
            PanelName::Collections | PanelName::History | PanelName::Environment
        )
    }

    /// Whether `name` is the pane a dock is currently showing: the dock is
    /// open, it holds the pane, and the pane's group has it in front. The
    /// centre has no open flag, so it always counts as open. This is what the
    /// View menu's checkmark reports.
    pub(crate) fn panel_visible(&self, name: PanelName, cx: &App) -> bool {
        let id = self.panel_id(name);
        let dock = self.dock.read(cx);
        [
            DockPlacement::Left,
            DockPlacement::Right,
            DockPlacement::Center,
        ]
        .into_iter()
        .any(|placement| {
            if placement != DockPlacement::Center && !dock.is_dock_open(placement) {
                return false;
            }
            dock.layout(placement).is_some_and(|tree| {
                let Some(node) = tree.find_panel_node(id) else {
                    return false;
                };
                let Some(node) = tree.find_node(node) else {
                    return false;
                };
                match node.kind() {
                    PaneRef::Tabs { panels, active_ix } => panels.get(active_ix) == Some(&id),
                    PaneRef::Split { .. } => false,
                }
            })
        })
    }

    /// Bring a pane to the front.
    ///
    /// Collections is the left dock alone; History and Environment live in the
    /// wider right-hand tools dock. Rebuilding the dock as a single tab group is
    /// deliberate: a pane the user asks for should not be hidden behind another.
    pub(crate) fn reveal_panel(
        &mut self,
        name: PanelName,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        log::debug!(target: "restbook_app::ui", "reveal panel: {}", name.as_str());

        match name {
            PanelName::Collections => {
                self.dock_intent.left = true;
                let layout = self.tabs_layout(&[PanelName::Collections], cx);
                self.dock.update(cx, |dock, cx| {
                    dock.set_dock(DockPlacement::Left, layout, window, cx);
                    dock.set_dock_size(
                        DockPlacement::Left,
                        px(HISTORY_WIDTH * theme::zoom(cx)),
                        window,
                        cx,
                    );
                    if !dock.is_dock_open(DockPlacement::Left) {
                        dock.toggle_dock(DockPlacement::Left, window, cx);
                    }
                });
            }
            PanelName::History | PanelName::Environment => {
                self.dock_intent.right = true;
                let names = [PanelName::History, PanelName::Environment];
                let active = names.iter().position(|each| *each == name).unwrap_or(0);
                let layout = names
                    .iter()
                    .fold(DockLayout::tabs(), |tabs, each| {
                        tabs.panel_view(self.panel_view(*each), cx)
                    })
                    .active_index(active);
                // The layout's `active_index` decides the tab on a fresh tree;
                // selecting by identity makes sure the requested pane is the
                // one in front even when the dock was already open. Without
                // this, clicking History while Environment was showing left
                // Environment in front, which read as a dead button.
                let selected = self.panel_id(name);
                self.dock.update(cx, |dock, cx| {
                    dock.set_dock(DockPlacement::Right, layout, window, cx);
                    dock.set_dock_size(
                        DockPlacement::Right,
                        px(TOOL_WIDTH * theme::zoom(cx)),
                        window,
                        cx,
                    );
                    // The tools dock has no tab bar to collapse it from once
                    // both panes are showing tabs, and a right dock cannot be
                    // reopened by dragging at zero width the way the bottom one
                    // can — this flag is what lets the titlebar icons and the
                    // tab menu close it again.
                    dock.set_dock_collapsible(DockPlacement::Right, true, window, cx);
                    if !dock.is_dock_open(DockPlacement::Right) {
                        dock.toggle_dock(DockPlacement::Right, window, cx);
                    }
                    dock.select_panel(selected, window, cx);
                });
            }
            // The centre panes are always mounted; there is nothing to reveal.
            PanelName::Request | PanelName::Response => {}
        }

        // A dock the user asked for should be visible even from a narrow window:
        // grow the window to the region minimums rather than open a dock the fit
        // would immediately collapse. The centre panes need none of this.
        if matches!(
            name,
            PanelName::Collections | PanelName::History | PanelName::Environment
        ) {
            self.ensure_docks_fit(window, cx);
        }
    }

    /// The titlebar's sidebar button. A closed sidebar keeps nothing on screen,
    /// so this is the only route back once it is shut — which is why it is
    /// always visible rather than living in a menu.
    pub(crate) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.dock_showing(DockPlacement::Left, cx);
        log::debug!(target: "restbook_app::ui", "titlebar: sidebar toggle, open={open}");
        if open {
            self.dock_intent.left = false;
            self.dock.update(cx, |dock, cx| {
                dock.toggle_dock(DockPlacement::Left, window, cx)
            });
            return;
        }
        self.dock_intent.left = true;
        self.reveal_panel(PanelName::Collections, window, cx);
        // Say it worked somewhere the user can see it. A dock that opens
        // off-screen (or under a stale layout) otherwise looks exactly like a
        // button that never fired.
        self.flash(chrome::FlashKind::Success, "Sidebar shown", cx);
    }

    /// The titlebar's tools button: one control for the whole right-hand dock,
    /// so opening and closing History plus Environment never depends on which
    /// of the two is in front.
    pub(crate) fn toggle_tools(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.dock_showing(DockPlacement::Right, cx);
        log::debug!(target: "restbook_app::ui", "titlebar: tools toggle, open={open}");
        if open {
            self.dock_intent.right = false;
            self.dock.update(cx, |dock, cx| {
                dock.toggle_dock(DockPlacement::Right, window, cx)
            });
            return;
        }
        self.dock_intent.right = true;
        self.reveal_panel(PanelName::History, window, cx);
        let now_open = self.dock.read(cx).is_dock_open(DockPlacement::Right);
        log::debug!(target: "restbook_app::ui", "titlebar: tools revealed, open={now_open}");
        // `reveal_panel` opened the dock, but only a live read says so; if it
        // stayed shut, the flash reports that instead of claiming success.
        let message = if now_open {
            "History and Environment shown"
        } else {
            "Could not show the tools dock"
        };
        let kind = if now_open {
            chrome::FlashKind::Success
        } else {
            chrome::FlashKind::Error
        };
        self.flash(kind, message, cx);
    }

    /// Show exactly the docks the window has room for.
    ///
    /// The user's intent is fixed; the window width decides how much of it can
    /// be honoured. An auto collapse or restore is suppressed from `layout.json`
    /// because the saved open flags are the intent, not this frame's fit.
    pub(crate) fn sync_dock_fit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let width = f32::from(window.viewport_size().width);
        self.apply_dock_fit(width, window, cx);
    }

    /// Apply the fit for an explicit width. `window.resize` updates
    /// `viewport_size` only on the next bounds event, so a caller that just
    /// resized must judge the fit against the width it asked for, not the stale
    /// one still reported this frame.
    fn apply_dock_fit(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) {
        let zoom = theme::zoom(cx);
        let want =
            layout_policy::visible_docks(width, self.center_orientation, zoom, self.dock_intent);
        let dock = self.dock.clone();
        let (is_left, is_right) = {
            let dock = dock.read(cx);
            (
                dock.is_dock_open(DockPlacement::Left),
                dock.is_dock_open(DockPlacement::Right),
            )
        };
        if (is_left, is_right) == (want.left, want.right) {
            return;
        }
        self.suppress_layout_save = true;
        dock.update(cx, |dock, cx| {
            // `toggle_dock` reports "changed", so only toggle a dock that is
            // present and in the wrong state.
            if dock.has_dock(DockPlacement::Left)
                && dock.is_dock_open(DockPlacement::Left) != want.left
            {
                dock.toggle_dock(DockPlacement::Left, window, cx);
            }
            if dock.has_dock(DockPlacement::Right)
                && dock.is_dock_open(DockPlacement::Right) != want.right
            {
                dock.toggle_dock(DockPlacement::Right, window, cx);
            }
        });
        self.suppress_layout_save = false;
        cx.notify();
    }

    /// Grow the window so the docks the user asked for fit, then re-apply the
    /// fit. An explicit toggle at a narrow width must not be a dead button, so
    /// the window gives way to the region minimums rather than the reverse.
    fn ensure_docks_fit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let zoom = theme::zoom(cx);
        let needed = layout_policy::needed_width(self.center_orientation, self.dock_intent, zoom);
        let current = f32::from(window.viewport_size().width);
        let width = if current + 0.5 < needed {
            // Cap to the display's usable width: a request larger than the
            // monitor would otherwise open a window off every edge.
            let cap = display_width(window, cx).unwrap_or(f32::INFINITY);
            let width = needed.min(cap);
            let height = window.viewport_size().height;
            window.resize(size(px(width), height));
            width
        } else {
            current
        };
        // Judge the fit at the width we just asked for: `viewport_size` still
        // reports the old size this frame.
        self.apply_dock_fit(width, window, cx);
    }

    /// Whether a dock is on screen *and* holds something. A dock emptied by
    /// `ClosePanel` still counts as open, but clicking its toggle should reveal
    /// a pane again rather than fold away an empty strip.
    pub(crate) fn dock_showing(&self, placement: DockPlacement, cx: &App) -> bool {
        let dock = self.dock.read(cx);
        dock.is_dock_open(placement) && !dock.is_empty(placement, cx)
    }

    /// A pane's dock identity, so [`Self::reveal_panel`] can bring its tab to
    /// the front by identity instead of relying on the layout's remembered
    /// active index.
    pub(crate) fn panel_id(&self, name: PanelName) -> PanelId {
        let entity_id = match name {
            PanelName::History => self.panes.history.entity_id(),
            PanelName::Environment => self.panes.environment.entity_id(),
            PanelName::Collections => self.panes.collections.entity_id(),
            PanelName::Request => self.panes.request.entity_id(),
            PanelName::Response => self.panes.response.entity_id(),
        };
        PanelId::from(entity_id)
    }
}
