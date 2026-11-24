//! The open-tab model, owned in one place.
//!
//! A [`Tab`] is what a tab holds; a [`TabEntry`] is the strip's read-only
//! projection of it. The workspace keeps the whole set in [`TabState`] and hands
//! the request pane only entries, so no tab field is maintained twice.
//!
//! A tab's identity is its [`TabId`], not its position: closing a tab shifts
//! every index after it, and a send, a draft write, or a dialog can outlive the
//! slot it started from.

use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::SharedString;
use gpui_kit::*;
use restbook_core::{Draft, Method, Origin, RequestDef, ResponseRecord};

use crate::notice::Notice;
use crate::panels::collections::Destination;

use super::chrome;
use super::delete::PendingDelete;
use super::persistence::CLOSED_TAB_LIMIT;
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

/// A tab's saved request and response. Switching tabs snapshots the document
/// into the tab being left and adopts the next one, so the panes never have to
/// observe more than one document.
#[derive(Clone)]
pub(crate) struct Tab {
    /// Stable for the life of the tab. A send outlives the index it started
    /// from, so the in-flight send is recorded against this, not a position.
    pub(crate) id: TabId,
    pub(crate) request: RequestDef,
    pub(crate) response: Option<ResponseRecord>,
    pub(crate) error: Option<restbook_core::Failure>,
    pub(crate) last_url: Option<String>,
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
            response: None,
            error: None,
            last_url: None,
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
            response: self.response.clone(),
            error: self.error.clone(),
            last_url: self.last_url.clone(),
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
            response: None,
            error: None,
            last_url: None,
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
    pub(crate) fn entry(&self) -> TabEntry {
        TabEntry {
            id: self.id,
            method: self.request.method,
            label: crate::panels::request::tab_label(&self.request),
            url: self.request.url.clone().into(),
            current: RequestStamp::of(&self.request),
            saved: self.saved.clone(),
            node: self.node.clone(),
            draft: self.draft,
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

    pub(crate) fn iter_mut(&mut self) -> std::slice::IterMut<'_, Tab> {
        self.tabs.iter_mut()
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
    pub(crate) fn entries(&self) -> Vec<TabEntry> {
        self.tabs.iter().map(Tab::entry).collect()
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

impl Workspace {
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

    /// A pasted cURL command replaced the active request. Confirm the import,
    /// then offer the first-save dialog so the request gets a name and a home
    /// instead of silently becoming another unsaved tab. A workspace with no
    /// collection at all skips the dialog: there is nowhere for it to land.
    pub(crate) fn imported(&mut self, cx: &mut Context<Self>) {
        self.notice(
            &Notice::Done("Imported a request from the clipboard.".into()),
            cx,
        );
        if self
            .panes
            .collections
            .read(cx)
            .default_destination()
            .is_none()
        {
            return;
        }
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
            if self
                .panes
                .collections
                .read(cx)
                .default_destination()
                .is_none()
            {
                self.flash(
                    chrome::FlashKind::Error,
                    "There is no collection to save into — create one in the sidebar first.",
                    cx,
                );
                return;
            }
            self.pending_save = true;
            cx.notify();
            return;
        }
        self.commit_save(node, None, None, cx);
    }

    /// Save the active request as a new node, even when the tab is already
    /// bound. The dialog opens with the bound name prefilled and `node` unset,
    /// so the save creates an entry instead of overwriting the source.
    pub(crate) fn save_active_as(&mut self, cx: &mut Context<Self>) {
        if self
            .panes
            .collections
            .read(cx)
            .default_destination()
            .is_none()
        {
            self.flash(
                chrome::FlashKind::Error,
                "There is no collection to save into — create one in the sidebar first.",
                cx,
            );
            return;
        }
        self.pending_save = true;
        cx.notify();
    }

    /// Write the open request into `node` (an existing collection node) or, with
    /// `None`, into `target` under `name`.
    ///
    /// The node the save landed on is written back onto the tab, so a tab saved
    /// for the first time is bound to it and the next Save updates rather than
    /// duplicates.
    pub(super) fn commit_save(
        &mut self,
        node: Option<String>,
        name: Option<&str>,
        target: Option<Destination>,
        cx: &mut Context<Self>,
    ) -> bool {
        let landed = self.panes.collections.update(cx, |panel, cx| {
            panel.save_current(node.as_deref(), name, target.as_ref(), cx)
        });
        let Some(node) = landed else {
            // Nothing was saved, and the pane could not say so on this path.
            self.flash(
                chrome::FlashKind::Error,
                "Couldn't save — that collection is no longer there.",
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
    /// A node that is already open is brought forward rather than re-adopted,
    /// so the tab on screen is never overwritten. Otherwise the request gets a
    /// tab of its own, except when the window is sitting on its lone untouched
    /// placeholder: taking that tab over keeps startup from stranding an empty
    /// request beside the one that was just opened. A tab the user made with
    /// New tab is theirs to keep, even before its first edit.
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

    pub(crate) fn new_tab(&mut self, cx: &mut Context<Self>) {
        // A new tab is a blank page, not a copy of whatever is on screen: the
        // active request may be unsaved, and cloning it made the new tab start
        // with work the user did not ask to carry over. Saving it creates a new
        // entry, while the tab left behind keeps its own edits.
        self.flush_leaving_draft(cx);
        let index = self.tabs.push(Tab::blank());
        self.tabs.set_active(index);
        self.adopt_active(cx);
        log::debug!(target: "restbook_app::ui", "new tab: {} open", self.tabs.len());
    }

    /// Open a stored draft as a new tab. Used when the sidebar's Unsaved row is
    /// clicked for a draft that is not already open — the normal case now that
    /// the window starts on a blank tab instead of restoring every draft.
    pub(crate) fn open_draft(&mut self, draft: Draft, cx: &mut Context<Self>) {
        let id = draft.id;
        self.flush_leaving_draft(cx);
        let index = self.tabs.push(Tab::from_draft(draft));
        self.tabs.set_active(index);
        self.adopt_active(cx);
        log::info!(
            target: "restbook_app::ui",
            "open draft: {} {} (id {})",
            self.tabs[index].request.method,
            crate::logging::redact_url(&self.tabs[index].request.url),
            id,
        );
    }

    /// The tab on screen, by its stable id rather than its position.
    pub(crate) fn active_tab_id(&self) -> Option<TabId> {
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
        self.tabs.insert(index + 1, copy);
        self.tabs.set_active(index + 1);
        self.adopt_active(cx);
        log::debug!(target: "restbook_app::ui", "duplicated tab {id}");
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
        log::debug!(target: "restbook_app::ui", "reopened a closed tab");
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
    pub(crate) fn snapshot_active(&mut self, cx: &App) {
        // An adoption queued for the next frame means the widgets still belong
        // to the tab being left. Folding them here would write the request on
        // screen into whichever tab is current, so two switches inside one
        // frame used to copy one tab's body onto another.
        let pending = self.panes.request.read(cx).has_pending_adoption(cx);
        let request = (!pending).then(|| self.panes.request.read(cx).current_request(cx));
        let (response, error, last_url) = {
            let document = self.document.read(cx);
            (
                document.response.clone(),
                document.error.clone(),
                document.last_url.clone(),
            )
        };
        let active = self.tabs.active();
        let Some(tab) = self.tabs.get_mut(active) else {
            return;
        };
        if let Some(request) = request {
            tab.request = request;
        }
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
    pub(crate) fn push_tabs(&mut self, cx: &mut Context<Self>) {
        let tabs = self.tabs.entries();
        let active = self.tabs.active();
        self.panes
            .request
            .update(cx, |panel, cx| panel.set_tabs(tabs, active, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::{Tab, TabState};
    use restbook_core::{Method, RequestDef};

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
        let entries = state.entries();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, first_id);
        assert_eq!(entries[1].id, second_id);
        assert_eq!(entries[0].method, Method::Put);
        assert_eq!(&*entries[0].url, "https://api.test/a");
    }
}
