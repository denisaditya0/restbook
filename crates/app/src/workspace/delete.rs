//! The confirmation, the act, and the one-step undo a destructive delete
//! offers.
//!
//! The canvas owns the dialog because it owns window placement; what a delete
//! removed is captured before the pane mutates the store, and lives only as long
//! as its toast.

use gpui_kit::component::button::*;
use gpui_kit::component::*;
use gpui_kit::*;

use restbook_core::{Collection, CollectionItem, Environment, Origin, Store};

use crate::notice::Notice;
use crate::panels::collections as collections_panel;
use crate::panels::request as request_panel;
use crate::workspace::Workspace;

use super::{close_tabs_copy, TabId};
/// A destructive act the sidebar asked for, held until render can open the
/// A destructive act the sidebar asked for, held until render can open the
/// alert dialog. The workspace owns the confirmation because it owns window
/// placement; the panes only name the thing.
#[derive(Clone)]
pub(crate) enum PendingDelete {
    /// A collection, a folder, or a saved request in the tree.
    Node(collections_panel::RowTarget),
    /// An environment, by name.
    Environment(String),
    /// One stored send, by history row id.
    HistorySend(i64),
    /// Every stored send, across every request.
    AllHistory,
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
pub(crate) enum Deleted {
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
    pub(super) fn noun(&self) -> &'static str {
        match self {
            Self::Collection(_) => "Collection",
            Self::Item { noun, .. } => noun,
            Self::Environment { .. } => "Environment",
        }
    }

    pub(super) fn deleted_message(&self) -> String {
        format!("{} deleted", self.noun())
    }

    pub(super) fn restored_message(&self) -> String {
        format!("{} restored", self.noun())
    }

    /// Put the captured value back. A collection is one store row, so a
    /// folder or request is re-inserted into its current collection rather
    /// than re-saving a stale copy of everything around it.
    pub(super) fn restore(&self, store: &dyn Store) -> restbook_core::Result<()> {
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
/// Capture what a pending delete will remove, before the pane mutates the
/// store. `None` means this kind has no faithful restore (history, tabs) or
/// the value is already gone.
pub(super) fn snapshot_delete(target: &PendingDelete, store: &dyn Store) -> Option<Deleted> {
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
        | PendingDelete::Tab(_)
        | PendingDelete::Tabs { .. } => None,
    }
}

/// Whether the store actually lost the captured value. The panes report their
/// own failures, but a delete that did not land must not offer an undo that
/// would appear to resurrect something still on screen.
pub(super) fn deletion_landed(deleted: &Deleted, store: &dyn Store) -> bool {
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

impl Workspace {
    /// Confirm a destructive delete before doing it. A modal alert dialog: it can
    /// name the thing, it is what a user expects before a collection or environment
    /// is destroyed, and it cannot be triggered by a mis-aimed second click.
    pub(crate) fn open_delete_dialog(
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
        let node_deleted = matches!(&target, PendingDelete::Node(_));
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
            PendingDelete::Tab(id) => self.close_tab(id, cx),
            PendingDelete::Tabs { ids, .. } => {
                for id in ids {
                    self.close_tab(id, cx);
                }
            }
        }

        // A deleted node leaves any tab opened from it without an owner.
        if node_deleted {
            self.drop_ownerless_tabs(cx);
        }

        // The panes report a failed write themselves, so only offer an undo
        // once the store actually lost the value.
        if let Some(deleted) = deleted {
            if deletion_landed(&deleted, self.store.as_ref()) {
                self.flash_undo(deleted, cx);
            }
        }
    }

    /// Drop every tab whose collection node no longer exists to an unsent
    /// request, so it cannot save back to a dead id or claim a stale owner.
    pub(crate) fn drop_ownerless_tabs(&mut self, cx: &mut Context<Self>) {
        let dead: Vec<usize> = {
            let collections = self.panes.collections.read(cx);
            self.tabs
                .iter()
                .enumerate()
                .filter(|(_, tab)| {
                    tab.node
                        .as_deref()
                        .is_some_and(|node| collections.saved_request(node).is_none())
                })
                .map(|(index, _)| index)
                .collect()
        };
        if dead.is_empty() {
            return;
        }
        let active = self.tabs.active();
        for index in dead {
            self.tabs[index].node = None;
            // The baseline went with the node: the tab is now unfiled, and a
            // stale baseline would read it as clean against a request nothing
            // saved.
            self.tabs[index].saved = None;
            if index == active {
                let document = self.document.clone();
                document.update(cx, |document, cx| {
                    document.origin = Origin::Standalone;
                    cx.notify();
                });
            }
        }
        self.push_tabs(cx);
        self.persist_tabs(cx);
    }
}
