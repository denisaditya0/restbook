//! Import a file: parse it, land it, and show what happened.
//!
//! The engine parses and the store persists; this module is the seam and the one
//! place that decides what a name collision means. Nothing is written until the
//! whole file has parsed, so a malformed file changes nothing. The default is one
//! new collection per parsed one.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gpui_kit::component::button::*;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Sizable as _, WindowExt as _};
use gpui_kit::*;
use restbook_core::import::{self, BulkImport, ImportFormat, Options, Skip};
use restbook_core::{Collection, CollectionItem, Store};

use crate::notice::Notice;
use crate::panels::collections::{import_into, Destination, ImportOutcome};
use crate::theme::{Metrics, Palette};

use super::destination_picker::{
    destination_picker, open_destination_select, selected_choice, DestinationChoice,
};
use super::Workspace;

/// A parsed file waiting for the dialog's answer. Kept on the workspace because
/// the dialog's content closure is rebuilt on every notify: re-parsing there
/// would both cost the work again and lose the parse's warnings.
pub(super) struct PendingImport {
    file: String,
    parsed: BulkImport,
}

/// Refuse files past this size. Parsing is synchronous once read, and a file
/// this big is far more likely to be hostile or a mistake than a real export.
const MAX_IMPORT_BYTES: u64 = 32 * 1024 * 1024;

/// The toast an import shows while it works. The type is the key, so the step
/// from reading to saving replaces one notification instead of stacking two.
struct ImportNotice;

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

/// Parse `text` as the format `file` names. The expensive half, and the one that
/// can fail without having written anything.
///
/// A failure names the reader that was tried and whether the file name or the
/// contents chose it; otherwise a wrong guess reports only what the wrong reader
/// missed.
fn parse(file: &str, text: &str) -> Result<BulkImport, String> {
    let (format, source) = match ImportFormat::from_file_name(file) {
        Some(format) => (format, "from the file name"),
        None => (ImportFormat::detect(text), "guessed from its contents"),
    };
    log::debug!(
        target: "restbook_app::import",
        "{file}: {} bytes, read as {} {source}",
        text.len(),
        format.label()
    );
    import::parse(
        format,
        text,
        Options {
            fallback_name: file,
            har_limit: None,
        },
    )
    .map_err(|error| {
        format!(
            "Could not import {file} as {} ({source}): {error}",
            format.label()
        )
    })
}

/// Save what `parsed` holds, either as new collections or into `target`.
///
/// A name already in the store is reported as a duplicate rather than
/// overwritten, which is the engine's rule. Counted requests are the ones actually
/// saved. A `target` the tree no longer holds errors and writes nothing: landing
/// elsewhere would be worse than not landing it.
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

/// The busy toast was closed before the file was read or parsed. Drop the work
/// and say so: a pick that returns to an unchanged window reads as a bug.
fn cancelled(
    workspace: &mut Workspace,
    file: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.end_import_busy(window, cx);
    workspace.notice(
        &Notice::Done(format!("Cancelled importing {file:?}.").into()),
        cx,
    );
}

impl Workspace {
    /// Ask for a file, parse it off the UI thread, then let the user choose
    /// where it lands. Called from render as well as from the action, so a
    /// dialog always has the window it needs.
    pub(crate) fn import_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // One import at a time: the empty state, the pane's button, and the menu
        // all route here, and two runs would fight over one busy toast.
        if self.import_busy {
            return;
        }
        // A previous attempt's parse must not survive into this one.
        self.import_pending = None;
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
            // A 29 MB document is a second of reading and parsing with nothing on
            // screen, which reads as a hang. The toast goes up before the read so
            // every step below is covered, and closing it cancels: the flag is
            // checked between the two background steps, which is as far as a
            // blocking parse can be interrupted.
            let cancel = Arc::new(AtomicBool::new(false));
            let watching = cancel.clone();
            let shown = workspace.update_in(cx, |this, window, cx| {
                this.show_import_busy(&file, "Reading and parsing", Some(watching), window, cx);
            });
            if shown.is_err() {
                return;
            }
            let read = cx
                .background_spawn({
                    let path = path.clone();
                    async move {
                        match std::fs::metadata(&path) {
                            Ok(meta) if meta.len() > MAX_IMPORT_BYTES => {
                                let size = meta.len() / (1024 * 1024);
                                let limit = MAX_IMPORT_BYTES / (1024 * 1024);
                                Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    format!(
                                        "the file is {size} MB, over the {limit} MB import limit"
                                    ),
                                ))
                            }
                            _ => std::fs::read_to_string(path),
                        }
                    }
                })
                .await;
            let text = match read {
                Ok(text) => text,
                Err(error) => {
                    let message = format!("Could not read {file}: {error}");
                    workspace
                        .update_in(cx, |this, window, cx| {
                            this.end_import_busy(window, cx);
                            this.notice(&Notice::Failed(message.into()), cx);
                        })
                        .ok();
                    return;
                }
            };
            if cancel.load(Ordering::Relaxed) {
                workspace
                    .update_in(cx, |this, window, cx| cancelled(this, &file, window, cx))
                    .ok();
                return;
            }
            let parsed = cx
                .background_spawn({
                    let file = file.clone();
                    async move { parse(&file, &text) }
                })
                .await;
            // A cancellation that arrived during the parse drops the result
            // rather than opening the destination dialog for it.
            if cancel.load(Ordering::Relaxed) {
                workspace
                    .update_in(cx, |this, window, cx| cancelled(this, &file, window, cx))
                    .ok();
                return;
            }
            workspace
                .update_in(cx, |this, window, cx| {
                    this.end_import_busy(window, cx);
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
                    this.import_pending = Some(PendingImport { file, parsed });
                    this.open_import_dialog(window, cx);
                })
                .ok();
        })
        .detach();
    }

    /// Show what an import is doing, for as long as it is doing it.
    ///
    /// The step is a line under the file name; closing the toast sets `cancel`
    /// where that is offered. A store write cannot be interrupted, so the saving
    /// phase passes `None` and the toast is only a report there.
    fn show_import_busy(
        &mut self,
        file: &str,
        step: impl Into<SharedString> + 'static,
        cancel: Option<Arc<AtomicBool>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.import_busy = true;
        let step = step.into();
        let mut notification = Notification::new()
            .id::<ImportNotice>()
            .autohide(false)
            .title("Importing")
            .message(file.to_owned())
            .content(move |_, _, cx| {
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Spinner::new().xsmall())
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(step.clone()),
                    )
                    .into_any_element()
            });
        if let Some(cancel) = cancel {
            notification = notification.on_close(move |_, _| cancel.store(true, Ordering::Relaxed));
        }
        window.push_notification(notification, cx);
    }

    /// Clear [`Workspace::import_busy`] and the toast. Every exit path calls it,
    /// so a finished import cannot leave a spinner on screen.
    fn end_import_busy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.import_busy {
            return;
        }
        self.import_busy = false;
        window.remove_notification::<ImportNotice>(cx);
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
        let requests = parsed.request_count();
        let plural = if requests == 1 { "" } else { "s" };
        self.show_import_busy(
            &file,
            format!("Saving {requests} request{plural}"),
            None,
            window,
            cx,
        );
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

        // The default is today's behavior — import as new collections — so the
        // picker opens on that row.
        let rows = self.panes.collections.read(cx).destinations();
        let select = open_destination_select(
            &rows,
            Some("New collection(s)"),
            DestinationChoice::New,
            window,
            cx,
        );

        let workspace = cx.entity();
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);

        // The footer button and Enter read the picker at the moment of the click,
        // not when the dialog was built.
        type Confirm = Rc<dyn Fn(&mut Window, &mut App) -> bool>;
        let confirm_workspace = workspace.clone();
        let confirm_select = select.clone();
        let confirm: Confirm = Rc::new(move |window, cx| {
            let target =
                selected_choice(&confirm_select, cx).and_then(|choice| choice.existing().cloned());
            let workspace = confirm_workspace.clone();
            window.defer(cx, move |window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let Some(pending) = workspace.import_pending.take() else {
                        return;
                    };
                    workspace.land_import(pending.file, pending.parsed, target, window, cx);
                });
            });
            true
        });
        let for_button = confirm.clone();
        let for_ok = confirm.clone();

        window.open_dialog(cx, move |dialog, _window, _cx| {
            let into = v_flex()
                .gap(px(metrics.space_xs))
                .child(div().text_xs().text_color(palette.muted).child("Into"))
                .child(destination_picker(&select, "Import destination"));

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
        self.end_import_busy(window, cx);
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
        self.refresh_collection_scope(cx);
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

/// How many rows of one section the report dialog draws before it stops and
/// counts the rest. A clean import has none; a file the reader could not fully
/// understand can have thousands, and one text line each will not do.
const REPORT_ROW_LIMIT: usize = 50;

fn section(heading: &str, rows: &[String], palette: &Palette, metrics: Metrics) -> AnyElement {
    let mut list = v_flex().gap(px(metrics.space_xs)).child(
        div()
            .text_xs()
            .text_color(palette.muted)
            .child(heading.to_owned()),
    );
    for row in rows.iter().take(REPORT_ROW_LIMIT) {
        list = list.child(div().text_sm().child(row.clone()));
    }
    if rows.len() > REPORT_ROW_LIMIT {
        list = list.child(
            div()
                .text_sm()
                .text_color(palette.muted)
                .child(format!("and {} more", rows.len() - REPORT_ROW_LIMIT)),
        );
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

    /// Every malformed shape errors with the file named, and none panics. The
    /// `.har` name forces one reader, so the corpus tests the parser, not the
    /// format guess.
    #[test]
    fn a_corpus_of_malformed_files_all_error_without_panicking() {
        let corpus = [
            "",
            "   ",
            "{",
            "[",
            "null",
            "not json",
            r#"{"log": {}}"#,
            r#"{"log": {"entries": "not a list"}}"#,
        ];
        for text in corpus {
            let store = MemoryStore::new();
            let error = as_new_collections(&store, "broken.har", text)
                .expect_err(&format!("malformed input parsed: {text:?}"));
            assert!(error.contains("broken.har"), "{text:?}: {error}");
            assert!(store.collections().unwrap().is_empty(), "{text:?}");
        }
    }

    #[test]
    fn a_guessed_reader_is_named_and_called_a_guess() {
        // `.json` is ambiguous, so the contents picked HAR and the message has
        // to say so: "no log.entries" alone does not explain why a spec was
        // read as a HAR.
        let error = parse("openapi.json", r#"{"log": {"pages": []}}"#).unwrap_err();
        assert!(
            error.contains("as a HAR log (guessed from its contents)"),
            "{error}"
        );
    }

    #[test]
    fn a_named_reader_is_not_called_a_guess() {
        let error = parse("session.har", r#"{"log": {}}"#).unwrap_err();
        assert!(
            error.contains("as a HAR log (from the file name)"),
            "{error}"
        );
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
            .save_collection(&Collection::new("target", "Target"))
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
