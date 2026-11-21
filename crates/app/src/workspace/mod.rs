//! The window root: engine handles, the dock layout, and the panes.
//!
//! The workspace owns what the panes do not: the engine handles, the shared
//! [`Document`], and the [`DockArea`] with its layout. Everything else is a pane,
//! built here rather than by the dock so it stays typed (`workspace.request()`
//! returns `Entity<RequestPanel>`); adding one is a name in [`PanelName`].

mod chrome;
mod history_sheet;
mod persistence;
mod send;
pub(crate) mod settings;
mod settings_view;
pub(crate) mod storage;
mod tabs;
mod updates;

pub(crate) use tabs::{Tab, TabAction, TabEntry, TabId, TabState};

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui_kit::component::button::*;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::dock::{
    panel_handle, DockArea, DockEvent, DockLayout, DockPlacement, DockSkin, PaneRef, PanelId,
    PanelState,
};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{h_flex, v_flex, WindowExt as _};
use gpui_kit::*;

use restbook_core::{
    Collection, CollectionItem, Draft, Environment, Method, Origin, RequestDef, Store,
};
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
use crate::panels::collections::{self as collections_panel, Event as CollectionsEvent};
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
use updates::Updates;

/// The dock's identity in the persisted layout. Bump the version when the
/// meaning of the tree changes, not when a panel is added.
const DOCK_ID: &str = "restbook";
const DOCK_VERSION: usize = 1;

/// The layout file lives beside the history in the same data directory.
const LAYOUT_FILE: &str = "layout.json";

/// A drag emits a layout change per frame, so the file is written on a pause.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

/// How many closed tabs Reopen Closed Tab can walk back through. The stack is
/// process-local and cheap, so this only stops an all-day session from holding
/// every request it ever closed.
const CLOSED_TAB_LIMIT: usize = 20;

/// The width the sidebar starts at, before the user drags its edge.
const HISTORY_WIDTH: f32 = 260.;

/// The width the right-hand tools dock starts at. Environment rows carry a
/// key, a value, and three controls, so the pane cannot use the sidebar's
/// width without clipping the controls that create an environment.
const TOOL_WIDTH: f32 = 400.;

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
    pending_update: Option<PendingUpdate>,
    /// Set by [`Self::flush`]. A normal close writes in
    /// `on_window_should_close` and the quit observers then fire; the second
    /// write would bump `updated_at` and duplicate the work.
    flushed: bool,
}

/// Nothing: the type is the identity. The kit keys in-app notifications by
/// `TypeId`, so one marker replaces the previous workspace notice instead of
/// stacking a second one under it.
struct WorkspaceNotice;

/// A newer release the launch check found, held until render can open the
/// dialog offering it. The `Asset` is what `Updates::install` downloads, so an
/// offer with no asset for this platform is never parked.
struct PendingUpdate {
    latest: String,
    notes: Option<String>,
    asset: crate::updater::Asset,
}

/// A destructive act the sidebar asked for, held until render can open the
/// alert dialog. The workspace owns the confirmation because it owns window
/// placement; the panes only name the thing.
#[derive(Clone)]
enum PendingDelete {
    /// A collection, a folder, or a saved request in the tree.
    Node(collections_panel::RowTarget),
    /// An environment, by name.
    Environment(String),
    /// One stored send, by history row id.
    HistorySend(i64),
    /// Every stored send, across every request.
    AllHistory,
    /// Every send owned by one request or draft.
    HistoryOwner(Origin),
    /// The open tab with this id, when closing it would lose unsaved work.
    Tab(TabId),
    /// A bulk close, with the number of target tabs that would lose work, so
    /// the dialog can ask once instead of once per tab.
    Tabs { ids: Vec<TabId>, unsaved: usize },
}

/// A value captured before a destructive action, kept only as long as its
/// toast lives. There is no trash store and no second level: a new destructive
/// act replaces the offer, and letting the toast expire drops the snapshot.
#[derive(Clone)]
enum Deleted {
    Collection(Collection),
    /// A folder or request, plus where it lived so it returns to the same
    /// sibling position. The subtree rides inside `item`, and every id is the
    /// one it had before, so open tabs keep their `node` binding.
    Item {
        collection_id: String,
        /// Folder ids from the collection root to the removed item's parent.
        parent: Vec<String>,
        /// The item's index among the siblings it was removed from.
        index: usize,
        /// Boxed: a saved request carries its whole draft, so an inline item
        /// would make every snapshot — which lives only as long as its toast —
        /// several hundred bytes wide.
        item: Box<CollectionItem>,
        noun: &'static str,
    },
    Environment {
        environment: Environment,
        /// Whether it was the send-time environment before it was deleted.
        was_active: bool,
    },
}

impl Deleted {
    fn noun(&self) -> &'static str {
        match self {
            Self::Collection(_) => "Collection",
            Self::Item { noun, .. } => noun,
            Self::Environment { .. } => "Environment",
        }
    }

    fn deleted_message(&self) -> String {
        format!("{} deleted", self.noun())
    }

    fn restored_message(&self) -> String {
        format!("{} restored", self.noun())
    }

    /// Put the captured value back. A collection is one store row, so a
    /// folder or request is re-inserted into its current collection rather
    /// than re-saving a stale copy of everything around it.
    fn restore(&self, store: &dyn Store) -> restbook_core::Result<()> {
        match self {
            Self::Collection(collection) => store.save_collection(collection),
            Self::Item {
                collection_id,
                parent,
                index,
                item,
                ..
            } => {
                let mut collections = store.collections()?;
                let Some(collection) = collections
                    .iter_mut()
                    .find(|collection| &collection.id == collection_id)
                else {
                    return Err(restbook_core::Error::Store(format!(
                        "collection {collection_id} is gone"
                    )));
                };
                let items = folder_items(&mut collection.items, parent)?;
                items.insert((*index).min(items.len()), item.as_ref().clone());
                store.save_collection(collection)
            }
            Self::Environment {
                environment,
                was_active,
            } => {
                store.save_environment(environment)?;
                if *was_active {
                    // Deleting an environment clears the selection; undoing
                    // the delete restores the scope a send resolved against.
                    store.set_active_environment(Some(environment.name.as_str()))?;
                }
                Ok(())
            }
        }
    }
}

/// The deepest `items` list a path names, or an error when a folder in the
/// path was removed in the meantime.
fn folder_items<'a>(
    items: &'a mut Vec<CollectionItem>,
    path: &[String],
) -> restbook_core::Result<&'a mut Vec<CollectionItem>> {
    let Some((head, rest)) = path.split_first() else {
        return Ok(items);
    };
    let Some(item) = items.iter_mut().find(|item| match item {
        CollectionItem::Folder(folder) => &folder.id == head,
        CollectionItem::Request(_) => false,
    }) else {
        return Err(restbook_core::Error::Store(format!(
            "folder {head} is gone"
        )));
    };
    match item {
        CollectionItem::Folder(folder) => folder_items(&mut folder.items, rest),
        CollectionItem::Request(_) => unreachable!("filtered to folders above"),
    }
}

/// Find a folder or request and the folder path to its parent, cloning it so
/// the caller can keep it after the tree is mutated.
fn removed_item(
    items: &[CollectionItem],
    id: &str,
    parent: &mut Vec<String>,
) -> Option<(usize, CollectionItem)> {
    for (index, item) in items.iter().enumerate() {
        match item {
            CollectionItem::Folder(folder) => {
                if folder.id == id {
                    return Some((index, item.clone()));
                }
                parent.push(folder.id.clone());
                if let Some(found) = removed_item(&folder.items, id, parent) {
                    return Some(found);
                }
                parent.pop();
            }
            CollectionItem::Request(request) => {
                if request.id == id {
                    return Some((index, item.clone()));
                }
            }
        }
    }
    None
}

fn contains_item(items: &[CollectionItem], id: &str) -> bool {
    items.iter().any(|item| match item {
        CollectionItem::Folder(folder) => folder.id == id || contains_item(&folder.items, id),
        CollectionItem::Request(request) => request.id == id,
    })
}

/// Capture what a pending delete will remove, before the pane mutates the
/// store. `None` means this kind has no faithful restore (history, tabs) or
/// the value is already gone.
fn snapshot_delete(target: &PendingDelete, store: &dyn Store) -> Option<Deleted> {
    match target {
        PendingDelete::Node(collections_panel::RowTarget::Collection(id)) => store
            .collections()
            .ok()?
            .into_iter()
            .find(|collection| &collection.id == id)
            .map(Deleted::Collection),
        PendingDelete::Node(collections_panel::RowTarget::Item(id)) => {
            for collection in store.collections().ok()? {
                let mut parent = Vec::new();
                if let Some((index, item)) = removed_item(&collection.items, id, &mut parent) {
                    let noun = match item {
                        CollectionItem::Folder(_) => "Folder",
                        CollectionItem::Request(_) => "Request",
                    };
                    return Some(Deleted::Item {
                        collection_id: collection.id,
                        parent,
                        index,
                        item: Box::new(item),
                        noun,
                    });
                }
            }
            None
        }
        PendingDelete::Environment(name) => {
            let environment = store
                .environments()
                .ok()?
                .into_iter()
                .find(|environment| &environment.name == name)?;
            let was_active = store.active_environment().ok().flatten().as_deref() == Some(name);
            Some(Deleted::Environment {
                environment,
                was_active,
            })
        }
        // History rows have no insert-with-id operation, and a closed tab is
        // already covered by Reopen Closed Tab; neither gets a second, weaker
        // recovery path here.
        PendingDelete::HistorySend(_)
        | PendingDelete::AllHistory
        | PendingDelete::HistoryOwner(_)
        | PendingDelete::Tab(_)
        | PendingDelete::Tabs { .. } => None,
    }
}

/// Whether the store actually lost the captured value. The panes report their
/// own failures, but a delete that did not land must not offer an undo that
/// would appear to resurrect something still on screen.
fn deletion_landed(deleted: &Deleted, store: &dyn Store) -> bool {
    match deleted {
        Deleted::Collection(collection) => store
            .collections()
            .map(|collections| collections.iter().all(|each| each.id != collection.id))
            .unwrap_or(false),
        Deleted::Item {
            collection_id,
            item,
            ..
        } => {
            let id = match item.as_ref() {
                CollectionItem::Folder(folder) => &folder.id,
                CollectionItem::Request(request) => &request.id,
            };
            store
                .collections()
                .map(|collections| {
                    collections
                        .iter()
                        .find(|collection| &collection.id == collection_id)
                        .is_some_and(|collection| !contains_item(&collection.items, id))
                })
                .unwrap_or(false)
        }
        Deleted::Environment { environment, .. } => store
            .environments()
            .map(|environments| {
                environments
                    .iter()
                    .all(|each| each.name != environment.name)
            })
            .unwrap_or(false),
    }
}

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
fn close_tabs_copy(count: usize, unsaved: usize) -> (String, String, &'static str) {
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

/// The pane names in a saved tree, drops unknown ones, in layout order.
fn panel_names(state: Option<&PanelState>) -> Vec<PanelName> {
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

/// The display name of a tree node, read from the store by id. The delete
/// dialog is raised by the workspace, which only receives the row id, so the
/// name comes back from the same place the sidebar read it.
fn stored_node_name(store: &dyn Store, id: &str) -> Option<String> {
    fn find_in(items: &[restbook_core::CollectionItem], id: &str) -> Option<String> {
        for item in items {
            match item {
                restbook_core::CollectionItem::Folder(folder) => {
                    if folder.id == id {
                        return Some(folder.name.clone());
                    }
                    if let Some(name) = find_in(&folder.items, id) {
                        return Some(name);
                    }
                }
                restbook_core::CollectionItem::Request(request) => {
                    if request.id == id {
                        return Some(request.name.clone());
                    }
                }
            }
        }
        None
    }

    let collections = store.collections().ok()?;
    for collection in &collections {
        if collection.id == id {
            return Some(collection.name.clone());
        }
        if let Some(name) = find_in(&collection.items, id) {
            return Some(name);
        }
    }
    None
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

    /// The last layout edit and the live draft are written when the window
    /// closes.
    ///
    /// The platform asks before it destroys the window, so this is the last
    /// point where a window still exists and the store can be written without
    /// a task being dropped at process exit.
    ///
    /// What follows is gpui-pre's own teardown on Windows, not this app's: its
    /// `WindowsWindow::drop` revokes drag-and-drop and destroys an HWND that
    /// the OS already destroyed for an OS-initiated close, and a window-scoped
    /// task can wake after its window was removed. `flush` marks the process as
    /// closing, and the log sink then drops those target-less records, so a
    /// clean shutdown ends at the line above instead of four `ERROR` lines.
    fn save_on_close(&self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn flush_on_quit(&self, cx: &mut Context<Self>) {
        cx.on_app_quit(|workspace, cx| {
            workspace.flush(cx);
            async {}
        })
        .detach();
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

    /// Install the panes from the saved arrangement when there is one, the default
    /// otherwise.
    ///
    /// The saved state is read for its *names and slot sizes* only: `DockArea::load`
    /// would rebuild panels from the registry, which this app deliberately does
    /// not use. Reading the names and re-adding the same entities survives a
    /// restart with no rebuild, and the sizes keep the proportions the user
    /// dragged instead of snapping back to the defaults.
    fn install_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = persistence::load(&self.layout_path) {
            let left = panel_names(state.left_dock.as_ref().map(|dock| dock.panel()));
            let center = panel_names(Some(&state.center));
            let right = panel_names(state.right_dock.as_ref().map(|dock| dock.panel()));
            if !left.is_empty() || !center.is_empty() || !right.is_empty() {
                let left_size = state.left_dock.as_ref().map(|dock| dock.size());
                let right_size = state.right_dock.as_ref().map(|dock| dock.size());
                let right_open = state.right_dock.as_ref().is_none_or(|dock| dock.open());
                // The centre is a vertical stack, so its two slots are the
                // request/response proportion the user dragged. `dump` writes
                // it; without passing it back the split would start 50/50 on
                // every launch.
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
    fn default_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        log::info!(target: "restbook_app::ui", "reset layout");
        self.default_layout(window, cx);
        persistence::save(&self.layout_path, &self.dock.read(cx).dump(cx));
        self.flash(chrome::FlashKind::Success, "Layout reset", cx);
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
                    if !Self::should_offer(current, &latest) {
                        return;
                    }
                    // No asset for this platform means newer exists but this
                    // machine cannot install it, so an offer would be a dead
                    // end. Settings still names the version.
                    if let Some(asset) = asset {
                        workspace.pending_update = Some(PendingUpdate {
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

    /// Ask whether to install a newer release, without sending the user to
    /// Settings first.
    ///
    /// A plain dialog with a composed footer, not an alert: an alert's
    /// confirm/cancel pair has no room for *Skip this version*, and nothing here
    /// is destructive. The download, the `sha256` check, and handing the package
    /// to the OS are [`Updater::install`]'s job; this decides whether to start it.
    fn open_update_offer_dialog(
        &mut self,
        offer: PendingUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings = self.settings.clone();
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);
        // The download runs in an entity the dialog does not own, so it survives
        // the dialog closing the moment the button is pressed.
        let updates = cx.new(|_| Updates::new());

        window.open_dialog(cx, move |dialog, _window, _cx| {
            let PendingUpdate {
                latest,
                notes,
                asset,
            } = &offer;

            let skip = Button::new("update-offer-skip")
                .label("Skip this version")
                .ghost()
                .on_click({
                    let settings = settings.clone();
                    let latest = latest.clone();
                    move |_, window, cx| {
                        settings.update(cx, |settings, cx| {
                            let mut next = settings.current().clone();
                            next.skipped_update = Some(latest.clone());
                            settings.update(next, cx);
                        });
                        window.close_dialog(cx);
                    }
                });
            let later = Button::new("update-offer-later")
                .label("Later")
                .ghost()
                .on_click(|_, window, cx| window.close_dialog(cx));
            let install = Button::new("update-offer-install")
                .label("Install now")
                .primary()
                .on_click({
                    let updates = updates.clone();
                    let latest = latest.clone();
                    let asset = asset.clone();
                    move |_, window, cx| {
                        updates.update(cx, |updates, cx| {
                            updates.install_offered(latest.clone(), asset.clone(), cx)
                        });
                        window.close_dialog(cx);
                    }
                });

            let detail = match notes.as_deref().map(str::trim) {
                Some(notes) if !notes.is_empty() => v_flex()
                    .gap(px(metrics.space_sm))
                    .child(
                        div()
                            .text_sm()
                            .child(format!("restbook {latest} is available.")),
                    )
                    .child(
                        div()
                            .id("update-offer-notes")
                            .max_h(px(240.))
                            .overflow_y_scroll()
                            .text_sm()
                            .text_color(palette.muted)
                            .child(notes.to_owned()),
                    ),
                _ => v_flex().child(
                    div()
                        .text_sm()
                        .child(format!("restbook {latest} is available.")),
                ),
            };

            dialog
                .title("Update available")
                .w_112()
                .child(v_flex().py(px(metrics.space_sm)).child(detail))
                .footer(DialogFooter::new().child(skip).child(later).child(install))
        });
    }

    /// Whether a found release should be offered to the user.
    ///
    /// Two ways a newer version is still not worth interrupting for: the user
    /// already said *Skip this version*, or the manifest publishes no build for
    /// this platform, in which case an offer could not be acted on. The second
    /// is the caller's business — it has the `Option<Asset>` — so this is the
    /// skip rule alone, kept apart from the task so it can be tested without a
    /// manifest.
    fn should_offer(current: &settings::AppSettings, latest: &str) -> bool {
        current.skipped_update.as_deref() != Some(latest)
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        // A new edit replaces the pending write, so a drag costs one save.
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
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

    /// Persist the active tab as a draft a pause after the last edit, mirroring the
    /// layout debounce. A burst of typing costs one write; only the active tab is
    /// live, the rest were snapshotted when they were left.
    ///
    /// The document notifies on every edit and every progress tick during a send, so
    /// this only bumps `draft_gen`; one live task re-reads it until things go quiet.
    fn schedule_draft_save(&mut self, cx: &mut Context<Self>) {
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

    /// Write the active tab to the drafts table, or clear its row once it has been
    /// filed into a collection. A blank URL is not worth restoring, so an empty new
    /// tab leaves nothing behind.
    ///
    /// This only runs at the end of a quiet debounce, never on a keystroke. A new
    /// draft's first insert is therefore synchronous: it is what gives the tab its
    /// row id, and until that id is bound every later write (and the close flush)
    /// would insert again instead of updating. Once bound, updates stay on the
    /// background executor, where a slow disk cannot stall the window.
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
            match self.store.save_draft(&draft) {
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

    /// Write one tab's draft synchronously. Used when the tab is leaving the
    /// screen (a switch or a quit): the background writer reads whichever tab
    /// is active when it wakes, and a task spawned here would be dropped with
    /// the process.
    ///
    /// `dirty` says a debounce was armed, so there are edits the store has not
    /// seen. Without it, a row that is already stored is left untouched so its
    /// `updated_at` does not reorder the Unsaved list on every switch.
    fn persist_tab_draft(&mut self, index: usize, dirty: bool, cx: &mut Context<Self>) {
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
    fn refresh_drafts(&mut self, cx: &mut Context<Self>) {
        self.panes
            .collections
            .update(cx, |panel, cx| panel.refresh_drafts(cx));
    }

    /// Take a snapshot of the active tab and load the one at `index`.
    fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index == self.tabs.active() || index >= self.tabs.len() {
            return;
        }
        log::info!(target: "restbook_app::ui", "select tab {index}");
        // Flush before the selection moves: the debounced writer reads the
        // active tab, so an edit-and-switch inside `SAVE_DEBOUNCE` would
        // otherwise be written against the tab being switched to.
        self.flush_leaving_draft(cx);
        self.tabs.set_active(index);
        self.adopt_active(cx);
        // The "Sending" marker in the document belongs to the tab that started
        // the send. Landing anywhere else clears it, so the newly active tab
        // shows its own state instead of inheriting someone else's in-flight
        // request, and its Send button is its own to press.
        if self.sending != self.active_tab_id() {
            self.document.update(cx, |document, cx| {
                document.forget_send();
                cx.notify();
            });
        }
    }

    /// Select a tab by its stable id, the way the strip reports a click.
    fn select_tab_id(&mut self, id: TabId, cx: &mut Context<Self>) {
        if let Some(index) = self.tabs.position(id) {
            self.select_tab(index, cx);
        }
    }

    /// Step to the next (`delta = 1`) or previous (`delta = -1`) tab, wrapping
    /// around at either end.
    fn select_relative(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.tabs.len();
        if len < 2 {
            return;
        }
        let next = wrap_index(self.tabs.active(), len, delta);
        self.select_tab(next, cx);
    }

    /// Write the active tab's draft before it stops being the one on screen.
    /// The debounced writer reads whichever tab is active when it wakes, so a
    /// switch inside `SAVE_DEBOUNCE` would otherwise write the wrong tab and
    /// leave the leaving tab's last edits in memory only.
    fn flush_leaving_draft(&mut self, cx: &mut Context<Self>) {
        let dirty = self.draft_task.take().is_some();
        self.snapshot_active(cx);
        if !self.tabs.is_empty() {
            let index = self.tabs.active();
            self.persist_tab_draft(index, dirty, cx);
        }
    }

    /// A pasted cURL command replaced the active request. Confirm the import,
    /// then offer the first-save dialog so the request gets a name and a home
    /// instead of silently becoming another unsaved tab. A workspace with no
    /// collection to save into skips the dialog: the sidebar would only be able
    /// to reject it.
    fn imported(&mut self, cx: &mut Context<Self>) {
        self.notice(
            &Notice::Done("Imported a request from the clipboard.".into()),
            cx,
        );
        if self.panes.collections.read(cx).save_destination().is_none() {
            return;
        }
        self.pending_save = true;
        cx.notify();
    }

    /// Copy the active request as a runnable `curl` command. It uses the live
    /// editor, not the document snapshot, so a field that notifies its own
    /// state (the raw body, an auth input) is not one keystroke behind.
    fn copy_curl(&mut self, cx: &mut Context<Self>) {
        let request = self.panes.request.read(cx).current_request(cx);
        if request.url.trim().is_empty() {
            self.flash(chrome::FlashKind::Error, "There is no request to copy.", cx);
            return;
        }
        let command = restbook_core::import::curl::to_curl(&request);
        cx.write_to_clipboard(ClipboardItem::new_string(command));
        self.flash(
            chrome::FlashKind::Success,
            "Copied the request as a cURL command.",
            cx,
        );
    }

    /// Save the active request.
    ///
    /// A tab that came from a collection node overwrites it silently: the name and
    /// destination were decided when it was created. A draft has neither, so it asks
    /// first — the dialog needs a window the event does not carry, so the request is
    /// parked until render.
    fn save_active(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.tabs.active()) else {
            return;
        };
        let node = tab.node.clone();
        if node.is_none() {
            if self.panes.collections.read(cx).save_destination().is_none() {
                self.flash(
                    chrome::FlashKind::Error,
                    "Nothing to save into — select a collection in the sidebar first.",
                    cx,
                );
                return;
            }
            self.pending_save = true;
            cx.notify();
            return;
        }
        self.commit_save(node, None, cx);
    }

    /// Save the active request as a new node, even when the tab is already
    /// bound. The dialog opens with the bound name prefilled and `node` unset,
    /// so the save creates an entry instead of overwriting the source.
    fn save_active_as(&mut self, cx: &mut Context<Self>) {
        if self.panes.collections.read(cx).save_destination().is_none() {
            self.flash(
                chrome::FlashKind::Error,
                "Nothing to save into — select a collection in the sidebar first.",
                cx,
            );
            return;
        }
        self.pending_save = true;
        cx.notify();
    }

    /// Write the open request into `node` (an existing collection node) or,
    /// with `None`, into the sidebar's selection under `name`.
    ///
    /// The node the save landed on is written back onto the tab, so a tab saved
    /// for the first time is bound to it and the next Save updates rather than
    /// duplicates.
    fn commit_save(
        &mut self,
        node: Option<String>,
        name: Option<&str>,
        cx: &mut Context<Self>,
    ) -> bool {
        let landed = self.panes.collections.update(cx, |panel, cx| {
            panel.save_current(node.as_deref(), name, cx)
        });
        let Some(node) = landed else {
            // Nothing was saved, and the pane could not say so on this path.
            self.flash(
                chrome::FlashKind::Error,
                "Nothing to save into — select a collection in the sidebar first.",
                cx,
            );
            return false;
        };
        // A saved request and its tab must compare equal, or the unsaved dot
        // would appear the moment the save completes. The tab is no longer a
        // draft, so its stored row is removed.
        let request = self.panes.request.read(cx).current_request(cx);
        let origin = Origin::Saved { node: node.clone() };
        let active = self.tabs.active();
        let Some(tab) = self.tabs.get_mut(active) else {
            return false;
        };
        let stale = {
            tab.node = Some(node);
            tab.saved = Some(RequestStamp::of(&request));
            tab.request = request;
            tab.draft.take()
        };
        if let Some(id) = stale {
            // The draft became this node: carry its sends across instead of
            // orphaning them under a draft id that is about to be deleted.
            let _ = self
                .store
                .reassign_history_origin(&Origin::Draft { draft: id }, &origin);
            let _ = self.store.delete_draft(id);
        }
        // The response pane scopes history on this, so the new owner's sends
        // show without waiting for a tab switch.
        self.document.update(cx, |document, cx| {
            document.origin = origin;
            cx.notify();
        });
        self.push_tabs(cx);
        if stale.is_some() {
            self.refresh_drafts(cx);
        }
        true
    }

    /// Ask for a name, then save. Called from render once `pending_save` is set,
    /// because only render has the window a dialog needs.
    fn open_save_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Prefill with the name a save would have picked on its own, so Enter
        // keeps the old behavior and typing a name is the only new step.
        let request = self.panes.request.read(cx).current_request(cx);
        let default_name = format!("{} {}", request.method, request.url);
        self.save_name
            .update(cx, |state, cx| state.set_value(default_name, window, cx));

        let destination = self.panes.collections.read(cx).save_destination();
        let field = self.save_name.clone();
        let name_input = self.save_name.clone();
        let workspace = cx.entity().downgrade();
        // One confirm for the footer button and for Enter: it reads the field at
        // the moment of the click, not when the dialog was built.
        type Confirm = Rc<dyn Fn(&mut Window, &mut App) -> bool>;
        let confirm: Confirm = Rc::new(move |_, cx| {
            let name = name_input.read(cx).value().trim().to_owned();
            if name.is_empty() {
                // An unnamed request would be unfindable in the tree; keep the
                // dialog open rather than saving something the user has to fix.
                return false;
            }
            workspace
                .update(cx, |workspace, cx| {
                    workspace.commit_save(None, Some(&name), cx)
                })
                .unwrap_or(false)
        });
        let for_button = confirm.clone();
        let first_build = std::cell::Cell::new(true);
        let focus_input = self.save_name.clone();
        // Built from the theme file, like every other pane, so a theme swap
        // reaches this dialog's muted labels too.
        let palette = Palette::new(cx);

        window.open_dialog(cx, move |dialog, window, cx| {
            // The dialog shell takes focus when it opens, so the name field has
            // to claim it back on the first build only — later builds are the
            // same dialog and must not steal the caret from the user.
            if first_build.replace(false) {
                focus_input.update(cx, |state, cx| {
                    state.focus_handle(cx).focus(window, cx);
                });
            }

            let destination = destination
                .clone()
                .unwrap_or_else(|| "no collection selected".to_owned());
            let metrics = Metrics::current(cx);
            let into = h_flex()
                .gap(px(metrics.space_sm))
                .items_center()
                .text_sm()
                .child(div().text_color(palette.muted).child("Into"))
                .child(div().child(destination));

            let name_row = v_flex()
                .gap(px(metrics.space_xs))
                .child(div().text_xs().text_color(palette.muted).child("Name"))
                .child(Input::new(&field).id("save-request-name"));

            let cancel = Button::new("save-request-cancel")
                .label("Cancel")
                .ghost()
                .on_click(|_, window, cx| window.close_dialog(cx));
            let save = Button::new("save-request-confirm")
                .label("Save")
                .primary()
                .on_click({
                    let confirm = for_button.clone();
                    move |_, window, cx| {
                        if confirm(window, cx) {
                            window.close_dialog(cx);
                        }
                    }
                });

            dialog
                .title("Save request")
                // w_112 is 28rem (448px), the nearest named width to the old
                // 460px; the app's rem scale has no 28.75rem step.
                .w_112()
                .child(
                    v_flex()
                        .gap(px(metrics.space_md))
                        .py(px(metrics.space_sm))
                        .child(name_row)
                        .child(into),
                )
                .footer(DialogFooter::new().child(cancel).child(save))
                .on_ok({
                    let confirm = for_button.clone();
                    move |_, window, cx| confirm(window, cx)
                })
        });
    }

    /// Confirm a destructive delete before doing it. A modal alert dialog, not
    /// the two-click dance the panes used to run: a dialog is what a user
    /// expects before a collection or an environment is destroyed, it can name
    /// the thing, and it cannot be triggered by a mis-aimed second click.
    fn open_delete_dialog(
        &mut self,
        request: PendingDelete,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // What the dialog says and what its confirm button does, per target.
        let (title, description, confirm_label) = match &request {
            PendingDelete::Node(collections_panel::RowTarget::Collection(id)) => (
                match stored_node_name(self.store.as_ref(), id) {
                    Some(name) => format!("Delete \u{201c}{name}\u{201d}?"),
                    None => "Delete collection".to_owned(),
                },
                "The collection and everything in it will be deleted. You can undo this \
                 from the toast for a few seconds."
                    .to_owned(),
                "Delete collection".to_owned(),
            ),
            PendingDelete::Node(collections_panel::RowTarget::Item(id)) => (
                match stored_node_name(self.store.as_ref(), id) {
                    Some(name) => format!("Delete \u{201c}{name}\u{201d}?"),
                    None => "Delete item".to_owned(),
                },
                "This item will be deleted. You can undo this from the toast for a few \
                 seconds."
                    .to_owned(),
                "Delete".to_owned(),
            ),
            PendingDelete::Environment(name) => (
                "Delete environment".to_owned(),
                format!(
                    "Delete the environment \"{name}\"? You can undo this from the toast \
                     for a few seconds."
                ),
                "Delete environment".to_owned(),
            ),
            PendingDelete::HistorySend(_) => (
                "Delete send".to_owned(),
                "Delete this send from history? This cannot be undone.".to_owned(),
                "Delete send".to_owned(),
            ),
            PendingDelete::AllHistory => (
                "Clear history".to_owned(),
                "Delete every stored send? This cannot be undone.".to_owned(),
                "Clear history".to_owned(),
            ),
            PendingDelete::HistoryOwner(_) => (
                "Clear this request's history".to_owned(),
                "Delete every stored send for this request? This cannot be undone.".to_owned(),
                "Clear history".to_owned(),
            ),
            PendingDelete::Tab(id) => {
                // The active tab's snapshot is stale by design, so name it from
                // the document, which the pane keeps in step with the editor.
                let label: SharedString = match self.tabs.position(*id) {
                    Some(_) if self.tabs.active_id() == Some(*id) => {
                        request_panel::tab_label(&self.document.read(cx).request)
                    }
                    Some(index) => request_panel::tab_label(&self.tabs[index].request),
                    None => "this tab".into(),
                };
                (
                    "Close tab".to_owned(),
                    format!("Close \"{label}\" without saving? The edits will be lost."),
                    "Close without saving".to_owned(),
                )
            }
            PendingDelete::Tabs { ids, unsaved } => {
                let (title, description, confirm_label) = close_tabs_copy(ids.len(), *unsaved);
                (title, description, confirm_label.to_owned())
            }
        };
        // The dialog builder runs again on every notify, so the confirm closure
        // needs its own owned copy of what to delete.
        let target = request;

        let workspace = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            let workspace = workspace.clone();
            let target = target.clone();
            alert
                .title(title.clone())
                .description(description.clone())
                .confirm()
                .ok_text(confirm_label.clone())
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.perform_delete(target.clone(), window, cx)
                        })
                        .ok();
                    true
                })
        });
    }

    /// Carry out the delete a dialog confirmed.
    fn perform_delete(
        &mut self,
        target: PendingDelete,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Retire the previous offer before the new act, including a delete
        // that has no undo of its own (history, tabs).
        self.dismiss_flash();
        let deleted = snapshot_delete(&target, self.store.as_ref());

        match target {
            PendingDelete::Node(collections_panel::RowTarget::Collection(id)) => {
                self.panes
                    .collections
                    .update(cx, |panel, cx| panel.delete_collection(&id, window, cx));
            }
            PendingDelete::Node(collections_panel::RowTarget::Item(id)) => {
                self.panes
                    .collections
                    .update(cx, |panel, cx| panel.delete_item(&id, window, cx));
            }
            PendingDelete::Environment(name) => {
                self.panes
                    .environment
                    .update(cx, |panel, cx| panel.delete_environment(&name, cx));
            }
            PendingDelete::HistorySend(id) => match self.store.delete_history(id) {
                Ok(_) => self.history_changed(cx),
                Err(error) => {
                    log::error!(
                        target: "restbook_app::ui",
                        "history delete id={id} failed: {error}"
                    );
                    self.notice(
                        &Notice::Failed(format!("Couldn't delete the send. {error}").into()),
                        cx,
                    );
                }
            },
            PendingDelete::AllHistory => match self.store.clear_history() {
                Ok(_) => self.history_changed(cx),
                Err(error) => {
                    log::error!(
                        target: "restbook_app::ui",
                        "history clear all failed: {error}"
                    );
                    self.notice(
                        &Notice::Failed(format!("Couldn't clear the history. {error}").into()),
                        cx,
                    );
                }
            },
            PendingDelete::HistoryOwner(origin) => {
                match self.store.delete_history_for_origin(&origin) {
                    Ok(_) => self.history_changed(cx),
                    Err(error) => {
                        log::error!(
                            target: "restbook_app::ui",
                            "history clear for owner failed: {error}"
                        );
                        self.notice(
                            &Notice::Failed(
                                format!("Couldn't clear this request's history. {error}").into(),
                            ),
                            cx,
                        );
                    }
                }
            }
            PendingDelete::Tab(id) => self.close_tab(id, cx),
            PendingDelete::Tabs { ids, .. } => {
                for id in ids {
                    self.close_tab(id, cx);
                }
            }
        }

        // The panes report a failed write themselves, so only offer an undo
        // once the store actually lost the value.
        if let Some(deleted) = deleted {
            if deletion_landed(&deleted, self.store.as_ref()) {
                self.flash_undo(deleted, cx);
            }
        }
    }

    /// Open the per-request history as a slide-in sheet. The response pane
    /// searched the rows; the sheet only draws them and restores a pick, which
    /// is a document write. Called from render once `pending_history` is set.
    fn open_history_sheet(
        &mut self,
        hits: Vec<restbook_core::HistoryHit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        log::info!(
            target: "restbook_app::ui",
            "opening response history sheet ({} rows)",
            hits.len(),
        );
        let origin = self.document.read(cx).origin.clone();
        let workspace = cx.entity().downgrade();
        history_sheet::open(
            hits,
            origin,
            self.document.clone(),
            self.store.clone(),
            workspace,
            window,
            cx,
        );
    }

    /// A history mutation landed in the store. The caller already wrote it, so
    /// this only tells every view that cached a list of it to re-read: the
    /// response pane keys its scoped list on the document revision, and the
    /// sidebar holds a search result that no longer matches.
    pub(crate) fn history_changed(&mut self, cx: &mut Context<Self>) {
        self.document.update(cx, |document, cx| {
            document.touch_history();
            cx.notify();
        });
        self.panes.history.update(cx, |panel, cx| panel.reload(cx));
    }

    /// Park a request to delete one send so the next render can open the alert
    /// dialog. The history sheet has a window but no `Context<Workspace>` of its
    /// own, so it asks through the workspace, the way the panes do.
    pub(crate) fn confirm_history_send(&mut self, id: i64, cx: &mut Context<Self>) {
        self.pending_delete = Some(PendingDelete::HistorySend(id));
        cx.notify();
    }

    /// Park a request to clear one owner's sends for the alert dialog.
    pub(crate) fn confirm_clear_history_for_origin(
        &mut self,
        origin: Origin,
        cx: &mut Context<Self>,
    ) {
        self.pending_delete = Some(PendingDelete::HistoryOwner(origin));
        cx.notify();
    }

    /// Open a saved request from the sidebar.
    ///
    /// A node that is already open is brought forward rather than re-adopted,
    /// so the tab on screen is never overwritten. Otherwise the request gets a
    /// tab of its own, except when the window is sitting on its lone untouched
    /// placeholder: taking that tab over keeps startup from stranding an empty
    /// request beside the one that was just opened. A tab the user made with
    /// New tab is theirs to keep, even before its first edit.
    fn open_saved(&mut self, node: String, request: RequestDef, cx: &mut Context<Self>) {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.node.as_deref() == Some(node.as_str()))
        {
            // Already open: switching to it is the whole effect, and it leaves
            // whatever the active tab holds (a draft, or a fresh New tab) alone.
            self.select_tab(index, cx);
            return;
        }

        let take_over =
            self.tabs.is_empty() || (self.tabs.len() == 1 && self.tabs[0].is_pristine());
        if !take_over {
            // `select_tab` snapshots the tab being left before it stops being the
            // active one.
            self.tabs.push(Tab::blank());
            let index = self.tabs.len() - 1;
            self.select_tab(index, cx);
        }
        self.adopt_saved(node, request, cx);
    }

    /// Write a saved request into the tab on screen and bind it to its node,
    /// so a later Save updates that node instead of appending a duplicate. Any
    /// draft the tab was persisting is stale, so its sends are reassigned and
    /// the row is deleted.
    fn adopt_saved(&mut self, node: String, request: RequestDef, cx: &mut Context<Self>) {
        self.panes
            .request
            .update(cx, |panel, cx| panel.adopt_request(request.clone(), cx));
        // The editor queues the request for its next render; the document must
        // be updated on the same tick or Send reads an empty URL until a second
        // click re-fires the editor's observer.
        self.document.update(cx, |document, cx| {
            document.request = request.clone();
            // The response belongs to the request that was on screen, not to the
            // one this row is about to open.
            document.response = None;
            document.error = None;
            document.last_url = None;
            // A Canceled strip belongs to the send that just ended.
            document.forget_cancel();
            // Sends belong to the node, not the URL, so they are still this
            // request's history in the response pane.
            document.origin = Origin::Saved { node: node.clone() };
            cx.notify();
        });

        // With no tab open (every one was closed) this revives a fresh one.
        let index = self.ensure_active_tab();
        let stale = {
            let tab = &mut self.tabs[index];
            tab.request = request.clone();
            tab.response = None;
            tab.error = None;
            tab.last_url = None;
            tab.saved = Some(RequestStamp::of(&request));
            tab.node = Some(node.clone());
            tab.draft.take()
        };
        if let Some(id) = stale {
            // The draft just became a saved request. Its sends are the same
            // requests, so they follow the new owner rather than disappearing
            // behind a dead draft id.
            let _ = self.store.reassign_history_origin(
                &Origin::Draft { draft: id },
                &Origin::Saved { node: node.clone() },
            );
            let _ = self.store.delete_draft(id);
        }
        self.push_tabs(cx);
        if stale.is_some() {
            self.refresh_drafts(cx);
        }
    }

    fn new_tab(&mut self, cx: &mut Context<Self>) {
        // A new tab is a blank page, not a copy of whatever is on screen: the
        // active request may be unsaved, and cloning it made the new tab start
        // with work the user did not ask to carry over. Saving it creates a new
        // entry, while the tab left behind keeps its own edits.
        self.flush_leaving_draft(cx);
        let index = self.tabs.push(Tab::blank());
        self.tabs.set_active(index);
        self.adopt_active(cx);
        log::info!(target: "restbook_app::ui", "new tab: {} open", self.tabs.len());
    }

    /// Open a stored draft as a new tab. Used when the sidebar's Unsaved row is
    /// clicked for a draft that is not already open — the normal case now that
    /// the window starts on a blank tab instead of restoring every draft.
    fn open_draft(&mut self, draft: Draft, cx: &mut Context<Self>) {
        let id = draft.id;
        self.flush_leaving_draft(cx);
        let index = self.tabs.push(Tab::from_draft(draft));
        self.tabs.set_active(index);
        self.adopt_active(cx);
        log::info!(
            target: "restbook_app::ui",
            "open draft: {} {} (id {})",
            self.tabs[index].request.method,
            self.tabs[index].request.url,
            id,
        );
    }

    /// The tab on screen, by its stable id rather than its position.
    fn active_tab_id(&self) -> Option<TabId> {
        self.tabs.active_id()
    }

    /// The active tab, creating a fresh one when the last tab was closed.
    /// Opening a saved request or a draft from the sidebar is the one action
    /// that revives an empty editor, so it is factored out instead of repeated
    /// in each event arm.
    fn ensure_active_tab(&mut self) -> usize {
        if self.tabs.is_empty() {
            self.tabs.push(Tab::blank());
            self.tabs.set_active(0);
        }
        self.tabs.active()
    }

    /// Close a tab, asking first when the close would throw work away. A clean
    /// tab (and a blank one) closes at once, so the strip stays quick; only a
    /// tab with edits that are not in its collection needs the alert dialog.
    ///
    /// The id is stable while the dialog is open: it is modal, so nothing else
    /// can close a tab underneath it.
    fn confirm_close_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.position(id) else {
            return;
        };
        if self.tab_has_unsaved_work(index, cx) {
            self.pending_delete = Some(PendingDelete::Tab(id));
            cx.notify();
        } else {
            self.close_tab(id, cx);
        }
    }

    /// Route a tab verb the strip reported.
    fn handle_tab_action(&mut self, action: TabAction, cx: &mut Context<Self>) {
        match action {
            TabAction::Close(id) => self.confirm_close_tab(id, cx),
            TabAction::CloseOthers(id) => {
                let ids = self
                    .tabs
                    .iter()
                    .filter(|tab| tab.id != id)
                    .map(|tab| tab.id)
                    .collect();
                self.confirm_close_tabs(ids, cx);
            }
            TabAction::CloseToRight(id) => {
                let Some(index) = self.tabs.position(id) else {
                    return;
                };
                let ids = self.tabs.iter().skip(index + 1).map(|tab| tab.id).collect();
                self.confirm_close_tabs(ids, cx);
            }
            TabAction::CloseAll => {
                let ids = self.tabs.iter().map(|tab| tab.id).collect();
                self.confirm_close_tabs(ids, cx);
            }
            TabAction::Duplicate(id) => self.duplicate_tab(id, cx),
            TabAction::MoveLeft(id) => self.move_tab(id, -1, cx),
            TabAction::MoveRight(id) => self.move_tab(id, 1, cx),
        }
    }

    /// Close a batch of tabs. One tab goes through the ordinary single-tab
    /// confirmation; a real batch asks once with a summary, so closing ten
    /// drafts is not ten dialogs.
    fn confirm_close_tabs(&mut self, ids: Vec<TabId>, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        if ids.len() == 1 {
            self.confirm_close_tab(ids[0], cx);
            return;
        }
        let unsaved = ids
            .iter()
            .filter_map(|id| self.tabs.position(*id))
            .filter(|index| self.tab_has_unsaved_work(*index, cx))
            .count();
        if unsaved == 0 {
            self.close_tabs(ids, cx);
        } else {
            self.pending_delete = Some(PendingDelete::Tabs { ids, unsaved });
            cx.notify();
        }
    }

    /// Close every tab in `ids` without asking. Called once a batch dialog was
    /// confirmed, or when none of them would lose work.
    fn close_tabs(&mut self, ids: Vec<TabId>, cx: &mut Context<Self>) {
        for id in ids {
            self.close_tab(id, cx);
        }
    }

    /// Copy a tab into an independent tab beside it and select the copy.
    fn duplicate_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.position(id) else {
            return;
        };
        // Snapshot and flush the live tab first, so a duplicate of the tab on
        // screen copies the edits that are in the editor, not the last snapshot.
        self.flush_leaving_draft(cx);
        let copy = self.tabs[index].copy_of();
        self.tabs.insert(index + 1, copy);
        self.tabs.set_active(index + 1);
        self.adopt_active(cx);
        log::info!(target: "restbook_app::ui", "duplicated tab {id}");
    }

    /// Move a tab one place left (`delta = -1`) or right (`delta = 1`), taking
    /// the selection with it when it is the tab being moved.
    fn move_tab(&mut self, id: TabId, delta: isize, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.position(id) else {
            return;
        };
        let target = index as isize + delta;
        if target < 0 || target as usize >= self.tabs.len() {
            return;
        }
        let target = target as usize;
        self.tabs.swap(index, target);
        if self.tabs.active() == index {
            self.tabs.set_active(target);
        } else if self.tabs.active() == target {
            self.tabs.set_active(index);
        }
        self.push_tabs(cx);
        log::info!(target: "restbook_app::ui", "moved tab {id} to {target}");
    }

    /// Reopen the most recently closed tab. The stack is process-local and
    /// emptied on launch, so this never resurrects work from a previous run.
    /// `pub(crate)` because the command palette is wired to it from another
    /// module that does not own the stack.
    pub(crate) fn reopen_closed_tab(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.closed.pop() else {
            self.flash(
                chrome::FlashKind::Error,
                "There is no closed tab to reopen.",
                cx,
            );
            return;
        };
        self.flush_leaving_draft(cx);
        let index = self.tabs.push(tab.reopened());
        self.tabs.set_active(index);
        self.adopt_active(cx);
        log::info!(target: "restbook_app::ui", "reopened a closed tab");
    }

    /// Whether closing the tab at `index` would lose work: it has a saved
    /// baseline and the request no longer matches it, or it was never saved and
    /// is not an empty page. A blank tab holds nothing worth a prompt.
    fn tab_has_unsaved_work(&self, index: usize, cx: &App) -> bool {
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        // The active tab is not snapshotted while it is on screen, so its live
        // request is the one that would be lost.
        let live;
        let request = if index == self.tabs.active() {
            live = self.panes.request.read(cx).current_request(cx);
            &live
        } else {
            &tab.request
        };
        close_needs_confirmation(
            tab.saved.as_deref(),
            &RequestStamp::of(request),
            request.url.trim().is_empty(),
        )
    }

    fn close_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.position(id) else {
            return;
        };

        // Snapshot before removal, but only the tab on screen: a background tab
        // was already snapshotted when it was left, and reading the live editor
        // into it would copy the wrong request.
        let was_active = index == self.tabs.active();
        if was_active {
            self.snapshot_active(cx);
        }

        let stale = self.tabs[index].draft;
        if let Some(id) = stale {
            let _ = self.store.delete_draft(id);
        }
        if self.sending == Some(self.tabs[index].id) {
            // The tab that owned the send is gone. The transfer still resolves
            // in the background, but nothing will take its result, so the send
            // slot is free again and the "Sending" marker goes with the tab.
            self.sending = None;
            self.document.update(cx, |document, cx| {
                document.forget_send();
                cx.notify();
            });
        }
        // Keep the content for Reopen Closed Tab. The draft row is gone, so a
        // reopened tab comes back as a copy with a new identity and no row.
        self.closed.push(self.tabs[index].clone());
        if self.closed.len() > CLOSED_TAB_LIMIT {
            self.closed.remove(0);
        }
        if was_active {
            // The armed debounce belonged to the tab that just closed; let it
            // go rather than have it write whichever tab becomes active.
            self.draft_task.take();
        }
        self.tabs.remove(index);

        if self.tabs.is_empty() {
            // The last tab is really gone: the request pane draws an empty
            // state, and no index would be safe to dereference.
            self.tabs.set_active(0);
            self.adopt_active(cx);
        } else if was_active {
            // The tab that took its place is the one at the same index, or the
            // last one when the closed tab was the rightmost.
            self.tabs.set_active(index.min(self.tabs.len() - 1));
            self.adopt_active(cx);
        } else {
            if index < self.tabs.active() {
                self.tabs.set_active(self.tabs.active() - 1);
            }
            self.push_tabs(cx);
        }

        if stale.is_some() {
            self.refresh_drafts(cx);
        }
        log::info!(
            target: "restbook_app::ui",
            "closed tab: {} open, active {}",
            self.tabs.len(),
            self.tabs.active(),
        );
    }

    /// Store the live editor and document in the tab being left.
    fn snapshot_active(&mut self, cx: &App) {
        let active = self.tabs.active();
        let Some(tab) = self.tabs.get_mut(active) else {
            return;
        };
        let request = self.panes.request.read(cx).current_request(cx);
        let (response, error, last_url) = {
            let document = self.document.read(cx);
            (
                document.response.clone(),
                document.error.clone(),
                document.last_url.clone(),
            )
        };
        tab.request = request;
        tab.response = response;
        tab.error = error;
        tab.last_url = last_url;
    }

    /// Write the active tab into the editor and the document. The editor applies
    /// the request on its next render, the same path a history pick uses. With
    /// no tab left, both are emptied so the strip below does not describe a
    /// request that is no longer open.
    fn adopt_active(&mut self, cx: &mut Context<Self>) {
        let active = self.tabs.active();
        let Some(tab) = self.tabs.get(active).cloned() else {
            self.clear_editor(cx);
            return;
        };
        // Read the owner before the fields below are moved into the closures.
        let origin = tab.origin();
        self.panes
            .request
            .update(cx, |panel, cx| panel.adopt_request(tab.request, cx));
        self.document.update(cx, |document, cx| {
            document.forget_cancel();
            document.response = tab.response;
            document.error = tab.error;
            document.last_url = tab.last_url;
            document.origin = origin;
            cx.notify();
        });
        self.push_tabs(cx);
    }

    /// Reset the editor and document for a window with no open request.
    fn clear_editor(&mut self, cx: &mut Context<Self>) {
        let blank = RequestDef::new(Method::Get, "");
        self.panes
            .request
            .update(cx, |panel, cx| panel.adopt_request(blank.clone(), cx));
        self.document.update(cx, |document, cx| {
            document.forget_cancel();
            document.request = blank;
            document.response = None;
            document.error = None;
            document.last_url = None;
            document.origin = Origin::default();
            cx.notify();
        });
        self.push_tabs(cx);
    }

    /// Hand the open-tab labels to the request pane, which draws the strip.
    /// Called after every change to the tab list, so the pane never has to
    /// guess what is open; the live label of the active tab is its own.
    fn push_tabs(&mut self, cx: &mut Context<Self>) {
        let tabs = self.tabs.entries();
        let active = self.tabs.active();
        self.panes
            .request
            .update(cx, |panel, cx| panel.set_tabs(tabs, active, cx));
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
    use super::{close_needs_confirmation, close_tabs_copy, wrap_index, Workspace};

    /// The lane's rule: a skipped version does not offer itself again, but
    /// skipping `0.1.4` must not silence `0.1.5`.
    #[test]
    fn a_skipped_version_is_not_offered_but_the_next_one_is() {
        use super::settings::AppSettings;

        let mut settings = AppSettings::default();
        assert!(Workspace::should_offer(&settings, "0.1.4"));

        settings.skipped_update = Some("0.1.4".to_owned());
        assert!(!Workspace::should_offer(&settings, "0.1.4"));
        assert!(Workspace::should_offer(&settings, "0.1.5"));
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

        use super::super::{deletion_landed, snapshot_delete, Deleted, PendingDelete};
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
