//! Import a file: parse it, land it, and show what happened.
//!
//! The engine parses and the store persists; this module is the seam between
//! them, and the one place that decides what a name collision means. Nothing is
//! written until the whole file has parsed, so a malformed file changes nothing.
//!
//! Parsing and landing are separate because the parsed file waits for one
//! decision: whether it becomes new collections or appends into a place that
//! already exists. The default is still one new collection per parsed one.

use std::rc::Rc;

use gpui_kit::component::button::*;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{h_flex, v_flex, WindowExt as _};
use gpui_kit::*;
use restbook_core::import::{self, BulkImport, ImportFormat, Options, Skip};
use restbook_core::{Collection, CollectionItem, Store};

use crate::icons::IconName;
use crate::notice::Notice;
use crate::panels::collections::{import_into, Destination, ImportOutcome};
use crate::theme::{Metrics, Palette};

use super::save_dialog::{with_destinations, DestinationPick};
use super::Workspace;

/// A parsed file waiting for the dialog's answer. Kept on the workspace because
/// the dialog's content closure is rebuilt on every notify: re-parsing there
/// would both cost the work again and lose the parse's warnings.
pub(super) struct PendingImport {
    file: String,
    parsed: BulkImport,
}

/// What an import put into the store, in the words the result dialog shows.
#[derive(Debug)]
struct Report {
    file: String,
    requests: usize,
    collections: Vec<String>,
    environments: Vec<String>,
    duplicates: Vec<String>,
    skipped: Vec<Skip>,
    notes: Vec<Skip>,
}

impl Report {
    fn summary(&self) -> String {
        let plural = |count: usize| if count == 1 { "" } else { "s" };
        let mut parts = vec![format!(
            "{} request{} in {} collection{}",
            self.requests,
            plural(self.requests),
            self.collections.len(),
            plural(self.collections.len()),
        )];
        if !self.environments.is_empty() {
            parts.push(format!(
                "{} environment{}",
                self.environments.len(),
                plural(self.environments.len())
            ));
        }
        if !self.duplicates.is_empty() {
            parts.push(format!("{} already present", self.duplicates.len()));
        }
        if !self.skipped.is_empty() {
            parts.push(format!("{} skipped", self.skipped.len()));
        }
        if !self.notes.is_empty() {
            let plural = if self.notes.len() == 1 { "" } else { "s" };
            parts.push(format!("{} note{}", self.notes.len(), plural));
        }
        format!("Imported {}", parts.join(", "))
    }

    fn is_empty(&self) -> bool {
        self.collections.is_empty()
            && self.environments.is_empty()
            && self.duplicates.is_empty()
            && self.skipped.is_empty()
    }
}

/// Parse `text` as the format `file` names. The expensive half of an import,
/// and the half that can fail without having written anything.
fn parse(file: &str, text: &str) -> Result<BulkImport, String> {
    let format = ImportFormat::from_file_name(file).unwrap_or_else(|| ImportFormat::detect(text));
    import::parse(
        format,
        text,
        Options {
            fallback_name: file,
            har_limit: None,
        },
    )
    .map_err(|error| format!("Could not import {file}: {error}"))
}

/// Save what `parsed` holds, either as new collections or into `target`.
///
/// A name already in the store is reported as a duplicate rather than
/// overwritten, which is the engine's rule for a batch import. Requests counted
/// in the report are the ones actually saved, not the ones read. A `target`
/// that the tree no longer holds is an error that writes nothing: landing the
/// file somewhere else than the user chose would be worse than not landing it.
fn land(
    store: &dyn Store,
    file: &str,
    parsed: BulkImport,
    target: Option<&Destination>,
) -> Result<Report, String> {
    let mut duplicates = Vec::new();
    let mut requests = 0;

    let collections = match target {
        None => {
            let existing = store
                .collections()
                .map_err(|error| format!("Could not import {file}: {error}"))?
                .into_iter()
                .map(|collection| collection.name)
                .collect::<Vec<_>>();
            let mut collections = Vec::new();
            for collection in &parsed.collections {
                if existing.contains(&collection.name) {
                    duplicates.push(collection.name.clone());
                    continue;
                }
                store.save_collection(collection).map_err(|error| {
                    format!(
                        "Could not import {file}: saving {:?} failed: {error}",
                        collection.name
                    )
                })?;
                requests += count_requests(&collection.items);
                collections.push(collection.name.clone());
            }
            collections
        }
        Some(target) => {
            let mut collections = store
                .collections()
                .map_err(|error| format!("Could not import {file}: {error}"))?;
            let incoming = parsed
                .collections
                .into_iter()
                .flat_map(|collection| collection.items)
                .collect::<Vec<_>>();
            let Some(outcome) = import_into(&mut collections, target, incoming) else {
                return Err(format!(
                    "Could not import {file}: {} no longer exists.",
                    destination_label(&collections, target)
                ));
            };
            let Some(collection) = collections
                .iter()
                .find(|each| each.id == outcome.collection)
            else {
                return Err(format!("Could not import {file}: the destination is gone."));
            };
            store.save_collection(collection).map_err(|error| {
                format!(
                    "Could not import {file}: saving {:?} failed: {error}",
                    collection.name
                )
            })?;
            let ImportOutcome {
                added,
                duplicates: skipped,
                ..
            } = outcome;
            requests += added;
            duplicates.extend(skipped);
            vec![collection.name.clone()]
        }
    };

    let existing = store
        .environments()
        .map_err(|error| format!("Could not import {file}: {error}"))?
        .into_iter()
        .map(|environment| environment.name)
        .collect::<Vec<_>>();
    let mut environments = Vec::new();
    for environment in &parsed.environments {
        if existing.contains(&environment.name) {
            duplicates.push(environment.name.clone());
            continue;
        }
        store.save_environment(environment).map_err(|error| {
            format!(
                "Could not import {file}: saving environment {:?} failed: {error}",
                environment.name
            )
        })?;
        environments.push(environment.name.clone());
    }

    Ok(Report {
        file: file.to_owned(),
        requests,
        collections,
        environments,
        duplicates,
        skipped: parsed.skipped,
        notes: parsed.notes,
    })
}

/// The destination as the user sees it, for an error that has to name it. A
/// folder the tree has lost stops the walk, so the label never claims more than
/// what is still there.
fn destination_label(collections: &[Collection], target: &Destination) -> String {
    let Some(collection) = collections.iter().find(|each| each.id == target.collection) else {
        return target.collection.clone();
    };
    let mut label = collection.name.clone();
    let mut items: &[CollectionItem] = &collection.items;
    for folder_id in &target.folders {
        let Some(folder) = items.iter().find_map(|item| match item {
            CollectionItem::Folder(folder) if &folder.id == folder_id => Some(folder),
            _ => None,
        }) else {
            break;
        };
        label = format!("{label} › {}", folder.name);
        items = &folder.items;
    }
    label
}

fn count_requests(items: &[CollectionItem]) -> usize {
    items
        .iter()
        .map(|item| match item {
            CollectionItem::Request(_) => 1,
            CollectionItem::Folder(folder) => count_requests(&folder.items),
        })
        .sum()
}

impl Workspace {
    /// Ask for a file, parse it off the UI thread, then let the user choose
    /// where it lands. Called from render as well as from the action, so a
    /// dialog always has the window it needs.
    pub(crate) fn import_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A previous attempt's choice must not survive into this one.
        self.import_pending = None;
        self.import_target = None;
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a request file".into()),
        });
        cx.spawn_in(window, async move |workspace, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let file = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("import")
                .to_owned();
            let read = cx
                .background_spawn({
                    let path = path.clone();
                    async move { std::fs::read_to_string(path) }
                })
                .await;
            let text = match read {
                Ok(text) => text,
                Err(error) => {
                    let message = format!("Could not read {file}: {error}");
                    workspace
                        .update_in(cx, |this, _window, cx| {
                            this.notice(&Notice::Failed(message.into()), cx);
                        })
                        .ok();
                    return;
                }
            };
            let parsed = cx
                .background_spawn({
                    let file = file.clone();
                    async move { parse(&file, &text) }
                })
                .await;
            workspace
                .update_in(cx, |this, window, cx| {
                    let parsed = match parsed {
                        Ok(parsed) => parsed,
                        Err(message) => {
                            this.notice(&Notice::Failed(message.into()), cx);
                            return;
                        }
                    };
                    // An environments-only bundle has no collection to place,
                    // so the picker would offer a choice that changes nothing.
                    if parsed.collections.is_empty() {
                        this.land_import(file, parsed, None, window, cx);
                        return;
                    }
                    this.import_target = None;
                    this.import_pending = Some(PendingImport { file, parsed });
                    this.open_import_dialog(window, cx);
                })
                .ok();
        })
        .detach();
    }

    /// Where a parsed file lands: new collections by default, or the place the
    /// dialog's picker chose. The write runs off the UI thread; the answer comes
    /// back through [`Self::finish_import`].
    fn land_import(
        &mut self,
        file: String,
        parsed: BulkImport,
        target: Option<Destination>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = self.store.clone();
        cx.spawn_in(window, async move |workspace, cx| {
            let landed = cx
                .background_spawn(
                    async move { land(store.as_ref(), &file, parsed, target.as_ref()) },
                )
                .await;
            workspace
                .update_in(cx, |this, window, cx| {
                    this.finish_import(landed, window, cx)
                })
                .ok();
        })
        .detach();
    }

    /// Ask where a parsed file should land. Modelled on the move dialog because
    /// the decision is the same one: one `Destination`, or the default of making
    /// new collections.
    pub(crate) fn open_import_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.import_pending.as_ref() else {
            return;
        };
        let title = format!("Import {:?}", pending.file);
        let requests = pending.parsed.request_count();
        let collections = pending.parsed.collections.len();
        // The default is today's behavior, so every open starts there.
        self.import_target = None;

        let workspace = cx.entity();
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);

        // The footer button and Enter read the picker at the moment of the click,
        // not when the dialog was built.
        type Confirm = Rc<dyn Fn(&mut Window, &mut App) -> bool>;
        let confirm_workspace = workspace.clone();
        let confirm: Confirm = Rc::new(move |window, cx| {
            let workspace = confirm_workspace.clone();
            window.defer(cx, move |window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let Some(pending) = workspace.import_pending.take() else {
                        return;
                    };
                    let target = workspace.import_target.clone();
                    workspace.land_import(pending.file, pending.parsed, target, window, cx);
                });
            });
            true
        });
        let for_button = confirm.clone();
        let for_ok = confirm.clone();

        window.open_dialog(cx, move |dialog, _window, cx| {
            let rows = workspace.read(cx).panes.collections.read(cx).destinations();
            let current = workspace.read(cx).import_target.clone();
            let current_label = match &current {
                None => "New collection(s)".to_owned(),
                Some(target) => rows
                    .iter()
                    .find(|row| &row.target == target)
                    .map(|row| row.label.clone())
                    .unwrap_or_else(|| "Choose a collection".to_owned()),
            };

            let menu_workspace = workspace.clone();
            let menu_rows = rows.clone();
            let menu_current = current.clone();
            let menu_pick: DestinationPick = Rc::new(|workspace, target| {
                workspace.import_target = Some(target.clone());
            });
            let menu = move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                let new_workspace = menu_workspace.clone();
                let menu = menu.item(
                    PopupMenuItem::new("New collection(s)")
                        .icon(IconName::Plus)
                        .checked(menu_current.is_none())
                        .on_click(move |_, _, cx| {
                            new_workspace.update(cx, |workspace, cx| {
                                workspace.import_target = None;
                                cx.notify();
                            });
                        }),
                );
                with_destinations(
                    menu,
                    &menu_rows,
                    menu_current.as_ref(),
                    &menu_workspace,
                    &menu_pick,
                )
                // Every folder in every collection: taller than a minimum window.
                .scrollable(true)
            };

            let into = h_flex()
                .gap(px(metrics.space_sm))
                .items_center()
                .text_sm()
                .child(div().text_color(palette.muted).child("Into"))
                .child(
                    Button::new("import-destination")
                        .label(current_label)
                        .ghost()
                        .compact()
                        .flex_1()
                        .min_w_0()
                        .dropdown_caret(true)
                        .tooltip("Where the requests are imported")
                        .dropdown_menu(menu),
                );

            let source = v_flex().gap(px(metrics.space_xs)).child(
                div().text_sm().text_color(palette.muted).child(format!(
                    "{requests} request{} in {collections} collection{}",
                    if requests == 1 { "" } else { "s" },
                    if collections == 1 { "" } else { "s" },
                )),
            );

            let cancel_workspace = workspace.clone();
            let cancel = Button::new("import-cancel")
                .label("Cancel")
                .ghost()
                .on_click(move |_, window, cx| {
                    cancel_workspace.update(cx, |workspace, _cx| {
                        workspace.import_pending = None;
                        workspace.import_target = None;
                    });
                    window.close_dialog(cx);
                });
            let import = Button::new("import-confirm")
                .label("Import")
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
                .title(title.clone())
                .w_112()
                .child(
                    v_flex()
                        .gap(px(metrics.space_md))
                        .py(px(metrics.space_sm))
                        .child(source)
                        .child(into),
                )
                .footer(DialogFooter::new().child(cancel).child(import))
                .on_ok({
                    let confirm = for_ok.clone();
                    move |_, window, cx| confirm(window, cx)
                })
        });
    }

    fn finish_import(
        &mut self,
        landed: Result<Report, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let report = match landed {
            Ok(report) => report,
            Err(message) => {
                self.notice(&Notice::Failed(message.into()), cx);
                return;
            }
        };
        if report.is_empty() {
            self.notice(
                &Notice::Failed(format!("Nothing to import from {:?}.", report.file).into()),
                cx,
            );
            return;
        }

        log::debug!(target: "restbook_app::ui", "import: {}", report.summary());
        self.panes
            .collections
            .update(cx, |panel, cx| panel.reload(Some(window), cx));
        if !report.environments.is_empty() {
            self.variables
                .update(cx, |variables, cx| variables.reload(cx));
        }
        self.open_import_report(&report, window, cx);
    }

    /// The outcome as one dialog: the counts first, then only the lists that
    /// are non-empty, so a clean import is two lines and a partial one names
    /// every item the engine or the store left out.
    fn open_import_report(&self, report: &Report, window: &mut Window, cx: &mut Context<Self>) {
        let metrics = Metrics::current(cx);
        let palette = Palette::new(cx);
        let title = format!("Import {:?}", report.file);
        let summary = report.summary();
        let collections = report.collections.clone();
        let environments = report.environments.clone();
        let duplicates = report.duplicates.clone();
        let skipped = report
            .skipped
            .iter()
            .map(|skip| format!("{} — {}", skip.at, skip.reason))
            .collect::<Vec<_>>();
        let notes = report
            .notes
            .iter()
            .map(|note| format!("{} — {}", note.at, note.reason))
            .collect::<Vec<_>>();

        window.open_dialog(cx, move |dialog, _window, _cx| {
            let mut body = v_flex()
                .gap(px(metrics.space_sm))
                .py(px(metrics.space_sm))
                .child(div().text_sm().child(summary.clone()));
            for (heading, rows) in [
                ("Collections", &collections),
                ("Environments", &environments),
                ("Already present", &duplicates),
                ("Skipped", &skipped),
                ("Notes", &notes),
            ] {
                if !rows.is_empty() {
                    body = body.child(section(heading, rows, &palette, metrics));
                }
            }

            let close = Button::new("import-report-close")
                .label("Close")
                .on_click(|_, window, cx| window.close_dialog(cx));

            dialog
                .title(title.clone())
                .w_112()
                .child(body)
                .footer(DialogFooter::new().child(close))
                .on_ok(|_, window, cx| {
                    window.close_dialog(cx);
                    true
                })
        });
    }
}

fn section(heading: &str, rows: &[String], palette: &Palette, metrics: Metrics) -> AnyElement {
    let mut list = v_flex().gap(px(metrics.space_xs)).child(
        div()
            .text_xs()
            .text_color(palette.muted)
            .child(heading.to_owned()),
    );
    for row in rows {
        list = list.child(div().text_sm().child(row.clone()));
    }
    list.into_any_element()
}

#[cfg(test)]
mod tests {
    // Named imports rather than `super::*`: the parent module glob-imports
    // `gpui_kit::*`, and that macro set shadows `#[test]`.
    use super::{land, parse, Report};
    use crate::panels::collections::Destination;
    use restbook_core::{Collection, CollectionItem, Store as _};
    use restbook_store::MemoryStore;

    /// Two hosts, both JSON, so neither entry is dropped as an asset.
    const HAR: &str = r#"{
      "log": { "entries": [
        { "request": { "method": "GET", "url": "https://api.test/users", "headers": [] },
          "response": { "content": { "mimeType": "application/json" } } },
        { "request": { "method": "POST", "url": "https://auth.test/token", "headers": [] },
          "response": { "content": { "mimeType": "application/json" } } }
      ] }
    }"#;

    /// The default path: parse, then land as new collections.
    fn as_new_collections(store: &MemoryStore, file: &str, text: &str) -> Result<Report, String> {
        land(store, file, parse(file, text)?, None)
    }

    fn destination(store: &MemoryStore, collection: &str) -> Destination {
        Destination {
            collection: store
                .collections()
                .unwrap()
                .into_iter()
                .find(|each| each.name == collection)
                .unwrap()
                .id,
            folders: Vec::new(),
        }
    }

    fn requests_in(items: &[CollectionItem]) -> usize {
        items
            .iter()
            .map(|item| match item {
                CollectionItem::Request(_) => 1,
                CollectionItem::Folder(folder) => requests_in(&folder.items),
            })
            .sum()
    }

    #[test]
    fn a_two_host_har_lands_one_collection_per_host() {
        let store = MemoryStore::new();
        let report = as_new_collections(&store, "session.har", HAR).unwrap();

        let mut names = report.collections.clone();
        names.sort();
        assert_eq!(names, ["api.test", "auth.test"]);
        assert_eq!(report.requests, 2);
        assert!(report.skipped.is_empty());
        assert_eq!(store.collections().unwrap().len(), 2);
    }

    #[test]
    fn a_malformed_file_changes_nothing_and_names_it() {
        let store = MemoryStore::new();
        let error = as_new_collections(&store, "broken.har", r#"{"log": {}}"#).unwrap_err();

        assert!(error.contains("broken.har"), "{error}");
        assert!(error.contains("HAR"), "{error}");
        assert!(store.collections().unwrap().is_empty());
    }

    #[test]
    fn an_existing_collection_is_reported_not_overwritten() {
        let store = MemoryStore::new();
        as_new_collections(&store, "session.har", HAR).unwrap();
        let second = as_new_collections(&store, "session.har", HAR).unwrap();

        assert!(second.collections.is_empty());
        assert_eq!(second.requests, 0);
        let mut names = second.duplicates.clone();
        names.sort();
        assert_eq!(names, ["api.test", "auth.test"]);
        assert_eq!(
            store.collections().unwrap().len(),
            2,
            "the first import is still the one on disk"
        );
    }

    #[test]
    fn a_bundle_carries_its_environments_across() {
        let store = MemoryStore::new();
        let bundle = r#"{"version": 1, "collections": [], "environments": [
            {"name": "dev", "vars": []}
        ]}"#;

        let report = as_new_collections(&store, "backup.json", bundle).unwrap();
        assert_eq!(report.environments, ["dev"]);
        assert_eq!(store.environments().unwrap()[0].name, "dev");
    }

    #[test]
    fn importing_into_an_existing_collection_appends_and_reports_it() {
        let store = MemoryStore::new();
        // One empty collection, and a file that parses into two: both merge
        // into the destination instead of making two new collections.
        store
            .save_collection(&Collection {
                id: "target".to_owned(),
                name: "Target".to_owned(),
                items: Vec::new(),
            })
            .unwrap();
        let target = destination(&store, "Target");

        let report = land(
            &store,
            "session.har",
            parse("session.har", HAR).unwrap(),
            Some(&target),
        )
        .unwrap();

        assert_eq!(report.collections, ["Target"], "the destination, once");
        assert_eq!(report.requests, 2);
        assert!(report.duplicates.is_empty());
        let collections = store.collections().unwrap();
        assert_eq!(collections.len(), 1, "no new collection was made");
        assert_eq!(requests_in(&collections[0].items), 2);
    }

    #[test]
    fn a_same_name_request_is_skipped_while_the_rest_land() {
        let store = MemoryStore::new();
        as_new_collections(&store, "session.har", HAR).unwrap();
        let target = destination(&store, "api.test");

        let report = land(
            &store,
            "session.har",
            parse("session.har", HAR).unwrap(),
            Some(&target),
        )
        .unwrap();

        assert_eq!(report.collections, ["api.test"]);
        assert_eq!(report.requests, 1, "only the request that was not there");
        assert_eq!(report.duplicates, ["GET /users"]);
        let collections = store.collections().unwrap();
        let api = collections
            .iter()
            .find(|each| each.name == "api.test")
            .unwrap();
        assert_eq!(requests_in(&api.items), 2);
    }

    #[test]
    fn an_unknown_destination_fails_and_leaves_the_store_alone() {
        let store = MemoryStore::new();
        as_new_collections(&store, "session.har", HAR).unwrap();
        let target = Destination {
            collection: "gone".to_owned(),
            folders: Vec::new(),
        };

        let error = land(
            &store,
            "session.har",
            parse("session.har", HAR).unwrap(),
            Some(&target),
        )
        .unwrap_err();

        assert!(error.contains("session.har"), "{error}");
        assert!(error.contains("gone"), "{error}");
        let collections = store.collections().unwrap();
        assert_eq!(collections.len(), 2);
        assert_eq!(
            collections
                .iter()
                .map(|each| requests_in(&each.items))
                .sum::<usize>(),
            2,
            "nothing was appended"
        );
    }
}
