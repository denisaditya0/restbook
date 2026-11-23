//! Import a file: parse it, land it, and show what happened.
//!
//! The engine parses and the store persists; this module is the seam between
//! them, and the one place that decides what a name collision means. Nothing is
//! written until the whole file has parsed, so a malformed file changes nothing.

use gpui_kit::component::button::*;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::{v_flex, WindowExt as _};
use gpui_kit::*;
use restbook_core::import::{self, ImportFormat, Options, Skip};
use restbook_core::{CollectionItem, Store};

use crate::notice::Notice;
use crate::theme::{Metrics, Palette};

use super::Workspace;

/// What an import put into the store, in the words the result dialog shows.
#[derive(Debug)]
struct Report {
    file: String,
    requests: usize,
    collections: Vec<String>,
    environments: Vec<String>,
    duplicates: Vec<String>,
    skipped: Vec<Skip>,
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
        format!("Imported {}", parts.join(", "))
    }

    fn is_empty(&self) -> bool {
        self.collections.is_empty()
            && self.environments.is_empty()
            && self.duplicates.is_empty()
            && self.skipped.is_empty()
    }
}

/// Parse `text` as the format `file` names, then save what it holds.
///
/// A name already in the store is reported as a duplicate rather than
/// overwritten, which is the engine's rule for a batch import. Requests counted
/// in the report are the ones actually saved, not the ones read.
fn land(store: &dyn Store, file: &str, text: &str) -> Result<Report, String> {
    let format = ImportFormat::from_file_name(file).unwrap_or_else(|| ImportFormat::detect(text));
    let parsed = import::parse(
        format,
        text,
        Options {
            fallback_name: file,
            har_limit: None,
        },
    )
    .map_err(|error| format!("Could not import {file}: {error}"))?;

    let existing = store
        .collections()
        .map_err(|error| format!("Could not import {file}: {error}"))?
        .into_iter()
        .map(|collection| collection.name)
        .collect::<Vec<_>>();
    let mut collections = Vec::new();
    let mut duplicates = Vec::new();
    let mut requests = 0;
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
    })
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
    /// Ask for a file, then import it off the UI thread.
    pub(crate) fn import_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a request file".into()),
        });
        let store = self.store.clone();
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
            let landed = cx
                .background_spawn(async move { land(store.as_ref(), &file, &text) })
                .await;
            workspace
                .update_in(cx, |this, window, cx| {
                    this.finish_import(landed, window, cx)
                })
                .ok();
        })
        .detach();
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

        log::info!(target: "restbook_app::ui", "import: {}", report.summary());
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
    use super::land;
    use restbook_core::Store as _;
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

    #[test]
    fn a_two_host_har_lands_one_collection_per_host() {
        let store = MemoryStore::new();
        let report = land(&store, "session.har", HAR).unwrap();

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
        let error = land(&store, "broken.har", r#"{"log": {}}"#).unwrap_err();

        assert!(error.contains("broken.har"), "{error}");
        assert!(error.contains("HAR"), "{error}");
        assert!(store.collections().unwrap().is_empty());
    }

    #[test]
    fn an_existing_collection_is_reported_not_overwritten() {
        let store = MemoryStore::new();
        land(&store, "session.har", HAR).unwrap();
        let second = land(&store, "session.har", HAR).unwrap();

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

        let report = land(&store, "backup.json", bundle).unwrap();
        assert_eq!(report.environments, ["dev"]);
        assert_eq!(store.environments().unwrap()[0].name, "dev");
    }
}
