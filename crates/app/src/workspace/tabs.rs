//! The open-tab model, owned in one place.
//!
//! A [`Tab`] is what a tab holds; a [`TabEntry`] is the strip's read-only projection
//! of it. [`TabState`] keeps the whole set and hands the request pane only entries,
//! so no field is maintained twice. Identity is the [`TabId`], not the position:
//! closing shifts every later index, and async work can outlive its slot.

use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::SharedString;
use gpui_kit::*;
use restbook_core::{Draft, Method, Origin, RequestDef, ResponseRecord};

use crate::document::Document;

use crate::notice::Notice;
use crate::panels::collections::Destination;

use super::chrome;
use super::delete::PendingDelete;
use super::persistence::{SavedTab, SavedTabs, CLOSED_TAB_LIMIT};
use super::{close_needs_confirmation, wrap_index, RequestStamp, Workspace};

/// A tab's stable identity for the life of the open tab.
pub(crate) type TabId = u64;

/// A bulk or per-tab verb the request panel reports to the workspace.
///
/// The panel names the tab; the workspace decides whether it can be done and
/// how (a confirmation, a batch summary, a reorder).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabAction {
    /// Close one tab, asking first if it would lose work.
    Close(TabId),
    /// Close every tab but this one.
    CloseOthers(TabId),
    /// Close every tab to the right of this one.
    CloseToRight(TabId),
    /// Close every tab.
    CloseAll,
    /// Copy this tab into a new, independent tab.
    Duplicate(TabId),
    /// Move this tab one place left.
    MoveLeft(TabId),
    /// Move this tab one place right.
    MoveRight(TabId),
}

/// A tab's saved request and binding. The response and the in-flight send live
/// in the tab's own [`Document`], so switching tabs never moves an outcome
/// between them: the tab carries only what the strip and the draft writer read.
#[derive(Clone)]
pub(crate) struct Tab {
    /// Stable for the life of the tab. A send outlives the index it started
    /// from, so the in-flight send is recorded against this, not a position.
    pub(crate) id: TabId,
    pub(crate) request: RequestDef,
    /// The request as it was when it was saved to a collection, if it ever
    /// was. Compared against the live request to mark the tab unsaved: a
    /// `RequestDef` is not comparable, so it is kept as its serialized form.
    pub(crate) saved: Option<String>,
    /// The collection node this tab was opened from, if it came from one. Save
    /// updates that node instead of creating a new one; `None` means the tab is
    /// a draft and Save creates a node. Kept as a plain id so a reorder or
    /// rename in the tree never invalidates it.
    pub(crate) node: Option<String>,
    /// The drafts-table row this tab is persisted as, if any. `None` means the
    /// tab has not been written yet, or has since been filed into a collection,
    /// so the next sync inserts a row.
    pub(crate) draft: Option<i64>,
}

impl Tab {
    pub(crate) fn blank() -> Self {
        Self {
            id: new_tab_id(),
            request: RequestDef::new(Method::Get, ""),
            saved: None,
            node: None,
            draft: None,
        }
    }

    /// A copy of this tab's live content under a new identity.
    ///
    /// The copy is deliberately unbound and has no draft row: sharing either
    /// with the source would make Save overwrite the source's node or write
    /// over its draft. The first debounce gives it a row of its own.
    pub(crate) fn copy_of(&self) -> Self {
        Self {
            id: new_tab_id(),
            request: self.request.clone(),
            saved: self.saved.clone(),
            node: None,
            draft: None,
        }
    }

    /// A closed tab brought back: a new identity, because the old one may have
    /// been reserved by a send, and no draft row, because close deleted it.
    pub(crate) fn reopened(mut self) -> Self {
        self.id = new_tab_id();
        self.draft = None;
        self
    }

    /// A tab opened from a stored draft row: its request, and the row it is
    /// already persisted as.
    pub(crate) fn from_draft(draft: restbook_core::Draft) -> Self {
        Self {
            id: new_tab_id(),
            request: draft.request,
            saved: None,
            node: None,
            draft: Some(draft.id),
        }
    }

    /// A tab that has never been touched: no collection binding, no draft row,
    /// and an empty URL. Only the placeholder the window opens on (and the one
    /// left after closing every tab) is in this state, which is what lets a
    /// sidebar pick take it over without stranding an empty request.
    pub(crate) fn is_pristine(&self) -> bool {
        self.node.is_none()
            && self.draft.is_none()
            && self.saved.is_none()
            && self.request.url.trim().is_empty()
    }

    /// Who owns this tab's sends: its saved node first, then its draft row, and
    /// nothing when it has never been persisted. Mirrored into the document so
    /// the response pane can scope history to this request instead of a URL.
    pub(crate) fn origin(&self) -> Origin {
        match (&self.node, self.draft) {
            (Some(node), _) => Origin::Saved { node: node.clone() },
            (None, Some(draft)) => Origin::Draft { draft },
            (None, None) => Origin::Standalone,
        }
    }

    /// The strip's view of this tab. This is the one place a tab's fields are
    /// projected, so adding a field to `Tab` is one edit here rather than a
    /// second hand-maintained mirror in the panel.
    pub(crate) fn entry(&self, pending: bool) -> TabEntry {
        TabEntry {
            id: self.id,
            method: self.request.method,
            label: crate::panels::request::tab_label(&self.request),
            url: self.request.url.clone().into(),
            current: RequestStamp::of(&self.request),
            saved: self.saved.clone(),
            node: self.node.clone(),
            draft: self.draft,
            insecure: self.request.insecure,
            pending,
        }
    }
}

/// One open tab, as the strip needs it. Every field is derived from [`Tab`],
/// and the type compares by value so `set_tabs` cannot drift on a new field.
#[derive(Clone, PartialEq)]
pub(crate) struct TabEntry {
    pub(crate) id: TabId,
    pub(crate) method: Method,
    pub(crate) label: SharedString,
    /// The full address, for the tab's tooltip when the label is truncated.
    pub(crate) url: SharedString,
    /// The request as it is now, folded to a comparable string. For an inactive
    /// tab this is its last snapshot; for the active one it is ignored in
    /// favour of the live request.
    pub(crate) current: String,
    /// The request as it was at its last save to a collection, if it ever was.
    /// `None` means "never saved", which is not the same as "unsaved": a
    /// fresh tab is an empty page, not unsaved work.
    pub(crate) saved: Option<String>,
    /// The saved node this tab was opened from, when it came from a collection.
    /// `Some` turns Save into an update-in-place; `None` means Save creates a
    /// new node. The workspace owns the value and only lends it to the strip.
    pub(crate) node: Option<String>,
    /// The `draft` row this tab is persisted as, when it has one. A draft has no
    /// node, so this is the only stable identity it can key on while it is
    /// moving to and from a collection.
    pub(crate) draft: Option<i64>,
    /// Whether the request skips certificate verification. Surfaced on the strip
    /// so an insecure tab is recognisable without opening its Settings tab.
    pub(crate) insecure: bool,
    /// Whether this tab owns the one in-flight send. The strip draws a spinner in
    /// place of its unsaved dot, so the send is visible on the tab that started it.
    pub(crate) pending: bool,
}

/// The whole open-tab set and which one is on screen. One owner for the list
/// and the selection, so a reorder or a close updates both together.
pub(crate) struct TabState {
    tabs: Vec<Tab>,
    active: usize,
}

impl TabState {
    pub(crate) fn new(tabs: Vec<Tab>) -> Self {
        Self { tabs, active: 0 }
    }

    pub(crate) fn len(&self) -> usize {
        self.tabs.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub(crate) fn get(&self, index: usize) -> Option<&Tab> {
        self.tabs.get(index)
    }

    pub(crate) fn get_mut(&mut self, index: usize) -> Option<&mut Tab> {
        self.tabs.get_mut(index)
    }

    pub(crate) fn iter(&self) -> std::slice::Iter<'_, Tab> {
        self.tabs.iter()
    }

    pub(crate) fn position(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    pub(crate) fn push(&mut self, tab: Tab) -> usize {
        self.tabs.push(tab);
        self.tabs.len() - 1
    }

    pub(crate) fn insert(&mut self, index: usize, tab: Tab) {
        self.tabs.insert(index, tab);
    }

    pub(crate) fn remove(&mut self, index: usize) -> Tab {
        self.tabs.remove(index)
    }

    pub(crate) fn swap(&mut self, left: usize, right: usize) {
        self.tabs.swap(left, right);
    }

    pub(crate) fn active(&self) -> usize {
        self.active
    }

    pub(crate) fn set_active(&mut self, index: usize) {
        self.active = index;
    }

    /// The active tab, by its stable id rather than its position.
    pub(crate) fn active_id(&self) -> Option<TabId> {
        self.tabs.get(self.active).map(|tab| tab.id)
    }

    /// Every tab as the strip draws it, in order.
    pub(crate) fn entries(&self, sending: Option<TabId>) -> Vec<TabEntry> {
        self.tabs
            .iter()
            .map(|tab| tab.entry(sending == Some(tab.id)))
            .collect()
    }
}

impl std::ops::Index<usize> for TabState {
    type Output = Tab;

    fn index(&self, index: usize) -> &Self::Output {
        &self.tabs[index]
    }
}

impl std::ops::IndexMut<usize> for TabState {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.tabs[index]
    }
}

/// Monotonic tab ids. The counter starts at one so a default (`0`) never
/// matches a real tab.
fn new_tab_id() -> TabId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The first non-empty, trimmed line of a clipboard paste. A paste that is
/// blank, or only whitespace, yields `None`.
fn clipboard_url(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

/// Rebuild the open tabs from what the layout file recorded.
///
/// A bound tab is resolved from the collection tree, a draft from its store row;
/// one that no longer resolves is dropped rather than opened missing. Survivors
/// keep their saved order, dropped ones are counted for the caller's single
/// message, and the active index lands on the same request.
pub(super) fn resolve_saved_tabs(
    saved: &SavedTabs,
    find_node: impl Fn(&str) -> Option<RequestDef>,
    drafts: &[Draft],
) -> (Vec<Tab>, usize, usize) {
    let mut tabs = Vec::with_capacity(saved.entries.len());
    let mut dropped = 0;
    let mut active = None;
    for (index, entry) in saved.entries.iter().enumerate() {
        let tab = match entry {
            SavedTab::Bound { node } => match find_node(node) {
                Some(request) => {
                    let mut tab = Tab::blank();
                    tab.saved = Some(RequestStamp::of(&request));
                    tab.request = request;
                    tab.node = Some(node.clone());
                    tab
                }
                None => {
                    dropped += 1;
                    continue;
                }
            },
            SavedTab::Draft { id } => match drafts.iter().find(|draft| draft.id == *id) {
                Some(draft) => Tab::from_draft(draft.clone()),
                None => {
                    dropped += 1;
                    continue;
                }
            },
        };
        if index == saved.active {
            active = Some(tabs.len());
        }
        tabs.push(tab);
    }
    let active = active.unwrap_or_else(|| saved.active.min(tabs.len().saturating_sub(1)));
    (tabs, dropped, active)
}

/// What a rebind writes into the tab's document.
///
/// The four rebinds differ only here: a first save leaves the request on
/// screen alone, an adoption replaces it, and a history pick restores the
/// outcome that was recorded with it.
enum DocState {
    /// Only the history owner moves; the request and its outcome stay.
    Keep,
    /// The document takes the new request and drops the old outcome.
    Replace,
    /// The document takes the new request and the outcome recorded with it.
    /// Boxed because a `ResponseRecord` is large and only this variant carries one.
    Restore {
        response: Box<ResponseRecord>,
        url: String,
    },
}

impl Workspace {
    /// Replace the single blank page with the tabs the last run had open.
    ///
    /// A run that ended with every tab closed is a state, not a first run: the
    /// window opens on the empty editor rather than resurrecting a blank page.
    /// A fresh tab is left alone when a record has no tabs and no close-all
    /// flag (a first run, or a file written before tabs were recorded), and
    /// when every saved tab was dropped because its request was deleted — there
    /// the loss is reported once over the fresh tab.
    pub(crate) fn restore_tabs(&mut self, saved: SavedTabs, cx: &mut Context<Self>) {
        let drafts = self.store.drafts().unwrap_or_default();
        let (tabs, dropped, active) = resolve_saved_tabs(
            &saved,
            |node| self.panes.collections.read(cx).saved_request(node),
            &drafts,
        );
        if tabs.is_empty() {
            if saved.entries.is_empty() && saved.closed_all {
                self.tabs = TabState::new(Vec::new());
                self.documents.clear();
                self.adopt_active(cx);
            }
            return;
        }
        self.tabs = TabState::new(tabs);
        // Each restored tab needs a document of its own, seeded from the tab so
        // the panes have an entity to observe before the first adopt.
        let seeded: Vec<(TabId, Entity<Document>)> = self
            .tabs
            .iter()
            .map(|tab| (tab.id, self.seeded_document(tab, cx)))
            .collect();
        self.documents = seeded.into_iter().collect();
        self.tabs.set_active(active);
        self.adopt_active(cx);
        if dropped > 0 {
            self.flash(
                chrome::FlashKind::Error,
                format!("Dropped {dropped} tab(s) whose request was deleted."),
                cx,
            );
        }
    }
    pub(crate) fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index == self.tabs.active() || index >= self.tabs.len() {
            return;
        }
        log::debug!(target: "restbook_app::ui", "select tab {index}");
        // Flush before the selection moves: the debounced writer reads the
        // active tab, so an edit-and-switch inside `SAVE_DEBOUNCE` would
        // otherwise be written against the tab being switched to.
        self.flush_leaving_draft(cx);
        self.tabs.set_active(index);
        self.adopt_active(cx);
        self.persist_tabs(cx);
    }

    /// Select a tab by its stable id, the way the strip reports a click.
    pub(crate) fn select_tab_id(&mut self, id: TabId, cx: &mut Context<Self>) {
        if let Some(index) = self.tabs.position(id) {
            self.select_tab(index, cx);
        }
    }

    /// Step to the next (`delta = 1`) or previous (`delta = -1`) tab, wrapping
    /// around at either end.
    pub(crate) fn select_relative(&mut self, delta: isize, cx: &mut Context<Self>) {
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

    /// A pasted cURL command becomes a request in its own tab, replacing the
    /// active one only when it is an untouched page. Confirm the import, then
    /// offer the first-save dialog so the request gets a name and a home; the
    /// dialog can create its destination, so an empty workspace is offered it.
    pub(crate) fn imported(&mut self, request: RequestDef, cx: &mut Context<Self>) {
        let index = self.place_for_pick(cx);
        self.bind_tab(index, request, None, None, None, DocState::Replace, cx);
        self.notice(
            &Notice::Done("Imported a request from the clipboard.".into()),
            cx,
        );
        self.pending_save = true;
        cx.notify();
    }

    /// Copy the active request as a runnable `curl` command. It uses the live
    /// editor, not the document snapshot, so a field that notifies its own
    /// state (the raw body, an auth input) is not one keystroke behind.
    pub(crate) fn copy_curl(&mut self, cx: &mut Context<Self>) {
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
    pub(crate) fn save_active(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.tabs.active()) else {
            return;
        };
        let node = tab.node.clone();
        if node.is_none() {
            self.pending_save = true;
            cx.notify();
            return;
        }
        self.commit_save(node, None, None, None, cx);
    }

    /// Save the active request as a new node, even when the tab is already
    /// bound. The dialog opens with the bound name prefilled and `node` unset,
    /// so the save creates an entry instead of overwriting the source.
    pub(crate) fn save_active_as(&mut self, cx: &mut Context<Self>) {
        self.pending_save = true;
        cx.notify();
    }

    /// What a rebind writes into the tab's document, decided by the caller.
    /// Bind tab `index` to its new request and owner, write both into the tab and
    /// its document, and point every pane at it.
    ///
    /// A caller that replaces a draft row reassigns its sends before calling this,
    /// so `draft` is the row the tab holds afterwards.
    #[allow(clippy::too_many_arguments)]
    fn bind_tab(
        &mut self,
        index: usize,
        request: RequestDef,
        node: Option<String>,
        draft: Option<i64>,
        saved: Option<String>,
        state: DocState,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.tabs.get(index).map(|tab| tab.id) else {
            return;
        };
        let adopt = !matches!(state, DocState::Keep);
        {
            let tab = &mut self.tabs[index];
            tab.request = request.clone();
            tab.saved = saved;
            tab.node = node;
            tab.draft = draft;
        }
        let origin = self.tabs[index].origin();
        if let Some(document) = self.documents.get(&id).cloned() {
            document.update(cx, |document, cx| {
                document.origin = origin;
                match state {
                    DocState::Keep => {}
                    DocState::Replace => {
                        document.request = request.clone();
                        document.response = None;
                        document.error = None;
                        document.last_url = None;
                        document.forget_cancel();
                    }
                    DocState::Restore { response, url } => {
                        document.request = request.clone();
                        document.response = Some(*response);
                        document.error = None;
                        // `url()` drives the cookie jar and the history scope,
                        // so it names the request that was recorded, not the
                        // tab's last send.
                        document.last_url = Some(url);
                        document.forget_cancel();
                    }
                }
                cx.notify();
            });
        }
        // The editor has to take the new request on this tick, or Send reads the
        // previous URL. A first save keeps the request already on screen, so
        // adopting it again would only reset a field mid-edit.
        if adopt {
            self.panes
                .request
                .update(cx, |panel, cx| panel.adopt_request(request, cx));
        }
        self.point_at(id, cx);
        self.push_tabs(cx);
        self.persist_tabs(cx);
    }

    /// Write the open request into `node`, into `target` under `name`, or into a
    /// collection `create` names that does not exist yet.
    ///
    /// The landed node is written back onto the tab, so a first save binds the tab
    /// and the next Save updates rather than duplicates.
    pub(super) fn commit_save(
        &mut self,
        node: Option<String>,
        name: Option<&str>,
        target: Option<Destination>,
        create: Option<&str>,
        cx: &mut Context<Self>,
    ) -> bool {
        // *New collection…* is a destination that does not exist yet: create it,
        // then save into its root, so one confirm leaves both behind.
        let target = match create {
            Some(collection_name) => {
                let collection_name = collection_name.trim();
                if collection_name.is_empty() {
                    return false;
                }
                let id = self.panes.collections.update(cx, |panel, cx| {
                    panel.create_collection(collection_name, None, cx)
                });
                let Some(id) = id else {
                    return false;
                };
                Some(Destination {
                    collection: id,
                    folders: Vec::new(),
                })
            }
            None => target,
        };
        let landed = self.panes.collections.update(cx, |panel, cx| {
            panel.save_current(node.as_deref(), name, target.as_ref(), cx)
        });
        let Some(node) = landed else {
            // Nothing was saved, and the pane could not say so on this path.
            self.flash(chrome::FlashKind::Error, "Couldn't save the request.", cx);
            return false;
        };
        // A saved request and its tab must compare equal, or the unsaved dot
        // would appear the moment the save completes.
        let request = self.panes.request.read(cx).current_request(cx);
        let active = self.tabs.active();
        let Some(stale) = self.tabs.get(active).map(|tab| tab.draft) else {
            return false;
        };
        if let Some(draft) = stale {
            // The draft became this node: carry its sends across instead of
            // orphaning them under a draft id that is about to be deleted.
            let origin = Origin::Saved { node: node.clone() };
            let _ = self
                .store
                .reassign_history_origin(&Origin::Draft { draft }, &origin);
            let _ = self.store.delete_draft(draft);
            self.remove_draft(draft, cx);
        }
        let saved = Some(RequestStamp::of(&request));
        self.bind_tab(active, request, Some(node), None, saved, DocState::Keep, cx);
        true
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

    /// Open a saved request from the sidebar.
    ///
    /// An already-open node is brought forward rather than re-adopted. Otherwise the
    /// request gets its own tab, except when the window sits on its lone untouched
    /// placeholder: taking that over keeps startup from stranding an empty request.
    /// A tab the user made with New tab is theirs to keep, even before its first edit.
    pub(crate) fn open_saved(&mut self, node: String, request: RequestDef, cx: &mut Context<Self>) {
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
            // `select_tab` snapshots the tab being left before it stops being
            // the active one. `push_tab` gives the new tab a document of its own.
            let index = self.push_tab(Tab::blank(), cx);
            self.select_tab(index, cx);
        }
        self.adopt_saved(node, request, cx);
    }

    /// Write a saved request into the tab on screen and bind it to its node,
    /// so a later Save updates that node instead of appending a duplicate. Any
    /// draft the tab was persisting is stale, so its sends are reassigned and
    /// the row is deleted.
    fn adopt_saved(&mut self, node: String, request: RequestDef, cx: &mut Context<Self>) {
        // With no tab open (every one was closed) this revives a fresh one.
        let index = self.ensure_active_tab(cx);
        let stale = self.tabs[index].draft;
        if let Some(draft) = stale {
            // The draft just became a saved request. Its sends are the same
            // requests, so they follow the new owner rather than disappearing
            // behind a dead draft id.
            let origin = Origin::Saved { node: node.clone() };
            let _ = self
                .store
                .reassign_history_origin(&Origin::Draft { draft }, &origin);
            let _ = self.store.delete_draft(draft);
            self.remove_draft(draft, cx);
        }
        let saved = Some(RequestStamp::of(&request));
        self.bind_tab(
            index,
            request,
            Some(node),
            None,
            saved,
            DocState::Replace,
            cx,
        );
    }

    /// Restore a stored send into the editor, its response included, without
    /// sending. The pick takes the active tab only while it is pristine, else
    /// opens a new one, and binds it to the row's owner when that still exists;
    /// with every tab closed it revives one first, because the request pane would
    /// otherwise keep drawing its empty state over the restored document.
    pub(crate) fn pick_history(&mut self, id: i64, cx: &mut Context<Self>) {
        let entry = match self.store.get_history(id) {
            Ok(Some(entry)) => entry,
            Ok(None) => {
                // The row vanished between the search and the click (for
                // example, a store replaced on disk). Nothing to restore.
                log::warn!(target: "restbook_app::ui", "history pick: id={id} vanished");
                return;
            }
            Err(error) => {
                log::error!(target: "restbook_app::ui", "history pick: id={id} failed: {error}");
                let document = self.document.clone();
                document.update(cx, |document, cx| {
                    document.error = Some(error.failure());
                    cx.notify();
                });
                return;
            }
        };
        log::info!(
            target: "restbook_app::ui",
            "history pick: restored {} {}",
            entry.request.method,
            crate::logging::redact_url(&entry.request.url),
        );
        // A pick keeps its owner: a saved or draft request lands in a tab bound
        // to that node or row, and never replaces a tab that already holds one.
        let (node, draft) = match &entry.origin {
            Origin::Saved { node } => {
                if self
                    .panes
                    .collections
                    .read(cx)
                    .saved_request(node)
                    .is_none()
                {
                    // The node was deleted since the send; only the response is
                    // left, so the pick is an ordinary unsent request.
                    (None, None)
                } else if let Some(index) = self
                    .tabs
                    .iter()
                    .position(|tab| tab.node.as_deref() == Some(node.as_str()))
                {
                    self.select_tab(index, cx);
                    return;
                } else {
                    (Some(node.clone()), None)
                }
            }
            Origin::Draft { draft } => {
                let alive = self
                    .store
                    .drafts()
                    .unwrap_or_default()
                    .iter()
                    .any(|row| row.id == *draft);
                if !alive {
                    (None, None)
                } else if let Some(index) =
                    self.tabs.iter().position(|tab| tab.draft == Some(*draft))
                {
                    self.select_tab(index, cx);
                    return;
                } else {
                    (None, Some(*draft))
                }
            }
            Origin::Standalone => (None, None),
        };

        let index = self.place_for_pick(cx);
        // A pick bound to a live node is not an edit: take the node's request as
        // the saved baseline so closing it does not ask to discard changes.
        let saved = node.as_ref().map(|_| RequestStamp::of(&entry.request));
        self.bind_tab(
            index,
            entry.request,
            node,
            draft,
            saved,
            DocState::Restore {
                response: Box::new(entry.response),
                url: entry.url,
            },
            cx,
        );
    }

    /// The tab a pick or a paste may take over: the active one only while it is
    /// an untouched page, otherwise a new tab. A tab with content or an owner is
    /// never replaced, so an import cannot destroy edits or a binding.
    pub(crate) fn place_for_pick(&mut self, cx: &mut Context<Self>) -> usize {
        if self.tabs.is_empty() {
            return self.ensure_active_tab(cx);
        }
        let active = self.tabs.active();
        if self.tabs.get(active).is_some_and(|tab| tab.is_pristine()) {
            return active;
        }
        let index = self.push_tab(Tab::blank(), cx);
        self.select_tab(index, cx);
        index
    }

    /// Open a new request tab whose URL is the text on the clipboard.
    ///
    /// A multi-line paste often carries a stray blank line, so the first
    /// non-empty line, trimmed, is the URL. An empty or missing clipboard
    /// reports a flash instead of opening a blank tab the user did not ask for.
    pub(crate) fn paste_url(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.notice(
                &Notice::Failed("The clipboard has no text to paste.".into()),
                cx,
            );
            return;
        };
        let Some(url) = clipboard_url(&text) else {
            self.notice(
                &Notice::Failed("The clipboard has no text to paste.".into()),
                cx,
            );
            return;
        };

        self.flush_leaving_draft(cx);
        let mut tab = Tab::blank();
        tab.request.url = url.to_owned();
        let index = self.push_tab(tab, cx);
        self.tabs.set_active(index);
        self.adopt_active(cx);
        log::info!(
            target: "restbook_app::ui",
            "paste url: {}",
            crate::logging::redact_url(url),
        );
        self.persist_tabs(cx);
    }

    pub(crate) fn new_tab(&mut self, cx: &mut Context<Self>) {
        // A new tab is a blank page, not a copy of whatever is on screen: the
        // active request may be unsaved, and cloning it made the new tab start
        // with work the user did not ask to carry over. Saving it creates a new
        // entry, while the tab left behind keeps its own edits.
        self.flush_leaving_draft(cx);
        let index = self.push_tab(Tab::blank(), cx);
        self.tabs.set_active(index);
        self.adopt_active(cx);
        log::debug!(target: "restbook_app::ui", "new tab: {} open", self.tabs.len());
        self.persist_tabs(cx);
    }

    /// Open a stored draft as a new tab. Used when the sidebar's Unsaved row is
    /// clicked for a draft that is not already open — the normal case now that
    /// the window starts on a blank tab instead of restoring every draft.
    pub(crate) fn open_draft(&mut self, draft: Draft, cx: &mut Context<Self>) {
        let id = draft.id;
        self.flush_leaving_draft(cx);
        let index = self.push_tab(Tab::from_draft(draft), cx);
        self.tabs.set_active(index);
        self.adopt_active(cx);
        log::info!(
            target: "restbook_app::ui",
            "open draft: {} {} (id {})",
            self.tabs[index].request.method,
            crate::logging::redact_url(&self.tabs[index].request.url),
            id,
        );
        self.persist_tabs(cx);
    }

    /// The tab on screen, by its stable id rather than its position.
    pub(crate) fn active_tab_id(&self) -> Option<TabId> {
        self.tabs.active_id()
    }

    /// The active tab, creating a fresh one when the last tab was closed.
    /// Opening a saved request, a draft, or a history pick is what revives an
    /// empty editor, so it is factored out instead of repeated in each event
    /// arm.
    pub(crate) fn ensure_active_tab(&mut self, cx: &mut Context<Self>) -> usize {
        if self.tabs.is_empty() {
            let index = self.push_tab(Tab::blank(), cx);
            self.tabs.set_active(index);
        }
        self.tabs.active()
    }

    /// Close a tab, asking first when the close would throw work away. A clean
    /// tab (and a blank one) closes at once, so the strip stays quick; only a
    /// tab with edits that are not in its collection needs the alert dialog.
    ///
    /// The id is stable while the dialog is open: it is modal, so nothing else
    /// can close a tab underneath it.
    pub(crate) fn confirm_close_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
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
    pub(crate) fn handle_tab_action(&mut self, action: TabAction, cx: &mut Context<Self>) {
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
    pub(crate) fn confirm_close_tabs(&mut self, ids: Vec<TabId>, cx: &mut Context<Self>) {
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
        let copy_id = copy.id;
        // The copy shares what the source shows, response included, under a new
        // document of its own.
        let fields = self.documents.get(&id).map(|source| {
            let source = source.read(cx);
            (
                source.request.clone(),
                source.response.clone(),
                source.error.clone(),
                source.last_url.clone(),
                source.origin.clone(),
            )
        });
        let document = match fields {
            Some((request, response, error, last_url, origin)) => cx.new(|_| {
                let mut document = Document::new();
                document.request = request;
                document.response = response;
                document.error = error;
                document.last_url = last_url;
                document.origin = origin;
                document
            }),
            None => self.seeded_document(&copy, cx),
        };
        self.tabs.insert(index + 1, copy);
        self.documents.insert(copy_id, document);
        self.tabs.set_active(index + 1);
        self.adopt_active(cx);
        log::debug!(target: "restbook_app::ui", "duplicated tab {id}");
        self.persist_tabs(cx);
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
        log::debug!(target: "restbook_app::ui", "moved tab {id} to {target}");
        self.persist_tabs(cx);
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
        let index = self.push_tab(tab.reopened(), cx);
        self.tabs.set_active(index);
        self.adopt_active(cx);
        log::debug!(target: "restbook_app::ui", "reopened a closed tab");
        self.persist_tabs(cx);
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

    pub(crate) fn close_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
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
            self.remove_draft(id, cx);
        }
        if self.sending == Some(id) {
            // Closing the sending tab cancels its transfer before the document
            // is dropped: an orphaned send would close the socket only at
            // process exit, and the freed slot could start a second on the wire.
            if let Some(document) = self.documents.get(&id).cloned() {
                document.update(cx, |document, cx| document.cancel(cx));
            }
            self.sending = None;
        }
        // Keep the content for Reopen Closed Tab. The draft row is gone, so a
        // reopened tab comes back as a copy with a new identity and no row.
        // The response lived in the tab's document and is not carried over.
        self.closed.push(self.tabs[index].clone());
        if self.closed.len() > CLOSED_TAB_LIMIT {
            self.closed.remove(0);
        }
        self.documents.remove(&id);
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

        log::info!(
            target: "restbook_app::ui",
            "closed tab: {} open, active {}",
            self.tabs.len(),
            self.tabs.active(),
        );
        self.persist_tabs(cx);
    }

    /// Store the live editor in the tab being left. The response already lives
    /// in that tab's document, so only the request has to be folded back.
    pub(crate) fn snapshot_active(&mut self, cx: &App) {
        // An adoption queued for the next frame means the widgets still belong
        // to the tab being left. Folding them here would write the request on
        // screen into whichever tab is current, so two switches inside one
        // frame used to copy one tab's body onto another.
        if self.panes.request.read(cx).has_pending_adoption(cx) {
            return;
        }
        let request = self.panes.request.read(cx).current_request(cx);
        let active = self.tabs.active();
        if let Some(tab) = self.tabs.get_mut(active) {
            tab.request = request;
        }
    }

    /// Point the panes at the active tab's document. The document already holds
    /// that tab's request, response, and in-flight state, so nothing is copied
    /// between tabs here: the editor adopts the request and every pane follows
    /// the new document.
    fn adopt_active(&mut self, cx: &mut Context<Self>) {
        let active = self.tabs.active();
        let Some(tab) = self.tabs.get(active) else {
            self.show_empty(cx);
            return;
        };
        let id = tab.id;
        let request = tab.request.clone();
        let origin = tab.origin();
        // Keep the tab snapshot authoritative for the request and its owner, so
        // a send names the right history scope.
        if let Some(document) = self.documents.get(&id).cloned() {
            document.update(cx, |document, cx| {
                document.request = request;
                document.origin = origin;
                cx.notify();
            });
        }
        self.point_at(id, cx);
        self.push_tabs(cx);
    }

    /// Point the editor and every pane at a blank document for a window with no
    /// open request.
    fn show_empty(&mut self, cx: &mut Context<Self>) {
        let blank = RequestDef::new(Method::Get, "");
        let document = self.empty_document.clone();
        document.update(cx, |document, cx| {
            document.forget_cancel();
            document.request = blank.clone();
            document.response = None;
            document.error = None;
            document.last_url = None;
            document.pending = None;
            document.progress = None;
            document.started_at = None;
            document.origin = Origin::default();
            cx.notify();
        });
        self.panes
            .request
            .update(cx, |panel, cx| panel.adopt_request(blank, cx));
        self.set_active_document(document, cx);
        self.refresh_collection_scope(cx);
        self.push_tabs(cx);
    }

    /// Push `tab` onto the strip and give it a document of its own, seeded from
    /// its request and binding.
    fn push_tab(&mut self, tab: Tab, cx: &mut Context<Self>) -> usize {
        let document = self.seeded_document(&tab, cx);
        let id = tab.id;
        let index = self.tabs.push(tab);
        self.documents.insert(id, document);
        index
    }

    /// A fresh document carrying `tab`'s request and history owner.
    fn seeded_document(&self, tab: &Tab, cx: &mut Context<Self>) -> Entity<Document> {
        let request = tab.request.clone();
        let origin = tab.origin();
        cx.new(|_| {
            let mut document = Document::new();
            document.request = request;
            document.origin = origin;
            document
        })
    }

    /// Point every pane at the document owning `id`, if it exists, and select
    /// the active tab's row in the Collections tree so the sidebar names where
    /// the request on screen lives.
    fn point_at(&mut self, id: TabId, cx: &mut Context<Self>) {
        if let Some(document) = self.documents.get(&id).cloned() {
            self.set_active_document(document, cx);
        }
        self.refresh_collection_scope(cx);
        self.reveal_binding(cx);
    }

    /// Select the active tab's collection row, expanding whatever it is nested
    /// in. A draft, an orphan, or the empty window has no node, so the selection
    /// stays where the user left it.
    fn reveal_binding(&mut self, cx: &mut Context<Self>) {
        let node = self
            .tabs
            .get(self.tabs.active())
            .and_then(|tab| tab.node.clone());
        if let Some(node) = node {
            self.panes
                .collections
                .update(cx, |panel, cx| panel.reveal_node(&node, cx));
        }
    }

    /// Make `document` the one the panes observe. Re-subscribes the draft writer
    /// and hands the new entity to each pane, which swaps its own observer.
    fn set_active_document(&mut self, document: Entity<Document>, cx: &mut Context<Self>) {
        let resubscribe = self.document != document || self.document_sub.is_none();
        self.document = document.clone();
        if resubscribe {
            self.document_sub =
                Some(cx.observe(&document, |this, _, cx| this.schedule_draft_save(cx)));
        }
        self.panes
            .request
            .update(cx, |pane, cx| pane.set_document(document.clone(), cx));
        self.panes
            .response
            .update(cx, |pane, cx| pane.set_document(document.clone(), cx));
        self.panes
            .history
            .update(cx, |pane, cx| pane.set_document(document.clone(), cx));
        self.panes
            .environment
            .update(cx, |pane, cx| pane.set_document(document.clone(), cx));
        self.panes
            .collections
            .update(cx, |pane, cx| pane.set_document(document, cx));
    }

    /// Hand the open-tab labels to the request pane, which draws the strip.
    /// Called after every change to the tab list, so the pane never has to
    /// guess what is open; the live label of the active tab is its own.
    pub(crate) fn push_tabs(&mut self, cx: &mut Context<Self>) {
        let tabs = self.tabs.entries(self.sending);
        let active = self.tabs.active();
        self.panes
            .request
            .update(cx, |panel, cx| panel.set_tabs(tabs, active, cx));
    }

    /// Hand the active tab's collection scope down: its variables to
    /// `Variables`, and its auth and headers to the request pane's read-only
    /// displays. A draft, an orphan, or the empty window has no owner, so it
    /// carries none.
    pub(crate) fn refresh_collection_scope(&mut self, cx: &mut Context<Self>) {
        let node = self
            .tabs
            .get(self.tabs.active())
            .and_then(|tab| tab.node.clone());
        let (vars, inherited) = match node {
            Some(node) => {
                let collections = self.panes.collections.read(cx);
                (
                    collections.variables_for(&node),
                    collections.inherited_defaults(&node),
                )
            }
            None => (Vec::new(), None),
        };
        self.variables
            .update(cx, |variables, cx| variables.set_collection(vars, cx));
        self.panes
            .request
            .update(cx, |panel, cx| panel.set_inherited(inherited, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        clipboard_url, resolve_saved_tabs, RequestStamp, SavedTab, SavedTabs, Tab, TabState,
    };
    use restbook_core::{Draft, Method, RequestDef};

    fn draft(id: i64, url: &str) -> Draft {
        Draft {
            id,
            request: RequestDef::new(Method::Get, url),
            updated_at: std::time::SystemTime::now().into(),
        }
    }

    fn bound(node: &str) -> SavedTab {
        SavedTab::Bound {
            node: node.to_owned(),
        }
    }

    /// A bound tab comes back with the node's current request, and with that
    /// request as its saved baseline so the first frame shows no unsaved dot.
    #[test]
    fn a_bound_tab_is_restored_from_its_node() {
        let saved = SavedTabs {
            entries: vec![bound("node-1")],
            active: 0,
            closed_all: false,
        };
        let request = RequestDef::new(Method::Post, "https://api.test/items");

        let (tabs, dropped, _) = resolve_saved_tabs(
            &saved,
            |node| (node == "node-1").then(|| request.clone()),
            &[],
        );

        assert_eq!(dropped, 0);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].node.as_deref(), Some("node-1"));
        assert_eq!(tabs[0].request.url, "https://api.test/items");
        assert_eq!(tabs[0].saved, Some(RequestStamp::of(&request)));
        assert!(tabs[0].draft.is_none());
    }

    /// A node deleted since the last run drops its tab rather than opening a
    /// request that no longer exists; the count is what the message reports.
    #[test]
    fn a_bound_tab_whose_node_is_gone_is_dropped() {
        let saved = SavedTabs {
            entries: vec![bound("node-1")],
            active: 0,
            closed_all: false,
        };

        let (tabs, dropped, _) = resolve_saved_tabs(&saved, |_| None, &[]);

        assert!(tabs.is_empty());
        assert_eq!(dropped, 1);
    }

    /// An unsaved tab comes back from its store row, keeping that row's id so
    /// the next draft write updates it instead of inserting a second one.
    #[test]
    fn a_draft_tab_is_restored_from_its_row() {
        let saved = SavedTabs {
            entries: vec![SavedTab::Draft { id: 7 }],
            active: 0,
            closed_all: false,
        };
        let drafts = vec![draft(7, "https://api.test/draft")];

        let (tabs, dropped, _) = resolve_saved_tabs(&saved, |_| None, &drafts);

        assert_eq!(dropped, 0);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].draft, Some(7));
        assert_eq!(tabs[0].request.url, "https://api.test/draft");
    }

    /// The row can be gone (deleted from the sidebar) while the tab is still in
    /// the layout file, so the draft tab is dropped like a missing node.
    #[test]
    fn a_draft_tab_whose_row_is_gone_is_dropped() {
        let saved = SavedTabs {
            entries: vec![SavedTab::Draft { id: 8 }],
            active: 0,
            closed_all: false,
        };

        let (tabs, dropped, _) =
            resolve_saved_tabs(&saved, |_| None, &[draft(7, "https://api.test")]);

        assert!(tabs.is_empty());
        assert_eq!(dropped, 1);
    }

    /// Survivors keep the saved order even when a tab in the middle is dropped,
    /// and the active index follows the same request rather than the old slot.
    #[test]
    fn restore_keeps_order_and_the_same_active_tab() {
        let saved = SavedTabs {
            entries: vec![
                bound("a"),
                SavedTab::Draft { id: 2 },
                bound("gone"),
                bound("b"),
            ],
            active: 3,
            closed_all: false,
        };
        let drafts = vec![draft(2, "https://api.test/draft")];

        let (tabs, dropped, active) = resolve_saved_tabs(
            &saved,
            |node| {
                (node != "gone")
                    .then(|| RequestDef::new(Method::Get, format!("https://api.test/{node}")))
            },
            &drafts,
        );

        assert_eq!(dropped, 1);
        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs[0].node.as_deref(), Some("a"));
        assert_eq!(tabs[1].draft, Some(2));
        assert_eq!(tabs[2].node.as_deref(), Some("b"));
        assert_eq!(
            active, 2,
            "the active tab is the same request, not the same slot"
        );
    }

    /// A duplicate shares the content but never the identity or the persistence
    /// binding: Save on the copy must create a new node, and the draft writer
    /// must give it a row of its own.
    #[test]
    fn a_duplicate_gets_its_own_id_and_no_draft() {
        let mut source = Tab::blank();
        source.request = RequestDef::new(Method::Post, "https://api.test/items");
        source.node = Some("node-1".to_owned());
        source.draft = Some(7);

        let copy = source.copy_of();

        assert_ne!(copy.id, source.id);
        assert_eq!(copy.request.url, source.request.url);
        assert!(copy.node.is_none(), "a copy is unbound, not an overwrite");
        assert!(copy.draft.is_none(), "a copy starts without a draft row");
    }

    /// A reopened tab is a new tab holding the old content: the id it had may
    /// be reserved by an in-flight send, and its draft row was deleted on close.
    #[test]
    fn a_reopened_tab_gets_a_new_id_and_no_draft() {
        let mut closed = Tab::blank();
        closed.request = RequestDef::new(Method::Get, "https://api.test");
        closed.draft = Some(3);
        let old_id = closed.id;

        let reopened = closed.reopened();

        assert_ne!(reopened.id, old_id);
        assert!(reopened.draft.is_none());
        assert_eq!(reopened.request.url, "https://api.test");
    }

    /// Tabs are addressed by id, so a close or a reorder cannot hand a verb to
    /// whichever tab happened to slide into the old index.
    #[test]
    fn tab_state_finds_a_tab_by_id_after_a_move() {
        let mut state = TabState::new(vec![Tab::blank(), Tab::blank(), Tab::blank()]);
        let moved = state[0].id;

        state.swap(0, 2);

        assert_eq!(state.position(moved), Some(2));
        assert_eq!(state.active_id(), Some(state[0].id));

        state.set_active(2);
        assert_eq!(state.active_id(), Some(moved));
    }

    /// The strip's projection carries every field the panel draws, so a new one
    /// cannot be added to `Tab` without passing through `entry`.
    #[test]
    fn entries_project_each_tab_in_order() {
        let mut first = Tab::blank();
        first.request = RequestDef::new(Method::Put, "https://api.test/a");
        let second = Tab::blank();
        let first_id = first.id;
        let second_id = second.id;

        let state = TabState::new(vec![first, second]);
        let entries = state.entries(None);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, first_id);
        assert_eq!(entries[1].id, second_id);
        assert_eq!(entries[0].method, Method::Put);
        assert_eq!(&*entries[0].url, "https://api.test/a");

        // The spinner follows the global send slot, so only the sending tab's
        // entry carries `pending`.
        let sending = state.entries(Some(second_id));
        assert!(!sending[0].pending);
        assert!(sending[1].pending);
    }

    #[test]
    fn a_blank_clipboard_yields_no_url() {
        assert!(clipboard_url("   \n\t\n").is_none());
    }

    #[test]
    fn the_first_non_empty_line_is_the_url() {
        assert_eq!(
            clipboard_url("\n  https://api.test/x  \nbody"),
            Some("https://api.test/x"),
        );
    }
}
