//! The Collections pane: saved requests in nested folders.
//!
//! A collection is a user-edited tree persisted as one store row, so every edit
//! mutates the working copy and saves the collection it touched. Opening a
//! request is an [`Event`] for the canvas; the tree walks and edits below the
//! view live in [`tree`], so they stay unit-testable without an `App` or a
//! `Window`.

mod tree;

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{h_flex, v_flex, WindowExt as _};
use gpui_kit::*;

use restbook_core::{Collection, CollectionItem, Draft, Folder, RequestDef, SavedRequest, Store};

use self::tree::{
    display_name, filter_paths, find_in_items_all, find_saved_mut, has_move_target, initial,
    insert_into, item_name, move_item, node_name, owner_collection, remove_item, rename_item,
    resolve_selection, sort_refs,
};

pub(crate) use self::tree::{Destination, DestinationRow};

use crate::document::Document;
use crate::icons::{self, IconName};
use crate::notice::Notice;
use crate::theme::{Metrics, Palette};

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
    collections: Vec<Collection>,
    /// The drafts the workspace persists, newest first. Read back from the store
    /// so the Unsaved list follows what is actually on disk.
    drafts: Vec<Draft>,
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
    /// The rows the current filter shows, keyed by the query, the data revision
    /// and the sort order, so a repaint with none of them changed does not
    /// re-run the filter. Each entry is a path of indices into `collections`,
    /// so the render walks the borrowed tree rather than a cloned copy. The
    /// empty-query case is not cached here; that tree is borrowed in place.
    filter_cache: Option<(String, u64, SortOrder, Vec<Vec<usize>>)>,
    /// Bumped on every mutation of `collections`; keys `filter_cache`.
    revision: u64,
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

        // Observe the document so "Save request" reflects the live request, and
        // the search box so typing in it re-filters: both notify their own
        // state, not this pane.
        cx.observe(&document, |_, _, cx| cx.notify()).detach();
        cx.observe(&query, |_, _, cx| cx.notify()).detach();
        // The pane no longer observes `Variables`: the environment chip moved
        // to the titlebar, so an environment change repaints no row here.

        Self {
            palette: Palette::new(cx),
            metrics: Metrics::current(cx),
            store,
            document,
            collections,
            drafts,
            query,
            new_collection,
            rename_input,
            selection: Selection::default(),
            sort: SortOrder::Manual,
            collapsed: HashSet::new(),
            renaming: None,
            composing: false,
            filter_cache: None,
            revision: 0,
            focus_handle: cx.focus_handle(),
        }
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
        self.drafts = self.store.drafts().unwrap_or_default();
        cx.notify();
    }

    /// Re-read the drafts without touching the tree. Called by the workspace
    /// when it writes or clears a draft, since the tabs live there, not here.
    pub(crate) fn refresh_drafts(&mut self, cx: &mut Context<Self>) {
        let started = std::time::Instant::now();
        self.drafts = self.store.drafts().unwrap_or_default();
        log::debug!(
            target: "restbook_app::perf",
            "drafts reload took {}ms",
            started.elapsed().as_millis(),
        );
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
    /// Callers that have a window announce the creation; the save dialog does
    /// not, because its own "Request saved." is the sentence for the step the
    /// user asked for.
    pub(crate) fn create_collection(
        &mut self,
        name: &str,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let id = new_id();
        self.collections.push(Collection {
            id: id.clone(),
            name: name.to_owned(),
            items: Vec::new(),
        });
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

    /// Save the open request into a destination. Called by the pane's + menu, the
    /// workspace's `Ctrl+S`, and the request pane's Save button. `node`, when
    /// given, updates that node in place rather than appending; `name` falls back
    /// to `"{method} {url}"`; `target` is where the save lands, and `None` means
    /// the sidebar's selection, which is what the pane's own + menu means.
    /// Returns the node id to bind the tab; a target that no longer exists is a
    /// normal state and yields `None`, not an error.
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
        let request = self.document.read(cx).request.clone();
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

    /// The selected collection as `(id, name)`, for a command that acts on one
    /// collection rather than on the tree.
    pub(crate) fn selected_collection(&self) -> Option<(&str, &str)> {
        let id = self.selection.collection.as_deref()?;
        let collection = self.collections.iter().find(|each| each.id == id)?;
        Some((collection.id.as_str(), collection.name.as_str()))
    }

    /// Every place a save can land, for the first-save dialog's picker.
    pub(crate) fn destinations(&self) -> Vec<DestinationRow> {
        tree::destination_rows(&self.collections)
    }

    /// Where a saved request currently lives, for the move dialog's prefill.
    pub(crate) fn destination_of(&self, id: &str) -> Option<Destination> {
        tree::destination_of(&self.collections, id)
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

    fn start_rename(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current = node_name(&self.collections, id).unwrap_or_default();
        self.renaming = Some(id.to_owned());
        self.rename_input
            .update(cx, |state, cx| state.set_value(current, window, cx));
        cx.notify();
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.renaming.take() else {
            return;
        };
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
            menu.item(PopupMenuItem::separator()).item(
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

    fn render_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.query.read(cx).value().to_string();

        if self.collections.is_empty() {
            return self.message("No collections yet. Add one above.");
        }

        // The empty filter borrows the tree in place: sorting reorders
        // references, so a repaint never deep-copies the collections.
        if query.trim().is_empty() {
            let mut ordered: Vec<&Collection> = self.collections.iter().collect();
            sort_refs(&mut ordered, self.sort);
            let mut rows = Vec::with_capacity(ordered.len());
            for collection in ordered {
                rows.push(self.render_collection(collection, &query, cx));
            }
            return self.list(rows);
        }

        // A non-empty filter is computed once per query/data/sort change, then
        // reused. It yields index paths, so drawing borrows the tree in place
        // instead of copying every match.
        let fresh = matches!(
            &self.filter_cache,
            Some((cached_query, cached_revision, cached_sort, _))
                if cached_query == &query
                    && *cached_revision == self.revision
                    && *cached_sort == self.sort
        );
        if !fresh {
            let paths = filter_paths(&self.collections, &query, self.sort);
            self.filter_cache = Some((query.clone(), self.revision, self.sort, paths));
        }
        let paths: &[Vec<usize>] = &self.filter_cache.as_ref().expect("cache set above").3;
        if paths.is_empty() {
            return self.message("No matches.");
        }

        let mut rows = Vec::with_capacity(paths.len());
        for path in paths {
            rows.push(self.render_path(path, &query, cx));
        }

        self.list(rows)
    }

    /// The scrolling body around already-built rows. Shared by the borrowed and
    /// filtered paths so both draw the same container.
    fn list(&self, rows: Vec<AnyElement>) -> AnyElement {
        v_flex()
            .id("collections-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .py(px(self.metrics.space_xs))
            .children(rows)
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

    /// Draw one row of the filtered tree by its index path. The unfiltered tree
    /// recurses through [`Self::render_collection`]; this flattens the filtered
    /// result so every visible row is built from the borrowed collections.
    fn render_path(&self, path: &[usize], query: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some((&collection_index, rest)) = path.split_first() else {
            return div().into_any_element();
        };
        let Some(collection) = self.collections.get(collection_index) else {
            return div().into_any_element();
        };
        if rest.is_empty() {
            return self.collection_row(collection, query, cx);
        }

        // Walk to the target item, carrying the ancestor folder ids for a
        // selection and the depth for indentation.
        let mut items: &[CollectionItem] = &collection.items;
        let mut ancestors: Vec<String> = Vec::new();
        let mut depth = 1;
        let mut target = None;
        for (step, &index) in rest.iter().enumerate() {
            let Some(item) = items.get(index) else {
                return div().into_any_element();
            };
            let last = step + 1 == rest.len();
            if last {
                target = Some(item);
                break;
            }
            match item {
                CollectionItem::Folder(folder) => {
                    ancestors.push(folder.id.clone());
                    items = &folder.items;
                }
                CollectionItem::Request(_) => return div().into_any_element(),
            }
            depth += 1;
        }

        match target {
            Some(CollectionItem::Folder(folder)) => {
                self.folder_row(&collection.id, folder, &ancestors, depth, cx)
            }
            Some(CollectionItem::Request(request)) => {
                self.render_request(&collection.id, request, &ancestors, depth, cx)
            }
            None => div().into_any_element(),
        }
    }

    fn render_collection(
        &self,
        collection: &Collection,
        query: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let collapsed = query.is_empty() && self.collapsed.contains(&collection.id);
        let mut column = v_flex()
            .w_full()
            .child(self.collection_row(collection, query, cx));

        if !collapsed {
            // The empty filter borrows the tree in stored order, so order
            // references here rather than copying the children to sort them.
            if query.trim().is_empty() && self.sort != SortOrder::Manual {
                let mut items: Vec<&CollectionItem> = collection.items.iter().collect();
                items.sort_by(|left, right| self.sort.compare(item_name(left), item_name(right)));
                for item in items {
                    column =
                        column.child(self.render_item(&collection.id, item, &[], 1, query, cx));
                }
            } else {
                for item in &collection.items {
                    column =
                        column.child(self.render_item(&collection.id, item, &[], 1, query, cx));
                }
            }
        }

        column.into_any_element()
    }

    /// The collection's own row, without its children.
    fn collection_row(
        &self,
        collection: &Collection,
        query: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let collapsed = query.is_empty() && self.collapsed.contains(&collection.id);
        let selected = self.selection.collection.as_deref() == Some(collection.id.as_str())
            && self.selection.folders.is_empty()
            && self.selection.request.is_none();
        let renaming = self.renaming.as_deref() == Some(collection.id.as_str());

        let toggle_id = collection.id.clone();
        let toggle = Button::new(format!("collection-toggle-{}", collection.id))
            .icon(if collapsed {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            })
            .ghost()
            .compact()
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.collapsed.remove(&toggle_id) {
                    this.collapsed.insert(toggle_id.clone());
                }
                cx.notify();
            }));

        let select_id = collection.id.clone();
        let select = cx.listener(move |this, _, _, cx| {
            this.selection = Selection::collection(&select_id);
            cx.notify();
        });

        let name_area = if renaming {
            h_flex()
                .id(format!("collection-rename-{}", collection.id))
                .flex_1()
                .min_w_0()
                .gap(px(self.metrics.space_xs))
                .items_center()
                .child(
                    Input::new(&self.rename_input)
                        .id("collection-rename-input")
                        .flex_1(),
                )
                .child(
                    Button::new("collection-rename-confirm")
                        .icon(IconName::Check)
                        .ghost()
                        .compact()
                        .on_click(
                            cx.listener(|this, _, window, cx| this.commit_rename(window, cx)),
                        ),
                )
        } else {
            h_flex()
                .id(format!("collection-name-{}", collection.id))
                .flex_1()
                .min_w_0()
                .gap(px(self.metrics.space_1p5))
                .items_center()
                .cursor_pointer()
                .on_click(select)
                // The monogram: one letter in a tinted square. It is the same
                // shape as the workspace chip in the titlebar, which is what
                // ties a collection to the window it is open in.
                .child(
                    div()
                        .flex_none()
                        .size(px(self.metrics.icon_sm))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .bg(self.palette.list_hover)
                        .text_color(self.palette.muted)
                        .text_size(px(self.metrics.font_badge))
                        .font_weight(FontWeight::BOLD)
                        .child(initial(&collection.name)),
                )
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(self.palette.foreground)
                        .child(collection.name.clone()),
                )
        };

        // Collection rows sit at the tree's left edge and are the only rows
        // that carry weight, so the eye can find a collection's start while
        // scrolling a long tree.
        let mut row = h_flex()
            .id(format!("collection-{}", collection.id))
            .w_full()
            .h(px(self.metrics.row_height))
            .px(px(self.metrics.space_sm))
            .gap(px(self.metrics.space_xs))
            .items_center()
            .rounded_sm()
            .hover(|style| style.bg(self.palette.list_hover));
        if selected {
            row = row.bg(self.palette.list_active);
        }

        let target = RowTarget::Collection(collection.id.clone());
        let tools = self.render_row_tools(&target, true, cx);
        let row = row.child(toggle).child(name_area).child(tools);
        // A right-click anywhere on the row opens the same menu, at the
        // pointer: `context_menu` records the press position and anchors the
        // popup there, so no second, invisible trigger is needed. The inline
        // rename input keeps its own right-click (select, paste) while open.
        if renaming {
            row.into_any_element()
        } else {
            let menu = self.row_menu(&target, true, cx);
            row.context_menu(menu).into_any_element()
        }
    }

    fn render_item(
        &self,
        collection_id: &str,
        item: &CollectionItem,
        path: &[String],
        depth: usize,
        query: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match item {
            CollectionItem::Folder(folder) => {
                self.render_folder(collection_id, folder, path, depth, query, cx)
            }
            CollectionItem::Request(request) => {
                self.render_request(collection_id, request, path, depth, cx)
            }
        }
    }

    fn render_folder(
        &self,
        collection_id: &str,
        folder: &Folder,
        path: &[String],
        depth: usize,
        query: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let collapsed = query.is_empty() && self.collapsed.contains(&folder.id);
        let mut own_path = path.to_vec();
        own_path.push(folder.id.clone());
        let mut column =
            v_flex()
                .w_full()
                .child(self.folder_row(collection_id, folder, path, depth, cx));

        if !collapsed {
            // Same as the collection level: only the borrowed tree needs its
            // children ordered here.
            if query.trim().is_empty() && self.sort != SortOrder::Manual {
                let mut children: Vec<&CollectionItem> = folder.items.iter().collect();
                children
                    .sort_by(|left, right| self.sort.compare(item_name(left), item_name(right)));
                for child in children {
                    column = column.child(self.render_item(
                        collection_id,
                        child,
                        &own_path,
                        depth + 1,
                        query,
                        cx,
                    ));
                }
            } else {
                for child in &folder.items {
                    column = column.child(self.render_item(
                        collection_id,
                        child,
                        &own_path,
                        depth + 1,
                        query,
                        cx,
                    ));
                }
            }
        }

        column.into_any_element()
    }

    /// The folder's own row, without its children.
    fn folder_row(
        &self,
        collection_id: &str,
        folder: &Folder,
        path: &[String],
        depth: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let collapsed = self.collapsed.contains(&folder.id);
        let selected = self.selection.collection.as_deref() == Some(collection_id)
            && self.selection.folder() == Some(folder.id.as_str())
            && self.selection.request.is_none();
        let renaming = self.renaming.as_deref() == Some(folder.id.as_str());

        let mut own_path = path.to_vec();
        own_path.push(folder.id.clone());

        let toggle_id = folder.id.clone();
        let toggle = Button::new(format!("folder-toggle-{}", folder.id))
            .icon(if collapsed {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            })
            .ghost()
            .compact()
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.collapsed.remove(&toggle_id) {
                    this.collapsed.insert(toggle_id.clone());
                }
                cx.notify();
            }));

        let select_collection = collection_id.to_owned();
        let select_path = own_path.clone();
        let select = cx.listener(move |this, _, _, cx| {
            this.selection = Selection {
                collection: Some(select_collection.clone()),
                folders: select_path.clone(),
                request: None,
            };
            cx.notify();
        });

        let name_area = if renaming {
            h_flex()
                .id(format!("folder-rename-{}", folder.id))
                .flex_1()
                .min_w_0()
                .gap(px(self.metrics.space_xs))
                .items_center()
                .child(
                    Input::new(&self.rename_input)
                        .id("folder-rename-input")
                        .flex_1(),
                )
                .child(
                    Button::new("folder-rename-confirm")
                        .icon(IconName::Check)
                        .ghost()
                        .compact()
                        .on_click(
                            cx.listener(|this, _, window, cx| this.commit_rename(window, cx)),
                        ),
                )
        } else {
            h_flex()
                .id(format!("folder-name-{}", folder.id))
                .flex_1()
                .min_w_0()
                .gap(px(self.metrics.space_xs))
                .cursor_pointer()
                .on_click(select)
                .child(icons::quiet(
                    if collapsed {
                        IconName::Folder
                    } else {
                        IconName::FolderOpen
                    },
                    icons::SMALL,
                    &self.palette,
                ))
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .text_color(self.palette.foreground)
                        .child(folder.name.clone()),
                )
        };

        let mut row = h_flex()
            .id(format!("folder-{}", folder.id))
            .w_full()
            .h(px(self.metrics.row_height))
            .px(px(self.metrics.space_sm))
            .gap(px(self.metrics.space_xs))
            .items_center()
            .rounded_sm()
            .pl(px(
                self.metrics.gutter * 0.5 + depth as f32 * self.metrics.indent
            ))
            .hover(|style| style.bg(self.palette.list_hover));
        if selected {
            row = row.bg(self.palette.list_active);
        }

        let target = RowTarget::Item(folder.id.clone());
        let tools = self.render_row_tools(&target, true, cx);
        let row = row.child(toggle).child(name_area).child(tools);
        if renaming {
            row.into_any_element()
        } else {
            let menu = self.row_menu(&target, true, cx);
            row.context_menu(menu).into_any_element()
        }
    }

    fn render_request(
        &self,
        collection_id: &str,
        request: &SavedRequest,
        path: &[String],
        depth: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selection.request.as_deref() == Some(request.id.as_str());
        let method = request.request.method;
        let label = display_name(&request.name, method).to_owned();
        let renaming = self.renaming.as_deref() == Some(request.id.as_str());

        let select_collection = collection_id.to_owned();
        let select_path = path.to_vec();
        let select_id = request.id.clone();
        let opened = request.request.clone();
        let opened_node = request.id.clone();
        let open = cx.listener(move |this, _, _, cx| {
            this.selection = Selection {
                collection: Some(select_collection.clone()),
                folders: select_path.clone(),
                request: Some(select_id.clone()),
            };
            cx.emit(Event::Open {
                node: opened_node.clone(),
                request: opened.clone(),
            });
            cx.notify();
        });

        // The verb wears its method color and keeps its own width, the way
        // every client in this family draws a tree row: `DEL  Delete comment`.
        // A fixed-width badge was reserving a gutter for a four-letter word and
        // pushed the names of short verbs out of alignment with each other.
        let method_badge = div()
            .flex_none()
            .text_size(px(self.metrics.font_badge))
            .font_weight(FontWeight::BOLD)
            .text_color(self.palette.method(method))
            .child(method.as_str());

        let name_area = if renaming {
            // Same inline editor as a collection or folder: a request's stored
            // name is only a label, so renaming it never touches the request.
            h_flex()
                .id(format!("request-rename-{}", request.id))
                .flex_1()
                .min_w_0()
                .gap(px(self.metrics.space_xs))
                .items_center()
                .child(
                    Input::new(&self.rename_input)
                        .id("request-rename-input")
                        .flex_1(),
                )
                .child(
                    Button::new("request-rename-confirm")
                        .icon(IconName::Check)
                        .ghost()
                        .compact()
                        .on_click(
                            cx.listener(|this, _, window, cx| this.commit_rename(window, cx)),
                        ),
                )
        } else {
            h_flex()
                .id(format!("request-name-{}", request.id))
                .flex_1()
                .min_w_0()
                .gap(px(self.metrics.space_sm))
                .items_center()
                .cursor_pointer()
                .on_click(open)
                .child(method_badge)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(if selected {
                            self.palette.foreground
                        } else {
                            self.palette.muted
                        })
                        .child(label),
                )
        };

        let mut row = h_flex()
            .id(format!("request-{}", request.id))
            .w_full()
            .h(px(self.metrics.row_height))
            .px(px(self.metrics.space_sm))
            // The 6 px step, not `sm`: the tree's other rows use `xs`, and a
            // request row a full step wider broke the vertical rhythm of a
            // column of names. This is the one row with a badge before its
            // name, so it gets a half-step more than the rest.
            .gap(px(self.metrics.space_1p5))
            .items_center()
            .rounded_sm()
            .pl(px(
                self.metrics.gutter * 0.5 + depth as f32 * self.metrics.indent
            ))
            .hover(|style| style.bg(self.palette.list_hover));
        if selected {
            row = row.bg(self.palette.list_active);
        }

        let target = RowTarget::Item(request.id.clone());
        let tools = self.render_row_tools(&target, true, cx);
        let row = row.child(name_area).child(tools);
        if renaming {
            row.into_any_element()
        } else {
            let menu = self.row_menu(&target, true, cx);
            row.context_menu(menu).into_any_element()
        }
    }

    /// The verbs a row offers, shared by the ellipsis dropdown and a right-click
    /// anywhere on the row.
    ///
    /// Move up/down are disabled where [`move_item`] would refuse them: at the
    /// ends of a sibling list, and on a collection, which sits at the top level
    /// and is not reordered by this pane. A disabled verb reads as "not now"
    /// rather than as a click that silently does nothing.
    ///
    /// Menu callbacks arrive with an `App`, not this panel's `Context`, so each
    /// one reaches the panel through the entity handle captured here.
    fn row_menu(
        &self,
        target: &RowTarget,
        allow_rename: bool,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let id = target.id().to_owned();
        let can_up = has_move_target(&self.collections, &id, true);
        let can_down = has_move_target(&self.collections, &id, false);
        let allow_move = self.is_request(&id);
        let panel = cx.entity();
        let target = target.clone();
        let verbs = RowVerbs {
            can_up,
            can_down,
            can_move: allow_move,
            can_rename: allow_rename,
        };

        move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            row_items(menu, id.clone(), target.clone(), verbs, panel.clone())
        }
    }

    /// The row's overflow menu: one ghost ellipsis that opens the same verbs a
    /// right-click does.
    ///
    /// The verbs are menu items instead of a button cluster, so a row spends
    /// one 20px affordance rather than 88px and a long name is not clipped.
    fn render_row_tools(
        &self,
        target: &RowTarget,
        allow_rename: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let menu_id = target.id().to_owned();
        let menu = self.row_menu(target, allow_rename, cx);

        // `justify_end` pins the button right so every row's name truncates at
        // the same column, and the fixed width keeps that column still whether
        // the row is a collection, a folder, or a request.
        h_flex()
            .flex_none()
            .justify_end()
            .w(px(self.metrics.tool_lane))
            .child(
                Button::new(format!("row-menu-{menu_id}"))
                    .icon(IconName::Ellipsis)
                    .ghost()
                    .compact()
                    .tooltip("More actions")
                    .dropdown_menu(menu),
            )
            .into_any_element()
    }
}

impl Render for CollectionsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = self.palette.sidebar;
        v_flex()
            .size_full()
            .bg(bg)
            .track_focus(&self.focus_handle)
            .child(self.render_header(cx))
            .children(self.composing.then(|| self.render_new_collection(cx)))
            .children((!self.drafts.is_empty()).then(|| self.render_unsaved(cx)))
            .child(self.render_list(cx))
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

    use gpui_kit::{AppContext as _, TestAppContext};
    use restbook_core::{Collection, CollectionItem, Folder, Store};
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
                id: "col-a".to_owned(),
                name: "A".to_owned(),
                items: vec![CollectionItem::Folder(Folder {
                    id: "fold-a".to_owned(),
                    name: "Inner".to_owned(),
                    items: Vec::new(),
                })],
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

    fn two_collections() -> (Collection, Collection) {
        let request = restbook_core::SavedRequest {
            id: "req-a".to_owned(),
            name: "Request A".to_owned(),
            request: restbook_core::RequestDef::new(restbook_core::Method::Get, "https://a"),
        };
        (
            Collection {
                id: "col-a".to_owned(),
                name: "A".to_owned(),
                items: vec![CollectionItem::Request(request)],
            },
            Collection {
                id: "col-b".to_owned(),
                name: "B".to_owned(),
                items: Vec::new(),
            },
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
}
