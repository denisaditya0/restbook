//! The collection tree: every edit and query the Collections pane performs.
//!
//! These are plain functions over `&[Collection]` — no `App`, no `Window`, no
//! entity — so a tree change is decided here and persisted by the pane. They
//! live beside the pane rather than inside it so the file that draws the tree
//! and the file that changes it can be read apart.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use restbook_core::{Collection, CollectionItem, Method, SavedRequest};

use super::{Selection, SortOrder};

/// The rows a search shows, as index paths into `collections`.
///
/// Each inner vec is `[collection_index, item_index, …]` in display order. A
/// request matches on name or stored URL and any ancestor of a match stays
/// visible; a matching collection or folder keeps its whole subtree. Paths, not
/// copies, keep typing in the filter from deep-cloning every match.
pub(super) fn filter_paths(
    collections: &[Collection],
    query: &str,
    order: SortOrder,
) -> Vec<Vec<usize>> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut roots: Vec<usize> = (0..collections.len()).collect();
    if order != SortOrder::Manual {
        roots.sort_by(|&a, &b| order.compare(&collections[a].name, &collections[b].name));
    }
    let mut paths = Vec::new();
    for index in roots {
        push_collection(&collections[index], index, &needle, order, &mut paths);
    }
    paths
}

fn push_collection(
    collection: &Collection,
    index: usize,
    needle: &str,
    order: SortOrder,
    paths: &mut Vec<Vec<usize>>,
) {
    let whole = contains(&collection.name, needle);
    if !whole
        && !collection
            .items
            .iter()
            .any(|each| item_visible(each, needle))
    {
        return;
    }
    paths.push(vec![index]);
    let mut prefix = vec![index];
    if whole {
        push_all(&collection.items, &mut prefix, order, paths);
    } else {
        push_matches(&collection.items, &mut prefix, needle, order, paths);
    }
}

/// Whether a row keeps itself: a request on name/URL, a folder on its own name
/// or on any descendant that matches.
fn item_visible(item: &CollectionItem, needle: &str) -> bool {
    match item {
        CollectionItem::Request(request) => {
            contains(&request.name, needle) || contains(&request.request.url, needle)
        }
        CollectionItem::Folder(folder) => {
            contains(&folder.name, needle)
                || folder.items.iter().any(|each| item_visible(each, needle))
        }
    }
}

/// Walk a filtered level: keep only the matching rows and their ancestors, in
/// display order.
fn push_matches(
    items: &[CollectionItem],
    prefix: &mut Vec<usize>,
    needle: &str,
    order: SortOrder,
    paths: &mut Vec<Vec<usize>>,
) {
    let mut visible: Vec<usize> = (0..items.len())
        .filter(|&index| item_visible(&items[index], needle))
        .collect();
    sort_indices(&mut visible, items, order);
    for index in visible {
        prefix.push(index);
        paths.push(prefix.clone());
        if let CollectionItem::Folder(folder) = &items[index] {
            if contains(&folder.name, needle) {
                push_all(&folder.items, prefix, order, paths);
            } else {
                push_matches(&folder.items, prefix, needle, order, paths);
            }
        }
        prefix.pop();
    }
}

/// Walk a whole subtree, every row visible, in display order.
fn push_all(
    items: &[CollectionItem],
    prefix: &mut Vec<usize>,
    order: SortOrder,
    paths: &mut Vec<Vec<usize>>,
) {
    let mut indices: Vec<usize> = (0..items.len()).collect();
    sort_indices(&mut indices, items, order);
    for index in indices {
        prefix.push(index);
        paths.push(prefix.clone());
        if let CollectionItem::Folder(folder) = &items[index] {
            push_all(&folder.items, prefix, order, paths);
        }
        prefix.pop();
    }
}

fn sort_indices(indices: &mut [usize], items: &[CollectionItem], order: SortOrder) {
    if order != SortOrder::Manual {
        indices.sort_by(|&a, &b| order.compare(item_name(&items[a]), item_name(&items[b])));
    }
}

fn contains(haystack: &str, needle: &str) -> bool {
    haystack.to_ascii_lowercase().contains(needle)
}

pub(super) fn item_name(item: &CollectionItem) -> &str {
    match item {
        CollectionItem::Folder(folder) => &folder.name,
        CollectionItem::Request(request) => &request.name,
    }
}

/// Insert `item` under the container named by `destination`. An unknown
/// collection or folder path leaves the tree untouched.
pub(super) fn insert_into(
    collections: &mut [Collection],
    destination: &Destination,
    item: CollectionItem,
) -> bool {
    let Some(collection) = collections
        .iter_mut()
        .find(|each| each.id == destination.collection)
    else {
        return false;
    };
    insert_in_items(&mut collection.items, &destination.folders, item)
}

fn insert_in_items(items: &mut Vec<CollectionItem>, path: &[String], item: CollectionItem) -> bool {
    match path.split_first() {
        None => {
            items.push(item);
            true
        }
        Some((folder_id, rest)) => {
            for existing in items.iter_mut() {
                if let CollectionItem::Folder(folder) = existing {
                    if &folder.id == folder_id {
                        return insert_in_items(&mut folder.items, rest, item);
                    }
                }
            }
            false
        }
    }
}

/// What an import into an existing place changed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ImportOutcome {
    /// The collection that was written, so the caller can save it back.
    pub(crate) collection: String,
    /// Requests actually added, folders counted through.
    pub(crate) added: usize,
    /// Names skipped because a request with that name was already there.
    pub(crate) duplicates: Vec<String>,
}

/// Append `incoming` under `target`, giving every arriving node a fresh id.
///
/// File ids can collide, so nothing keeps its id. A request named like an
/// existing direct child of `target` is skipped and reported, never overwritten;
/// folders are always inserted new, so two `Auth` folders can coexist where two
/// `GET /users` requests cannot. `None` when the target path is gone.
pub(crate) fn import_into(
    collections: &mut [Collection],
    target: &Destination,
    incoming: Vec<CollectionItem>,
) -> Option<ImportOutcome> {
    let selection = Selection {
        collection: Some(target.collection.clone()),
        folders: target.folders.clone(),
        request: None,
    };
    let collection_id = collections
        .iter()
        .find(|each| each.id == target.collection)?
        .id
        .clone();
    let items = resolve_selection_mut(collections, &selection)?;

    let mut names = items
        .iter()
        .filter_map(|item| match item {
            CollectionItem::Request(request) => Some(request.name.clone()),
            CollectionItem::Folder(_) => None,
        })
        .collect::<Vec<_>>();
    let mut added = 0;
    let mut duplicates = Vec::new();
    for item in incoming {
        match item {
            CollectionItem::Request(request) => {
                if names.contains(&request.name) {
                    duplicates.push(request.name);
                    continue;
                }
                names.push(request.name.clone());
                added += 1;
                items.push(reid(CollectionItem::Request(request)));
            }
            item @ CollectionItem::Folder(_) => {
                added += count_requests(&item);
                items.push(reid(item));
            }
        }
    }

    Some(ImportOutcome {
        collection: collection_id,
        added,
        duplicates,
    })
}

fn count_requests(item: &CollectionItem) -> usize {
    match item {
        CollectionItem::Request(_) => 1,
        CollectionItem::Folder(folder) => folder.items.iter().map(count_requests).sum(),
    }
}

/// Replace every id in `item`, recursively, with one this process just made.
fn reid(item: CollectionItem) -> CollectionItem {
    match item {
        CollectionItem::Request(mut request) => {
            request.id = fresh_id();
            CollectionItem::Request(request)
        }
        CollectionItem::Folder(mut folder) => {
            folder.id = fresh_id();
            folder.items = folder.items.into_iter().map(reid).collect();
            CollectionItem::Folder(folder)
        }
    }
}

/// A process-unique id, the same shape the pane's `new_id` gives a new node:
/// a timestamp plus a counter is enough, because the stores key on the string.
fn fresh_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{count:x}")
}

/// Remove the item with `id` from anywhere in the tree, and report which
/// collection it was removed from so that collection can be re-saved.
pub(super) fn remove_item(collections: &mut [Collection], id: &str) -> Option<String> {
    take_item(collections, id).map(|(collection, _)| collection)
}

/// The item with `id`, removed from the tree, and the collection it came out of.
fn take_item(collections: &mut [Collection], id: &str) -> Option<(String, CollectionItem)> {
    for collection in collections.iter_mut() {
        if let Some(item) = take_from_items(&mut collection.items, id) {
            return Some((collection.id.clone(), item));
        }
    }
    None
}

fn take_from_items(items: &mut Vec<CollectionItem>, id: &str) -> Option<CollectionItem> {
    if let Some(index) = items.iter().position(|item| item_id(item) == id) {
        return Some(items.remove(index));
    }
    for item in items.iter_mut() {
        if let CollectionItem::Folder(folder) = item {
            if let Some(found) = take_from_items(&mut folder.items, id) {
                return Some(found);
            }
        }
    }
    None
}

/// Move the item with `id` one place up or down among its siblings, and report
/// the collection it moved in. A move at either end does nothing.
pub(super) fn move_item(collections: &mut [Collection], id: &str, up: bool) -> Option<String> {
    for collection in collections.iter_mut() {
        if move_in_items(&mut collection.items, id, up) {
            return Some(collection.id.clone());
        }
    }
    None
}

/// Whether [`move_item`] would actually move `id` one place up or down, asked
/// by the row menu so a verb that cannot do anything is shown disabled.
pub(super) fn has_move_target(collections: &[Collection], id: &str, up: bool) -> bool {
    collections
        .iter()
        .any(|collection| has_move_target_in(&collection.items, id, up))
}

fn has_move_target_in(items: &[CollectionItem], id: &str, up: bool) -> bool {
    if let Some(index) = items.iter().position(|item| item_id(item) == id) {
        return if up {
            index > 0
        } else {
            index + 1 < items.len()
        };
    }
    items.iter().any(|item| match item {
        CollectionItem::Folder(folder) => has_move_target_in(&folder.items, id, up),
        CollectionItem::Request(_) => false,
    })
}

fn move_in_items(items: &mut [CollectionItem], id: &str, up: bool) -> bool {
    if let Some(index) = items.iter().position(|item| item_id(item) == id) {
        let target = if up {
            index.checked_sub(1)
        } else {
            Some(index + 1).filter(|next| *next < items.len())
        };
        return match target {
            Some(target) => {
                items.swap(index, target);
                true
            }
            None => false,
        };
    }
    for item in items.iter_mut() {
        if let CollectionItem::Folder(folder) = item {
            if move_in_items(&mut folder.items, id, up) {
                return true;
            }
        }
    }
    false
}

/// Rename a collection, a folder, or a saved request. A request's stored name is
/// what the tree shows, so it can be given a readable one without touching the
/// request itself — the method the row paints still comes from the request.
pub(super) fn rename_item(collections: &mut [Collection], id: &str, name: &str) -> bool {
    for collection in collections.iter_mut() {
        if collection.id == id {
            collection.name = name.to_owned();
            return true;
        }
        if rename_in_items(&mut collection.items, id, name) {
            return true;
        }
    }
    false
}

fn rename_in_items(items: &mut [CollectionItem], id: &str, name: &str) -> bool {
    for item in items.iter_mut() {
        match item {
            CollectionItem::Request(request) if request.id == id => {
                request.name = name.to_owned();
                return true;
            }
            CollectionItem::Folder(folder) => {
                if folder.id == id {
                    folder.name = name.to_owned();
                    return true;
                }
                if rename_in_items(&mut folder.items, id, name) {
                    return true;
                }
            }
            CollectionItem::Request(_) => {}
        }
    }
    false
}

/// The destination a saved request currently lives in, for prefilling a move.
pub(crate) fn destination_of(collections: &[Collection], id: &str) -> Option<Destination> {
    for collection in collections {
        let mut folders = Vec::new();
        if path_to_request(&collection.items, id, &mut folders) {
            return Some(Destination {
                collection: collection.id.clone(),
                folders,
            });
        }
    }
    None
}

fn path_to_request(items: &[CollectionItem], id: &str, folders: &mut Vec<String>) -> bool {
    for item in items {
        match item {
            CollectionItem::Request(request) if request.id == id => return true,
            CollectionItem::Request(_) => {}
            CollectionItem::Folder(folder) => {
                folders.push(folder.id.clone());
                if path_to_request(&folder.items, id, folders) {
                    return true;
                }
                folders.pop();
            }
        }
    }
    false
}

/// Move a saved request to another collection or folder.
///
/// Returns the ids that changed, target first — also the write order, so a failure
/// between the two writes leaves the request in two places, not none. `None` means
/// nothing changed: an unknown request or target, or a move into the same folder.
pub(crate) fn move_to(
    collections: &mut [Collection],
    id: &str,
    target: &Destination,
) -> Option<(String, String)> {
    let current = destination_of(collections, id)?;
    if &current == target {
        return None;
    }
    // Prove the target before taking anything out of the tree, so a stale id
    // cannot turn a move into a delete.
    resolve_destination(collections, target)?;

    let (source, item) = take_item(collections, id)?;
    if !insert_into(collections, target, item.clone()) {
        // Unreachable after the check above, but a lost request would be the
        // worst outcome of a bug here, so put it back rather than trust it.
        insert_into(collections, &current, item);
        return None;
    }
    Some((target.collection.clone(), source))
}

fn resolve_destination<'a>(
    collections: &'a [Collection],
    target: &Destination,
) -> Option<&'a [CollectionItem]> {
    resolve_selection(
        collections,
        &Selection {
            collection: Some(target.collection.clone()),
            folders: target.folders.clone(),
            request: None,
        },
    )
}

/// The items addressed by a selection's folder path, or `None` when the
/// collection or any folder on the path is missing.
pub(super) fn resolve_selection<'a>(
    collections: &'a [Collection],
    selection: &Selection,
) -> Option<&'a [CollectionItem]> {
    let collection = collections
        .iter()
        .find(|each| Some(each.id.as_str()) == selection.collection.as_deref())?;
    let mut items: &[CollectionItem] = &collection.items;
    for folder_id in &selection.folders {
        let folder = items.iter().find_map(|item| match item {
            CollectionItem::Folder(folder) if &folder.id == folder_id => Some(folder),
            _ => None,
        })?;
        items = &folder.items;
    }
    Some(items)
}

/// The mutable items addressed by a selection's folder path, for an edit that
/// appends rather than replaces.
fn resolve_selection_mut<'a>(
    collections: &'a mut [Collection],
    selection: &Selection,
) -> Option<&'a mut Vec<CollectionItem>> {
    let collection = collections
        .iter_mut()
        .find(|each| Some(each.id.as_str()) == selection.collection.as_deref())?;
    resolve_items_mut(&mut collection.items, &selection.folders)
}

fn resolve_items_mut<'a>(
    items: &'a mut Vec<CollectionItem>,
    path: &[String],
) -> Option<&'a mut Vec<CollectionItem>> {
    match path.split_first() {
        None => Some(items),
        Some((folder_id, rest)) => {
            for item in items.iter_mut() {
                if let CollectionItem::Folder(folder) = item {
                    if &folder.id == folder_id {
                        return resolve_items_mut(&mut folder.items, rest);
                    }
                }
            }
            None
        }
    }
}

/// The id of the top-level collection that holds `id`, whether `id` is the
/// collection itself or a folder or request nested inside it.
pub(super) fn owner_collection(collections: &[Collection], id: &str) -> Option<String> {
    collections.iter().find_map(|collection| {
        if collection.id == id || find_in_items(&collection.items, id).is_some() {
            Some(collection.id.clone())
        } else {
            None
        }
    })
}

/// The name of a collection or folder with `id`, for prefilling a rename.
pub(super) fn node_name(collections: &[Collection], id: &str) -> Option<String> {
    if let Some(collection) = collections.iter().find(|each| each.id == id) {
        return Some(collection.name.clone());
    }
    find_in_items_all(collections, id).map(|item| match item {
        CollectionItem::Folder(folder) => folder.name.clone(),
        CollectionItem::Request(request) => request.name.clone(),
    })
}

/// Find an item anywhere in the tree by id.
fn find_in_items<'a>(items: &'a [CollectionItem], id: &str) -> Option<&'a CollectionItem> {
    for item in items {
        if item_id(item) == id {
            return Some(item);
        }
        if let CollectionItem::Folder(folder) = item {
            if let Some(found) = find_in_items(&folder.items, id) {
                return Some(found);
            }
        }
    }
    None
}

pub(super) fn find_in_items_all<'a>(
    collections: &'a [Collection],
    id: &str,
) -> Option<&'a CollectionItem> {
    collections
        .iter()
        .find_map(|collection| find_in_items(&collection.items, id))
}

/// The saved request with `id`, mutably, so a re-save can replace its request
/// in place. Folders are skipped: this is only ever asked for a request node.
pub(super) fn find_saved_mut<'a>(
    collections: &'a mut [Collection],
    id: &str,
) -> Option<&'a mut SavedRequest> {
    collections
        .iter_mut()
        .find_map(|collection| find_saved_in_items_mut(&mut collection.items, id))
}

fn find_saved_in_items_mut<'a>(
    items: &'a mut [CollectionItem],
    id: &str,
) -> Option<&'a mut SavedRequest> {
    for item in items.iter_mut() {
        match item {
            CollectionItem::Request(saved) if saved.id == id => return Some(saved),
            CollectionItem::Request(_) => {}
            CollectionItem::Folder(folder) => {
                if let Some(found) = find_saved_in_items_mut(&mut folder.items, id) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// The saved request with `id`, for a reader that only needs its content.
pub(super) fn find_saved<'a>(collections: &'a [Collection], id: &str) -> Option<&'a SavedRequest> {
    collections
        .iter()
        .find_map(|collection| find_saved_in_items(&collection.items, id))
}

fn find_saved_in_items<'a>(items: &'a [CollectionItem], id: &str) -> Option<&'a SavedRequest> {
    for item in items {
        match item {
            CollectionItem::Request(saved) if saved.id == id => return Some(saved),
            CollectionItem::Request(_) => {}
            CollectionItem::Folder(folder) => {
                if let Some(found) = find_saved_in_items(&folder.items, id) {
                    return Some(found);
                }
            }
        }
    }
    None
}

fn item_id(item: &CollectionItem) -> &str {
    match item {
        CollectionItem::Folder(folder) => &folder.id,
        CollectionItem::Request(request) => &request.id,
    }
}

/// The label a saved request row shows: its saved name with a leading method
/// stripped, so the colored badge beside it is not repeated. A name that does
/// not begin with the method is shown as-is.
pub(super) fn display_name(name: &str, method: Method) -> String {
    let prefix = format!("{method} ");
    name.strip_prefix(&prefix).unwrap_or(name).to_owned()
}

/// The monogram a collection row shows: the first character of its name,
/// uppercased, or `?` for a nameless collection.
pub(super) fn initial(name: &str) -> String {
    name.chars()
        .next()
        .map(|first| first.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_owned())
}

/// Where a save lands: the top-level collection and the folder path inside it.
///
/// One value for both "what the sidebar has selected" and "what the dialog was
/// told", so a save is given a destination instead of reading a highlight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Destination {
    pub(crate) collection: String,
    pub(crate) folders: Vec<String>,
}

/// One line of the destination picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DestinationRow {
    /// The full path, `Collection › Folder`, because the menu is flat.
    pub(crate) label: String,
    pub(crate) target: Destination,
}

/// Every place a request can be saved, in tree order: each collection, then its
/// folders depth-first.
pub(crate) fn destination_rows(collections: &[Collection]) -> Vec<DestinationRow> {
    let mut rows = Vec::new();
    for collection in collections {
        let mut target = Destination {
            collection: collection.id.clone(),
            folders: Vec::new(),
        };
        rows.push(DestinationRow {
            label: collection.name.clone(),
            target: target.clone(),
        });
        push_folder_rows(&mut rows, &collection.items, &collection.name, &mut target);
    }
    rows
}

fn push_folder_rows(
    rows: &mut Vec<DestinationRow>,
    items: &[CollectionItem],
    prefix: &str,
    target: &mut Destination,
) {
    for item in items {
        let CollectionItem::Folder(folder) = item else {
            continue;
        };
        target.folders.push(folder.id.clone());
        let label = format!("{prefix} › {}", folder.name);
        rows.push(DestinationRow {
            label: label.clone(),
            target: target.clone(),
        });
        push_folder_rows(rows, &folder.items, &label, target);
        target.folders.pop();
    }
}

/// The destination a save starts on: the selection when it still names a place,
/// else the first collection, so an unselected sidebar can still save.
pub(crate) fn default_destination(
    collections: &[Collection],
    selection: &Selection,
) -> Option<Destination> {
    if selection.collection.is_some() && resolve_selection(collections, selection).is_some() {
        return Some(Destination {
            collection: selection.collection.clone()?,
            folders: selection.folders.clone(),
        });
    }
    collections.first().map(|collection| Destination {
        collection: collection.id.clone(),
        folders: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use restbook_core::{Collection, CollectionItem, Folder, Method, RequestDef, SavedRequest};

    use super::{
        default_destination, destination_of, destination_rows, display_name, filter_paths,
        find_in_items_all, has_move_target, import_into, initial, insert_into, item_id, move_item,
        move_to, node_name, owner_collection, remove_item, rename_item, resolve_selection,
        Destination, Selection, SortOrder,
    };

    fn destination(collection: &str, folders: &[&str]) -> Destination {
        Destination {
            collection: collection.to_owned(),
            folders: folders.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    fn saved(id: &str, method: Method, url: &str) -> SavedRequest {
        SavedRequest {
            id: id.to_owned(),
            name: format!("{method} {url}"),
            request: RequestDef::new(method, url),
        }
    }

    fn folder(id: &str, name: &str, items: Vec<CollectionItem>) -> CollectionItem {
        CollectionItem::Folder(Folder {
            id: id.to_owned(),
            name: name.to_owned(),
            items,
        })
    }

    fn sample() -> Vec<Collection> {
        vec![Collection {
            id: "col-1".to_owned(),
            name: "Sample".to_owned(),
            items: vec![
                folder(
                    "fold-1",
                    "Group",
                    vec![
                        CollectionItem::Request(saved(
                            "req-1",
                            Method::Get,
                            "https://example.com/a",
                        )),
                        CollectionItem::Request(saved(
                            "req-2",
                            Method::Post,
                            "https://example.com/b",
                        )),
                    ],
                ),
                CollectionItem::Request(saved("req-3", Method::Get, "https://example.com/top")),
            ],
        }]
    }

    /// A 3,000-request tree is filtered in one pass. The bound is deliberately
    /// generous because a debug build is not a benchmark, but the measured time
    /// belongs in the failure message: this is the test that keeps the `0.2.0`
    /// filter rewrite honest about the tree the app is expected to survive. The
    /// whole test, fixture included, ran in roughly 10 ms on the development
    /// machine; the 250 ms bound leaves room for a slow CI scheduler.
    #[test]
    fn a_three_thousand_request_tree_filters_without_a_stall() {
        let tree = large_tree(3_000);
        let started = std::time::Instant::now();
        let paths = filter_paths(&tree, "needle-2999", SortOrder::Manual);
        let elapsed = started.elapsed();

        // The collection stays as the match's ancestor; only the matching row is
        // kept under it, and the path points straight at it without a clone.
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], vec![0]);
        assert_eq!(paths[1], vec![0, 2999]);
        assert!(
            elapsed < std::time::Duration::from_millis(250),
            "filtering 3,000 requests took {elapsed:?}",
        );
    }

    fn large_tree(count: usize) -> Vec<Collection> {
        let items = (0..count)
            .map(|index| {
                CollectionItem::Request(saved(
                    &format!("req-{index}"),
                    Method::Get,
                    &format!("https://example.com/needle-{index}"),
                ))
            })
            .collect();
        vec![Collection {
            id: "col-large".to_owned(),
            name: "Large".to_owned(),
            items,
        }]
    }

    fn into_folder(id: &str) -> Selection {
        Selection {
            collection: Some("col-1".to_owned()),
            folders: vec![id.to_owned()],
            request: None,
        }
    }

    #[test]
    fn resolve_selection_walks_the_folder_path() {
        let tree = sample();
        let items = resolve_selection(&tree, &into_folder("fold-1")).unwrap();
        assert_eq!(items.len(), 2);

        let root = Selection::collection("col-1");
        assert_eq!(resolve_selection(&tree, &root).unwrap().len(), 2);

        let missing = into_folder("nope");
        assert!(resolve_selection(&tree, &missing).is_none());
        assert!(resolve_selection(&tree, &Selection::default()).is_none());
    }

    #[test]
    fn insert_into_a_folder_lands_the_request_there() {
        let mut tree = sample();
        let target = destination("col-1", &["fold-1"]);
        assert!(insert_into(
            &mut tree,
            &target,
            CollectionItem::Request(saved("req-4", Method::Get, "https://example.com/new"))
        ));

        let items = resolve_selection(&tree, &into_folder("fold-1")).unwrap();
        assert_eq!(items.len(), 3);
        assert!(matches!(&items[2], CollectionItem::Request(r) if r.id == "req-4"));
    }

    #[test]
    fn insert_into_the_collection_root_lands_at_the_top() {
        let mut tree = sample();
        let target = destination("col-1", &[]);
        assert!(insert_into(
            &mut tree,
            &target,
            CollectionItem::Request(saved("req-5", Method::Put, "https://example.com/root"))
        ));
        assert_eq!(tree[0].items.len(), 3);
        assert!(matches!(&tree[0].items[2], CollectionItem::Request(r) if r.id == "req-5"));
    }

    #[test]
    fn insert_into_an_unknown_target_is_a_no_op() {
        let mut tree = sample();
        let unknown_folder = destination("col-1", &["nope"]);
        assert!(!insert_into(
            &mut tree,
            &unknown_folder,
            CollectionItem::Request(saved("req-6", Method::Get, "https://example.com/x"))
        ));
        assert!(!insert_into(
            &mut tree,
            &destination("nope", &[]),
            CollectionItem::Request(saved("req-7", Method::Get, "https://example.com/y"))
        ));
        assert_eq!(tree[0].items.len(), 2);
    }

    #[test]
    fn import_into_a_folder_appends_and_gives_every_node_a_fresh_id() {
        let mut tree = sample();
        let target = destination("col-1", &["fold-1"]);
        let outcome = import_into(
            &mut tree,
            &target,
            vec![
                CollectionItem::Request(saved("req-1", Method::Get, "https://example.com/c")),
                // The same id the tree already uses: a file can carry one, and
                // keeping it would make two nodes indistinguishable.
                folder(
                    "fold-1",
                    "Nested",
                    vec![CollectionItem::Request(saved(
                        "req-1",
                        Method::Get,
                        "https://example.com/d",
                    ))],
                ),
            ],
        )
        .unwrap();

        assert_eq!(outcome.collection, "col-1");
        assert_eq!(outcome.added, 2);
        assert!(outcome.duplicates.is_empty());

        let items = resolve_selection(&tree, &into_folder("fold-1")).unwrap();
        assert_eq!(items.len(), 4);
        let ids = items.iter().map(item_id).collect::<Vec<_>>();
        assert_eq!(
            ids[0], "req-1",
            "the rows that were already there keep theirs"
        );
        assert!(ids[2..].iter().all(|id| !matches!(*id, "req-1" | "fold-1")));
    }

    #[test]
    fn import_into_skips_a_same_name_direct_child_and_reports_it() {
        let mut tree = sample();
        let target = destination("col-1", &["fold-1"]);
        let outcome = import_into(
            &mut tree,
            &target,
            vec![
                CollectionItem::Request(saved("req-1", Method::Get, "https://example.com/a")),
                CollectionItem::Request(saved("req-9", Method::Get, "https://example.com/new")),
            ],
        )
        .unwrap();

        assert_eq!(outcome.added, 1);
        assert_eq!(outcome.duplicates, ["GET https://example.com/a"]);
        assert_eq!(
            resolve_selection(&tree, &into_folder("fold-1"))
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn import_into_an_unknown_target_writes_nothing() {
        let mut tree = sample();
        let incoming = vec![CollectionItem::Request(saved(
            "req-8",
            Method::Get,
            "https://example.com/x",
        ))];

        assert!(import_into(
            &mut tree,
            &destination("col-1", &["nope"]),
            incoming.clone()
        )
        .is_none());
        assert!(import_into(&mut tree, &destination("nope", &[]), incoming).is_none());
        assert_eq!(tree[0].items.len(), 2);
        assert_eq!(
            resolve_selection(&tree, &into_folder("fold-1"))
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn remove_item_reports_the_owning_collection() {
        let mut tree = sample();
        assert_eq!(remove_item(&mut tree, "req-2").as_deref(), Some("col-1"));

        let items = resolve_selection(&tree, &into_folder("fold-1")).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(remove_item(&mut tree, "missing"), None);
    }

    #[test]
    fn remove_item_takes_whole_folders_too() {
        let mut tree = sample();
        assert_eq!(remove_item(&mut tree, "fold-1").as_deref(), Some("col-1"));
        assert_eq!(tree[0].items.len(), 1);
        assert!(matches!(&tree[0].items[0], CollectionItem::Request(r) if r.id == "req-3"));
    }

    #[test]
    fn move_item_swaps_siblings_and_stops_at_the_ends() {
        let mut tree = sample();
        assert_eq!(
            move_item(&mut tree, "req-2", true).as_deref(),
            Some("col-1")
        );

        let items = resolve_selection(&tree, &into_folder("fold-1")).unwrap();
        assert!(matches!(&items[0], CollectionItem::Request(r) if r.id == "req-2"));
        assert!(matches!(&items[1], CollectionItem::Request(r) if r.id == "req-1"));

        // The first sibling cannot move up, the last cannot move down.
        assert_eq!(move_item(&mut tree, "req-2", true), None);
        assert_eq!(move_item(&mut tree, "req-1", false), None);
        assert_eq!(move_item(&mut tree, "missing", true), None);
    }

    #[test]
    fn has_move_target_matches_what_move_item_does() {
        let mut tree = sample();

        // A collection is at the top level, so neither verb applies to it.
        assert!(!has_move_target(&tree, "col-1", true));
        assert!(!has_move_target(&tree, "col-1", false));

        // Inside the folder, the first sibling cannot move up, the last cannot
        // move down.
        assert!(!has_move_target(&tree, "req-1", true));
        assert!(has_move_target(&tree, "req-1", false));
        assert!(has_move_target(&tree, "req-2", true));
        assert!(!has_move_target(&tree, "req-2", false));

        // At the collection root the folder comes first, then the request.
        assert!(!has_move_target(&tree, "fold-1", true));
        assert!(has_move_target(&tree, "fold-1", false));
        assert!(has_move_target(&tree, "req-3", true));
        assert!(!has_move_target(&tree, "req-3", false));

        // An only child sits at both ends at once.
        let collection = tree.iter_mut().find(|each| each.id == "col-1").unwrap();
        collection.items.retain(|item| item_id(item) != "req-3");
        assert!(!has_move_target(&tree, "fold-1", false));

        // An unknown id is not movable either.
        assert!(!has_move_target(&tree, "missing", true));
        assert!(!has_move_target(&tree, "missing", false));
    }

    #[test]
    fn move_item_orders_top_level_items() {
        let mut tree = sample();
        assert_eq!(
            move_item(&mut tree, "req-3", true).as_deref(),
            Some("col-1")
        );
        assert!(matches!(&tree[0].items[0], CollectionItem::Request(r) if r.id == "req-3"));
        assert!(matches!(&tree[0].items[1], CollectionItem::Folder(f) if f.id == "fold-1"));
    }

    #[test]
    fn filter_keeps_ancestors_of_a_match() {
        let tree = sample();
        // Only `req-2` matches "b"; its collection and folder survive as
        // ancestors above it.
        assert_eq!(
            filter_paths(&tree, "b", SortOrder::Manual),
            vec![vec![0], vec![0, 0], vec![0, 0, 1]],
        );
    }

    #[test]
    fn filter_matches_a_request_url() {
        let tree = sample();
        // `/top` is only in `req-3`, the collection's second child.
        assert_eq!(
            filter_paths(&tree, "/top", SortOrder::Manual),
            vec![vec![0], vec![0, 1]],
        );
    }

    #[test]
    fn filter_keeps_a_matching_collections_whole_subtree() {
        let tree = sample();
        // "Sample" matches the collection, so every row beneath it is visible.
        assert_eq!(
            filter_paths(&tree, "sam", SortOrder::Manual),
            vec![
                vec![0],
                vec![0, 0],
                vec![0, 0, 0],
                vec![0, 0, 1],
                vec![0, 1]
            ],
        );
    }

    #[test]
    fn filter_orders_visible_rows_by_name() {
        // Two matching siblings, stored "two" then "one", so a name sort flips
        // them: the filter must order rows, not just pick them.
        let tree = vec![Collection {
            id: "c".to_owned(),
            name: "C".to_owned(),
            items: vec![
                CollectionItem::Request(saved("r-two", Method::Get, "https://x/two")),
                CollectionItem::Request(saved("r-one", Method::Get, "https://x/one")),
            ],
        }];
        assert_eq!(
            filter_paths(&tree, "x/", SortOrder::Manual),
            vec![vec![0], vec![0, 0], vec![0, 1]],
        );
        assert_eq!(
            filter_paths(&tree, "x/", SortOrder::NameAsc),
            vec![vec![0], vec![0, 1], vec![0, 0]],
        );
    }

    #[test]
    fn filter_returns_nothing_when_nothing_matches() {
        let tree = sample();
        assert!(filter_paths(&tree, "zzz", SortOrder::Manual).is_empty());
    }

    #[test]
    fn an_empty_query_yields_no_paths() {
        let tree = sample();
        assert!(filter_paths(&tree, "", SortOrder::Manual).is_empty());
        assert!(filter_paths(&tree, "  ", SortOrder::Manual).is_empty());
    }

    #[test]
    fn rename_item_renames_collections_folders_and_requests() {
        let mut tree = sample();
        assert!(rename_item(&mut tree, "fold-1", "Renamed"));
        let CollectionItem::Folder(fold) = &tree[0].items[0] else {
            panic!("expected a folder");
        };
        assert_eq!(fold.name, "Renamed");

        assert!(rename_item(&mut tree, "col-1", "Other"));
        assert_eq!(tree[0].name, "Other");

        // A request's name is only a label: renaming it must not touch the
        // request the row paints from.
        assert!(rename_item(&mut tree, "req-1", "Fetch a thing"));
        let CollectionItem::Request(request) = find_in_items_all(&tree, "req-1").unwrap() else {
            panic!("expected a request");
        };
        assert_eq!(request.name, "Fetch a thing");
        assert_eq!(request.request.url, "https://example.com/a");

        assert!(!rename_item(&mut tree, "missing", "x"));
    }

    #[test]
    fn owner_collection_finds_the_containing_collection() {
        let tree = sample();
        assert_eq!(owner_collection(&tree, "req-2").as_deref(), Some("col-1"));
        assert_eq!(owner_collection(&tree, "fold-1").as_deref(), Some("col-1"));
        assert_eq!(owner_collection(&tree, "col-1").as_deref(), Some("col-1"));
        assert_eq!(owner_collection(&tree, "missing"), None);
    }

    #[test]
    fn node_name_reads_every_kind_of_node() {
        let tree = sample();
        assert_eq!(node_name(&tree, "col-1").as_deref(), Some("Sample"));
        assert_eq!(node_name(&tree, "fold-1").as_deref(), Some("Group"));
        // Requests are renameable, so their name is prefillable too.
        assert_eq!(
            node_name(&tree, "req-1").as_deref(),
            Some("GET https://example.com/a")
        );
        assert_eq!(node_name(&tree, "missing"), None);
    }

    #[test]
    fn find_in_items_all_locates_nested_requests() {
        let tree = sample();
        assert!(find_in_items_all(&tree, "req-1").is_some());
        assert!(find_in_items_all(&tree, "missing").is_none());
    }

    #[test]
    fn display_name_strips_a_leading_method() {
        assert_eq!(
            display_name("GET https://example.com", Method::Get),
            "https://example.com"
        );
        assert_eq!(display_name("List things", Method::Get), "List things");
        assert_eq!(display_name("POST", Method::Get), "POST");
    }

    #[test]
    fn a_collection_initial_is_one_uppercased_letter() {
        assert_eq!(initial("acme"), "A");
        assert_eq!(initial("  spaced"), " ");
        assert_eq!(initial(""), "?");
    }

    /// A request's home, for prefilling a move or deciding one would be a no-op.
    #[test]
    fn destination_of_reads_the_folder_path() {
        let tree = sample();
        assert_eq!(
            destination_of(&tree, "req-1"),
            Some(destination("col-1", &["fold-1"])),
        );
        assert_eq!(
            destination_of(&tree, "req-3"),
            Some(destination("col-1", &[])),
        );
        assert_eq!(destination_of(&tree, "missing"), None);
    }

    fn two_collections() -> Vec<Collection> {
        let mut tree = sample();
        tree.push(Collection {
            id: "col-2".to_owned(),
            name: "Other".to_owned(),
            items: vec![folder("fold-2", "Elsewhere", Vec::new())],
        });
        tree
    }

    /// A move reports both collections, target first: that is also the order
    /// they must be written in.
    #[test]
    fn moving_a_request_reports_the_target_then_the_source() {
        let mut tree = two_collections();
        let changed = move_to(&mut tree, "req-3", &destination("col-2", &[]));
        assert_eq!(changed, Some(("col-2".to_owned(), "col-1".to_owned())));

        assert!(find_in_items_all(&tree, "req-3").is_some());
        let source = tree.iter().find(|each| each.id == "col-1").unwrap();
        assert!(source.items.iter().all(|item| item_id(item) != "req-3"));
        let target = tree.iter().find(|each| each.id == "col-2").unwrap();
        assert!(target.items.iter().any(|item| item_id(item) == "req-3"));
    }

    #[test]
    fn moving_a_request_into_a_folder_nests_it() {
        let mut tree = two_collections();
        let target = destination("col-2", &["fold-2"]);
        assert_eq!(
            move_to(&mut tree, "req-1", &target),
            Some(("col-2".to_owned(), "col-1".to_owned())),
        );
        let items = resolve_selection(&tree, &into_folder_2("fold-2")).unwrap();
        assert!(matches!(items.first(), Some(CollectionItem::Request(r)) if r.id == "req-1"));
    }

    fn into_folder_2(id: &str) -> Selection {
        Selection {
            collection: Some("col-2".to_owned()),
            folders: vec![id.to_owned()],
            request: None,
        }
    }

    /// Moving something to the place it already is must not rewrite the tree (or
    /// report a move that did not happen).
    #[test]
    fn moving_a_request_to_its_own_folder_is_a_no_op() {
        let mut tree = sample();
        assert_eq!(
            move_to(&mut tree, "req-1", &destination("col-1", &["fold-1"])),
            None
        );
        assert_eq!(tree[0].items.len(), 2);
        assert_eq!(
            move_to(&mut tree, "req-3", &destination("col-1", &[])),
            None
        );
        assert_eq!(tree[0].items.len(), 2);
    }

    /// A target that is not there is refused before anything is taken, because a
    /// move that fails after the removal would delete the request.
    #[test]
    fn moving_to_an_unknown_target_changes_nothing() {
        let mut tree = two_collections();
        assert_eq!(move_to(&mut tree, "req-3", &destination("nope", &[])), None);
        assert_eq!(
            move_to(&mut tree, "req-3", &destination("col-2", &["nope"])),
            None
        );
        assert!(find_in_items_all(&tree, "req-3").is_some());
        assert_eq!(tree[1].items.len(), 1, "the folder is still alone");
    }

    /// The picker lists every place a save can land, and its labels name the
    /// path because the menu is flat.
    #[test]
    fn destination_rows_cover_every_collection_and_folder() {
        let rows = destination_rows(&sample());
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["Sample", "Sample › Group"]);
        assert_eq!(rows[0].target, destination("col-1", &[]));
        assert_eq!(rows[1].target, destination("col-1", &["fold-1"]));
    }

    /// Nothing selected is not nothing to save into: the dialog opens on the
    /// first collection instead of refusing.
    #[test]
    fn a_default_destination_falls_back_to_the_first_collection() {
        let tree = sample();
        assert_eq!(
            default_destination(&tree, &Selection::default()),
            Some(destination("col-1", &[])),
        );
        assert_eq!(
            default_destination(&tree, &into_folder("fold-1")),
            Some(destination("col-1", &["fold-1"])),
        );

        // A selection that no longer exists (a deleted collection) falls back
        // rather than pointing at nothing.
        let stale = Selection::collection("gone");
        assert_eq!(
            default_destination(&tree, &stale),
            Some(destination("col-1", &[])),
        );
        assert_eq!(default_destination(&[], &Selection::default()), None);
    }
}
