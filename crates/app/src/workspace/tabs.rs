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
use restbook_core::{Method, Origin, RequestDef, ResponseRecord};

use super::RequestStamp;

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
    pub(crate) error: Option<String>,
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
