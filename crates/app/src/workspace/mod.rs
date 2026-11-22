//! The window root: engine handles, the dock layout, and the panes.
//!
//! The workspace owns what the panes do not: the engine handles, the shared
//! [`Document`], and the [`DockArea`] with its layout. Everything else is a pane,
//! built here rather than by the dock so it stays typed (`workspace.request()`
//! returns `Entity<RequestPanel>`); adding one is a name in [`PanelName`].

mod chrome;
mod delete;
mod history_sheet;
mod persistence;
mod send;
pub(crate) mod settings;
mod settings_view;
pub(crate) mod storage;
mod tabs;
mod updates;

pub(crate) use delete::{Deleted, PendingDelete};
pub(super) use persistence::{HISTORY_WIDTH, TOOL_WIDTH};
pub(crate) use tabs::{Tab, TabAction, TabEntry, TabId, TabState};

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::*;
use gpui_kit::component::dock::{
    panel_handle, DockArea, DockEvent, DockLayout, DockPlacement, DockSkin, PaneRef, PanelId,
};
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{h_flex, v_flex, WindowExt as _};
use gpui_kit::*;

use restbook_core::{Origin, RequestDef, Store};
use restbook_transport::ReqwestTransport;

use crate::actions::{
    self as actions, CloseTab, CopyCurl, NewTab, NextTab, OpenSettings, PreviousTab,
    ReopenClosedTab, ResetZoom, SaveRequestAs, ToggleCommandPalette, ToggleSidebar, ToggleTools,
    ZoomIn, ZoomOut,
};
use crate::document::Document;
use crate::icons::IconName;
use crate::logging;
use crate::notice::Notice;
use crate::panels::collections::Event as CollectionsEvent;
use crate::panels::command_palette::{self, Command};
use crate::panels::environment::Event as EnvironmentEvent;
use crate::panels::history::Event as HistoryEvent;
use crate::panels::request::{self as request_panel, Event as RequestEvent};
use crate::panels::response::Event as ResponseEvent;
use crate::panels::{
    CollectionsPanel, EnvironmentPanel, HistoryPanel, RequestPanel, ResponsePanel,
};
use crate::theme::{self, Metrics, Palette};
use crate::variables::Variables;

/// The dock's identity in the persisted layout. Bump the version when the
/// meaning of the tree changes, not when a panel is added.
const DOCK_ID: &str = "restbook";
const DOCK_VERSION: usize = 1;

/// The layout file lives beside the history in the same data directory.
const LAYOUT_FILE: &str = "layout.json";

pub(crate) struct Workspace {
    /// One transport for the life of the app: it owns the tokio runtime and
    /// the pooled reqwest clients, both of which are expensive to build.
    transport: Arc<ReqwestTransport>,
    store: Arc<dyn Store>,
    /// What the titlebar calls this session: the workspace directory name, or
    /// the app name when running against the per-user database.
    workspace_name: String,
    /// The workspace root, when one is open. `None` means the per-user database
    /// is in use, so collections are not shareable from this window.
    workspace_root: Option<PathBuf>,
    /// The request, and the outcome of sending it. One per window; the tab
    /// strip swaps its contents in and out so panes can keep one entity.
    document: Entity<Document>,
    /// Environments and globals, the scopes a send resolves against.
    variables: Entity<Variables>,
    /// Application preferences, shared with every pane that has an app-level
    /// default to honor (today, the response pane's pretty-print start state).
    settings: Entity<settings::Settings>,
    /// The panes this window has open.
    panes: Panes,
    /// One entry per open request tab. Only the active tab lives in the
    /// document; the others are snapshots taken when the tab was left.
    tabs: TabState,
    /// Recently closed tabs, newest last, for Reopen Closed Tab. Process-local
    /// on purpose: a reopened draft is recovered from this stack, not from a
    /// file, so it is cleared on launch.
    closed: Vec<Tab>,
    /// The tab whose send is on the wire, by [`Tab::id`]. At most one send runs
    /// at a time, and it is named by identity rather than by index: closing a
    /// tab shifts every index after it, and the response still has to find the
    /// tab that asked for it.
    sending: Option<u64>,
    dock: Entity<DockArea>,
    layout_path: PathBuf,
    save_task: Option<Task<()>>,
    /// The debounced draft write, separate from the layout's so typing and a
    /// drag do not cancel each other.
    draft_task: Option<Task<()>>,
    /// Bumped on every document notify. The live `draft_task` compares it
    /// against the value it last saw instead of a fresh timer task being spawned
    /// per tick: a send notifies the document ~10-20 times a second, and only
    /// the last edit should start the debounce.
    draft_gen: u64,
    /// The last message an action reported. Kept so [`Self::dismiss_flash`] has
    /// something to retire and so `issue_notice` can re-read it; the kit owns the
    /// visible toast, so this no longer needs a timer.
    flash: Option<chrome::Flash>,
    /// A message waiting for a `Window` to be shown in. `flash` is called from
    /// paths that have none, and a notification is window-level, so the message
    /// is parked here and `render` drains it.
    pending_notice: Option<chrome::Flash>,
    /// The window root's focus target: keyboard actions dispatched while no
    /// pane owns focus still reach the workspace's handlers.
    focus_handle: FocusHandle,
    /// The name field of the first-save dialog. It lives here, not in the
    /// dialog's content closure, because that closure is rebuilt on every notify
    /// and an input created inside it would lose the caret mid-typing.
    save_name: Entity<InputState>,
    /// A draft asked to be saved, but the dialog needs a window and the event the
    /// request arrived on had none. The next render opens the dialog.
    pending_save: bool,
    /// Rows the response pane wants shown as a history sheet. Set by its event
    /// and consumed by render, which is the only place with a window to open a
    /// sheet on.
    pending_history: Option<Vec<restbook_core::HistoryHit>>,
    /// What a sidebar pane asked to delete, waiting for render to raise the
    /// confirmation dialog. Events carry no window, so the ask is parked here
    /// and consumed the same way `pending_save` and `pending_history` are.
    pending_delete: Option<PendingDelete>,
    /// A newer release the launch check found. The check runs in a task with no
    /// window, so the offer is parked here and raised by render, the same way
    /// `pending_save` and `pending_delete` are.
    pending_update: Option<updates::PendingUpdate>,
    /// Set by [`Self::flush`]. A normal close writes in
    /// `on_window_should_close` and the quit observers then fire; the second
    /// write would bump `updated_at` and duplicate the work.
    flushed: bool,
}

/// Nothing: the type is the identity. The kit keys in-app notifications by
/// `TypeId`, so one marker replaces the previous workspace notice instead of
/// stacking a second one under it.
struct WorkspaceNotice;

/// A request rendered into a value that can be compared and stored: the same
/// request always yields the same text, and any edit yields different text.
/// A `RequestDef` is not comparable, which is why the unsaved marker works on
/// this form rather than on two requests.
pub(crate) struct RequestStamp;

impl RequestStamp {
    pub(crate) fn of(request: &RequestDef) -> String {
        serde_json::to_string(request).unwrap_or_default()
    }
}

/// Whether closing a tab would throw work away. A tab that was never saved is
/// only worth a prompt once it holds something: a blank page is not unsaved
/// work, but a draft the sidebar lists is.
fn close_needs_confirmation(saved: Option<&str>, current: &str, blank: bool) -> bool {
    match saved {
        Some(baseline) => baseline != current,
        None => !blank,
    }
}

/// The words a bulk close uses. One dialog can stand for many tabs, so the
/// summary has to say two things at once: how many are going, and how many of
/// those are holding unsaved work.
pub(super) fn close_tabs_copy(count: usize, unsaved: usize) -> (String, String, &'static str) {
    (
        format!("Close {count} tabs?"),
        format!("{unsaved} tabs have unsaved changes. Closing them will discard those changes."),
        "Discard changes",
    )
}

/// The tab after stepping `delta` from `current`, wrapping around at either end
/// of `len`. Empty is the caller's business; it never asks for an index.
fn wrap_index(current: usize, len: usize, delta: isize) -> usize {
    (current as isize + delta).rem_euclid(len as isize) as usize
}

/// The open panes, typed. They outlive every layout change: the dock only ever
/// holds these entities, and none is reconstructed behind the workspace's back.
struct Panes {
    history: Entity<HistoryPanel>,
    request: Entity<RequestPanel>,
    response: Entity<ResponsePanel>,
    environment: Entity<EnvironmentPanel>,
    collections: Entity<CollectionsPanel>,
}

/// Every pane the workspace can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelName {
    History,
    Request,
    Response,
    Environment,
    Collections,
}

impl PanelName {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PanelName::History => "history",
            PanelName::Request => "request",
            PanelName::Response => "response",
            PanelName::Environment => "environment",
            PanelName::Collections => "collections",
        }
    }

    /// The name a menu shows. Kept apart from [`Self::as_str`], which is the
    /// persisted spelling and must not change with a label edit.
    fn label(self) -> &'static str {
        match self {
            PanelName::History => "History",
            PanelName::Request => "Request",
            PanelName::Response => "Response",
            PanelName::Environment => "Environment",
            PanelName::Collections => "Collections",
        }
    }

    /// Every pane, in the order the View menu lists them.
    ///
    /// [`Self::parse`] searches this same list, so a new variant cannot be
    /// read from a saved layout while staying invisible to the menu.
    pub(crate) fn all() -> [PanelName; 5] {
        [
            PanelName::Collections,
            PanelName::History,
            PanelName::Environment,
            PanelName::Request,
            PanelName::Response,
        ]
    }

    fn parse(name: &str) -> Option<Self> {
        Self::all().into_iter().find(|panel| panel.as_str() == name)
    }
}

impl Workspace {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state_dir = storage::data_dir();
        let storage::StoreChoice {
            store,
            name: workspace_name,
            workspace_root,
        } = storage::open_store(&state_dir);
        let transport =
            Arc::new(ReqwestTransport::new().expect("failed to start the restbook http runtime"));

        // Start on a fresh request every time. Drafts left open last session
        // stay in the store and are listed under Unsaved in the sidebar, so
        // they are one click away without the window opening on whatever
        // happened to be the last tab.
        let tabs = TabState::new(vec![Tab::blank()]);

        let (dock, _skin) = DockSkin::dock_area(DOCK_ID, Some(DOCK_VERSION), window, cx);

        let document = cx.new(|_| Document::new());
        let variables = cx.new(|cx| Variables::new(store.clone(), cx));
        let save_name = cx.new(|cx| InputState::new(window, cx).placeholder("Request name"));
        let settings = cx.new(|_| settings::Settings::new(state_dir.join(settings::SETTINGS_FILE)));

        // Install the theme *before* any pane is built: every pane snapshots
        // its palette with `Palette::new(cx)` in its constructor, so the tokens
        // have to be in place first or the window opens on the component
        // defaults and only corrects itself on the next theme change.
        let current = settings.read(cx).current().clone();
        theme::ThemeRegistry::install(
            cx,
            &storage::load_theme_text(current.theme.as_deref()),
            current.zoom,
            current.appearance,
        );
        apply_appearance(window, cx);
        let panes = Panes {
            history: cx.new(|cx| HistoryPanel::new(store.clone(), settings.clone(), window, cx)),
            request: cx
                .new(|cx| RequestPanel::new(document.clone(), variables.clone(), window, cx)),
            response: cx.new(|cx| {
                ResponsePanel::new(
                    document.clone(),
                    store.clone(),
                    settings.clone(),
                    window,
                    cx,
                )
            }),
            environment: cx
                .new(|cx| EnvironmentPanel::new(variables.clone(), document.clone(), window, cx)),
            collections: cx
                .new(|cx| CollectionsPanel::new(store.clone(), document.clone(), window, cx)),
        };

        let mut workspace = Self {
            transport,
            store,
            workspace_name,
            workspace_root,
            document,
            variables,
            settings,
            panes,
            tabs,
            closed: Vec::new(),
            sending: None,
            dock,
            layout_path: state_dir.join(LAYOUT_FILE),
            save_task: None,
            draft_task: None,
            draft_gen: 0,
            flash: None,
            pending_notice: None,
            focus_handle: cx.focus_handle(),
            save_name,
            pending_save: false,
            pending_history: None,
            pending_delete: None,
            pending_update: None,
            flushed: false,
        };

        workspace.listen(window, cx);
        workspace.install_layout(window, cx);
        // The saved layout is read for names only, so the strip is told what
        // is open here rather than by whichever install path ran.
        workspace.push_tabs(cx);
        workspace.save_on_close(window, cx);
        workspace.flush_on_quit(cx);
        // After the first frame's state is in place, so an update notice is a
        // toast over a working window rather than a reason not to open one.
        workspace.check_updates_on_launch(cx);
        workspace
    }

    /// Subscribe to the panes. A pane reports what it cannot do itself; only
    /// the workspace knows who can.
    fn listen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.subscribe(&self.panes.history, |this, _, event: &HistoryEvent, cx| {
            log::info!(target: "restbook_app::ui", "{}", event.summary());
            let id = match event {
                HistoryEvent::Picked { id } => *id,
                HistoryEvent::Delete { id } => {
                    this.pending_delete = Some(PendingDelete::HistorySend(*id));
                    cx.notify();
                    return;
                }
                HistoryEvent::ClearAll => {
                    this.pending_delete = Some(PendingDelete::AllHistory);
                    cx.notify();
                    return;
                }
            };
            match this.store.get_history(id) {
                Ok(Some(entry)) => {
                    log::info!(
                        target: "restbook_app::ui",
                        "history pick: restored {} {}",
                        entry.request.method,
                        entry.request.url,
                    );
                    // The full request goes to the editor, so params, headers,
                    // body, and auth come back — not just method and URL. The
                    // stored response is shown without a send.
                    this.panes.request.update(cx, |panel, cx| {
                        panel.adopt_request(entry.request.clone(), cx)
                    });
                    // The editor only queues the request for its next render, so
                    // the document has to be told now. Send reads the document,
                    // and without this the button stays inert until a second
                    // click re-fires the editor's observer.
                    this.document.update(cx, |document, cx| {
                        document.request = entry.request;
                        document.response = Some(entry.response);
                        document.error = None;
                        cx.notify();
                    });
                }
                Ok(None) => {
                    // The row vanished between the search and the click (for
                    // example, a store replaced on disk). Nothing to restore.
                    log::warn!(target: "restbook_app::ui", "history pick: id={id} vanished");
                }
                Err(error) => {
                    log::error!(target: "restbook_app::ui", "history pick: id={id} failed: {error}");
                    this.document.update(cx, |document, cx| {
                        document.error = Some(error.to_string());
                        cx.notify();
                    });
                }
            }
        })
        .detach();

        cx.subscribe(
            &self.panes.response,
            |this, _, event: &ResponseEvent, cx| {
                log::info!(target: "restbook_app::ui", "{}", event.summary());
                match event {
                    // The pane reports the history request instead of opening a
                    // sheet itself: where an overlay sits is the canvas's decision.
                    // Render opens it, because this event carries no window.
                    ResponseEvent::ShowHistory { hits } => {
                        this.pending_history = Some(hits.clone());
                        cx.notify();
                    }
                }
            },
        )
        .detach();

        cx.subscribe(&self.panes.request, |this, _, event: &RequestEvent, cx| {
            if let Some(line) = event.summary() {
                log::info!(target: "restbook_app::ui", "{line}");
            }
            match event {
                RequestEvent::Send => this.send(cx),
                RequestEvent::Cancel => {
                    this.document.update(cx, |document, cx| document.cancel(cx))
                }
                RequestEvent::SelectTab(id) => this.select_tab_id(*id, cx),
                RequestEvent::NewTab => this.new_tab(cx),
                RequestEvent::CloseTab(id) => this.confirm_close_tab(*id, cx),
                RequestEvent::TabAction(action) => this.handle_tab_action(*action, cx),
                RequestEvent::Save => this.save_active(cx),
                RequestEvent::SaveAs => this.save_active_as(cx),
                RequestEvent::Imported => this.imported(cx),
                RequestEvent::Notice(notice) => this.notice(notice, cx),
                RequestEvent::FetchToken(config) => this.fetch_token(config.clone(), cx),
            }
        })
        .detach();

        // A saved request opens like a history pick: the editor adopts it, and
        // the next render writes every tab. Opening it is also what makes the
        // tab "saved": the stored request becomes the baseline the unsaved
        // marker compares against, so the dot only appears on a later edit.
        cx.subscribe(
            &self.panes.collections,
            |this, _, event: &CollectionsEvent, cx| {
                if let Some(line) = event.summary() {
                    log::info!(target: "restbook_app::ui", "{line}");
                }
                match event {
                    CollectionsEvent::Open { node, request } => {
                        this.open_saved(node.clone(), request.clone(), cx);
                    }
                    CollectionsEvent::OpenDraft { id } => {
                        // The tab that holds the draft, if it is still open.
                        // Drafts are no longer restored as tabs at startup, so a
                        // row that has no tab is reopened from the store instead
                        // of doing nothing.
                        if let Some(index) = this.tabs.iter().position(|tab| tab.draft == Some(*id))
                        {
                            this.select_tab(index, cx);
                        } else if let Some(draft) = this
                            .store
                            .drafts()
                            .unwrap_or_default()
                            .into_iter()
                            .find(|draft| draft.id == *id)
                        {
                            this.open_draft(draft, cx);
                        }
                    }
                    CollectionsEvent::Notice(notice) => this.notice(notice, cx),
                    CollectionsEvent::ConfirmDelete(target) => {
                        this.pending_delete = Some(PendingDelete::Node(target.clone()));
                        cx.notify();
                    }
                }
            },
        )
        .detach();

        // A failed environment write has no window of its own, so the variables
        // entity hands the message up and the workspace shows it through the one
        // notice path every pane uses.
        cx.subscribe(&self.variables, |this, _, notice: &Notice, cx| {
            this.notice(notice, cx)
        })
        .detach();

        // The Environment pane's only intent is a confirmed delete, which the
        // workspace turns into an alert dialog the same way it does for nodes.
        cx.subscribe(
            &self.panes.environment,
            |this, _, event: &EnvironmentEvent, cx| {
                let EnvironmentEvent::ConfirmDelete(name) = event;
                this.pending_delete = Some(PendingDelete::Environment(name.clone()));
                cx.notify();
            },
        )
        .detach();

        // Every edit mirrors into the document, so observing it is the cue to
        // re-persist the active tab after a pause, the way a drag cues the
        // layout write.
        cx.observe(&self.document, |this, _, cx| this.schedule_draft_save(cx))
            .detach();

        cx.subscribe(&self.dock, |this, _, event, cx| {
            if matches!(event, DockEvent::LayoutChanged) {
                this.schedule_save(cx);
            }
        })
        .detach();

        // A change to the theme, appearance, or zoom in the settings dialog is
        // applied here rather than by the dialog: the dialog has no window to
        // switch the palette on, and the workspace is what holds every pane. The
        // observer fires on any settings write, so it re-derives the theme from
        // the current values and fans the redraw out.
        cx.observe_in(&self.settings, window, |this, _, window, cx| {
            this.reload_theme(window, cx);
        })
        .detach();

        // Follow the OS appearance live while the setting is `System`: the
        // platform flips `WindowAppearance` when the user changes it, and the
        // theme has to re-resolve because `ThemeMode` itself has no System
        // variant to track it. A pinned Light/Dark ignores this.
        cx.observe_window_appearance(window, |this, window, cx| {
            if theme::appearance(cx) == theme::Appearance::System {
                this.reload_theme(window, cx);
            }
        })
        .detach();
    }

    /// Re-install the theme from the current settings and redraw every pane.
    /// Called when the user changes the theme, appearance, or zoom in Settings.
    fn reload_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.settings.read(cx).current().clone();
        theme::ThemeRegistry::install(
            cx,
            &storage::load_theme_text(current.theme.as_deref()),
            current.zoom,
            current.appearance,
        );
        self.refresh_theme(window, cx);
    }

    /// Ask the release channel once after launch, when the preference allows it
    /// and a channel is configured.
    ///
    /// A failed check is logged, not shown: an unreachable update server must
    /// not be the first thing a user sees. A newer version becomes an offer the
    /// user can act on without visiting Settings, unless that version was
    /// skipped. The task has no window, so the offer is parked for render.
    fn check_updates_on_launch(&mut self, cx: &mut Context<Self>) {
        if !self.settings.read(cx).current().check_updates {
            return;
        }
        let Some(url) = crate::updater::manifest_url() else {
            return;
        };

        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move { crate::updater::check(&url) })
                .await;
            this.update(cx, |workspace, cx| match outcome {
                Ok(crate::updater::Outcome::Available {
                    latest,
                    notes,
                    asset,
                }) => {
                    let current = workspace.settings.read(cx).current();
                    if !updates::should_offer(current, &latest) {
                        return;
                    }
                    // No asset for this platform means newer exists but this
                    // machine cannot install it, so an offer would be a dead
                    // end. Settings still names the version.
                    if let Some(asset) = asset {
                        workspace.pending_update = Some(updates::PendingUpdate {
                            latest,
                            notes,
                            asset,
                        });
                        cx.notify();
                    }
                }
                // Up to date is the normal state and does not deserve a toast on
                // every launch; a failed check is not the user's problem.
                Ok(_) => {}
                Err(error) => log::warn!("update check failed: {error}"),
            })
            .ok();
        })
        .detach();
    }
    /// A tab group of the named panes, in order.
    ///
    /// A pane goes in through [`panel_handle`], not as a bare entity: the
    /// handle carries the presentation half of the `Panel` trait with it, which
    /// is how a pane can say `title_bar(false)` and draw its own chrome.
    fn tabs_layout(&self, names: &[PanelName], cx: &App) -> DockLayout {
        names.iter().fold(DockLayout::tabs(), |tabs, name| {
            tabs.panel_view(self.panel_view(*name), cx)
        })
    }

    /// One pane as a dock handle, by name.
    fn panel_view(&self, name: PanelName) -> Arc<dyn gpui_kit::component::dock::BasePanelView> {
        match name {
            PanelName::History => panel_handle(self.panes.history.clone()),
            PanelName::Request => panel_handle(self.panes.request.clone()),
            PanelName::Response => panel_handle(self.panes.response.clone()),
            PanelName::Environment => panel_handle(self.panes.environment.clone()),
            PanelName::Collections => panel_handle(self.panes.collections.clone()),
        }
    }

    /// The named panes stacked vertically, top to bottom.
    ///
    /// `sizes` is a restored stack's slot sizes, in the same order as `names`.
    /// It is only a starting point: the user drags the handle from there, and
    /// an absent or short list leaves that slot unconstrained, which makes the
    /// split share the space evenly.
    fn stack_layout(
        &self,
        names: &[PanelName],
        sizes: Option<&[Pixels]>,
        cx: &App,
    ) -> Option<DockLayout> {
        let first = names.first()?;
        let size_at = |ix: usize| sizes.and_then(|sizes| sizes.get(ix).copied());
        let mut split = DockLayout::v_split().child(self.tabs_layout(&[*first], cx), size_at(0));
        for (ix, name) in names[1..].iter().enumerate() {
            split = split.child(self.tabs_layout(&[*name], cx), size_at(ix + 1));
        }
        Some(split)
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        // A new edit replaces the pending write, so a drag costs one save.
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(persistence::SAVE_DEBOUNCE)
                .await;
            this.update(cx, |workspace, cx| {
                workspace.save_task = None;
                persistence::save(&workspace.layout_path, &workspace.dock.read(cx).dump(cx));
            })
            .ok();
        }));
    }

    /// Show a confirmation in the bottom-right for a few seconds.
    ///
    /// Actions routed through an event arrive without a `Window`, so
    /// `push_notification` is unavailable; this is the replacement. A new flash
    /// replaces the pending clear, so two quick saves do not leave the first one's
    /// timer to wipe the second one's message.
    fn flash(
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
    /// A notification is window-level, and most of the paths that report an
    /// outcome have no `Window`, so [`Self::flash`] parks the message and this
    /// issues it from render. The kit stacks these above dialogs, which is what
    /// the hand-drawn toast could not do: it was part of the dock tree, so an
    /// open dialog covered it.
    fn issue_notice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn flash_undo(&mut self, deleted: Deleted, cx: &mut Context<Self>) {
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
    fn set_flash(&mut self, kind: chrome::FlashKind, message: SharedString) {
        self.flash = Some(chrome::Flash {
            kind,
            message,
            action: None,
        });
    }

    /// Dismiss the current toast and retire whatever undo it offered. Every
    /// destructive act does this first, so a new delete never leaves the
    /// previous one's Undo looking like it belongs to the new message.
    fn dismiss_flash(&mut self) {
        self.flash = None;
        self.pending_notice = None;
    }

    /// Put a captured value back after the toast's Undo was clicked, then
    /// refresh the pane that shows it.
    fn undo_delete(&mut self, deleted: Deleted, window: &mut Window, cx: &mut Context<Self>) {
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
    /// The pane has no public reload hook, but `delete_collection` always
    /// re-reads the store, and deleting an unknown id is a documented no-op.
    /// This ID cannot collide with a real one (ids are generated), so the call
    /// is only the refresh seam; its own "Deleted collection." notice is
    /// replaced by the undo result flashed by the caller.
    fn reload_collections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn notice(&mut self, notice: &Notice, cx: &mut Context<Self>) {
        match notice {
            Notice::Done(message) => log::info!(target: "restbook_app::action", "{message}"),
            Notice::Failed(message) => log::error!(target: "restbook_app::action", "{message}"),
        }
        let flash = chrome::Flash::from_notice(notice);
        self.flash(flash.kind, flash.message, cx);
    }

    /// Open the Settings dialog. It reads the settings entity the workspace
    /// owns, so a toggle takes effect everywhere at once; the workspace root is
    /// passed along so the dialog can show where data is being read from, and a
    /// weak self handle lets the dialog run the two data-clearing actions.
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        settings_view::open(
            self.settings.clone(),
            self.workspace_root.clone(),
            cx.entity().downgrade(),
            window,
            cx,
        );
    }

    /// Ask for confirmation before wiping every stored send. Settings closes
    /// first, so the alert is the only overlay left when it appears.
    pub(crate) fn request_clear_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        self.pending_delete = Some(PendingDelete::AllHistory);
        cx.notify();
    }

    /// Drop the shared cookie jar. This is the only reset short of restarting
    /// the app, and Settings is its one entry point.
    pub(crate) fn clear_cookies(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        restbook_core::Transport::clear_cookies(self.transport.as_ref());
        // A notification, not the workspace toast: the button that calls this
        // lives in the Settings dialog, and a window-level layer is the only
        // thing drawn above an open dialog.
        window.push_notification(Notification::success("Cookies cleared."), cx);
    }

    /// Open the command palette, routing every command back here.
    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.entity().downgrade();
        command_palette::open(window, cx, move |command, window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.run_command(command, window, cx)
                })
                .ok();
        });
    }

    fn run_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        log::info!(target: "restbook_app::ui", "command: {command:?}");
        match command {
            Command::Send => self.send(cx),
            Command::NewTab => self.new_tab(cx),
            Command::CloseTab => {
                if let Some(id) = self.tabs.active_id() {
                    self.confirm_close_tab(id, cx);
                }
            }
            Command::NextTab => self.select_relative(1, cx),
            Command::PreviousTab => self.select_relative(-1, cx),
            Command::ReopenClosedTab => self.reopen_closed_tab(cx),
            Command::FocusUrl => self
                .panes
                .request
                .update(cx, |panel, cx| panel.focus_url(window, cx)),
            Command::PasteCurl => self
                .panes
                .request
                .update(cx, |panel, cx| panel.paste_curl(window, cx)),
            Command::CopyCurl => self.copy_curl(cx),
            // Reveal first, then focus: History and Environment are not docked
            // by default, so focusing a pane no dock is showing would aim at
            // nothing. The centre panes are always mounted, so the reveal is a
            // no-op for them.
            Command::FocusPanel(name) => {
                self.reveal_panel(name, window, cx);
                self.focus_panel(name, window, cx);
            }
            Command::ClosePanel(name) => self.close_panel(name, window, cx),
            // The two pane commands are the status bar's two corner buttons, so
            // a palette entry and its button are one command: same scope (the
            // whole dock), same open *and* close behavior.
            Command::ToggleEnvironment => self.toggle_tools(window, cx),
            Command::ToggleCollections => self.toggle_sidebar(window, cx),
            Command::ResetLayout => self.reset_layout(window, cx),
            Command::OpenSettings => self.open_settings(window, cx),
            Command::ZoomIn => self.zoom_by(theme::ZOOM_STEP, window, cx),
            Command::ZoomOut => self.zoom_by(1.0 / theme::ZOOM_STEP, window, cx),
            Command::ResetZoom => self.set_zoom(1.0, window, cx),
        }
    }

    /// Step the zoom by a multiplicative `factor`, persist it, and tell every
    /// pane to rebuild its cached palette and metrics.
    fn zoom_by(&mut self, factor: f32, window: &mut Window, cx: &mut Context<Self>) {
        let next = theme::zoom(cx) * factor;
        self.set_zoom(next, window, cx);
    }

    /// Apply a zoom level, persist it to `settings.json`, and fan the change out
    /// to every pane. Zoom is a multiplier over the theme's metric block, so a
    /// pane redraws from the same code path as a theme switch.
    fn set_zoom(&mut self, zoom: f32, window: &mut Window, cx: &mut Context<Self>) {
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
    fn refresh_theme(&mut self, window: &mut Window, cx: &mut App) {
        apply_appearance(window, cx);
        self.panes
            .history
            .update(cx, |pane, cx| pane.refresh_theme(cx));
        self.panes
            .request
            .update(cx, |pane, cx| pane.refresh_theme(cx));
        self.panes
            .response
            .update(cx, |pane, cx| pane.refresh_theme(cx));
        self.panes
            .environment
            .update(cx, |pane, cx| pane.refresh_theme(cx));
        self.panes
            .collections
            .update(cx, |pane, cx| pane.refresh_theme(cx));
        cx.refresh_windows();
    }

    fn focus_panel(&self, name: PanelName, window: &mut Window, cx: &mut App) {
        let handle = match name {
            PanelName::History => self.panes.history.read(cx).focus_handle(cx),
            PanelName::Environment => self.panes.environment.read(cx).focus_handle(cx),
            PanelName::Collections => self.panes.collections.read(cx).focus_handle(cx),
            PanelName::Request => self.panes.request.read(cx).focus_handle(cx),
            PanelName::Response => self.panes.response.read(cx).focus_handle(cx),
        };
        handle.focus(window, cx);
    }

    /// Close a pane wherever it is docked.
    ///
    /// This removes the panel from the tree; the entity itself stays alive, so
    /// Reset Layout can put it back. Closing a centre pane is allowed for the
    /// same reason, and Reset Layout is the way back.
    fn close_panel(&mut self, name: PanelName, window: &mut Window, cx: &mut Context<Self>) {
        log::info!(target: "restbook_app::ui", "close panel: {}", name.as_str());
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
    fn has_corner_toggle(name: PanelName) -> bool {
        matches!(
            name,
            PanelName::Collections | PanelName::History | PanelName::Environment
        )
    }

    /// Whether `name` is the pane a dock is currently showing: the dock is
    /// open, it holds the pane, and the pane's group has it in front. The
    /// centre has no open flag, so it always counts as open. This is what the
    /// View menu's checkmark reports.
    fn panel_visible(&self, name: PanelName, cx: &App) -> bool {
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
    /// The left dock is the collection tree and nothing else; History and
    /// Environment live in the wider right-hand tools dock, where the
    /// environment's key/value rows and controls have room. Rebuilding a dock
    /// as a single tab group is deliberate: it is the arrangement the app
    /// ships with, and a pane the user asks for should not be hidden behind
    /// another one.
    fn reveal_panel(&mut self, name: PanelName, window: &mut Window, cx: &mut Context<Self>) {
        log::info!(target: "restbook_app::ui", "reveal panel: {}", name.as_str());

        match name {
            PanelName::Collections => {
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
    }

    /// The titlebar's sidebar button. A closed sidebar keeps nothing on screen,
    /// so this is the only route back once it is shut — which is why it is
    /// always visible rather than living in a menu.
    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.dock_showing(DockPlacement::Left, cx);
        log::info!(target: "restbook_app::ui", "titlebar: sidebar toggle, open={open}");
        if open {
            self.dock.update(cx, |dock, cx| {
                dock.toggle_dock(DockPlacement::Left, window, cx)
            });
            return;
        }
        self.reveal_panel(PanelName::Collections, window, cx);
        // Say it worked somewhere the user can see it. A dock that opens
        // off-screen (or under a stale layout) otherwise looks exactly like a
        // button that never fired.
        self.flash(chrome::FlashKind::Success, "Sidebar shown", cx);
    }

    /// The titlebar's tools button: one control for the whole right-hand dock,
    /// so opening and closing History plus Environment never depends on which
    /// of the two is in front.
    fn toggle_tools(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.dock_showing(DockPlacement::Right, cx);
        log::info!(target: "restbook_app::ui", "titlebar: tools toggle, open={open}");
        if open {
            self.dock.update(cx, |dock, cx| {
                dock.toggle_dock(DockPlacement::Right, window, cx)
            });
            return;
        }
        self.reveal_panel(PanelName::History, window, cx);
        let now_open = self.dock.read(cx).is_dock_open(DockPlacement::Right);
        log::info!(target: "restbook_app::ui", "titlebar: tools revealed, open={now_open}");
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

    /// Whether a dock is on screen *and* holds something. A dock emptied by
    /// `ClosePanel` still counts as open, but clicking its toggle should reveal
    /// a pane again rather than fold away an empty strip.
    fn dock_showing(&self, placement: DockPlacement, cx: &App) -> bool {
        let dock = self.dock.read(cx);
        dock.is_dock_open(placement) && !dock.is_empty(placement, cx)
    }

    /// A pane's dock identity, so [`Self::reveal_panel`] can bring its tab to
    /// the front by identity instead of relying on the layout's remembered
    /// active index.
    fn panel_id(&self, name: PanelName) -> PanelId {
        let entity_id = match name {
            PanelName::History => self.panes.history.entity_id(),
            PanelName::Environment => self.panes.environment.entity_id(),
            PanelName::Collections => self.panes.collections.entity_id(),
            PanelName::Request => self.panes.request.entity_id(),
            PanelName::Response => self.panes.response.entity_id(),
        };
        PanelId::from(entity_id)
    }

    /// Write everything a closing window still owes the disk.
    ///
    /// Runs from `on_window_should_close` and again from the quit observers
    /// (Cmd+Q, a session shutdown); the first call sets `flushed` so a normal
    /// close does not write twice. The writes are synchronous on purpose: a
    /// task spawned here would be dropped with the process, and the debounces
    /// are exactly what stand between the last edit and the store.
    pub(crate) fn flush(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.flushed, true) {
            return;
        }

        // Nobody is left to show a response to, and the exit would abandon the
        // socket anyway; cancelling lets the transport close it deliberately.
        if self.document.read(cx).is_pending() {
            let url = self.document.read(cx).last_url.clone().unwrap_or_default();
            log::info!(target: "restbook_app::ui", "closing with a request in flight: {url}");
            self.document.update(cx, |document, cx| document.cancel(cx));
        }

        // No timer has a window to paint into any more. The draft's last edit
        // is written directly below instead of waiting out its debounce.
        let layout_dirty = self.save_task.take().is_some();
        let draft_dirty = self.draft_task.take().is_some();

        if layout_dirty {
            persistence::save(&self.layout_path, &self.dock.read(cx).dump(cx));
        }
        self.flush_draft(draft_dirty, cx);

        log::info!(target: "restbook_app::ui", "window closing; last state written");
        // From here the process is teardown only. gpui-pre logs target-less
        // errors while it destroys the window; the sink drops them once told
        // (see `logging::mark_closing`).
        crate::logging::mark_closing();
    }

    /// The close path's draft write: the same row `persist_draft` maintains,
    /// but on the foreground because a detached write would be cancelled with
    /// the process.
    ///
    /// `dirty` says a debounce was still armed, so there are edits the store
    /// has not seen. Without it nothing new is written — the saved row keeps
    /// its `updated_at`, so closing does not reorder the Unsaved list. A tab
    /// that has been filed into a collection is always cleaned up, even when
    /// its delete task has not run yet, so closing right after a save does not
    /// bring the stale row back on the next launch.
    fn flush_draft(&mut self, dirty: bool, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        self.snapshot_active(cx);
        // Every tab is written, not only the one on screen: a background tab's
        // last edits are in its snapshot, and a draft that was never given a
        // row would otherwise be lost with the process. Only the active tab can
        // have edits the store has not seen, so it alone carries `dirty`.
        let active = self.tabs.active();
        for index in 0..self.tabs.len() {
            self.persist_tab_draft(index, index == active && dirty, cx);
        }
    }

    /// Where the open request lives, as breadcrumb parts.
    ///
    /// The tab knows the request, not its collection; so this is the request's
    /// own name over its method, and nothing invented when a tab is unsaved.
    /// The URL is truncated in the titlebar, so it carries itself as its own
    /// tooltip: the full address is the one thing you want when it is cut off.
    fn breadcrumb(&self, cx: &App) -> Vec<chrome::TitlePart> {
        let request = &self.document.read(cx).request;
        let mut parts = vec![chrome::TitlePart::new(request_panel::tab_label(request))];
        if !request.url.trim().is_empty() {
            parts.push(
                chrome::TitlePart::new(request.url.clone()).with_tooltip(request.url.clone()),
            );
        }
        parts
    }

    /// The status line's contents: the last URL that went out, or why nothing
    /// did. The numbers (status, duration, size) are not here — the response
    /// pane prints them once, in its own header.
    fn status(&self, cx: &App) -> chrome::Status {
        let document = self.document.read(cx);
        if let Some(error) = &document.error {
            return chrome::Status {
                url: document.last_url.clone().map(SharedString::from),
                message: Some(SharedString::from(error.clone())),
            };
        }

        let Some(response) = &document.response else {
            return chrome::Status::default();
        };

        chrome::Status {
            url: document
                .last_url
                .clone()
                .or_else(|| response.final_url.clone())
                .map(SharedString::from),
            message: None,
        }
    }
}

/// Match the native window chrome to the app appearance: `System` hands it back
/// to the OS, `Light`/`Dark` pin it. The component theme switches through
/// `Theme::change`, which makes `cx.theme()` — and every fallback color in
/// [`Palette`] — follow the same axis.
///
/// `System` has no `ThemeMode` variant, so the OS appearance resolves it.
/// The tooltip of a status-bar dock toggle: what it does, plus its shortcut.
///
/// The bar has no room for a label, so the tooltip is the only place the
/// shortcut can be discovered — and it is read from the live keymap, so it can
/// never drift from the binding.
fn toggle_tooltip(label: &'static str, shortcut: Option<String>) -> SharedString {
    match shortcut {
        Some(keys) => format!("{label} ({keys})").into(),
        None => label.into(),
    }
}

fn apply_appearance(window: &mut Window, cx: &mut App) {
    let appearance = theme::appearance(cx);
    let mode = appearance.mode(Some(window.appearance()));
    gpui_kit::component::Theme::change(mode, Some(window), cx);
    cx.set_window_appearance(appearance.window_appearance());
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Deferred rather than opened here: a dialog is a window-level overlay,
        // and the dialog stack is rebuilt after this element returns.
        if std::mem::take(&mut self.pending_save) {
            let this = cx.entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |workspace, cx| workspace.open_save_dialog(window, cx));
            });
        }

        // Same deferral as the save dialog: a sheet is a window-level overlay,
        // and its layer is built after this element returns.
        if let Some(hits) = self.pending_history.take() {
            let this = cx.entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |workspace, cx| {
                    workspace.open_history_sheet(hits, window, cx);
                });
            });
        }

        // And the same for the delete confirmation: an alert dialog is a
        // window-level overlay, so it opens after this element returns.
        if let Some(request) = self.pending_delete.take() {
            let this = cx.entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |workspace, cx| {
                    workspace.open_delete_dialog(request, window, cx);
                });
            });
        }

        // And the update offer: found by the launch check, which has no window.
        if let Some(offer) = self.pending_update.take() {
            let this = cx.entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |workspace, cx| {
                    workspace.open_update_offer_dialog(offer, window, cx);
                });
            });
        }

        // A confirmation has no window to reach until here either. The kit draws
        // it above dialogs, which the hand-drawn toast never could.
        if self.pending_notice.is_some() {
            let this = cx.entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |workspace, cx| workspace.issue_notice(window, cx));
            });
        }

        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);
        // Read once for the View menu: a checkmark is a snapshot of this frame,
        // the same way every other value in the menu is.
        let panel_visibility: Vec<(PanelName, bool)> = PanelName::all()
            .into_iter()
            .map(|name| (name, self.panel_visible(name, cx)))
            .collect();
        let dock = self.dock.clone();
        let breadcrumb = self.breadcrumb(cx);
        let status = self.status(cx);
        // The chip is window-wide chrome, not pane content, so the canvas is
        // what places it. The pick mutates the shared `Variables` entity, which
        // is the same one the Environment pane reads.
        let environment_chip = {
            let variables = self.variables.clone();
            let environments = self
                .variables
                .read(cx)
                .environments()
                .iter()
                .map(|each| each.name.clone())
                .collect();
            // Clears the placeholder's own measured width; a narrower cap is
            // what ellipsised `No environment` to `No enviro…`.
            let cap = metrics.gutter * 16.0;
            chrome::environment_chip(
                self.variables.read(cx).active(),
                environments,
                move |name, _, cx| {
                    variables.update(cx, |vars, cx| vars.set_active(name, cx));
                },
                cap,
            )
        };

        // The trailing slot holds the app's own controls. The kit appends the
        // real window controls (minimize/maximize/close) after everything the
        // app adds, so this slot must NOT draw its own close button: it would
        // sit to the left of the kit's and read as a second, broken one.
        // What is here is grouped apart from the window controls with a hairline
        // divider, so `menu | — □ ✕` reads as two groups.
        //
        // It no longer holds the panel toggles. Those live at the leading edge,
        // beside the docks they open, because a dock control belongs on the
        // side it controls; see `chrome::titlebar`'s `panels` slot.
        let controls = h_flex()
            .flex_none()
            .items_center()
            .gap(px(metrics.space_2xs))
            .child(environment_chip)
            .child({
                // The window menu. Settings lives here as well as in the command
                // palette: a mouse user has no other route to it, and an
                // undiscoverable settings screen is the same as none.
                //
                // It no longer repeats the panel toggles. Every entry here is a
                // command with no other pointer route; a pane that has a
                // dedicated button beside the dock it lives in does not need a
                // second entry one menu deep.
                let workspace = cx.entity();
                let menu = move |mut menu: PopupMenu,
                                 _: &mut Window,
                                 _: &mut Context<PopupMenu>| {
                    let palette = workspace.clone();
                    menu = menu.item(
                        PopupMenuItem::new("Command palette\u{2026}")
                            .icon(IconName::Search)
                            .on_click(move |_, window, cx| {
                                palette
                                    .update(cx, |workspace, cx| workspace.open_palette(window, cx));
                            }),
                    );
                    let settings = workspace.clone();
                    menu = menu.item(
                        PopupMenuItem::new("Settings\u{2026}")
                            .icon(IconName::Settings)
                            .on_click(move |_, window, cx| {
                                settings.update(cx, |workspace, cx| {
                                    workspace.open_settings(window, cx)
                                });
                            }),
                    );
                    // The clipboard pair is a request command, so it sits above
                    // the diagnostics block and below the window commands.
                    let paste = workspace.clone();
                    menu = menu.item(
                        PopupMenuItem::new("Paste cURL")
                            .icon(IconName::ClipboardPaste)
                            .on_click(move |_, window, cx| {
                                paste.update(cx, |workspace, cx| {
                                    workspace
                                        .panes
                                        .request
                                        .update(cx, |panel, cx| panel.paste_curl(window, cx))
                                });
                            }),
                    );
                    let copy = workspace.clone();
                    menu = menu.item(
                        PopupMenuItem::new("Copy as cURL")
                            .icon(IconName::Copy)
                            .on_click(move |_, _, cx| {
                                copy.update(cx, |workspace, cx| workspace.copy_curl(cx));
                            }),
                    );
                    // Save as creates a new collection entry even when the tab
                    // came from one, so an experiment can be branched without
                    // overwriting the request it was opened from.
                    let save_as = workspace.clone();
                    menu = menu.item(
                        PopupMenuItem::new("Save request as\u{2026}")
                            .icon(IconName::Save)
                            .on_click(move |_, _, cx| {
                                save_as.update(cx, |workspace, cx| workspace.save_active_as(cx));
                            }),
                    );
                    // The View section: the panes with no other pointer route.
                    // Collections and the tools dock each have a toggle in the
                    // status bar's corners, so repeating them here would be a
                    // second way to do one thing; Request and Response have no
                    // dedicated control, and Reset Layout below is the way back
                    // from either. Clicking a pane reveals it and then focuses
                    // it, the same pair the palette's focus commands run.
                    menu = menu.separator();
                    for &(name, visible) in &panel_visibility {
                        if Self::has_corner_toggle(name) {
                            continue;
                        }
                        let target = workspace.clone();
                        menu =
                            menu.item(PopupMenuItem::new(name.label()).checked(visible).on_click(
                                move |_, window, cx| {
                                    target.update(cx, |workspace, cx| {
                                        workspace.reveal_panel(name, window, cx);
                                        workspace.focus_panel(name, window, cx);
                                    });
                                },
                            ));
                    }
                    let reset = workspace.clone();
                    menu = menu.item(
                        PopupMenuItem::new("Reset layout")
                            .icon(IconName::RefreshCcw)
                            .on_click(move |_, window, cx| {
                                reset
                                    .update(cx, |workspace, cx| workspace.reset_layout(window, cx));
                            }),
                    );
                    // The diagnostics pair is a run of quiet utilities, not a
                    // command the window is about; keep it last and below a
                    // separator so a missing-the-target click does not copy a
                    // log by accident.
                    let diagnostics = workspace.clone();
                    menu = menu.item(
                        PopupMenuItem::new("Copy log path")
                            .icon(IconName::ClipboardCopy)
                            .on_click(move |_, _, cx: &mut App| {
                                let message = match logging::file_path() {
                                    Some(path) => {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            path.display().to_string(),
                                        ));
                                        format!("Log path copied: {}", path.display())
                                    }
                                    None => "No log file is being written".to_owned(),
                                };
                                diagnostics.update(cx, |workspace, cx| {
                                    workspace.flash(chrome::FlashKind::Success, message, cx)
                                });
                            }),
                    );
                    let logs = workspace.clone();
                    menu = menu.separator();
                    menu.item(
                        PopupMenuItem::new("Copy recent log")
                            .icon(IconName::ClipboardCopy)
                            .on_click(move |_, _, cx: &mut App| {
                                let message = match logging::tail(200) {
                                    Some(text) if !text.is_empty() => {
                                        let lines = text.lines().count();
                                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                                        format!("Copied the last {lines} log lines")
                                    }
                                    _ => "No log file is being written".to_owned(),
                                };
                                logs.update(cx, |workspace, cx| {
                                    workspace.flash(chrome::FlashKind::Success, message, cx)
                                });
                            }),
                    )
                };
                Button::new("window-menu")
                    // `Menu`, not `Ellipsis`: the ellipsis was also the glyph on
                    // the "Command palette" item inside this very menu, so the
                    // button and one of its entries wore the same shape.
                    .icon(IconName::Menu)
                    .ghost()
                    .compact()
                    .tooltip("Menu")
                    .dropdown_menu(menu)
            })
            // A hairline between the app's buttons and the kit's window
            // controls, so the two groups do not read as one run of glyphs.
            .child(
                div()
                    .flex_none()
                    .h(px(14.))
                    .w(px(1.))
                    .mx(px(metrics.space_xs))
                    .bg(palette.border),
            )
            .into_any_element();

        // The dock toggles sit in the status bar's two bottom corners, each
        // under the dock it opens: the sidebar control bottom-left, the tools
        // control bottom-right. They are the only route back to a closed dock,
        // so they stay visible, and their shortcut rides in the tooltip rather
        // than costing a label in a 24px bar.
        let left_toggle = {
            let target = cx.entity();
            let hint = actions::shortcut(&ToggleSidebar, window);
            Button::new("status-toggle-sidebar")
                .icon(IconName::PanelLeft)
                .ghost()
                .compact()
                .tooltip(toggle_tooltip("Show or hide the sidebar", hint))
                .on_click(move |_, window, cx| {
                    target.update(cx, |workspace, cx| workspace.toggle_sidebar(window, cx));
                })
                .into_any_element()
        };

        let right_toggle = {
            let target = cx.entity();
            let hint = actions::shortcut(&ToggleTools, window);
            Button::new("status-toggle-tools")
                .icon(IconName::PanelRight)
                .ghost()
                .compact()
                .tooltip(toggle_tooltip("Show or hide History and Environment", hint))
                .on_click(move |_, window, cx| {
                    target.update(cx, |workspace, cx| workspace.toggle_tools(window, cx));
                })
                .into_any_element()
        };

        v_flex()
            .size_full()
            .bg(palette.background)
            .text_color(palette.foreground)
            .track_focus(&self.focus_handle)
            // Canvas-level commands: they must work from any pane, so they are
            // caught here rather than in the pane that happens to have focus.
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.open_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &NewTab, _, cx| this.new_tab(cx)))
            .on_action(cx.listener(|this, _: &CopyCurl, _, cx| this.copy_curl(cx)))
            .on_action(cx.listener(|this, _: &CloseTab, _, cx| {
                if let Some(id) = this.tabs.active_id() {
                    this.confirm_close_tab(id, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextTab, _, cx| this.select_relative(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousTab, _, cx| this.select_relative(-1, cx)))
            .on_action(cx.listener(|this, _: &ReopenClosedTab, _, cx| this.reopen_closed_tab(cx)))
            .on_action(cx.listener(|this, _: &SaveRequestAs, _, cx| this.save_active_as(cx)))
            // Settings is app-level, so its key works with focus anywhere, the
            // same as the menu entry and the palette command.
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            // Zoom is canvas-level for the same reason: it must work with focus
            // anywhere, and the level lives in the theme global rather than in
            // one pane. The palette reaches the same two helpers through
            // `run_command`, so a zoom is one code path whichever opened it.
            .on_action(cx.listener(|this, _: &ZoomIn, window, cx| {
                this.zoom_by(theme::ZOOM_STEP, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, window, cx| {
                this.zoom_by(1.0 / theme::ZOOM_STEP, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ResetZoom, window, cx| this.set_zoom(1.0, window, cx)),
            )
            // The dock toggles are canvas-level for the same reason as the zoom
            // keys: they must work from any pane, and the docks belong to the
            // workspace rather than to whichever pane holds focus.
            .on_action(
                cx.listener(|this, _: &ToggleSidebar, window, cx| this.toggle_sidebar(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleTools, window, cx| this.toggle_tools(window, cx)),
            )
            .child(chrome::titlebar(
                &palette,
                &metrics,
                &self.workspace_name,
                breadcrumb,
                controls,
            ))
            .child(div().flex_1().min_h_0().child(dock))
            .child(chrome::status_bar(
                &palette,
                &metrics,
                &chrome::WorkspaceChip {
                    root: self.workspace_root.clone(),
                },
                &status,
                left_toggle,
                right_toggle,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::updates;
    use super::{close_needs_confirmation, close_tabs_copy, wrap_index};

    /// The lane's rule: a skipped version does not offer itself again, but
    /// skipping `0.1.4` must not silence `0.1.5`.
    #[test]
    fn a_skipped_version_is_not_offered_but_the_next_one_is() {
        use super::settings::AppSettings;

        let mut settings = AppSettings::default();
        assert!(updates::should_offer(&settings, "0.1.4"));

        settings.skipped_update = Some("0.1.4".to_owned());
        assert!(!updates::should_offer(&settings, "0.1.4"));
        assert!(updates::should_offer(&settings, "0.1.5"));
    }

    #[test]
    fn a_blank_tab_closes_without_asking() {
        assert!(!close_needs_confirmation(None, "{}", true));
    }

    #[test]
    fn an_unsaved_draft_asks_before_it_is_closed() {
        assert!(close_needs_confirmation(
            None,
            "{\"url\":\"example.com\"}",
            false
        ));
    }

    #[test]
    fn a_saved_tab_only_asks_once_it_has_been_edited() {
        let saved = Some("one");
        assert!(!close_needs_confirmation(saved, "one", false));
        assert!(close_needs_confirmation(saved, "two", false));
    }

    /// The batch dialog is the only place a user learns how much work one click
    /// is about to discard, so the sentence names the count and the cost.
    #[test]
    fn a_batch_close_summary_names_the_tabs_and_the_loss() {
        let (title, description, confirm) = close_tabs_copy(4, 2);
        assert_eq!(title, "Close 4 tabs?");
        assert!(description.starts_with("2 tabs have unsaved changes"));
        assert!(description.contains("discard"));
        assert_eq!(confirm, "Discard changes");
    }

    /// Tab rotation wraps in both directions instead of stopping at an end.
    #[test]
    fn tab_rotation_wraps_at_both_ends() {
        assert_eq!(wrap_index(0, 3, -1), 2);
        assert_eq!(wrap_index(2, 3, 1), 0);
        assert_eq!(wrap_index(1, 3, 1), 2);
        assert_eq!(wrap_index(1, 3, -1), 0);
    }

    /// The undo path is a snapshot plus a re-save, so it can be proved against
    /// the in-memory store without a window or an executor.
    mod undo_delete {
        use restbook_core::{
            Collection, CollectionItem, Environment, Folder, Kv, Method, RequestDef, SavedRequest,
            Store,
        };
        use restbook_store::MemoryStore;

        use super::super::delete::{deletion_landed, snapshot_delete, Deleted, PendingDelete};
        use crate::panels::collections::RowTarget;

        fn request(id: &str) -> CollectionItem {
            CollectionItem::Request(SavedRequest {
                id: id.to_owned(),
                name: format!("request {id}"),
                request: RequestDef::new(Method::Get, "https://example.com"),
            })
        }

        fn folder(id: &str, items: Vec<CollectionItem>) -> CollectionItem {
            CollectionItem::Folder(Folder {
                id: id.to_owned(),
                name: format!("folder {id}"),
                items,
            })
        }

        fn collection(id: &str, items: Vec<CollectionItem>) -> Collection {
            Collection {
                id: id.to_owned(),
                name: format!("collection {id}"),
                items,
            }
        }

        fn item_ids(items: &[CollectionItem]) -> Vec<&str> {
            items
                .iter()
                .map(|item| match item {
                    CollectionItem::Folder(folder) => folder.id.as_str(),
                    CollectionItem::Request(request) => request.id.as_str(),
                })
                .collect()
        }

        #[test]
        fn a_deleted_collection_restores_its_subtree_and_ids() {
            let store = MemoryStore::new();
            store
                .save_collection(&collection(
                    "col-1",
                    vec![folder("fold-1", vec![request("req-1")])],
                ))
                .unwrap();

            let target = PendingDelete::Node(RowTarget::Collection("col-1".into()));
            let deleted = snapshot_delete(&target, &store).expect("collection snapshot");
            assert_eq!(deleted.deleted_message(), "Collection deleted");

            store.delete_collection("col-1").unwrap();
            assert!(deletion_landed(&deleted, &store));

            deleted.restore(&store).unwrap();
            let restored = store.collections().unwrap();
            assert_eq!(restored.len(), 1);
            assert_eq!(restored[0].id, "col-1");
            let CollectionItem::Folder(saved) = &restored[0].items[0] else {
                panic!("expected the folder back");
            };
            assert_eq!(saved.id, "fold-1");
            assert_eq!(item_ids(&saved.items), vec!["req-1"]);
        }

        #[test]
        fn a_deleted_request_returns_to_its_sibling_position() {
            let store = MemoryStore::new();
            let mut tree = collection(
                "col-1",
                vec![request("req-a"), request("req-b"), request("req-c")],
            );
            store.save_collection(&tree).unwrap();

            let target = PendingDelete::Node(RowTarget::Item("req-b".into()));
            let deleted = snapshot_delete(&target, &store).expect("item snapshot");
            assert_eq!(deleted.deleted_message(), "Request deleted");

            tree.items.remove(1);
            store.save_collection(&tree).unwrap();
            assert!(deletion_landed(&deleted, &store));

            deleted.restore(&store).unwrap();
            let restored = store.collections().unwrap();
            assert_eq!(
                item_ids(&restored[0].items),
                vec!["req-a", "req-b", "req-c"]
            );
        }

        #[test]
        fn a_deleted_folder_returns_to_its_nested_path_with_its_subtree() {
            let store = MemoryStore::new();
            let mut tree = collection(
                "col-1",
                vec![folder(
                    "outer",
                    vec![request("req-a"), folder("inner", vec![request("req-in")])],
                )],
            );
            store.save_collection(&tree).unwrap();

            let target = PendingDelete::Node(RowTarget::Item("inner".into()));
            let deleted = snapshot_delete(&target, &store).expect("folder snapshot");
            assert_eq!(deleted.deleted_message(), "Folder deleted");

            let CollectionItem::Folder(outer) = &mut tree.items[0] else {
                panic!("expected the outer folder");
            };
            outer.items.remove(1);
            store.save_collection(&tree).unwrap();
            assert!(deletion_landed(&deleted, &store));

            deleted.restore(&store).unwrap();
            let restored = store.collections().unwrap();
            let CollectionItem::Folder(outer) = &restored[0].items[0] else {
                panic!("expected the outer folder back");
            };
            assert_eq!(item_ids(&outer.items), vec!["req-a", "inner"]);
            let CollectionItem::Folder(inner) = &outer.items[1] else {
                panic!("expected the inner folder back");
            };
            assert_eq!(item_ids(&inner.items), vec!["req-in"]);
        }

        #[test]
        fn a_deleted_environment_restores_its_rows_and_active_scope() {
            let store = MemoryStore::new();
            let environment = Environment {
                name: "dev".to_owned(),
                vars: vec![Kv::new("host", "localhost")],
            };
            store.save_environment(&environment).unwrap();
            store.set_active_environment(Some("dev")).unwrap();

            let target = PendingDelete::Environment("dev".into());
            let deleted = snapshot_delete(&target, &store).expect("environment snapshot");
            assert_eq!(deleted.deleted_message(), "Environment deleted");

            store.delete_environment("dev").unwrap();
            store.set_active_environment(None).unwrap();
            assert!(deletion_landed(&deleted, &store));

            deleted.restore(&store).unwrap();
            let restored = store.environments().unwrap();
            assert_eq!(restored.len(), 1);
            assert_eq!(restored[0].name, "dev");
            assert_eq!(restored[0].vars, vec![Kv::new("host", "localhost")]);
            assert_eq!(store.active_environment().unwrap().as_deref(), Some("dev"));
        }

        /// A delete that did not land must not advertise an undo: the offer
        /// would look like it resurrects something that is still on screen.
        #[test]
        fn a_snapshot_that_is_still_present_is_not_undoable() {
            let store = MemoryStore::new();
            store
                .save_collection(&collection("col-1", Vec::new()))
                .unwrap();
            let target = PendingDelete::Node(RowTarget::Collection("col-1".into()));
            let deleted = snapshot_delete(&target, &store).unwrap();
            assert!(!deletion_landed(&deleted, &store));
        }

        /// History clears have no insert-with-id operation on the store, so the
        /// toast correctly offers no undo for them.
        #[test]
        fn history_deletes_offer_no_undo() {
            let store = MemoryStore::new();
            assert!(snapshot_delete(&PendingDelete::HistorySend(7), &store).is_none());
            assert!(snapshot_delete(&PendingDelete::AllHistory, &store).is_none());
        }

        /// The enum is only ever held by the toast, so this pins the nouns the
        /// action label is shown beside.
        #[test]
        fn restored_copy_names_the_same_noun() {
            let deleted = Deleted::Item {
                collection_id: "col-1".to_owned(),
                parent: Vec::new(),
                index: 0,
                item: Box::new(request("req-1")),
                noun: "Request",
            };
            assert_eq!(deleted.deleted_message(), "Request deleted");
            assert_eq!(deleted.restored_message(), "Request restored");
        }
    }

    /// The menu, the palette, and the persisted layout all walk [`PanelName::all`],
    /// so the list has to name every variant exactly once and round-trip through
    /// the persisted spelling.
    #[test]
    fn every_panel_name_is_listed_once_and_parses() {
        use super::PanelName;

        let all = PanelName::all();
        for (index, name) in all.iter().enumerate() {
            assert!(
                !all[..index].contains(name),
                "{name:?} appears twice in PanelName::all"
            );
            assert_eq!(PanelName::parse(name.as_str()), Some(*name));
            assert!(!name.label().is_empty());
        }
        // `parse` is the only reader of `all`, so a name the list forgot would
        // also be dropped from a saved layout. Pin the count to the variants
        // this build knows about.
        assert_eq!(all.len(), 5);
    }

    /// The titlebar's View section lists a pane only when that pane has no
    /// status-bar toggle, so the two routes never both exist for one pane.
    ///
    /// Collections sits under the left toggle and History plus Environment
    /// under the right one; Request and Response have no corner control and are
    /// the only entries the menu draws.
    #[test]
    fn the_view_menu_lists_only_panes_with_no_corner_toggle() {
        use super::{PanelName, Workspace};

        let listed: Vec<PanelName> = PanelName::all()
            .into_iter()
            .filter(|name| !Workspace::has_corner_toggle(*name))
            .collect();

        assert_eq!(listed, vec![PanelName::Request, PanelName::Response]);
    }

    /// The status bar's two dock toggles are the only route back to a closed
    /// dock, so a click on one has to reach its handler and change the dock.
    /// This renders the real `chrome::status_bar` and clicks the real button.
    ///
    /// The status bar is not window chrome, so it has none of the drag hazard
    /// the titlebar had — but it is the same promise ("a click toggles the
    /// dock"), and the moved-press case guards it either way.
    mod status_toggles {
        use std::cell::RefCell;
        use std::rc::Rc;
        use std::sync::Arc;

        use gpui_kit::component::button::{Button, ButtonVariants as _};
        use gpui_kit::component::dock::{DockArea, DockLayout, DockPlacement, DockSkin};
        use gpui_kit::component::{v_flex, IconName};
        use gpui_kit::{
            point, px, AppContext as _, Entity, InteractiveElement as _, IntoElement,
            ParentElement as _, Styled as _, TestAppContext,
        };
        use restbook_store::MemoryStore;

        use crate::panels::collections::CollectionsPanel;
        use crate::panels::history::HistoryPanel;
        use crate::theme::{Metrics, Palette};

        use super::super::chrome;
        use super::super::settings;

        /// A status bar with one real toggle in its left corner, wired the way
        /// the workspace wires it: count the click, then toggle the dock.
        struct Harness {
            dock: Entity<DockArea>,
            collections: Entity<CollectionsPanel>,
            clicks: Rc<RefCell<usize>>,
        }

        impl gpui_kit::Render for Harness {
            fn render(
                &mut self,
                _: &mut gpui_kit::Window,
                cx: &mut gpui_kit::Context<Self>,
            ) -> impl IntoElement {
                let palette = Palette::new(cx);
                let metrics = Metrics::current(cx);
                let dock = self.dock.clone();
                let collections = self.collections.clone();
                let clicks = self.clicks.clone();

                let left_toggle = Button::new("status-toggle-sidebar")
                    .icon(IconName::PanelLeft)
                    .ghost()
                    .debug_selector(|| "status-toggle-sidebar".into())
                    .on_click(move |_, window, cx| {
                        *clicks.borrow_mut() += 1;
                        let layout = DockLayout::tabs()
                            .panel_view(super::super::panel_handle(collections.clone()), cx);
                        dock.update(cx, |dock, cx| {
                            dock.set_dock(DockPlacement::Left, layout, window, cx);
                            // A fresh dock is open; the toggle's open branch
                            // only ever runs against a closed one.
                            if dock.is_dock_open(DockPlacement::Left) {
                                dock.toggle_dock(DockPlacement::Left, window, cx);
                            }
                            dock.toggle_dock(DockPlacement::Left, window, cx);
                        });
                    })
                    .into_any_element();
                let right_toggle = gpui_kit::Empty.into_any_element();

                v_flex()
                    .size_full()
                    .child(v_flex().flex_1().min_h_0().child(self.dock.clone()))
                    .child(chrome::status_bar(
                        &palette,
                        &metrics,
                        &chrome::WorkspaceChip { root: None },
                        &chrome::Status::default(),
                        left_toggle,
                        right_toggle,
                    ))
            }
        }

        fn harness(window: &mut gpui_kit::Window, cx: &mut gpui_kit::Context<Harness>) -> Harness {
            let store: Arc<dyn restbook_core::Store> = Arc::new(MemoryStore::new());
            let (dock, _skin) = DockSkin::dock_area("status-test", None, window, cx);
            let settings =
                cx.new(|_| settings::Settings::new(std::env::temp_dir().join("status-test.json")));
            let history = cx.new(|cx| HistoryPanel::new(store.clone(), settings, window, cx));
            let document = cx.new(|_| super::super::Document::new());
            let collections = cx.new(|cx| CollectionsPanel::new(store, document, window, cx));
            dock.update(cx, |dock, cx| {
                let layout = DockLayout::tabs().panel_view(super::super::panel_handle(history), cx);
                dock.set_dock(DockPlacement::Right, layout, window, cx);
            });
            Harness {
                dock,
                collections,
                clicks: Rc::new(RefCell::new(0)),
            }
        }

        /// A press and release on the corner button must produce exactly one
        /// click and open the sidebar.
        #[gpui_kit::test]
        fn a_click_in_the_status_bar_reaches_its_button(cx: &mut TestAppContext) {
            cx.update(gpui_kit::init);
            cx.update(gpui_kit::base::init);
            let (view, cx) = cx.add_window_view(harness);
            cx.update(|window, cx| window.draw(cx).clear(cx));

            let target = cx
                .debug_bounds("status-toggle-sidebar")
                .expect("the corner toggle must be laid out")
                .center();
            cx.simulate_mouse_down(target, gpui_kit::MouseButton::Left, Default::default());
            cx.simulate_mouse_up(target, gpui_kit::MouseButton::Left, Default::default());
            cx.run_until_parked();

            view.update(cx, |harness, cx| {
                assert_eq!(
                    *harness.clicks.borrow(),
                    1,
                    "the corner toggle must receive exactly one click"
                );
                assert!(
                    harness.dock.read(cx).is_dock_open(DockPlacement::Left),
                    "the click must open the dock the button controls"
                );
            });
        }

        /// Press, slide a pixel, release: still one click, and no drag started.
        #[gpui_kit::test]
        fn a_slightly_moved_press_still_clicks_the_button(cx: &mut TestAppContext) {
            cx.update(gpui_kit::init);
            cx.update(gpui_kit::base::init);
            let (view, cx) = cx.add_window_view(harness);
            cx.update(|window, cx| window.draw(cx).clear(cx));

            let target = cx
                .debug_bounds("status-toggle-sidebar")
                .expect("the corner toggle must be laid out")
                .center();
            let moved = target + point(px(1.), px(1.));
            cx.simulate_mouse_down(target, gpui_kit::MouseButton::Left, Default::default());
            cx.simulate_mouse_move(moved, None, Default::default());
            cx.simulate_mouse_up(moved, gpui_kit::MouseButton::Left, Default::default());
            cx.run_until_parked();

            view.update(cx, |harness, _| {
                assert_eq!(
                    *harness.clicks.borrow(),
                    1,
                    "a one-pixel slide must still count as the click"
                );
            });
        }
    }

    /// The titlebar's leading dock buttons are the only route back to a closed
    /// screen, so each button is the only route back to its dock; it therefore
    /// has to toggle, or reopening is impossible from the window itself.
    ///
    /// Building the whole `Workspace` would open a window against the real
    /// data directory, so this drives the dock and the panes directly and runs
    /// the same steps `toggle_sidebar` and `toggle_tools` take.
    mod dock_toggles {
        use std::sync::Arc;

        use gpui_kit::component::dock::{DockLayout, DockPlacement, DockSkin};
        use gpui_kit::{AppContext as _, Entity, IntoElement, TestAppContext};
        use restbook_store::MemoryStore;

        use crate::panels::environment::EnvironmentPanel;
        use crate::panels::history::HistoryPanel;
        use crate::variables::Variables;

        use super::super::{panel_handle, settings, Document, PanelName};

        struct Docks {
            dock: Entity<gpui_kit::component::dock::DockArea>,
            history: Entity<HistoryPanel>,
            environment: Entity<EnvironmentPanel>,
        }

        impl gpui_kit::Render for Docks {
            fn render(
                &mut self,
                _: &mut gpui_kit::Window,
                _: &mut gpui_kit::Context<Self>,
            ) -> impl IntoElement {
                gpui_kit::Empty
            }
        }

        fn docks(window: &mut gpui_kit::Window, cx: &mut gpui_kit::Context<Docks>) -> Docks {
            let store: Arc<dyn restbook_core::Store> = Arc::new(MemoryStore::new());
            let (dock, _skin) = DockSkin::dock_area("test-dock", None, window, cx);
            let settings =
                cx.new(|_| settings::Settings::new(std::env::temp_dir().join("docks-test.json")));
            let history = cx.new(|cx| HistoryPanel::new(store.clone(), settings, window, cx));
            let document = cx.new(|_| Document::new());
            let variables = cx.new(|cx| Variables::new(store, cx));
            let environment = cx.new(|cx| EnvironmentPanel::new(variables, document, window, cx));
            Docks {
                dock,
                history,
                environment,
            }
        }

        /// The reveal half of `toggle_tools`: both tools panes in one tab
        /// group, collapsible (which is what the toggle needs to close it), and
        /// open.
        fn reveal_tools(
            docks: &mut Docks,
            window: &mut gpui_kit::Window,
            cx: &mut gpui_kit::Context<Docks>,
        ) {
            let history = docks.history.clone();
            let environment = docks.environment.clone();
            docks.dock.update(cx, |dock, cx| {
                let layout = [PanelName::History, PanelName::Environment]
                    .into_iter()
                    .fold(DockLayout::tabs(), |tabs, name| {
                        let handle = match name {
                            PanelName::History => panel_handle(history.clone()),
                            _ => panel_handle(environment.clone()),
                        };
                        tabs.panel_view(handle, cx)
                    });
                dock.set_dock(DockPlacement::Right, layout, window, cx);
                dock.set_dock_collapsible(DockPlacement::Right, true, window, cx);
                if !dock.is_dock_open(DockPlacement::Right) {
                    dock.toggle_dock(DockPlacement::Right, window, cx);
                }
            });
        }

        /// Two tools panes sharing the dock is the shape that used to refuse to
        /// close: the tab bar's own collapse button is hidden behind the tabs,
        /// so the titlebar button is the only route. It must close the dock on
        /// the second click whatever pane is in front.
        #[gpui_kit::test]
        fn the_tools_dock_closes_with_two_panes_showing_tabs(cx: &mut TestAppContext) {
            cx.update(gpui_kit::init);
            let (view, cx) = cx.add_window_view(docks);
            cx.update(|window, cx| {
                view.update(cx, |docks, cx| {
                    reveal_tools(docks, window, cx);
                    assert!(
                        docks.dock.read(cx).is_dock_open(DockPlacement::Right),
                        "the open branch must open the dock"
                    );

                    docks.dock.update(cx, |dock, cx| {
                        dock.toggle_dock(DockPlacement::Right, window, cx)
                    });
                    assert!(
                        !docks.dock.read(cx).is_dock_open(DockPlacement::Right),
                        "a second click must put the tools dock away"
                    );
                });
            });
        }

        /// The sidebar is a dock of its own, so its button toggles it without
        /// touching the tools dock. The two buttons must not share state.
        #[gpui_kit::test]
        fn the_sidebar_toggles_independently_of_the_tools_dock(cx: &mut TestAppContext) {
            cx.update(gpui_kit::init);
            let (view, cx) = cx.add_window_view(docks);
            cx.update(|window, cx| {
                view.update(cx, |docks, cx| {
                    reveal_tools(docks, window, cx);

                    // `reveal_panel(Collections)`: the sidebar holds the tree
                    // alone, which for this test is any panel it can own.
                    let history = docks.history.clone();
                    docks.dock.update(cx, |dock, cx| {
                        let layout = DockLayout::tabs().panel_view(panel_handle(history), cx);
                        dock.set_dock(DockPlacement::Left, layout, window, cx);
                        if !dock.is_dock_open(DockPlacement::Left) {
                            dock.toggle_dock(DockPlacement::Left, window, cx);
                        }
                    });

                    docks.dock.update(cx, |dock, cx| {
                        dock.toggle_dock(DockPlacement::Left, window, cx)
                    });
                    let dock = docks.dock.read(cx);
                    assert!(
                        !dock.is_dock_open(DockPlacement::Left),
                        "the sidebar button must close the sidebar"
                    );
                    assert!(
                        dock.is_dock_open(DockPlacement::Right),
                        "closing the sidebar must leave the tools dock alone"
                    );
                });
            });
        }
    }
}
