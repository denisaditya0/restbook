//! The Collections pane: saved requests in nested folders.
//!
//! A collection is a user-edited tree persisted as one store row, so every edit
//! mutates the working copy and saves. Opening a request is an [`Event`] for the
//! canvas; the tree walks and edits live in [`tree`] so they stay testable
//! without an `App` or a `Window`.

mod settings;
mod tree;

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{h_flex, v_flex, WindowExt as _};
use gpui_kit::*;

use restbook_core::{
    Auth, Collection, CollectionItem, Draft, Folder, Kv, Method, RequestDef, SavedRequest, Store,
};

use self::tree::{
    display_name, filter_paths, find_in_items_all, find_saved_mut, has_move_target, initial,
    insert_into, item_name, move_item, node_name, owner_collection, remove_item, rename_item,
    resolve_selection,
};

pub(crate) use self::tree::{import_into, Destination, DestinationRow, ImportOutcome};

use crate::actions::{shortcut, ImportFile, PasteCurl, RenameSelected};
use crate::document::Document;
use crate::icons::{self, IconName};
use crate::notice::Notice;
use crate::panels::editor::InheritedDefaults;
use crate::theme::{Metrics, Palette};

use super::empty::{self, Action};

// `Open` carries a whole `RequestDef`, which is much larger than `OpenDraft`'s
// `i64`. The variant is constructed and matched in exactly two places, so the
// size difference never shows up as a cost; boxing it would only add noise.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Event {
    /// A saved request row was clicked. The workspace decides what opening it
    /// means; the pane hands over the whole request rather than restoring it.
    /// `node` is the row's id, so the tab can later save back over the same
    /// node instead of appending a duplicate.
    Open { node: String, request: RequestDef },
    /// An unsaved draft row was clicked. The pane does not own the tabs, so it
    /// only names the stored draft; the workspace selects the tab holding it.
    OpenDraft { id: i64 },
    /// A mutation finished and wants to say so. The pane has no window on every
    /// path (`Ctrl+S` reaches it through the workspace), so it hands the
    /// confirmation up rather than shouting into a window it may not have; the
    /// workspace owns the transient toast.
    Notice(Notice),
    /// The user asked to delete a row. The pane cannot open a dialog (it is a
    /// dock panel with no window on every path), so it names the row and the
    /// workspace raises the confirmation and performs the delete.
    ConfirmDelete(RowTarget),
    /// The user asked to move a saved request elsewhere. Like `ConfirmDelete`,
    /// the destination picker is a window-level dialog, so the pane only names
    /// the node; the workspace opens the picker and calls back.
    Move { node: String },
    /// The header asked for the save dialog. Saving is a collection verb, so it
    /// lives here now instead of in the window menu; the pane has no window, so
    /// the workspace raises the dialog.
    SaveRequest,
    /// The header asked for a file to import. Deferred like the request pane's
    /// empty-state button, for the same reason: a picker needs a window.
    Import,
    /// The header asked to export. The workspace owns the picker and the write.
    Export,
    /// The settings disclosure saved a collection's variables, auth, or headers.
    /// The workspace re-reads the active tab's collection scope so the next send
    /// and the inspector see the new rows.
    CollectionChanged,
}

/// One line for the interaction log, derived from the event itself. Exhaustive,
/// so a new variant cannot be added without deciding how it reads there.
impl Event {
    pub(crate) fn summary(&self) -> Option<String> {
        match self {
            Event::Open { node, request } => Some(format!(
                "event: open saved request {} {} (node {node})",
                request.method, request.url
            )),
            Event::OpenDraft { id } => Some(format!("event: open draft id={id}")),
            Event::ConfirmDelete(target) => Some(format!("event: confirm delete {}", target.id())),
            Event::Move { node } => Some(format!("event: move request {node}")),
            Event::SaveRequest => Some("event: save request from collections".to_owned()),
            Event::Import => Some("event: import file".to_owned()),
            Event::Export => Some("event: export".to_owned()),
            Event::CollectionChanged => Some("event: collection settings saved".to_owned()),
            // Logged by `Workspace::notice`, which owns the flash too.
            Event::Notice(_) => None,
        }
    }
}

/// Where a new folder or saved request lands, and which row is highlighted.
///
/// `collection` is the top-level collection; `folders` walks down from it (an
/// empty path means the collection root); `request` is set when a saved request
/// row is the selection. One selection drives every mutation, so it stays
/// visible in the tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Selection {
    collection: Option<String>,
    folders: Vec<String>,
    request: Option<String>,
}

impl Selection {
    fn collection(id: &str) -> Self {
        Self {
            collection: Some(id.to_owned()),
            ..Self::default()
        }
    }

    /// The deepest folder in the path, or `None` for a collection root.
    fn folder(&self) -> Option<&str> {
        self.folders.last().map(String::as_str)
    }
}

/// The row the pane menu's row verbs act on: the deepest thing selected, so
/// Rename and Delete name the row the tree is highlighting. A selected request
/// wins over its folder, and a folder over its collection, because that is the
/// row the user last clicked.
fn selection_target(selection: &Selection) -> Option<RowTarget> {
    if let Some(request) = &selection.request {
        return Some(RowTarget::Item(request.clone()));
    }
    if let Some(folder) = selection.folder() {
        return Some(RowTarget::Item(folder.to_owned()));
    }
    selection.collection.clone().map(RowTarget::Collection)
}

/// A row's own kind, so the shared tool buttons know how to act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowTarget {
    Collection(String),
    Item(String),
}

impl RowTarget {
    pub(crate) fn id(&self) -> &str {
        match self {
            RowTarget::Collection(id) | RowTarget::Item(id) => id,
        }
    }
}

/// Which row verbs apply, decided by the caller because it holds the tree.
#[derive(Clone, Copy)]
struct RowVerbs {
    can_up: bool,
    can_down: bool,
    can_move: bool,
    can_rename: bool,
}

/// One visible row of the tree, flattened so the virtual list can build a row
/// without walking `collections`.
///
/// The tree is flattened once per query/revision/sort change, and `uniform_list`
/// builds only the slice in view.
#[derive(Clone)]
struct FlatRow {
    kind: RowKind,
    /// The node's own id (`Collection::id`, `Folder::id`, `SavedRequest::id`).
    id: SharedString,
    /// The collection this row lives in, for a selection and a save.
    collection: SharedString,
    /// What the row shows: a collection or folder name, or a request's label.
    name: SharedString,
    /// 0 for a collection, one more for each folder below it.
    depth: usize,
    collapsed: bool,
    /// Folder ids from the collection root down.
    ///
    /// A folder's row includes its own id (the selection path ends at it); a
    /// request's carries only its ancestors; a collection's is empty.
    path: Vec<SharedString>,
    verbs: RowVerbs,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Collection,
    Folder,
    Request(Method),
}

impl FlatRow {
    /// Whether this row is the tree's current selection.
    fn is_selected(&self, selection: &Selection) -> bool {
        match self.kind {
            RowKind::Collection => {
                selection.collection.as_deref() == Some(self.collection.as_ref())
                    && selection.folders.is_empty()
                    && selection.request.is_none()
            }
            RowKind::Folder => {
                selection.collection.as_deref() == Some(self.collection.as_ref())
                    && selection.folder() == Some(self.id.as_ref())
                    && selection.request.is_none()
            }
            RowKind::Request(_) => selection.request.as_deref() == Some(self.id.as_ref()),
        }
    }

    /// The row verb target: a collection is its own kind; a folder or request
    /// is an item.
    fn target(&self) -> RowTarget {
        match self.kind {
            RowKind::Collection => RowTarget::Collection(self.id.to_string()),
            RowKind::Folder | RowKind::Request(_) => RowTarget::Item(self.id.to_string()),
        }
    }
}

/// The verbs a row offers, appended to `menu`.
///
/// One builder for the row's own context menu and the dock's pane menu, so the
/// two cannot disagree about what a row can do.
fn row_items(
    menu: PopupMenu,
    id: String,
    target: RowTarget,
    verbs: RowVerbs,
    panel: Entity<CollectionsPanel>,
) -> PopupMenu {
    let mut menu = menu;
    let up_panel = panel.clone();
    let up_id = id.clone();
    menu = menu.item(
        PopupMenuItem::new("Move up")
            .icon(IconName::ArrowUp)
            .disabled(!verbs.can_up)
            .on_click(move |_, window, cx| {
                up_panel.update(cx, |this, cx| this.move_row(&up_id, true, window, cx));
            }),
    );

    let down_panel = panel.clone();
    let down_id = id.clone();
    menu = menu.item(
        PopupMenuItem::new("Move down")
            .icon(IconName::ArrowDown)
            .disabled(!verbs.can_down)
            .on_click(move |_, window, cx| {
                down_panel.update(cx, |this, cx| this.move_row(&down_id, false, window, cx));
            }),
    );

    if verbs.can_move {
        let move_panel = panel.clone();
        let move_id = id.clone();
        menu = menu.item(
            PopupMenuItem::new("Move to…")
                .icon(IconName::Folder)
                .on_click(move |_, _, cx| {
                    move_panel.update(cx, |_, cx| {
                        cx.emit(Event::Move {
                            node: move_id.clone(),
                        })
                    });
                }),
        );
    }

    if verbs.can_rename {
        let rename_panel = panel.clone();
        let rename_id = id;
        menu = menu.item(PopupMenuItem::separator()).item(
            PopupMenuItem::new("Rename")
                .icon(IconName::Pencil)
                .on_click(move |_, window, cx| {
                    rename_panel.update(cx, |this, cx| this.start_rename(&rename_id, window, cx));
                }),
        );
    }

    // Delete asks the workspace for a confirmation dialog rather than arming
    // itself on a first click: the workspace owns window-level overlays, and a
    // red button that silently reverts on any other click was easy to miss and
    // easy to arm by accident.
    menu.item(PopupMenuItem::separator())
        .item(
            PopupMenuItem::new("Delete")
                .icon(IconName::Trash)
                .on_click(move |_, _, cx| {
                    panel.update(cx, |_, cx| cx.emit(Event::ConfirmDelete(target.clone())));
                }),
        )
        .scrollable(true)
}

/// The order the tree is drawn in. `Manual` is the order the user arranged;
/// the name orders sort a copy for display, so flipping a sort never rewrites
/// the stored collection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SortOrder {
    #[default]
    Manual,
    NameAsc,
    NameDesc,
}

impl SortOrder {
    fn label(self) -> &'static str {
        match self {
            SortOrder::Manual => "Manual order",
            SortOrder::NameAsc => "Name A-Z",
            SortOrder::NameDesc => "Name Z-A",
        }
    }

    pub(super) fn compare(self, left: &str, right: &str) -> std::cmp::Ordering {
        let ordering = left.to_lowercase().cmp(&right.to_lowercase());
        match self {
            SortOrder::NameDesc => ordering.reverse(),
            SortOrder::Manual | SortOrder::NameAsc => ordering,
        }
    }
}

pub(crate) struct CollectionsPanel {
    palette: Palette,
    metrics: Metrics,
    store: Arc<dyn Store>,
    /// The request being edited, observed so a save reads the live one.
    document: Entity<Document>,
    /// The document observer, replaced when the active tab changes.
    document_sub: Option<Subscription>,
    collections: Vec<Collection>,
    /// The drafts the workspace persists, newest first. Read back from the store
    /// so the Unsaved list follows what is actually on disk.
    drafts: Vec<Draft>,
    /// Counts full draft reads, so a test can prove a row update does not re-read
    /// every draft and its body.
    #[cfg(test)]
    draft_reads: std::cell::Cell<usize>,
    /// The one filter state in the pane, shared by the header input and the
    /// tree walk below.
    query: Entity<InputState>,
    new_collection: Entity<InputState>,
    rename_input: Entity<InputState>,
    selection: Selection,
    /// How the tree is ordered for display. View state only; the store keeps
    /// the manual order the user arranged.
    sort: SortOrder,
    /// Collapsed node ids. Expanded by default: a collection the user just
    /// filled should show its contents without a second click.
    collapsed: HashSet<String>,
    /// The node whose name is being edited inline, if any.
    renaming: Option<String>,
    /// Whether the "new collection" field is showing. The header's plus button
    /// reveals it, so an always-visible empty input does not eat a row.
    composing: bool,
    /// The flattened tree for the current query, data revision and sort order,
    /// so a repaint with none of them changed builds only the rows in view. The
    /// `Rc` is shared with the virtual list's closure without copying.
    flat_cache: Option<(String, u64, SortOrder, Rc<Vec<FlatRow>>)>,
    /// Bumped on every mutation of `collections`, and on a collapse toggle
    /// (which changes the flattened row set); keys `flat_cache`.
    revision: u64,
    /// The selected collection's settings disclosure, loaded once per target.
    /// `None` when the selected row is not a collection root.
    settings: Option<settings::CollectionSettings>,
    /// A collection whose settings are queued to load after the frame.
    settings_pending: Option<String>,
    /// Whether the disclosure body is open. Collapsed shows only its header.
    settings_open: bool,
    focus_handle: FocusHandle,
}

impl EventEmitter<Event> for CollectionsPanel {}
impl EventEmitter<gpui_kit::component::dock::PanelEvent> for CollectionsPanel {}

/// Behavior half of the dock seam.
impl gpui_kit::component::dock::BasePanel for CollectionsPanel {
    fn panel_name(&self) -> &'static str {
        "collections"
    }
}

/// Presentation half of the dock seam. The pane draws its own header, so the
/// dock does not put a one-panel title bar above it.
impl gpui_kit::component::dock::Panel for CollectionsPanel {
    fn tab_name(&self, _cx: &App) -> Option<SharedString> {
        Some("Collections".into())
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        icons::title(IconName::Folder, "Collections", &self.metrics)
    }

    fn title_bar(&self, _cx: &App) -> bool {
        false
    }

    /// The tree draws its own header flush to the top; the group's padding
    /// would leave a band above it.
    fn inner_padding(&self, _cx: &App) -> bool {
        false
    }

    /// The pane's own items in the dock's ellipsis menu: the same "new" verbs
    /// the `+` menu offers, followed by the selected row's verbs. The tree
    /// mutations land at the selection, so they are disabled until a collection
    /// is picked, exactly as they are in `+`.
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let has_collection = self.selection.collection.is_some();
        let panel = cx.entity();

        let new_request = panel.clone();
        let mut menu = menu.item(
            PopupMenuItem::new("New Request")
                .icon(IconName::FileText)
                .disabled(!has_collection)
                .on_click(move |_, window, cx| {
                    new_request.update(cx, |this, cx| {
                        this.save_request(None, None, None, Some(window), cx);
                    });
                }),
        );

        let new_folder = panel.clone();
        menu = menu.item(
            PopupMenuItem::new("New Folder")
                .icon(IconName::FolderClosed)
                .disabled(!has_collection)
                .on_click(move |_, window, cx| {
                    new_folder.update(cx, |this, cx| this.add_folder(window, cx));
                }),
        );

        let new_collection = panel.clone();
        menu = menu.item(PopupMenuItem::separator()).item(
            PopupMenuItem::new("New Collection")
                .icon(IconName::Folder)
                .on_click(move |_, window, cx| {
                    new_collection.update(cx, |this, cx| {
                        this.composing = true;
                        let handle = this.new_collection.read(cx).focus_handle(cx);
                        handle.focus(window, cx);
                        cx.notify();
                    });
                }),
        );

        if let Some(target) = selection_target(&self.selection) {
            let id = target.id().to_owned();
            let can_up = has_move_target(&self.collections, &id, true);
            let can_down = has_move_target(&self.collections, &id, false);
            let allow_move = self.is_request(&id);
            menu = menu.separator();
            menu = row_items(
                menu,
                id,
                target,
                RowVerbs {
                    can_up,
                    can_down,
                    can_move: allow_move,
                    can_rename: true,
                },
                panel,
            );
        }

        menu
    }
}

impl Focusable for CollectionsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl CollectionsPanel {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        document: Entity<Document>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let collections = match store.collections() {
            Ok(collections) => collections,
            Err(error) => {
                window.push_notification(Notification::error(error.to_string()), cx);
                Vec::new()
            }
        };
        let drafts = store.drafts().unwrap_or_default();

        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Filter collections"));
        let new_collection = cx.new(|cx| InputState::new(window, cx).placeholder("New collection"));
        let rename_input = cx.new(|cx| InputState::new(window, cx).placeholder("name"));

        // Enter confirms the inline rename without reaching for the confirm
        // button; Escape and the focus move live on the editing row itself.
        cx.subscribe_in(
            &rename_input,
            window,
            |this, _input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { secondary, shift } = event {
                    if !secondary && !shift {
                        this.commit_rename(window, cx);
                    }
                }
            },
        )
        .detach();

        // Observe the document so "Save request" reflects the live request, and
        // the search box so typing in it re-filters: both notify their own
        // state, not this pane.
        let document_sub = Some(cx.observe(&document, |_, _, cx| cx.notify()));
        cx.observe(&query, |_, _, cx| cx.notify()).detach();
        // The pane no longer observes `Variables`: the environment chip moved
        // to the titlebar, so an environment change repaints no row here.

        Self {
            palette: Palette::new(cx),
            metrics: Metrics::current(cx),
            store,
            document,
            document_sub,
            collections,
            drafts,
            #[cfg(test)]
            draft_reads: std::cell::Cell::new(0),
            query,
            new_collection,
            rename_input,
            selection: Selection::default(),
            sort: SortOrder::Manual,
            collapsed: HashSet::new(),
            renaming: None,
            composing: false,
            flat_cache: None,
            revision: 0,
            settings: None,
            settings_pending: None,
            settings_open: false,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Re-point the pane at the active tab's document.
    pub(crate) fn set_document(&mut self, document: Entity<Document>, cx: &mut Context<Self>) {
        if self.document == document {
            return;
        }
        self.document = document.clone();
        self.document_sub = Some(cx.observe(&document, |_, _, cx| cx.notify()));
        cx.notify();
    }

    /// Re-read the palette and metrics after a theme, zoom, or appearance
    /// change. The pane caches both for its render bodies, so the workspace
    /// calls this on every pane rather than each pane observing the theme
    /// global.
    pub(crate) fn refresh_theme(&mut self, cx: &App) {
        self.palette = Palette::new(cx);
        self.metrics = Metrics::current(cx);
    }

    /// Read the collections back from the store. Called after a save, a delete,
    /// or an import, so the pane shows what is actually persisted.
    pub(crate) fn reload(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        match self.store.collections() {
            Ok(collections) => {
                self.collections = collections;
                self.revision += 1;
                self.prune_selection();
            }
            Err(error) => {
                if let Some(window) = window {
                    window.push_notification(Notification::error(error.to_string()), cx);
                }
            }
        }
        self.drafts = self.read_drafts();
        cx.notify();
    }

    /// The drafts, read back from the store. A test counts this so a row update
    /// can prove it no longer re-reads every draft and its body.
    fn read_drafts(&self) -> Vec<Draft> {
        let started = std::time::Instant::now();
        #[cfg(test)]
        self.draft_reads.set(self.draft_reads.get() + 1);
        let drafts = self.store.drafts().unwrap_or_default();
        log::debug!(
            target: "restbook_app::perf",
            "drafts reload took {}ms",
            started.elapsed().as_millis(),
        );
        drafts
    }

    /// Replace or insert one draft row, newest first, without re-reading the
    /// store. The caller holds the row it just wrote.
    pub(crate) fn upsert_draft(&mut self, draft: Draft, cx: &mut Context<Self>) {
        match self.drafts.iter_mut().find(|row| row.id == draft.id) {
            Some(row) => *row = draft,
            None => self.drafts.push(draft),
        }
        self.drafts
            .sort_by_key(|row| std::cmp::Reverse(row.updated_at));
        cx.notify();
    }

    /// Drop one draft row by id, without re-reading the store.
    pub(crate) fn remove_draft(&mut self, id: i64, cx: &mut Context<Self>) {
        self.drafts.retain(|row| row.id != id);
        cx.notify();
    }

    /// Persist the collection with `id` and fold the store's answer back in.
    ///
    /// Returns whether the write landed, so a mutation's toast says what
    /// actually happened. Failures are reported here too, since a `Ctrl+S` save
    /// has no window to notify.
    fn persist(&mut self, id: &str, window: Option<&mut Window>, cx: &mut Context<Self>) -> bool {
        if !self.write_collection(id, cx) {
            return false;
        }
        self.reload(window, cx);
        true
    }

    /// Write one collection without re-reading the tree.
    ///
    /// A move changes two collections, and reloading between the writes would
    /// hand the second write the store's copy of the first — which still holds
    /// the request the move just took out of it, so the request would be written
    /// back where it started.
    fn write_collection(&self, id: &str, cx: &mut Context<Self>) -> bool {
        let Some(collection) = self.collections.iter().find(|each| each.id == id) else {
            return false;
        };
        match self.store.save_collection(collection) {
            Ok(()) => true,
            Err(error) => {
                self.notice(Notice::Failed(error.to_string().into()), cx);
                false
            }
        }
    }

    /// Raise a confirmation to the workspace, which owns the transient toast.
    ///
    /// The pane cannot draw its own: it is a dock panel, so a toast anchored to
    /// it would be clipped to the pane, and `Ctrl+S` reaches the save through
    /// the workspace with no window to notify.
    fn notice(&self, notice: Notice, cx: &mut Context<Self>) {
        cx.emit(Event::Notice(notice));
    }

    /// Drop a selection that no longer points at anything after an edit.
    fn prune_selection(&mut self) {
        if self.selection.collection.is_some()
            && resolve_selection(&self.collections, &self.selection).is_none()
        {
            self.selection = Selection::default();
            return;
        }
        if let Some(request) = &self.selection.request {
            if find_in_items_all(&self.collections, request).is_none() {
                self.selection.request = None;
            }
        }
    }

    /// Create a top-level collection called `name`, persist it, and select it so
    /// the tree shows where it landed. Returns its id, or `None` when the write
    /// failed (already reported by [`Self::persist`]).
    ///
    /// Callers with a window announce the creation; the save dialog does not,
    /// because its own "Request saved." covers the step the user asked for.
    pub(crate) fn create_collection(
        &mut self,
        name: &str,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let id = new_id();
        self.collections.push(Collection::new(id.clone(), name));
        self.revision += 1;
        self.selection = Selection::collection(&id);
        self.persist(&id, window, cx).then_some(id)
    }

    fn add_collection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.new_collection.read(cx).value().trim().to_owned();
        let name = if typed.is_empty() {
            "New collection".to_owned()
        } else {
            typed
        };
        self.new_collection
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.composing = false;
        let Some(id) = self.create_collection(&name, Some(window), cx) else {
            return;
        };
        let name = self
            .collections
            .iter()
            .find(|each| each.id == id)
            .map(|each| each.name.clone())
            .unwrap_or_else(|| "Collection".to_owned());
        self.notice(
            Notice::Done(format!("Created collection \"{name}\".").into()),
            cx,
        );
    }

    fn add_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(collection_id) = self.selection.collection.clone() else {
            window.push_notification(Notification::error("Select a collection first."), cx);
            return;
        };
        let parent = self
            .selection
            .folder()
            .map(str::to_owned)
            .unwrap_or_else(|| collection_id.clone());
        let destination = Destination {
            collection: collection_id.clone(),
            folders: self.selection.folders.clone(),
        };
        let folder = CollectionItem::Folder(Folder {
            id: new_id(),
            name: "New folder".to_owned(),
            items: Vec::new(),
        });
        if insert_into(&mut self.collections, &destination, folder) {
            self.revision += 1;
            self.collapsed.remove(&parent);
            if self.persist(&collection_id, Some(window), cx) {
                self.notice(Notice::Done("Created folder.".into()), cx);
            }
        }
    }

    /// Save the open request into a destination (pane + menu, `Ctrl+S`, or the
    /// request pane's Save button). `node` updates a node in place; `name` falls
    /// back to `"{method} {url}"`; `target` is where the save lands, and `None`
    /// means the sidebar's selection. Returns the node id to bind the tab, or
    /// `None` for a vanished target (a normal state, not an error).
    fn save_request(
        &mut self,
        node: Option<&str>,
        name: Option<&str>,
        target: Option<&Destination>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        // Update in place: the tab already points at a node, so replace its
        // request and let the save be a rename-free overwrite.
        if let Some(node) = node {
            if let Some(collection_id) = owner_collection(&self.collections, node) {
                let request = self.document.read(cx).request.clone();
                if let Some(saved) = find_saved_mut(&mut self.collections, node) {
                    saved.request = request;
                    self.revision += 1;
                    if self.persist(&collection_id, window, cx) {
                        self.notice(Notice::Done("Request updated.".into()), cx);
                    }
                    return Some(node.to_owned());
                }
            }
        }

        let destination = match target {
            Some(target) => target.clone(),
            None => tree::default_destination(&self.collections, &self.selection)?,
        };
        // A request created inside a collection inherits its auth by default,
        // the way Postman and Bruno do; an update keeps the user's own choice.
        let mut request = self.document.read(cx).request.clone();
        if matches!(request.auth, Auth::None) {
            request.auth = Auth::Inherit;
        }
        let name = name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{} {}", request.method, request.url));
        let id = new_id();
        let saved = SavedRequest {
            id: id.clone(),
            name,
            request,
        };
        let parent = destination
            .folders
            .last()
            .cloned()
            .unwrap_or_else(|| destination.collection.clone());
        if insert_into(
            &mut self.collections,
            &destination,
            CollectionItem::Request(saved),
        ) {
            self.revision += 1;
            self.collapsed.remove(&parent);
            if self.persist(&destination.collection, window, cx) {
                self.notice(Notice::Done("Request saved.".into()), cx);
            }
            return Some(id);
        }
        None
    }

    /// The workspace's entry point: save the open request, into `target` when the
    /// dialog chose one. Used by the `SaveRequest` action, which arrives with no
    /// window. Returns the node the request now lives at, so the tab binds to it.
    pub(crate) fn save_current(
        &mut self,
        node: Option<&str>,
        name: Option<&str>,
        target: Option<&Destination>,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        self.save_request(node, name, target, None, cx)
    }

    /// Overwrite the request stored at `node` with `request`, then persist its
    /// collection. The quit path saves a dirty bound tab without switching to
    /// it, so it cannot read the live document the way [`Self::save_request`]
    /// does. Returns whether the write landed.
    pub(crate) fn save_request_at(
        &mut self,
        node: &str,
        request: RequestDef,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(collection_id) = owner_collection(&self.collections, node) else {
            return false;
        };
        let Some(saved) = find_saved_mut(&mut self.collections, node) else {
            return false;
        };
        saved.request = request;
        self.revision += 1;
        self.persist(&collection_id, None, cx)
    }

    /// The selected collection as `(id, name)`, for a command that acts on one
    /// collection rather than on the tree.
    pub(crate) fn selected_collection(&self) -> Option<(&str, &str)> {
        let id = self.selection.collection.as_deref()?;
        let collection = self.collections.iter().find(|each| each.id == id)?;
        Some((collection.id.as_str(), collection.name.as_str()))
    }

    /// The saved request with `id`, if the tree still holds it. A tab bound to
    /// a node that was deleted resolves to `None`, which is how a restore drops
    /// it.
    pub(crate) fn saved_request(&self, id: &str) -> Option<RequestDef> {
        tree::find_saved(&self.collections, id).map(|saved| saved.request.clone())
    }

    /// The auth and headers a saved request inherits from its collection, keyed
    /// by the request's node id. `None` for an orphan or a draft: it has no
    /// collection to inherit from. Cloned because the caller prepares outside
    /// the pane borrow.
    pub(crate) fn inherited_for(&self, node: &str) -> Option<(Auth, Vec<Kv>)> {
        let collection = tree::owner(&self.collections, node)?;
        Some((collection.auth.clone(), collection.headers.clone()))
    }

    /// The variable rows a saved request inherits from its collection, keyed by
    /// the request's node id. Empty for an orphan or a draft: it has no
    /// collection to inherit from.
    pub(crate) fn variables_for(&self, node: &str) -> Vec<Kv> {
        tree::owner(&self.collections, node)
            .map(|collection| collection.variables.clone())
            .unwrap_or_default()
    }

    /// The collection a saved request belongs to, in the shape the editor's
    /// read-only displays need: its name, auth, and headers. `None` for a draft
    /// or an orphan, which inherits nothing.
    pub(crate) fn inherited_defaults(&self, node: &str) -> Option<InheritedDefaults> {
        let collection = tree::owner(&self.collections, node)?;
        Some(InheritedDefaults {
            collection: collection.name.clone(),
            auth: collection.auth.clone(),
            headers: collection.headers.clone(),
        })
    }

    /// Every place a save can land, for the first-save dialog's picker.
    pub(crate) fn destinations(&self) -> Vec<DestinationRow> {
        tree::destination_rows(&self.collections)
    }

    /// Where a saved request currently lives, for the move dialog's prefill.
    pub(crate) fn destination_of(&self, id: &str) -> Option<Destination> {
        tree::destination_of(&self.collections, id)
    }

    /// Select `node` and expand every ancestor so its row is on screen. A node
    /// the tree no longer holds leaves the selection alone: the tab still
    /// carries it, but there is no row to show.
    pub(crate) fn reveal_node(&mut self, node: &str, cx: &mut Context<Self>) {
        let Some(destination) = self.destination_of(node) else {
            return;
        };
        let mut expanded = self.collapsed.remove(&destination.collection);
        for folder in &destination.folders {
            expanded |= self.collapsed.remove(folder);
        }
        self.selection = Selection {
            collection: Some(destination.collection),
            folders: destination.folders,
            request: Some(node.to_owned()),
        };
        if expanded {
            // The collapsed set changed, so the flattened row cache is stale.
            self.revision += 1;
        }
        cx.notify();
    }

    /// The request row the tree has selected, so a test can tell whether a tab
    /// switch revealed its owner.
    #[cfg(test)]
    pub(crate) fn selected_node(&self) -> Option<&str> {
        self.selection.request.as_deref()
    }

    /// Whether `id` names a saved request rather than a folder. Only a request
    /// moves between collections: a folder would take its children with it,
    /// which is a different feature.
    fn is_request(&self, id: &str) -> bool {
        matches!(
            find_in_items_all(&self.collections, id),
            Some(CollectionItem::Request(_))
        )
    }

    /// Move a saved request to `target` and write both collections, target
    /// first. Returns the ids that changed; `None` when the move was a no-op or a
    /// write failed (the pane has already said why, and the tree is reloaded from
    /// whatever did land).
    pub(crate) fn move_request(
        &mut self,
        id: &str,
        target: &Destination,
        cx: &mut Context<Self>,
    ) -> Option<(String, String)> {
        let (target_id, source_id) = tree::move_to(&mut self.collections, id, target)?;
        self.revision += 1;
        self.prune_selection();
        if !self.write_collection(&target_id, cx) || !self.write_collection(&source_id, cx) {
            self.reload(None, cx);
            return None;
        }
        self.notice(Notice::Done("Request moved.".into()), cx);
        self.reload(None, cx);
        Some((target_id, source_id))
    }

    /// Where the save dialog opens on: the selection, or the first collection so
    /// a save does not require one. `None` only when there is nothing to save
    /// into at all.
    pub(crate) fn default_destination(&self) -> Option<Destination> {
        tree::default_destination(&self.collections, &self.selection)
    }

    pub(crate) fn delete_collection(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.store.delete_collection(id) {
            Ok(()) => {
                self.collections.retain(|each| each.id != id);
                self.revision += 1;
                if self.selection.collection.as_deref() == Some(id) {
                    self.selection = Selection::default();
                }
                self.reload(Some(window), cx);
                self.notice(Notice::Done("Deleted collection.".into()), cx);
            }
            Err(error) => self.notice(Notice::Failed(error.to_string().into()), cx),
        }
    }

    pub(crate) fn delete_item(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(collection_id) = remove_item(&mut self.collections, id) {
            self.revision += 1;
            self.prune_selection();
            if self.persist(&collection_id, Some(window), cx) {
                self.notice(Notice::Done("Deleted.".into()), cx);
            }
        }
    }

    /// Start renaming the selected row, the keyboard twin of the row menu's
    /// Rename. A tree with nothing selected is silent: there is no row to name.
    fn rename_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = selection_target(&self.selection) else {
            return;
        };
        let id = target.id().to_owned();
        self.start_rename(&id, window, cx);
    }

    fn start_rename(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current = node_name(&self.collections, id).unwrap_or_default();
        self.renaming = Some(id.to_owned());
        // Focus and select the whole name so typing replaces it, the way an
        // inline editor is expected to open under the keyboard.
        self.rename_input.update(cx, |state, cx| {
            state.set_value(current, window, cx);
            state.focus(window, cx);
            state.select_all(window, cx);
        });
        cx.notify();
    }

    /// Abandon an inline rename, leaving the row's name untouched, and hand the
    /// keyboard back to the tree so it keeps navigating.
    fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.renaming.take() else {
            return;
        };
        self.focus_handle.focus(window, cx);
        let name = self.rename_input.read(cx).value().trim().to_owned();
        if !name.is_empty() && rename_item(&mut self.collections, &id, &name) {
            self.revision += 1;
            if let Some(collection_id) = owner_collection(&self.collections, &id) {
                if self.persist(&collection_id, Some(window), cx) {
                    self.notice(Notice::Done("Renamed.".into()), cx);
                }
                return;
            }
        }
        cx.notify();
    }

    /// Move a sibling one place and persist the collection it lived in.
    fn move_row(&mut self, id: &str, up: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(collection_id) = move_item(&mut self.collections, id, up) {
            self.revision += 1;
            self.persist(&collection_id, Some(window), cx);
        }
    }

    /// Expand or collapse a collection or folder. A toggle changes which rows
    /// exist, so the flattened cache is invalidated.
    fn toggle_collapse(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(id) {
            self.collapsed.insert(id.to_owned());
        }
        self.revision += 1;
        cx.notify();
    }

    /// Flatten the visible tree into rows, honouring the filter, the sort, and
    /// which nodes are collapsed.
    fn flatten(&self, query: &str) -> Vec<FlatRow> {
        let paths = if query.trim().is_empty() {
            self.expanded_paths()
        } else {
            filter_paths(&self.collections, query, self.sort)
        };
        paths
            .iter()
            .filter_map(|path| self.row_from_path(path))
            .collect()
    }

    /// Every row of the unfiltered tree, in display order, skipping the
    /// children of a collapsed node.
    fn expanded_paths(&self) -> Vec<Vec<usize>> {
        let mut roots: Vec<usize> = (0..self.collections.len()).collect();
        if self.sort != SortOrder::Manual {
            roots.sort_by(|&a, &b| {
                self.sort
                    .compare(&self.collections[a].name, &self.collections[b].name)
            });
        }
        let mut paths = Vec::new();
        for index in roots {
            paths.push(vec![index]);
            let collection = &self.collections[index];
            if self.collapsed.contains(&collection.id) {
                continue;
            }
            let mut prefix = vec![index];
            self.push_expanded_items(&collection.items, &mut prefix, &mut paths);
        }
        paths
    }

    fn push_expanded_items(
        &self,
        items: &[CollectionItem],
        prefix: &mut Vec<usize>,
        paths: &mut Vec<Vec<usize>>,
    ) {
        let mut indices: Vec<usize> = (0..items.len()).collect();
        if self.sort != SortOrder::Manual {
            indices.sort_by(|&a, &b| {
                self.sort
                    .compare(item_name(&items[a]), item_name(&items[b]))
            });
        }
        for index in indices {
            prefix.push(index);
            paths.push(prefix.clone());
            if let CollectionItem::Folder(folder) = &items[index] {
                if !self.collapsed.contains(&folder.id) {
                    self.push_expanded_items(&folder.items, prefix, paths);
                }
            }
            prefix.pop();
        }
    }

    /// Turn one index path into a row, without holding a borrow past this call.
    fn row_from_path(&self, path: &[usize]) -> Option<FlatRow> {
        let (&collection_index, rest) = path.split_first()?;
        let collection = self.collections.get(collection_index)?;
        let collection_id: SharedString = collection.id.clone().into();

        if rest.is_empty() {
            return Some(FlatRow {
                kind: RowKind::Collection,
                id: collection.id.clone().into(),
                collection: collection_id,
                name: collection.name.clone().into(),
                depth: 0,
                collapsed: self.collapsed.contains(&collection.id),
                path: Vec::new(),
                verbs: RowVerbs {
                    can_up: false,
                    can_down: false,
                    can_move: false,
                    can_rename: true,
                },
            });
        }

        let mut items: &[CollectionItem] = &collection.items;
        let mut ancestors: Vec<SharedString> = Vec::new();
        for &step in &rest[..rest.len() - 1] {
            match items.get(step)? {
                CollectionItem::Folder(folder) => {
                    ancestors.push(folder.id.clone().into());
                    items = &folder.items;
                }
                CollectionItem::Request(_) => return None,
            }
        }

        let last = *rest.last()?;
        let parent_len = items.len();
        let depth = rest.len();
        let verbs = |can_move| RowVerbs {
            can_up: last > 0,
            can_down: last + 1 < parent_len,
            can_move,
            can_rename: true,
        };

        match items.get(last)? {
            CollectionItem::Folder(folder) => {
                let mut row_path = ancestors;
                row_path.push(folder.id.clone().into());
                Some(FlatRow {
                    kind: RowKind::Folder,
                    id: folder.id.clone().into(),
                    collection: collection_id,
                    name: folder.name.clone().into(),
                    depth,
                    collapsed: self.collapsed.contains(&folder.id),
                    path: row_path,
                    verbs: verbs(false),
                })
            }
            CollectionItem::Request(request) => {
                let method = request.request.method;
                Some(FlatRow {
                    kind: RowKind::Request(method),
                    id: request.id.clone().into(),
                    collection: collection_id,
                    name: display_name(&request.name, method).into(),
                    depth,
                    collapsed: false,
                    path: ancestors,
                    verbs: verbs(true),
                })
            }
        }
    }

    /// The pane's own chrome: the collection switcher, the active environment
    /// chip, and the add menu on the first row; the filter and the sort control
    /// on the second. The dock draws no title bar above this pane, so the header
    /// is the only place these controls live.
    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;
        v_flex()
            .flex_none()
            .border_b_1()
            .border_color(palette.border)
            .child(
                h_flex()
                    .h(px(self.metrics.header_height))
                    .flex_none()
                    .items_center()
                    .gap(px(self.metrics.space_xs))
                    .pl(px(self.metrics.space_md))
                    .pr(px(self.metrics.space_sm))
                    .child(self.render_switcher(cx))
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(palette.subtle)
                            .child(self.collections.len().to_string()),
                    )
                    .child(self.render_actions(cx)),
            )
            .child(self.render_search(cx))
            .into_any_element()
    }

    /// The collection switcher. The header title is the menu, so picking a
    /// collection is one click from the tree it shows; a pick changes the
    /// selection an add uses, not which tab is open.
    fn render_switcher(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self
            .selection
            .collection
            .as_deref()
            .and_then(|id| self.collections.iter().find(|each| each.id == id))
            .map(|each| each.name.clone())
            .unwrap_or_else(|| "Collections".to_owned());

        let panel = cx.entity();
        let entries: Vec<(String, String)> = self
            .collections
            .iter()
            .map(|each| (each.id.clone(), each.name.clone()))
            .collect();
        let selected = self.selection.collection.clone();

        let menu = move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            if entries.is_empty() {
                menu = menu.item(PopupMenuItem::new("No collections yet.").disabled(true));
            }
            for (id, name) in &entries {
                let id = id.clone();
                let panel = panel.clone();
                menu = menu.item(
                    PopupMenuItem::new(name.clone())
                        .icon(IconName::Folder)
                        .checked(selected.as_deref() == Some(id.as_str()))
                        .on_click(move |_, _, cx| {
                            panel.update(cx, |this, cx| {
                                this.selection = Selection::collection(&id);
                                cx.notify();
                            });
                        }),
                );
            }
            menu.item(PopupMenuItem::separator())
                .item(
                    PopupMenuItem::new("New collection…")
                        .icon(IconName::Plus)
                        .on_click({
                            let panel = panel.clone();
                            move |_, window, cx| {
                                panel.update(cx, |this, cx| {
                                    this.composing = true;
                                    let handle = this.new_collection.read(cx).focus_handle(cx);
                                    handle.focus(window, cx);
                                    cx.notify();
                                });
                            }
                        }),
                )
                // One row per collection: unbounded, so it can outgrow the
                // window and clip the last choices without a scrollbar.
                .scrollable(true)
        };

        Button::new("collections-switcher")
            .label(current)
            .ghost()
            .compact()
            .flex_1()
            .min_w_0()
            .dropdown_caret(true)
            .tooltip("Switch collection")
            .dropdown_menu(menu)
            .into_any_element()
    }

    /// The filter row. It writes into the pane's existing `query` state, so the
    /// header input and the tree walk are one filter rather than two that drift.
    fn render_search(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;
        h_flex()
            .flex_none()
            .items_center()
            .gap(px(self.metrics.space_xs))
            .px(px(self.metrics.space_sm))
            .pb(px(self.metrics.space_xs))
            .child(
                Input::new(&self.query)
                    .id("collections-filter")
                    .flex_1()
                    .prefix(icons::quiet(IconName::Search, icons::SMALL, palette)),
            )
            .child(self.render_sort(cx))
            .into_any_element()
    }

    fn render_new_collection(&self, cx: &mut Context<Self>) -> AnyElement {
        let add = Button::new("add-collection-confirm")
            .label("Add")
            .compact()
            .on_click(cx.listener(|this, _, window, cx| this.add_collection(window, cx)));

        h_flex()
            .px(px(self.metrics.space_sm))
            .pb(px(self.metrics.space_xs))
            .gap(px(self.metrics.space_xs))
            .items_center()
            .child(
                Input::new(&self.new_collection)
                    .id("new-collection")
                    .flex_1(),
            )
            .child(add)
            .into_any_element()
    }

    /// The add control. Its menu is the one place a new request, folder, or
    /// collection starts: the two tree mutations land at the selection, so they
    /// are disabled until a collection is picked; the new-collection field is
    /// always available.
    fn render_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let has_collection = self.selection.collection.is_some();
        let panel = cx.entity();

        let menu = move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            menu = menu
                .item(
                    PopupMenuItem::new("Save request here")
                        .icon(IconName::FileText)
                        .disabled(!has_collection)
                        .on_click({
                            let panel = panel.clone();
                            move |_, window, cx| {
                                panel.update(cx, |this, cx| {
                                    this.save_request(None, None, None, Some(window), cx);
                                });
                            }
                        }),
                )
                .item(
                    PopupMenuItem::new("New Folder")
                        .icon(IconName::FolderClosed)
                        .disabled(!has_collection)
                        .on_click({
                            let panel = panel.clone();
                            move |_, window, cx| {
                                panel.update(cx, |this, cx| this.add_folder(window, cx));
                            }
                        }),
                )
                // Saving is a collection verb: the dialog asks for a name and a
                // destination, so it belongs here rather than in the window menu.
                .item(PopupMenuItem::separator())
                .item(
                    PopupMenuItem::new("Save request\u{2026}")
                        .icon(IconName::Save)
                        .on_click({
                            let panel = panel.clone();
                            move |_, _, cx| {
                                panel.update(cx, |_, cx| cx.emit(Event::SaveRequest));
                            }
                        }),
                )
                // Import and export move collections in and out of the app, so
                // they sit beside the tree they act on.
                .item(PopupMenuItem::separator())
                .item(
                    PopupMenuItem::new("Import\u{2026}")
                        .icon(IconName::Upload)
                        .on_click({
                            let panel = panel.clone();
                            move |_, _, cx| {
                                panel.update(cx, |_, cx| cx.emit(Event::Import));
                            }
                        }),
                )
                .item(
                    PopupMenuItem::new("Export\u{2026}")
                        .icon(IconName::Download)
                        .on_click({
                            let panel = panel.clone();
                            move |_, _, cx| {
                                panel.update(cx, |_, cx| cx.emit(Event::Export));
                            }
                        }),
                )
                .item(PopupMenuItem::separator())
                .item(
                    PopupMenuItem::new("New Collection")
                        .icon(IconName::Folder)
                        .on_click({
                            let panel = panel.clone();
                            move |_, window, cx| {
                                panel.update(cx, |this, cx| {
                                    this.composing = true;
                                    let handle = this.new_collection.read(cx).focus_handle(cx);
                                    handle.focus(window, cx);
                                    cx.notify();
                                });
                            }
                        }),
                );
            menu
        };

        Button::new("collections-add")
            .icon(IconName::Plus)
            .ghost()
            .compact()
            .tooltip("Add")
            .dropdown_menu(menu)
            .into_any_element()
    }

    /// The sort control. Only the drawn order changes: the stored collection
    /// keeps the manual order until a row is moved.
    fn render_sort(&self, cx: &mut Context<Self>) -> AnyElement {
        let panel = cx.entity();
        let order = self.sort;
        let menu = move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            for choice in [SortOrder::Manual, SortOrder::NameAsc, SortOrder::NameDesc] {
                let panel = panel.clone();
                menu = menu.item(
                    PopupMenuItem::new(choice.label())
                        .checked(order == choice)
                        .on_click(move |_, _, cx| {
                            panel.update(cx, |this, cx| {
                                this.sort = choice;
                                cx.notify();
                            });
                        }),
                );
            }
            menu
        };

        let icon = match self.sort {
            SortOrder::NameDesc => IconName::SortDescending,
            SortOrder::Manual | SortOrder::NameAsc => IconName::SortAscending,
        };

        Button::new("collections-sort")
            .icon(icon)
            .ghost()
            .compact()
            .tooltip("Sort")
            .dropdown_menu(menu)
            .into_any_element()
    }

    /// The unsaved drafts above the tree. The pane does not own the tabs, so a
    /// row only announces the id; the workspace selects the tab that holds it.
    fn render_unsaved(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;
        let rows = self
            .drafts
            .iter()
            .map(|draft| {
                let id = draft.id;
                let method = draft.request.method;
                let url = if draft.request.url.trim().is_empty() {
                    "Untitled request".to_owned()
                } else {
                    draft.request.url.clone()
                };
                let open = cx.listener(move |_, _, _, cx| {
                    cx.emit(Event::OpenDraft { id });
                    cx.notify();
                });

                h_flex()
                    .id(format!("draft-{}", id))
                    .w_full()
                    .h(px(self.metrics.row_height))
                    .px(px(self.metrics.space_sm))
                    .gap(px(self.metrics.space_sm))
                    .items_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(palette.list_hover))
                    .on_click(open)
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(self.metrics.font_badge))
                            .font_weight(FontWeight::BOLD)
                            .text_color(palette.method(method))
                            .child(method.as_str()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(palette.muted)
                            .child(url),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        v_flex()
            .flex_none()
            .border_b_1()
            .border_color(palette.border)
            .py(px(self.metrics.space_xs))
            .child(
                h_flex()
                    .h(px(self.metrics.row_height))
                    .items_center()
                    .px(px(self.metrics.space_md))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(palette.subtle)
                            .child(format!("Unsaved ({})", self.drafts.len())),
                    ),
            )
            .children(rows)
            .into_any_element()
    }

    fn render_list(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if self.collections.is_empty() {
            return empty::empty_state_inline(
                "No collections yet",
                Some(
                    "Import an OpenAPI document, paste a cURL, or create a collection \
                     with the + button.",
                ),
                vec![
                    Action::new("Import…", shortcut(&ImportFile, window)),
                    Action::new("Paste cURL", shortcut(&PasteCurl, window)),
                ],
                &self.palette,
                &self.metrics,
            );
        }

        let query = self.query.read(cx).value().to_string();

        // Flatten once per query/data/sort change, then let the virtual list
        // build only the rows in view. This is what keeps an imported document
        // with thousands of nodes from costing a full walk on every repaint.
        let fresh = matches!(
            &self.flat_cache,
            Some((cached_query, cached_revision, cached_sort, _))
                if cached_query == &query
                    && *cached_revision == self.revision
                    && *cached_sort == self.sort
        );
        if !fresh {
            let rows = Rc::new(self.flatten(&query));
            self.flat_cache = Some((query, self.revision, self.sort, rows));
        }
        let rows = self.flat_cache.as_ref().expect("cache set above").3.clone();

        if rows.is_empty() {
            return self.message("No matches.");
        }

        let selection = self.selection.clone();
        let renaming = self.renaming.clone();
        let palette = self.palette.clone();
        let metrics = self.metrics;
        let rename_input = self.rename_input.clone();
        let panel = cx.entity();

        v_flex()
            .id("collections-list")
            .flex_1()
            .min_h_0()
            .py(px(self.metrics.space_xs))
            .child(
                uniform_list("collections-rows", rows.len(), move |range, _, _| {
                    range
                        .map(|index| {
                            row_element(
                                &rows[index],
                                &selection,
                                renaming.as_deref(),
                                &palette,
                                &metrics,
                                &panel,
                                &rename_input,
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .size_full(),
            )
            .into_any_element()
    }

    fn message(&self, text: &str) -> AnyElement {
        div()
            .px(px(self.metrics.space_md))
            .py(px(self.metrics.space_sm))
            .text_sm()
            .text_color(self.palette.muted)
            .child(text.to_owned())
            .into_any_element()
    }
}

/// Build one tree row from its flattened form.
///
/// These row builders are free of the pane's `&self` so the virtual list's
/// closure can call them with only the small data it captured; every action
/// goes back through the panel's `Entity`.
fn row_element(
    row: &FlatRow,
    selection: &Selection,
    renaming: Option<&str>,
    palette: &Palette,
    metrics: &Metrics,
    panel: &Entity<CollectionsPanel>,
    rename_input: &Entity<InputState>,
) -> AnyElement {
    let selected = row.is_selected(selection);
    let renaming = renaming == Some(row.id.as_ref());
    match row.kind {
        RowKind::Collection => collection_row(
            row,
            selected,
            renaming,
            palette,
            metrics,
            panel,
            rename_input,
        ),
        RowKind::Folder => folder_row(
            row,
            selected,
            renaming,
            palette,
            metrics,
            panel,
            rename_input,
        ),
        RowKind::Request(_) => request_row(
            row,
            selected,
            renaming,
            palette,
            metrics,
            panel,
            rename_input,
        ),
    }
}

/// The shared inline rename editor: an input and a confirm button, identical
/// whether the row is a collection, a folder, or a request.
fn rename_area(
    prefix: &str,
    id: &str,
    metrics: &Metrics,
    panel: &Entity<CollectionsPanel>,
    rename_input: &Entity<InputState>,
) -> AnyElement {
    h_flex()
        .id(format!("{prefix}-rename-{id}"))
        .flex_1()
        .min_w_0()
        .gap(px(metrics.space_xs))
        .items_center()
        // Escape bubbles up from the input to cancel the whole rename; Enter is
        // caught through the input's own `PressEnter` event.
        .on_key_down({
            let panel = panel.clone();
            move |event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    panel.update(cx, |this, cx| this.cancel_rename(window, cx));
                }
            }
        })
        .child(
            Input::new(rename_input)
                .id(format!("{prefix}-rename-input"))
                .flex_1(),
        )
        .child(
            Button::new(format!("{prefix}-rename-confirm"))
                .icon(IconName::Check)
                .ghost()
                .compact()
                .on_click({
                    let panel = panel.clone();
                    move |_, window, app| {
                        panel.update(app, |this, cx| this.commit_rename(window, cx));
                    }
                }),
        )
        .into_any_element()
}

fn collection_row(
    row: &FlatRow,
    selected: bool,
    renaming: bool,
    palette: &Palette,
    metrics: &Metrics,
    panel: &Entity<CollectionsPanel>,
    rename_input: &Entity<InputState>,
) -> AnyElement {
    let toggle = Button::new(format!("collection-toggle-{}", row.id))
        .icon(if row.collapsed {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        })
        .ghost()
        .compact()
        .on_click({
            let panel = panel.clone();
            let id = row.id.to_string();
            move |_, _, app| {
                panel.update(app, |this, cx| this.toggle_collapse(&id, cx));
            }
        });

    let name_area = if renaming {
        rename_area("collection", &row.id, metrics, panel, rename_input)
    } else {
        h_flex()
            .id(format!("collection-name-{}", row.id))
            .flex_1()
            .min_w_0()
            .gap(px(metrics.space_1p5))
            .items_center()
            .cursor_pointer()
            .on_click({
                let panel = panel.clone();
                let id = row.id.to_string();
                move |_, window, app| {
                    panel.update(app, |this, cx| {
                        this.selection = Selection::collection(&id);
                        // Give the pane keyboard focus, so `f2` (scoped to the
                        // `Collections` context) reaches the tree after a click.
                        window.focus(&this.focus_handle, cx);
                        cx.notify();
                    });
                }
            })
            // The monogram: one letter in a tinted square, the same shape as
            // the workspace chip in the titlebar.
            .child(
                div()
                    .flex_none()
                    .size(px(metrics.icon_sm))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .bg(palette.list_hover)
                    .text_color(palette.muted)
                    .text_size(px(metrics.font_badge))
                    .font_weight(FontWeight::BOLD)
                    .child(initial(row.name.as_ref())),
            )
            .child(
                div()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(palette.foreground)
                    .child(row.name.clone()),
            )
            .into_any_element()
    };

    let mut element = h_flex()
        .id(format!("collection-{}", row.id))
        .w_full()
        .h(px(metrics.row_height))
        .px(px(metrics.space_sm))
        .gap(px(metrics.space_xs))
        .items_center()
        .rounded_sm()
        .hover(|style| style.bg(palette.list_hover));
    if selected {
        element = element.bg(palette.list_active);
    }

    let target = row.target();
    let tools = row_tools(&target, row.verbs, metrics, panel);
    let element = element.child(toggle).child(name_area).child(tools);
    if renaming {
        element.into_any_element()
    } else {
        let menu = row_menu(panel, target, row.verbs);
        element.context_menu(menu).into_any_element()
    }
}

fn folder_row(
    row: &FlatRow,
    selected: bool,
    renaming: bool,
    palette: &Palette,
    metrics: &Metrics,
    panel: &Entity<CollectionsPanel>,
    rename_input: &Entity<InputState>,
) -> AnyElement {
    let toggle = Button::new(format!("folder-toggle-{}", row.id))
        .icon(if row.collapsed {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        })
        .ghost()
        .compact()
        .on_click({
            let panel = panel.clone();
            let id = row.id.to_string();
            move |_, _, app| {
                panel.update(app, |this, cx| this.toggle_collapse(&id, cx));
            }
        });

    let name_area = if renaming {
        rename_area("folder", &row.id, metrics, panel, rename_input)
    } else {
        h_flex()
            .id(format!("folder-name-{}", row.id))
            .flex_1()
            .min_w_0()
            .gap(px(metrics.space_xs))
            .cursor_pointer()
            .on_click({
                let panel = panel.clone();
                let collection = row.collection.to_string();
                let path: Vec<String> = row.path.iter().map(|id| id.to_string()).collect();
                move |_, window, app| {
                    panel.update(app, |this, cx| {
                        this.selection = Selection {
                            collection: Some(collection.clone()),
                            folders: path.clone(),
                            request: None,
                        };
                        window.focus(&this.focus_handle, cx);
                        cx.notify();
                    });
                }
            })
            .child(icons::quiet(
                if row.collapsed {
                    IconName::Folder
                } else {
                    IconName::FolderOpen
                },
                icons::SMALL,
                palette,
            ))
            .child(
                div()
                    .truncate()
                    .text_sm()
                    .text_color(palette.foreground)
                    .child(row.name.clone()),
            )
            .into_any_element()
    };

    let mut element = h_flex()
        .id(format!("folder-{}", row.id))
        .w_full()
        .h(px(metrics.row_height))
        .px(px(metrics.space_sm))
        .gap(px(metrics.space_xs))
        .items_center()
        .rounded_sm()
        .pl(px(metrics.gutter * 0.5 + row.depth as f32 * metrics.indent))
        .hover(|style| style.bg(palette.list_hover));
    if selected {
        element = element.bg(palette.list_active);
    }

    let target = row.target();
    let tools = row_tools(&target, row.verbs, metrics, panel);
    let element = element.child(toggle).child(name_area).child(tools);
    if renaming {
        element.into_any_element()
    } else {
        let menu = row_menu(panel, target, row.verbs);
        element.context_menu(menu).into_any_element()
    }
}

fn request_row(
    row: &FlatRow,
    selected: bool,
    renaming: bool,
    palette: &Palette,
    metrics: &Metrics,
    panel: &Entity<CollectionsPanel>,
    rename_input: &Entity<InputState>,
) -> AnyElement {
    let RowKind::Request(method) = row.kind else {
        return div().into_any_element();
    };
    let method_badge = div()
        .flex_none()
        .text_size(px(metrics.font_badge))
        .font_weight(FontWeight::BOLD)
        .text_color(palette.method(method))
        .child(method.as_str());

    let name_area = if renaming {
        rename_area("request", &row.id, metrics, panel, rename_input)
    } else {
        h_flex()
            .id(format!("request-name-{}", row.id))
            .flex_1()
            .min_w_0()
            .gap(px(metrics.space_sm))
            .items_center()
            .cursor_pointer()
            .on_click({
                let panel = panel.clone();
                let collection = row.collection.to_string();
                let path: Vec<String> = row.path.iter().map(|id| id.to_string()).collect();
                let id = row.id.to_string();
                move |_, window, app| {
                    panel.update(app, |this, cx| {
                        this.selection = Selection {
                            collection: Some(collection.clone()),
                            folders: path.clone(),
                            request: Some(id.clone()),
                        };
                        window.focus(&this.focus_handle, cx);
                        // Read the request at click time rather than caching a
                        // copy per row; the flattened row carries only what a
                        // row draws.
                        if let Some(request) = this.saved_request(&id) {
                            cx.emit(Event::Open {
                                node: id.clone(),
                                request,
                            });
                        }
                        cx.notify();
                    });
                }
            })
            .child(method_badge)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(if selected {
                        palette.foreground
                    } else {
                        palette.muted
                    })
                    .child(row.name.clone()),
            )
            .into_any_element()
    };

    let mut element = h_flex()
        .id(format!("request-{}", row.id))
        .w_full()
        .h(px(metrics.row_height))
        .px(px(metrics.space_sm))
        // The 6 px step, not `sm`: a request row has a badge before its name,
        // so it gets a half-step more than the rest of the tree.
        .gap(px(metrics.space_1p5))
        .items_center()
        .rounded_sm()
        .pl(px(metrics.gutter * 0.5 + row.depth as f32 * metrics.indent))
        .hover(|style| style.bg(palette.list_hover));
    if selected {
        element = element.bg(palette.list_active);
    }

    let target = row.target();
    let tools = row_tools(&target, row.verbs, metrics, panel);
    let element = element.child(name_area).child(tools);
    if renaming {
        element.into_any_element()
    } else {
        let menu = row_menu(panel, target, row.verbs);
        element.context_menu(menu).into_any_element()
    }
}

/// The row's overflow menu: one ghost ellipsis that opens the same verbs a
/// right-click does. `justify_end` pins it so every name truncates at the same
/// column, and the fixed lane keeps that column still across row kinds.
fn row_tools(
    target: &RowTarget,
    verbs: RowVerbs,
    metrics: &Metrics,
    panel: &Entity<CollectionsPanel>,
) -> AnyElement {
    let menu = row_menu(panel, target.clone(), verbs);
    h_flex()
        .flex_none()
        .justify_end()
        .w(px(metrics.tool_lane))
        .child(
            Button::new(format!("row-menu-{}", target.id()))
                .icon(IconName::Ellipsis)
                .ghost()
                .compact()
                .tooltip("More actions")
                .dropdown_menu(menu),
        )
        .into_any_element()
}

/// The verbs a row offers, shared by the ellipsis dropdown and a right-click
/// anywhere on the row. The panel handle is what the menu callbacks use instead
/// of a `Context`, since they arrive with an `App`.
fn row_menu(
    panel: &Entity<CollectionsPanel>,
    target: RowTarget,
    verbs: RowVerbs,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    let id = target.id().to_owned();
    let panel = panel.clone();
    move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
        row_items(menu, id.clone(), target.clone(), verbs, panel.clone())
    }
}

impl Render for CollectionsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = self.palette.sidebar;
        self.sync_settings(window, cx);
        v_flex()
            .size_full()
            .bg(bg)
            .track_focus(&self.focus_handle)
            .key_context("Collections")
            .on_action(
                cx.listener(|this, _: &RenameSelected, window, cx| {
                    this.rename_selected(window, cx)
                }),
            )
            .child(self.render_header(cx))
            .children(self.composing.then(|| self.render_new_collection(cx)))
            .children((!self.drafts.is_empty()).then(|| self.render_unsaved(cx)))
            .child(self.render_list(window, cx))
            .children(self.render_settings(cx))
    }
}

/// A process-unique id for a new collection, folder, or saved request. The
/// stores key on the string and nothing outside the app needs a globally
/// unique value, so a timestamp plus a counter is enough — no new dependency.
fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{count:x}")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui_kit::{
        AppContext as _, Focusable as _, KeyDownEvent, Keystroke, Modifiers, PlatformInput,
        TestAppContext, VisualTestContext,
    };
    use restbook_core::{Auth, Collection, CollectionItem, Folder, Store};
    use restbook_store::{FolderStore, MemoryStore};

    use super::{selection_target, CollectionsPanel, Destination, RowTarget, Selection};
    use crate::document::Document;

    fn into_folder(id: &str) -> Selection {
        Selection {
            collection: Some("col-1".to_owned()),
            folders: vec![id.to_owned()],
            request: None,
        }
    }

    /// The pane menu's row verbs act on the deepest selected row, so a folder
    /// or request wins over the collection that contains it, and an empty
    /// selection offers no row verbs at all.
    #[test]
    fn the_pane_menu_acts_on_the_deepest_selection() {
        assert_eq!(selection_target(&Selection::default()), None);

        assert_eq!(
            selection_target(&Selection::collection("col-1")),
            Some(RowTarget::Collection("col-1".to_owned())),
        );
        assert_eq!(
            selection_target(&into_folder("fold-1")),
            Some(RowTarget::Item("fold-1".to_owned())),
        );
        assert_eq!(
            selection_target(&Selection {
                request: Some("req-1".to_owned()),
                ..into_folder("fold-1")
            }),
            Some(RowTarget::Item("req-1".to_owned())),
        );
    }

    /// The 0.2.0 behaviour: a draft saves where the dialog says, with nothing
    /// highlighted in the sidebar. Before this, an unselected tree refused the
    /// save outright and the destination was whatever row was clicked last.
    #[gpui_kit::test]
    fn a_request_saves_into_a_chosen_destination_without_a_selection(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        crate::test_support::init_ui(cx);

        let store = Arc::new(MemoryStore::new());
        store
            .save_collection(&Collection {
                items: vec![CollectionItem::Folder(Folder {
                    id: "fold-a".to_owned(),
                    name: "Inner".to_owned(),
                    items: Vec::new(),
                })],
                ..Collection::new("col-a", "A")
            })
            .unwrap();

        let for_panel: Arc<dyn Store> = store.clone();
        let document = cx.update(|cx| cx.new(|_| Document::new()));
        let (panel, cx) =
            cx.add_window_view(|window, cx| CollectionsPanel::new(for_panel, document, window, cx));

        // Nothing is selected in the pane: `Selection::default()` is what
        // `CollectionsPanel::new` starts with.
        let target = cx.update(|_, cx| panel.read(cx).destinations()[1].target.clone());
        let landed = panel.update(cx, |panel, cx| {
            panel.save_current(None, Some("Saved"), Some(&target), cx)
        });
        assert!(landed.is_some(), "the save did not land");

        let collections = store.collections().unwrap();
        let CollectionItem::Folder(folder) = &collections[0].items[0] else {
            panic!("the folder is gone");
        };
        assert_eq!(folder.items.len(), 1);
        assert!(matches!(
            &folder.items[0],
            CollectionItem::Request(saved) if saved.name == "Saved"
        ));
        assert_eq!(
            collections[0].items.len(),
            1,
            "the request went to the root"
        );
    }

    /// A request created inside a collection inherits the collection's auth by
    /// default, the way Postman and Bruno model it; the user opts out with
    /// `None`.
    #[gpui_kit::test]
    fn a_new_request_saved_into_a_collection_inherits_its_auth(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        crate::test_support::init_ui(cx);

        let store = Arc::new(MemoryStore::new());
        store
            .save_collection(&Collection {
                auth: Auth::Bearer {
                    token: "collection-token".to_owned(),
                },
                ..Collection::new("col-a", "A")
            })
            .unwrap();

        let for_panel: Arc<dyn Store> = store.clone();
        // A brand-new live request has no auth of its own.
        let document = cx.update(|cx| cx.new(|_| Document::new()));
        let (panel, cx) =
            cx.add_window_view(|window, cx| CollectionsPanel::new(for_panel, document, window, cx));

        let target = cx.update(|_, cx| panel.read(cx).destinations()[0].target.clone());
        let landed = panel.update(cx, |panel, cx| {
            panel.save_current(None, Some("Saved"), Some(&target), cx)
        });
        assert!(landed.is_some(), "the save did not land");

        let collections = store.collections().unwrap();
        let CollectionItem::Request(saved) = &collections[0].items[0] else {
            panic!("the request is missing");
        };
        assert!(
            matches!(saved.request.auth, Auth::Inherit),
            "a new request must inherit the collection's auth by default"
        );
    }

    /// Saving over an existing node keeps the request's own auth, so the
    /// inherit default never overrides an explicit `None`.
    #[gpui_kit::test]
    fn an_update_keeps_the_requests_own_auth(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        crate::test_support::init_ui(cx);

        let store = Arc::new(MemoryStore::new());
        store
            .save_collection(&Collection {
                items: vec![CollectionItem::Request(restbook_core::SavedRequest {
                    id: "req-a".to_owned(),
                    name: "Request A".to_owned(),
                    request: restbook_core::RequestDef::new(
                        restbook_core::Method::Get,
                        "https://a",
                    ),
                })],
                ..Collection::new("col-a", "A")
            })
            .unwrap();

        let for_panel: Arc<dyn Store> = store.clone();
        let document = cx.update(|cx| cx.new(|_| Document::new()));
        let (panel, cx) =
            cx.add_window_view(|window, cx| CollectionsPanel::new(for_panel, document, window, cx));

        let landed = panel.update(cx, |panel, cx| {
            panel.save_current(Some("req-a"), None, None, cx)
        });
        assert_eq!(landed.as_deref(), Some("req-a"), "the update did not land");

        let collections = store.collections().unwrap();
        let CollectionItem::Request(saved) = &collections[0].items[0] else {
            panic!("the request is missing");
        };
        assert!(
            matches!(saved.request.auth, Auth::None),
            "an update must not turn an explicit None into Inherit"
        );
    }

    fn two_collections() -> (Collection, Collection) {
        let request = restbook_core::SavedRequest {
            id: "req-a".to_owned(),
            name: "Request A".to_owned(),
            request: restbook_core::RequestDef::new(restbook_core::Method::Get, "https://a"),
        };
        (
            Collection {
                items: vec![CollectionItem::Request(request)],
                ..Collection::new("col-a", "A")
            },
            Collection::new("col-b", "B"),
        )
    }

    /// One window view per test, since `Palette::new` snapshots the theme.
    fn open(
        cx: &mut TestAppContext,
        store: Arc<dyn Store>,
    ) -> (
        gpui_kit::Entity<CollectionsPanel>,
        &mut gpui_kit::VisualTestContext,
    ) {
        let document = cx.update(|cx| cx.new(|_| Document::new()));
        cx.add_window_view(|window, cx| CollectionsPanel::new(store, document, window, cx))
    }

    /// A row the workspace just wrote updates the Unsaved list in place; only a
    /// full reload reads every draft back. This is what keeps a send, a save, and
    /// a close off `Store::drafts()`.
    #[gpui_kit::test]
    fn a_draft_row_updates_without_re_reading_the_store(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        crate::test_support::init_ui(cx);

        let store = Arc::new(MemoryStore::new());
        let (panel, cx) = open(cx, store);
        let base = cx.update(|_, cx| panel.read(cx).draft_reads.get());

        let draft = restbook_core::Draft {
            id: 7,
            request: restbook_core::RequestDef::new(restbook_core::Method::Get, "https://a"),
            updated_at: Default::default(),
        };
        cx.update(|_, cx| {
            panel.update(cx, |panel, cx| panel.upsert_draft(draft.clone(), cx));
        });
        assert_eq!(
            cx.update(|_, cx| panel.read(cx).draft_reads.get()),
            base,
            "upserting a row must not read the store"
        );
        assert_eq!(
            cx.update(|_, cx| panel.read(cx).drafts.len()),
            1,
            "the row is now on the list"
        );

        cx.update(|_, cx| {
            panel.update(cx, |panel, cx| panel.remove_draft(7, cx));
        });
        assert_eq!(
            cx.update(|_, cx| panel.read(cx).draft_reads.get()),
            base,
            "removing a row must not read the store"
        );
        assert!(cx.update(|_, cx| panel.read(cx).drafts.is_empty()));

        // The full-read path still reads, which is what makes the count meaningful.
        cx.update(|_, cx| {
            panel.update(cx, |panel, cx| panel.reload(None, cx));
        });
        assert_eq!(
            cx.update(|_, cx| panel.read(cx).draft_reads.get()),
            base + 1,
            "a full reload reads the drafts once"
        );
    }

    /// Dispatch a key through the real keymap, after a paint, the way the
    /// platform delivers it.
    fn press(cx: &mut VisualTestContext, key: &str) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            window.dispatch_event(
                PlatformInput::KeyDown(KeyDownEvent {
                    keystroke: Keystroke {
                        modifiers: Modifiers::default(),
                        key: key.to_owned(),
                        key_char: None,
                    },
                    is_held: false,
                    prefer_character_input: false,
                }),
                cx,
            );
        });
        cx.run_until_parked();
    }

    /// Dispatch `F2` through the real keymap, after a paint, the way the
    /// platform delivers a function key: a `KeyDownEvent` with no text.
    fn press_f2(cx: &mut VisualTestContext) {
        press(cx, "f2");
    }

    /// A pane holding one collection, selected and armed for rename by `F2`.
    fn arm_rename_on_a_collection(
        cx: &mut TestAppContext,
    ) -> (gpui_kit::Entity<CollectionsPanel>, &mut VisualTestContext) {
        cx.update(gpui_kit::init);
        crate::test_support::init_ui(cx);
        cx.update(|cx| cx.bind_keys(crate::actions::bindings(cx)));

        let store = Arc::new(MemoryStore::new());
        store
            .save_collection(&Collection::new("col-a", "A"))
            .unwrap();
        let (panel, cx) = open(cx, store);

        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        cx.update(|window, cx| {
            panel.update(cx, |this, cx| {
                this.selection = Selection::collection("col-a");
                window.focus(&this.focus_handle, cx);
                cx.notify();
            });
        });
        press_f2(cx);
        (panel, cx)
    }

    /// `F2` renames the selected row: the keyboard route the row menu also
    /// offers. It fires because the pane declares the `Collections` key context
    /// the binding is scoped to, so this also pins that the two agree.
    #[gpui_kit::test]
    fn f2_arms_the_rename_on_the_selected_row(cx: &mut TestAppContext) {
        let (panel, cx) = arm_rename_on_a_collection(cx);
        let renaming = cx.update(|_, cx| panel.read(cx).renaming.clone());
        assert_eq!(renaming.as_deref(), Some("col-a"));
    }

    /// The rename opens ready to type: the input takes focus and selects the
    /// whole name, so a keystroke replaces it without a click first.
    #[gpui_kit::test]
    fn f2_focuses_the_rename_input(cx: &mut TestAppContext) {
        let (panel, cx) = arm_rename_on_a_collection(cx);
        // Render the inline editor, then ask the window where focus actually is.
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let focused = cx.update(|window, cx| {
            panel
                .read(cx)
                .rename_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        });
        assert!(
            focused,
            "the rename input takes focus so typing lands in it"
        );
    }

    /// Escape abandons the inline rename and leaves the name as it was.
    #[gpui_kit::test]
    fn escape_cancels_the_rename(cx: &mut TestAppContext) {
        let (panel, cx) = arm_rename_on_a_collection(cx);
        press(cx, "escape");
        let renaming = cx.update(|_, cx| panel.read(cx).renaming.clone());
        assert_eq!(renaming, None, "escape ends the inline rename");
    }

    /// Enter confirms the rename without reaching for the confirm button.
    #[gpui_kit::test]
    fn enter_commits_the_rename(cx: &mut TestAppContext) {
        let (panel, cx) = arm_rename_on_a_collection(cx);
        press(cx, "enter");
        let renaming = cx.update(|_, cx| panel.read(cx).renaming.clone());
        assert_eq!(renaming, None, "enter ends the inline rename");
    }

    /// A move writes both collections; the store is what proves the request left
    /// one and arrived in the other.
    #[gpui_kit::test]
    fn moving_a_request_reaches_both_collections_in_the_store(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        crate::test_support::init_ui(cx);

        let store = Arc::new(MemoryStore::new());
        let (a, b) = two_collections();
        store.save_collection(&a).unwrap();
        store.save_collection(&b).unwrap();

        let (panel, cx) = open(cx, store.clone());
        let target = Destination {
            collection: "col-b".to_owned(),
            folders: Vec::new(),
        };
        let moved = panel.update(cx, |panel, cx| panel.move_request("req-a", &target, cx));
        assert_eq!(
            moved,
            Some(("col-b".to_owned(), "col-a".to_owned())),
            "the changed collections come back target first",
        );

        let saved = store.collections().unwrap();
        let source = saved.iter().find(|each| each.id == "col-a").unwrap();
        assert!(
            source.items.is_empty(),
            "the request stayed in its old home"
        );
        let target = saved.iter().find(|each| each.id == "col-b").unwrap();
        assert!(matches!(
            target.items.first(),
            Some(CollectionItem::Request(request)) if request.id == "req-a"
        ));
    }

    /// The folder-workspace half: a move is two collection files, and a fresh
    /// handle reading them back is what proves both were rewritten.
    #[gpui_kit::test]
    fn a_move_rewrites_both_collection_files_in_a_workspace(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        crate::test_support::init_ui(cx);

        let root = std::env::temp_dir().join(format!("restbook-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let store = Arc::new(FolderStore::open(&root).unwrap());
        let (a, b) = two_collections();
        store.save_collection(&a).unwrap();
        store.save_collection(&b).unwrap();

        let (panel, cx) = open(cx, store.clone());
        let target = Destination {
            collection: "col-b".to_owned(),
            folders: Vec::new(),
        };
        assert!(panel
            .update(cx, |panel, cx| panel.move_request("req-a", &target, cx))
            .is_some());

        // Read the files back rather than the handle that wrote them.
        let reopened = FolderStore::open(&root).unwrap();
        let saved = reopened.collections().unwrap();
        let source = saved.iter().find(|each| each.id == "col-a").unwrap();
        assert!(source.items.is_empty(), "col-a still lists the request");
        let target = saved.iter().find(|each| each.id == "col-b").unwrap();
        assert!(matches!(
            target.items.first(),
            Some(CollectionItem::Request(request)) if request.id == "req-a"
        ));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The virtual list draws from `flatten`, so a collapse has to change the
    /// flattened row set: an expanded tree yields every node, a collapsed
    /// collection only its own row. This is what keeps a large import from
    /// painting thousands of rows.
    #[gpui_kit::test]
    fn collapsing_a_collection_removes_its_rows_from_the_flattened_tree(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        crate::test_support::init_ui(cx);

        let store = Arc::new(MemoryStore::new());
        let request = |id: &str| {
            CollectionItem::Request(restbook_core::SavedRequest {
                id: id.to_owned(),
                name: id.to_owned(),
                request: restbook_core::RequestDef::new(restbook_core::Method::Get, "https://a"),
            })
        };
        store
            .save_collection(&Collection {
                items: vec![CollectionItem::Folder(Folder {
                    id: "fold-a".to_owned(),
                    name: "Inner".to_owned(),
                    items: vec![request("r1"), request("r2")],
                })],
                ..Collection::new("col-a", "A")
            })
            .unwrap();

        let (panel, cx) = open(cx, store);

        let expanded = cx.update(|_, cx| panel.read(cx).flatten("").len());
        assert_eq!(expanded, 4, "collection, folder, and two requests");

        panel.update(cx, |panel, cx| panel.toggle_collapse("col-a", cx));
        let collapsed = cx.update(|_, cx| panel.read(cx).flatten("").len());
        assert_eq!(collapsed, 1, "only the collection row survives");
    }

    /// 0.2.7 / backlog 3.6: switching to a tab names where the request on screen
    /// lives, so the sidebar selects its row and expands its ancestors instead
    /// of leaving the selection wherever it was clicked last.
    #[gpui_kit::test]
    fn revealing_a_request_selects_it_and_expands_its_ancestors(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        crate::test_support::init_ui(cx);

        let store = Arc::new(MemoryStore::new());
        store
            .save_collection(&Collection {
                items: vec![CollectionItem::Folder(Folder {
                    id: "fold-a".to_owned(),
                    name: "Inner".to_owned(),
                    items: vec![CollectionItem::Request(restbook_core::SavedRequest {
                        id: "req-a".to_owned(),
                        name: "A".to_owned(),
                        request: restbook_core::RequestDef::new(
                            restbook_core::Method::Get,
                            "https://a",
                        ),
                    })],
                })],
                ..Collection::new("col-a", "A")
            })
            .unwrap();

        let (panel, cx) = open(cx, store);

        // Both ancestors start collapsed, so the reveal is what expands them.
        panel.update(cx, |panel, cx| {
            panel.toggle_collapse("col-a", cx);
            panel.toggle_collapse("fold-a", cx);
        });
        assert_eq!(cx.update(|_, cx| panel.read(cx).flatten("").len()), 1);

        panel.update(cx, |panel, cx| panel.reveal_node("req-a", cx));

        assert_eq!(
            cx.update(|_, cx| panel.read(cx).flatten("").len()),
            3,
            "revealing expands the collection and the folder"
        );
        assert_eq!(
            cx.update(|_, cx| panel.read(cx).selected_node().map(str::to_owned)),
            Some("req-a".to_owned())
        );

        // A node the tree no longer holds cannot be shown, so the selection is
        // left alone rather than cleared.
        panel.update(cx, |panel, cx| panel.reveal_node("gone", cx));
        assert_eq!(
            cx.update(|_, cx| panel.read(cx).selected_node().map(str::to_owned)),
            Some("req-a".to_owned())
        );
    }
}
