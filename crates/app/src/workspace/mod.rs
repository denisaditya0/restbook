//! The window root: engine handles, the dock layout, and the panes.
//!
//! The workspace owns what the panes do not: the engine handles, the shared
//! [`Document`], and the [`DockArea`] with its layout. Everything else is a pane,
//! built here rather than by the dock so it stays typed (`workspace.request()`
//! returns `Entity<RequestPanel>`); adding one is a name in [`PanelName`].

mod chrome;
mod delete;
mod destination_picker;
mod dock;
mod export;
mod import;
mod move_dialog;
mod persistence;
mod save_dialog;
mod send;
pub(crate) mod settings;
mod settings_view;
mod snapshot;
pub(crate) mod storage;
mod tabs;
mod updates;

pub(crate) use delete::PendingDelete;
pub(crate) use export::ExportState;
use import::PendingImport;
pub(crate) use tabs::{Tab, TabAction, TabEntry, TabId, TabState};

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::*;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::dock::{DockArea, DockEvent, DockSkin};
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex, WindowExt as _};
use gpui_kit::*;

use restbook_core::{RequestDef, Store};
use restbook_transport::ReqwestTransport;

use crate::actions::{
    self as actions, CloseTab, CopyCurl, ExportFile, FindInResponse, ImportFile, NewTab, NextTab,
    OpenSettings, PreviousTab, ReopenClosedTab, ResetZoom, SaveRequestAs, ShowShortcuts,
    ToggleCommandPalette, ToggleSidebar, ToggleTools, ZoomIn, ZoomOut,
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

/// How the centre's two panes are arranged. The canvas owns this: it is the only
/// module that builds a [`DockLayout`](gpui_kit::component::dock::DockLayout), and
/// the dock persists the split's axis, so a restart reads the choice back from
/// `layout.json` without a field of our own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CenterOrientation {
    /// Request over Response, the first-run arrangement.
    #[default]
    Stacked,
    /// Request beside Response, each taking half the width.
    SideBySide,
}

impl CenterOrientation {
    /// The orientation a persisted centre split recorded. Anything that is not a
    /// horizontal stack — a fresh file, or a centre that holds tabs rather than a
    /// split — reads as the default.
    pub(crate) fn from_axis(axis: Option<Axis>) -> Self {
        match axis {
            Some(Axis::Horizontal) => CenterOrientation::SideBySide,
            _ => CenterOrientation::Stacked,
        }
    }

    /// The other orientation, for the toggle.
    fn flipped(self) -> Self {
        match self {
            CenterOrientation::Stacked => CenterOrientation::SideBySide,
            CenterOrientation::SideBySide => CenterOrientation::Stacked,
        }
    }
}

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
    /// How the history this session writes is protected, for the status bar.
    history: storage::HistoryMode,
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
    /// Which way the centre split runs. Kept in memory so the status-bar control
    /// can name the next action; the dock persists the axis itself (see
    /// [`CenterOrientation`]). Set from the saved layout on install and flipped by
    /// [`Workspace::toggle_center_orientation`].
    center_orientation: CenterOrientation,
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
    /// The name field of the save dialog's *New collection…* row. The destination
    /// choice itself lives in the picker entity, which the dialog captures.
    save_new_collection_name: Entity<InputState>,
    /// A saved request asked to move, waiting for render to raise the destination
    /// picker: the request came from a menu with no window.
    pending_move: Option<String>,
    /// The move dialog's request. The destination choice lives in the picker.
    move_node: Option<String>,
    /// A draft asked to be saved, but the dialog needs a window and the event the
    /// request arrived on had none. The next render opens the dialog.
    pending_save: bool,
    /// The empty request state asked for a file to import. The event carries no
    /// window, so the picker opens from the next render, the same way a save is
    /// deferred.
    pending_import: bool,
    /// The parsed file the import dialog is deciding about. Parsed once, on the
    /// background executor, and parked here because the dialog's content
    /// closure is rebuilt on every notify.
    import_pending: Option<PendingImport>,
    /// Whether an import is reading, parsing, or landing. It owns the busy
    /// toast (U65) and is what stops a second pick while one is in flight.
    import_busy: bool,
    /// The Collections pane asked to export. Same deferral as `pending_import`.
    pending_export: bool,
    /// The export picker's choices. Rebuilt each time the dialog opens so
    /// "include secrets" is always off and a stale scope cannot leak a file.
    pub(crate) export_state: ExportState,
    /// Whether the response pane asked for its earlier sends. Set by its event
    /// and consumed by render, because the event carries no window and reveal
    /// needs one.
    pending_history: bool,
    /// Whether the response pane asked for an image snapshot. Same deferral as
    /// `pending_history`: its event carries no window, and a dialog needs one.
    pending_snapshot: bool,
    /// Whether the response pane asked to flip the centre split. Same deferral:
    /// rebuilding the dock needs a window the event does not carry.
    pending_toggle_split: bool,
    /// The snapshot dialog's "include secrets" tick. Reset every time the dialog
    /// opens, like the export picker's, so a remembered tick cannot leak.
    pub(crate) snapshot_include_secrets: bool,
    /// Whether the next snapshot is clipped to the compact box. Remembered for
    /// the session: it hides nothing, so resetting it would only be annoying.
    pub(crate) snapshot_compact: bool,
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
pub(crate) struct WorkspaceNotice;

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
            history,
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
        let save_new_collection_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("Collection name"));
        let settings = cx.new(|_| settings::Settings::new(state_dir.join(settings::SETTINGS_FILE)));
        if let Some(root) = &workspace_root {
            settings.update(cx, |settings, cx| settings.record_workspace(root, cx));
        }

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
            history: cx.new(|cx| {
                HistoryPanel::new(
                    store.clone(),
                    document.clone(),
                    settings.clone(),
                    window,
                    cx,
                )
            }),
            request: cx
                .new(|cx| RequestPanel::new(document.clone(), variables.clone(), window, cx)),
            response: cx.new(|cx| {
                let transport: Arc<dyn restbook_core::Transport> = transport.clone();
                ResponsePanel::new(
                    document.clone(),
                    store.clone(),
                    transport,
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
            history,
            document,
            variables,
            settings,
            panes,
            tabs,
            closed: Vec::new(),
            sending: None,
            center_orientation: CenterOrientation::default(),
            dock,
            layout_path: state_dir.join(LAYOUT_FILE),
            save_task: None,
            draft_task: None,
            draft_gen: 0,
            flash: None,
            pending_notice: None,
            focus_handle: cx.focus_handle(),
            save_name,
            save_new_collection_name,
            pending_move: None,
            move_node: None,
            pending_save: false,
            pending_import: false,
            import_pending: None,
            import_busy: false,
            pending_export: false,
            export_state: ExportState::default(),
            pending_history: false,
            pending_snapshot: false,
            pending_toggle_split: false,
            snapshot_include_secrets: false,
            snapshot_compact: false,
            pending_delete: None,
            pending_update: None,
            flushed: false,
        };

        workspace.listen(window, cx);
        workspace.install_layout(window, cx);
        // After the layout, so the saved tabs land over a known arrangement; a
        // dropped tab changes nothing here, it just does not open.
        if let Some(saved) = persistence::load_tabs(&workspace.layout_path) {
            workspace.restore_tabs(saved, cx);
        }
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
            log::debug!(target: "restbook_app::ui", "{}", event.summary());
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
                        crate::logging::redact_url(&entry.request.url),
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
                        document.error = Some(error.failure());
                        cx.notify();
                    });
                }
            }
        })
        .detach();

        cx.subscribe(
            &self.panes.response,
            |this, _, event: &ResponseEvent, cx| {
                crate::logging::event(event.summary());
                match event {
                    // The pane reports the intent; where the result appears is
                    // the canvas's decision. Render defers the reveal, because
                    // this event carries no window.
                    ResponseEvent::ShowHistory => {
                        this.pending_history = true;
                        cx.notify();
                    }
                    ResponseEvent::Snapshot => {
                        this.pending_snapshot = true;
                        cx.notify();
                    }
                    ResponseEvent::ToggleSplit => {
                        this.pending_toggle_split = true;
                        cx.notify();
                    }
                    ResponseEvent::Retry => this.send(cx),
                    ResponseEvent::DeleteCookie { name } => this.delete_cookie(name, cx),
                    ResponseEvent::UpdateCookie { name, value } => {
                        this.update_cookie(name, value, cx)
                    }
                }
            },
        )
        .detach();

        cx.subscribe(&self.panes.request, |this, _, event: &RequestEvent, cx| {
            if let Some(line) = event.summary() {
                crate::logging::event(line);
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
                RequestEvent::Import => {
                    this.pending_import = true;
                    cx.notify();
                }
                RequestEvent::CopyCurl => this.copy_curl(cx),
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
                    crate::logging::event(line);
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
                    CollectionsEvent::Move { node } => {
                        this.pending_move = Some(node.clone());
                        cx.notify();
                    }
                    CollectionsEvent::SaveRequest => this.save_active_as(cx),
                    CollectionsEvent::Import => {
                        this.pending_import = true;
                        cx.notify();
                    }
                    CollectionsEvent::Export => {
                        this.pending_export = true;
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

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        // A new edit replaces the pending write, so a drag costs one save.
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(persistence::SAVE_DEBOUNCE)
                .await;
            this.update(cx, |workspace, cx| {
                workspace.save_task = None;
                persistence::save(
                    &workspace.layout_path,
                    &workspace.dock.read(cx).dump(cx),
                    &workspace.tabs_snapshot(),
                );
            })
            .ok();
        }));
    }

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

    /// Show the earlier sends of the request on screen: scope the History pane
    /// to it, reveal the tools dock, and focus the list. The pane is the same
    /// one the status bar toggle opens; no overlay is involved.
    pub(crate) fn show_history_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panes
            .history
            .update(cx, |panel, cx| panel.scope_to_request(cx));
        self.reveal_panel(PanelName::History, window, cx);
        self.focus_panel(PanelName::History, window, cx);
    }

    /// Forget one cookie in the shared jar, on the response pane's behalf. The
    /// pane reads the jar to draw the Cookies tab, but only the canvas mutates
    /// it, which is also the only side that can report the outcome.
    fn delete_cookie(&mut self, name: &str, cx: &mut Context<Self>) {
        let url = self.document.read(cx).url();
        let removed = restbook_core::Transport::delete_cookie(self.transport.as_ref(), &url, name);
        self.panes.response.update(cx, |_, cx| cx.notify());
        if removed {
            self.flash(
                chrome::FlashKind::Success,
                format!("Deleted cookie {name}."),
                cx,
            );
        } else {
            self.flash(
                chrome::FlashKind::Error,
                format!("Cookie {name} is not in the session."),
                cx,
            );
        }
    }

    /// Replace one cookie's value in the shared jar.
    fn update_cookie(&mut self, name: &str, value: &str, cx: &mut Context<Self>) {
        let url = self.document.read(cx).url();
        let updated =
            restbook_core::Transport::update_cookie(self.transport.as_ref(), &url, name, value);
        self.panes.response.update(cx, |_, cx| cx.notify());
        if updated {
            self.flash(
                chrome::FlashKind::Success,
                format!("Updated cookie {name}."),
                cx,
            );
        } else {
            self.flash(
                chrome::FlashKind::Error,
                format!("Cookie {name} is not in the session."),
                cx,
            );
        }
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

    /// Open another workspace by starting a fresh process in it. The store and
    /// every pane are built from the root at startup, so this is a relaunch,
    /// not an in-place switch; the child is pinned with `RESTBOOK_WORKSPACE`.
    pub(crate) fn open_workspace(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        match storage::relaunch_in_workspace(&root) {
            Ok(()) => cx.quit(),
            Err(error) => self.notice_flash(
                chrome::FlashKind::Error,
                format!("Couldn\u{2019}t open {}: {error}", root.display()),
                cx,
            ),
        }
    }

    /// Show the keyboard shortcuts sheet. The rows come from the palette's own
    /// command table and the live keymap, so a rebinding changes the sheet with
    /// no second list to edit.
    fn open_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = command_palette::shortcut_rows(window);
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);

        window.open_dialog(cx, move |dialog, _, _| {
            let rows = rows.clone();
            let list = v_flex()
                .gap(px(metrics.space_xs))
                .max_h(px(480.))
                .overflow_y_scrollbar()
                .children(rows.iter().map(|(label, hint)| {
                    h_flex()
                        .justify_between()
                        .items_center()
                        .gap(px(metrics.space_md))
                        .child(div().text_sm().child(label.clone()))
                        .child(
                            div()
                                .text_sm()
                                .text_color(palette.muted)
                                .child(hint.clone()),
                        )
                }));

            dialog
                .title("Keyboard shortcuts")
                .w_112()
                .child(list)
                .footer(
                    DialogFooter::new().child(
                        Button::new("shortcuts-close")
                            .label("Close")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
                )
                .on_ok(|_, window, cx| {
                    window.close_dialog(cx);
                    true
                })
        });
    }

    fn run_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        log::debug!(target: "restbook_app::ui", "command: {command:?}");
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
            Command::FindInResponse => {
                self.reveal_panel(PanelName::Response, window, cx);
                self.panes
                    .response
                    .update(cx, |panel, cx| panel.open_find(window, cx))
            }
            Command::PasteCurl => self
                .panes
                .request
                .update(cx, |panel, cx| panel.paste_curl(window, cx)),
            Command::CopyCurl => self.copy_curl(cx),
            Command::ImportFile => self.import_file(window, cx),
            Command::ExportFile => self.open_export_dialog(window, cx),
            Command::Snapshot => self.open_snapshot_dialog(window, cx),
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
            Command::ToggleSideBySide => self.toggle_center_orientation(window, cx),
            Command::ResetLayout => self.reset_layout(window, cx),
            Command::OpenSettings => self.open_settings(window, cx),
            Command::ShowShortcuts => self.open_shortcuts(window, cx),
            Command::ZoomIn => self.zoom_by(theme::ZOOM_STEP, window, cx),
            Command::ZoomOut => self.zoom_by(1.0 / theme::ZOOM_STEP, window, cx),
            Command::ResetZoom => self.set_zoom(1.0, window, cx),
        }
    }
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
            log::debug!(target: "restbook_app::ui", "closing with a request in flight: {url}");
            self.document.update(cx, |document, cx| document.cancel(cx));
        }

        // No timer has a window to paint into any more. The draft's last edit
        // is written directly below instead of waiting out its debounce.
        let layout_dirty = self.save_task.take().is_some();
        let draft_dirty = self.draft_task.take().is_some();

        if layout_dirty {
            persistence::save(
                &self.layout_path,
                &self.dock.read(cx).dump(cx),
                &self.tabs_snapshot(),
            );
        }
        self.flush_draft(draft_dirty, cx);

        log::debug!(target: "restbook_app::ui", "window closing; last state written");
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
                message: Some(SharedString::from(error.kind.title())),
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

        // The empty-state import button has no window either; the picker is
        // opened after this element returns, like every other deferred surface.
        if let Some(node) = self.pending_move.take() {
            let this = cx.entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |workspace, cx| {
                    workspace.open_move_dialog(node, window, cx)
                });
            });
        }

        if std::mem::take(&mut self.pending_import) {
            let this = cx.entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |workspace, cx| workspace.import_file(window, cx));
            });
        }

        if std::mem::take(&mut self.pending_export) {
            let this = cx.entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |workspace, cx| workspace.open_export_dialog(window, cx));
            });
        }

        // Same deferral as the save dialog: the History pane has to be revealed,
        // and a dock change needs the frame to settle first.
        if std::mem::take(&mut self.pending_history) {
            let this = cx.entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |workspace, cx| {
                    workspace.show_history_pane(window, cx);
                });
            });
        }

        // A snapshot is a window-level dialog, so it opens after this element
        // returns for the same reason the export picker does.
        if std::mem::take(&mut self.pending_snapshot) {
            let this = cx.entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |workspace, cx| {
                    workspace.open_snapshot_dialog(window, cx);
                });
            });
        }

        // The split flip rebuilds the dock, which needs a window the response
        // pane's event does not carry, so it runs after this element returns.
        if std::mem::take(&mut self.pending_toggle_split) {
            let this = cx.entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |workspace, cx| {
                    workspace.toggle_center_orientation(window, cx);
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
                    let shortcuts = workspace.clone();
                    menu = menu.item(
                        PopupMenuItem::new("Keyboard shortcuts")
                            .icon(IconName::Keyboard)
                            .on_click(move |_, window, cx| {
                                shortcuts.update(cx, |workspace, cx| {
                                    workspace.open_shortcuts(window, cx)
                                });
                            }),
                    );
                    let snapshot = workspace.clone();
                    menu = menu.item(
                        PopupMenuItem::new("Snapshot\u{2026}")
                            .icon(IconName::Image)
                            .on_click(move |_, window, cx| {
                                snapshot.update(cx, |workspace, cx| {
                                    workspace.open_snapshot_dialog(window, cx)
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
                    // Save, import, and export are commands about collections, so
                    // they live on the Collections pane. The window menu keeps
                    // only what this window is: commands, panes, and diagnostics.
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
                    // The menu is taller than a 640x480 window. The kit's
                    // scrollable mode caps it and shows a vertical scrollbar
                    // rather than clipping the diagnostics rows.
                    .scrollable(true)
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
            .on_action(cx.listener(|this, _: &ImportFile, window, cx| this.import_file(window, cx)))
            .on_action(
                cx.listener(|this, _: &ExportFile, window, cx| this.open_export_dialog(window, cx)),
            )
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
            .on_action(cx.listener(|this, _: &FindInResponse, window, cx| {
                this.reveal_panel(PanelName::Response, window, cx);
                this.panes
                    .response
                    .update(cx, |panel, cx| panel.open_find(window, cx))
            }))
            .on_action(
                cx.listener(|this, _: &ShowShortcuts, window, cx| this.open_shortcuts(window, cx)),
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
                    history: self.history,
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

    /// The centre's persisted axis is the only record of the split direction, so
    /// reading it back has to invert the build exactly: a horizontal stack is
    /// side by side, and anything else is the default stacked layout.
    #[test]
    fn the_centre_orientation_reads_back_from_the_saved_axis() {
        use super::CenterOrientation;
        use gpui_kit::Axis;

        assert_eq!(
            CenterOrientation::from_axis(Some(Axis::Horizontal)),
            CenterOrientation::SideBySide
        );
        assert_eq!(
            CenterOrientation::from_axis(Some(Axis::Vertical)),
            CenterOrientation::Stacked
        );
        // A fresh file, or a centre that holds tabs rather than a split, has no
        // axis to read and must not guess side by side.
        assert_eq!(
            CenterOrientation::from_axis(None),
            CenterOrientation::Stacked
        );
    }

    /// The toggle alternates, and it is an involution: two flips are the start.
    #[test]
    fn the_centre_orientation_toggle_alternates() {
        use super::CenterOrientation;

        let stacked = CenterOrientation::Stacked;
        let side = stacked.flipped();
        assert_eq!(side, CenterOrientation::SideBySide);
        assert_eq!(side.flipped(), stacked);
    }

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

    /// *New collection…* is one confirm: the collection and the request land
    /// together, and the tab binds to the new node so the next Save updates it.
    mod save_creation {
        use gpui_kit::{AppContext as _, TestAppContext};
        use restbook_core::CollectionItem;

        use super::super::Workspace;

        /// The real window root. `gpui_kit::open_window` wraps the app in
        /// `base::Root`, and that wrapper installs a window's component state —
        /// notifications and dialogs need it, and a bare `add_window_view` does
        /// not.
        fn open(
            cx: &mut TestAppContext,
        ) -> (
            gpui_kit::Entity<Workspace>,
            &mut gpui_kit::VisualTestContext,
        ) {
            let mut built = None;
            let (_root, cx) = cx.add_window_view(|window, cx| {
                let workspace = cx.new(|cx| Workspace::new(window, cx));
                built = Some(workspace.clone());
                gpui_kit::base::Root::new(workspace, window, cx)
            });
            (built.expect("the view built the workspace"), cx)
        }

        fn collections(
            cx: &mut gpui_kit::VisualTestContext,
            workspace: &gpui_kit::Entity<Workspace>,
        ) -> Vec<restbook_core::Collection> {
            cx.update(|_, cx| workspace.read(cx).store.collections().unwrap())
        }

        #[gpui_kit::test]
        fn creating_a_collection_and_saving_lands_both(cx: &mut TestAppContext) {
            cx.update(gpui_kit::init);
            crate::test_support::init_ui(cx);
            let _dir = crate::test_support::DataDir::new("save-creation");
            let (workspace, cx) = open(cx);

            // The confirmation the save raises is a window-level notification;
            // everything asserted below is set synchronously.
            let landed = cx.update(|_, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.commit_save(None, Some("First request"), None, Some("My API"), cx)
                })
            });
            assert!(landed, "the save did not land");

            let saved = collections(cx, &workspace);
            assert_eq!(saved.len(), 1);
            assert_eq!(saved[0].name, "My API");
            let CollectionItem::Request(request) = &saved[0].items[0] else {
                panic!("the request is not at the new collection's root");
            };
            assert_eq!(request.name, "First request");

            // The tab is bound to the node, so the next Save updates instead of
            // appending a second copy.
            let node = cx.update(|_, cx| workspace.read(cx).tabs[0].node.clone());
            assert_eq!(node.as_deref(), Some(request.id.as_str()));
        }

        #[gpui_kit::test]
        fn an_unnamed_new_collection_creates_nothing(cx: &mut TestAppContext) {
            cx.update(gpui_kit::init);
            crate::test_support::init_ui(cx);
            let _dir = crate::test_support::DataDir::new("save-creation-blank");
            let (workspace, cx) = open(cx);

            let landed = cx.update(|_, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.commit_save(None, Some("R"), None, Some("  "), cx)
                })
            });
            assert!(!landed);
            assert!(collections(cx, &workspace).is_empty());
        }
    }

    /// A tab keeps its own request, body included.
    ///
    /// The editor applies an adopted request on its next frame, so a switch that
    /// arrives before that frame has to wait rather than fold the widgets it can
    /// still see back into the tab it is leaving. That fold is what left the
    /// second tab showing the first tab's raw body.
    mod tab_bodies {
        use gpui_kit::{Entity, TestAppContext, VisualTestContext};
        use restbook_core::{Body, Method, RequestDef};

        use super::super::Workspace;

        fn with_body(url: &str, text: &str) -> RequestDef {
            RequestDef {
                body: Body::Raw {
                    mime: "application/json".to_owned(),
                    text: text.to_owned(),
                },
                ..RequestDef::new(Method::Post, url)
            }
        }

        fn plain(url: &str) -> RequestDef {
            RequestDef::new(Method::Get, url)
        }

        fn open(
            cx: &mut VisualTestContext,
            workspace: &Entity<Workspace>,
            node: &str,
            request: RequestDef,
        ) {
            let node = node.to_owned();
            cx.update(|_, cx| {
                workspace.update(cx, |workspace, cx| workspace.open_saved(node, request, cx));
            });
        }

        fn body_of(cx: &mut VisualTestContext, workspace: &Entity<Workspace>) -> Body {
            cx.update(|_, cx| {
                workspace
                    .read(cx)
                    .panes
                    .request
                    .read(cx)
                    .current_request(cx)
                    .body
            })
        }

        fn raw_text(body: &Body) -> Option<&str> {
            match body {
                Body::Raw { text, .. } => Some(text.as_str()),
                _ => None,
            }
        }

        #[gpui_kit::test]
        fn the_second_tab_opens_with_its_own_body(cx: &mut TestAppContext) {
            cx.update(gpui_kit::init);
            crate::test_support::init_ui(cx);
            let _dir = crate::test_support::DataDir::new("tab-bodies");
            let (workspace, cx) = cx.add_window_view(Workspace::new);

            open(
                cx,
                &workspace,
                "node-a",
                with_body("https://api.test/a", "{\"a\":1}"),
            );
            cx.run_until_parked();
            open(
                cx,
                &workspace,
                "node-b",
                plain("http://localhost:3000/users/2"),
            );
            cx.run_until_parked();

            assert!(matches!(body_of(cx, &workspace), Body::None));
        }

        /// Two switches with no frame between them: the editor still shows the
        /// previous tab when the second switch snapshots, so the snapshot must
        /// not overwrite the tab that was about to be shown.
        #[gpui_kit::test]
        fn a_switch_before_the_adoption_lands_keeps_each_tab_intact(cx: &mut TestAppContext) {
            cx.update(gpui_kit::init);
            crate::test_support::init_ui(cx);
            let _dir = crate::test_support::DataDir::new("tab-bodies-quick");
            let (workspace, cx) = cx.add_window_view(Workspace::new);

            open(
                cx,
                &workspace,
                "node-a",
                with_body("https://api.test/a", "{\"a\":1}"),
            );
            cx.run_until_parked();
            open(
                cx,
                &workspace,
                "node-b",
                plain("http://localhost:3000/users/2"),
            );
            cx.run_until_parked();

            // Back to the first tab and to the second again, with no frame in
            // between either switch.
            cx.update(|_, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.select_tab(0, cx);
                    workspace.select_tab(1, cx);
                });
            });
            cx.run_until_parked();

            let bodies: Vec<Body> = cx.update(|_, cx| {
                workspace
                    .read(cx)
                    .tabs
                    .iter()
                    .map(|tab| tab.request.body.clone())
                    .collect()
            });
            assert_eq!(raw_text(&bodies[0]), Some("{\"a\":1}"));
            assert!(matches!(bodies[1], Body::None));
            assert!(matches!(body_of(cx, &workspace), Body::None));
        }
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
                        let layout = DockLayout::tabs().panel_view(
                            crate::workspace::dock::panel_handle(collections.clone()),
                            cx,
                        );
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
                        &chrome::WorkspaceChip {
                            root: None,
                            history: crate::workspace::storage::HistoryMode::Encrypted,
                        },
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
            let document = cx.new(|_| super::super::Document::new());
            let history = cx
                .new(|cx| HistoryPanel::new(store.clone(), document.clone(), settings, window, cx));
            let collections = cx.new(|cx| CollectionsPanel::new(store, document, window, cx));
            dock.update(cx, |dock, cx| {
                let layout = DockLayout::tabs()
                    .panel_view(crate::workspace::dock::panel_handle(history), cx);
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

        use super::super::{settings, Document, PanelName};
        use crate::workspace::dock::panel_handle;

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
            let document = cx.new(|_| Document::new());
            let history = cx
                .new(|cx| HistoryPanel::new(store.clone(), document.clone(), settings, window, cx));
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
