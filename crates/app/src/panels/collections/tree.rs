//! The collection tree: every edit and query the Collections pane performs.
//!
//! These are plain functions over `&[Collection]` — no `App`, no `Window`, no
//! entity — so a tree change is decided here and persisted by the pane. They
//! live beside the pane rather than inside it so the file that draws the tree
//! and the file that changes it can be read apart.

use restbook_core::{Collection, CollectionItem, Folder, Method, SavedRequest};

use super::{Selection, SortOrder};

/// The tree a search shows: a saved request matches on its name or its stored
/// URL, and any ancestor of a match stays visible; a collection or folder whose
/// own name matches keeps its whole subtree. An empty query keeps everything.
pub(super) fn filter_tree(collections: &[Collection], query: &str) -> Vec<Collection> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return collections.to_vec();
    }
    collections
        .iter()
        .filter_map(|collection| filter_collection(collection, &needle))
        .collect()
}

fn filter_collection(collection: &Collection, needle: &str) -> Option<Collection> {
    if contains(&collection.name, needle) {
        return Some(collection.clone());
    }
    let items = collection
        .items
        .iter()
        .filter_map(|item| filter_item(item, needle))
        .collect::<Vec<_>>();
    if items.is_empty() {
        None
    } else {
        Some(Collection {
            id: collection.id.clone(),
            name: collection.name.clone(),
            items,
        })
    }
}

fn filter_item(item: &CollectionItem, needle: &str) -> Option<CollectionItem> {
    match item {
        CollectionItem::Request(request) => {
            let matches = contains(&request.name, needle) || contains(&request.request.url, needle);
            matches.then(|| item.clone())
        }
        CollectionItem::Folder(folder) => {
            if contains(&folder.name, needle) {
                return Some(item.clone());
            }
            let items = folder
                .items
                .iter()
                .filter_map(|each| filter_item(each, needle))
                .collect::<Vec<_>>();
            if items.is_empty() {
                None
            } else {
                Some(CollectionItem::Folder(Folder {
                    id: folder.id.clone(),
                    name: folder.name.clone(),
                    items,
                }))
            }
        }
    }
}

fn contains(haystack: &str, needle: &str) -> bool {
    haystack.to_ascii_lowercase().contains(needle)
}

/// Order a list of collection references for display without copying the tree.
/// Nested item ordering is applied where the borrow path renders them.
pub(super) fn sort_refs(collections: &mut [&Collection], order: SortOrder) {
    if order == SortOrder::Manual {
        return;
    }
    collections.sort_by(|left, right| order.compare(&left.name, &right.name));
}

/// Sort a copy of the tree for display. Every level is ordered by name; a
/// request's stored name already starts with its method, so the method groups
/// together under the same order the tree shows.
pub(super) fn sort_tree(collections: &mut [Collection], order: SortOrder) {
    if order == SortOrder::Manual {
        return;
    }
    collections.sort_by(|left, right| order.compare(&left.name, &right.name));
    for collection in collections.iter_mut() {
        sort_items(&mut collection.items, order);
    }
}

fn sort_items(items: &mut [CollectionItem], order: SortOrder) {
    items.sort_by(|left, right| order.compare(item_name(left), item_name(right)));
    for item in items.iter_mut() {
        if let CollectionItem::Folder(folder) = item {
            sort_items(&mut folder.items, order);
        }
    }
}

pub(super) fn item_name(item: &CollectionItem) -> &str {
    match item {
        CollectionItem::Folder(folder) => &folder.name,
        CollectionItem::Request(request) => &request.name,
    }
}

/// Insert `item` under the container named by `selection`. An unknown
/// collection or folder path leaves the tree untouched.
pub(super) fn insert_into(
    collections: &mut [Collection],
    selection: &Selection,
    item: CollectionItem,
) -> bool {
    let Some(collection_id) = selection.collection.as_deref() else {
        return false;
    };
    let Some(collection) = collections.iter_mut().find(|each| each.id == collection_id) else {
        return false;
    };
    insert_in_items(&mut collection.items, &selection.folders, item)
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

/// Remove the item with `id` from anywhere in the tree, and report which
/// collection it was removed from so that collection can be re-saved.
pub(super) fn remove_item(collections: &mut [Collection], id: &str) -> Option<String> {
    for collection in collections.iter_mut() {
        if remove_from_items(&mut collection.items, id) {
            return Some(collection.id.clone());
        }
    }
    None
}

fn remove_from_items(items: &mut Vec<CollectionItem>, id: &str) -> bool {
    if let Some(index) = items.iter().position(|item| item_id(item) == id) {
        items.remove(index);
        return true;
    }
    for item in items.iter_mut() {
        if let CollectionItem::Folder(folder) = item {
            if remove_from_items(&mut folder.items, id) {
                return true;
            }
        }
    }
    false
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

#[cfg(test)]
mod tests {
    use restbook_core::{Collection, CollectionItem, Folder, Method, RequestDef, SavedRequest};

    use super::{
        display_name, filter_tree, find_in_items_all, has_move_target, initial, insert_into,
        item_id, move_item, node_name, owner_collection, remove_item, rename_item,
        resolve_selection, Selection,
    };

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
        let filtered = filter_tree(&tree, "needle-2999");
        let elapsed = started.elapsed();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].items.len(), 1);
        assert!(
            matches!(&filtered[0].items[0], CollectionItem::Request(request) if request.id == "req-2999"),
            "the filter kept the wrong row",
        );
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
        let selection = into_folder("fold-1");
        assert!(insert_into(
            &mut tree,
            &selection,
            CollectionItem::Request(saved("req-4", Method::Get, "https://example.com/new"))
        ));

        let items = resolve_selection(&tree, &selection).unwrap();
        assert_eq!(items.len(), 3);
        assert!(matches!(&items[2], CollectionItem::Request(r) if r.id == "req-4"));
    }

    #[test]
    fn insert_into_the_collection_root_lands_at_the_top() {
        let mut tree = sample();
        let selection = Selection::collection("col-1");
        assert!(insert_into(
            &mut tree,
            &selection,
            CollectionItem::Request(saved("req-5", Method::Put, "https://example.com/root"))
        ));
        assert_eq!(tree[0].items.len(), 3);
        assert!(matches!(&tree[0].items[2], CollectionItem::Request(r) if r.id == "req-5"));
    }

    #[test]
    fn insert_into_an_unknown_target_is_a_no_op() {
        let mut tree = sample();
        let unknown_folder = into_folder("nope");
        assert!(!insert_into(
            &mut tree,
            &unknown_folder,
            CollectionItem::Request(saved("req-6", Method::Get, "https://example.com/x"))
        ));
        assert!(!insert_into(
            &mut tree,
            &Selection::default(),
            CollectionItem::Request(saved("req-7", Method::Get, "https://example.com/y"))
        ));
        assert_eq!(tree[0].items.len(), 2);
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
        let filtered = filter_tree(&tree, "b");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].items.len(), 1);

        let CollectionItem::Folder(fold) = &filtered[0].items[0] else {
            panic!("expected the folder to survive as an ancestor");
        };
        assert_eq!(fold.items.len(), 1);
        assert!(matches!(&fold.items[0], CollectionItem::Request(r) if r.id == "req-2"));
    }

    #[test]
    fn filter_matches_a_request_url() {
        let tree = sample();
        let filtered = filter_tree(&tree, "/top");
        let CollectionItem::Request(request) = &filtered[0].items[0] else {
            panic!("expected the top-level request");
        };
        assert_eq!(request.id, "req-3");
    }

    #[test]
    fn filter_keeps_a_matching_collections_whole_subtree() {
        let tree = sample();
        let filtered = filter_tree(&tree, "sam");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].items.len(), 2);
    }

    #[test]
    fn filter_returns_nothing_when_nothing_matches() {
        let tree = sample();
        assert!(filter_tree(&tree, "zzz").is_empty());
    }

    #[test]
    fn an_empty_query_keeps_everything() {
        let tree = sample();
        assert_eq!(filter_tree(&tree, "").len(), 1);
        assert_eq!(filter_tree(&tree, "  ").len(), 1);
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
}
