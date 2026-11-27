//! Export a collection or the whole workspace to a file.
//!
//! The engine writes the format; this module owns the person's choices (scope,
//! format, secrets), the save prompt, and the write. Secrets are redacted unless
//! ticked, and the tick is deliberately not remembered: a remembered
//! "include secrets" is a silent leak on the next export.

use std::path::PathBuf;

use gpui_kit::component::button::*;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::{h_flex, v_flex, Disableable as _, Selectable as _, WindowExt as _};
use gpui_kit::*;
use restbook_core::import::{self, ExportFormat, ExportOptions};
use restbook_core::{Collection, Environment, Store};

use crate::notice::Notice;
use crate::theme::{Metrics, Palette};

use super::{storage, Workspace};

/// What the picker is exporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The collection the sidebar had selected when the dialog opened.
    Collection,
    /// Every collection, plus every environment (bundle only).
    Everything,
}

/// The picker's state. It lives on the workspace because the dialog content is
/// rebuilt on every notify, and a choice made in one build has to survive into
/// the next.
#[derive(Debug, Clone)]
pub(crate) struct ExportState {
    pub(crate) scope: Scope,
    pub(crate) format: ExportFormat,
    pub(crate) include_secrets: bool,
    /// The sidebar's selection when the dialog opened: `(id, name)`. `None`
    /// leaves only *Everything* available.
    pub(crate) collection: Option<(String, String)>,
}

impl Default for ExportState {
    fn default() -> Self {
        Self {
            scope: Scope::Everything,
            format: ExportFormat::Bundle,
            include_secrets: false,
            collection: None,
        }
    }
}

impl ExportState {
    /// The format actually written: *Everything* is a bundle because it is the
    /// only format that carries environments.
    fn effective_format(&self) -> ExportFormat {
        match self.scope {
            Scope::Collection => self.format,
            Scope::Everything => ExportFormat::Bundle,
        }
    }
}

/// The data an export writes, read at the moment of the write so a stale tree
/// cannot be exported after an edit.
fn source(
    store: &dyn Store,
    state: &ExportState,
) -> Result<(Vec<Collection>, Vec<Environment>), String> {
    let collections = store.collections().map_err(|error| error.to_string())?;
    match state.scope {
        Scope::Everything => {
            let environments = store.environments().map_err(|error| error.to_string())?;
            Ok((collections, environments))
        }
        Scope::Collection => {
            let Some((id, _)) = &state.collection else {
                return Err("No collection is selected.".to_owned());
            };
            let collection = collections
                .into_iter()
                .find(|collection| &collection.id == id)
                .ok_or_else(|| "The selected collection is no longer there.".to_owned())?;
            Ok((vec![collection], Vec::new()))
        }
    }
}

/// Render the file's text. Split from the write so a test can assert what a
/// scope carries without touching the filesystem.
fn render(store: &dyn Store, state: &ExportState) -> Result<String, String> {
    let (collections, environments) = source(store, state)?;
    import::export(
        state.effective_format(),
        &collections,
        &environments,
        ExportOptions {
            include_secrets: state.include_secrets,
        },
    )
    .map_err(|error| error.to_string())
}

pub(super) fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "restbook".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The name the save prompt starts with, extension included.
fn suggested_file_name(state: &ExportState, workspace: &str) -> String {
    let extension = state.effective_format().extension();
    match (&state.scope, &state.collection) {
        (Scope::Collection, Some((_, name))) => format!("{}.{extension}", slug(name)),
        _ => format!("{}-backup.{extension}", slug(workspace)),
    }
}

impl Workspace {
    pub(crate) fn open_export_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let collection = self
            .panes
            .collections
            .read(cx)
            .selected_collection()
            .map(|(id, name)| (id.to_owned(), name.to_owned()));
        self.export_state = ExportState {
            scope: if collection.is_some() {
                Scope::Collection
            } else {
                Scope::Everything
            },
            format: ExportFormat::Postman,
            include_secrets: false,
            collection,
        };

        let workspace = cx.entity();
        let metrics = Metrics::current(cx);
        let palette = Palette::new(cx);

        window.open_dialog(cx, move |dialog, _window, cx| {
            let state = workspace.read(cx).export_state.clone();
            let format = state.effective_format();

            // Scope: the two places a file can come from.
            let scope = h_flex()
                .gap(px(metrics.space_xs))
                .child(
                    Button::new("export-scope-collection")
                        .label("This collection")
                        .compact()
                        .ghost()
                        .selected(state.scope == Scope::Collection)
                        .toggled(state.scope == Scope::Collection)
                        .disabled(state.collection.is_none())
                        .on_click({
                            let workspace = workspace.clone();
                            move |_, _, cx| {
                                workspace.update(cx, |this, cx| {
                                    this.export_state.scope = Scope::Collection;
                                    cx.notify();
                                });
                            }
                        }),
                )
                .child(
                    Button::new("export-scope-everything")
                        .label("Everything")
                        .compact()
                        .ghost()
                        .selected(state.scope == Scope::Everything)
                        .toggled(state.scope == Scope::Everything)
                        .on_click({
                            let workspace = workspace.clone();
                            move |_, _, cx| {
                                workspace.update(cx, |this, cx| {
                                    this.export_state.scope = Scope::Everything;
                                    cx.notify();
                                });
                            }
                        }),
                );

            // Formats: only one collection can be written as Postman or HAR,
            // and *Everything* is a bundle by definition.
            let mut formats = h_flex().gap(px(metrics.space_xs));
            for (wanted, label) in [
                (ExportFormat::Postman, "Postman"),
                (ExportFormat::Har, "HAR"),
                (ExportFormat::Bundle, "Bundle"),
            ] {
                formats = formats.child(
                    Button::new(format!("export-format-{}", format_id(wanted)))
                        .label(label)
                        .compact()
                        .ghost()
                        .selected(format == wanted)
                        .toggled(format == wanted)
                        .disabled(
                            state.scope == Scope::Everything && wanted != ExportFormat::Bundle,
                        )
                        .on_click({
                            let workspace = workspace.clone();
                            move |_, _, cx| {
                                workspace.update(cx, |this, cx| {
                                    this.export_state.format = wanted;
                                    cx.notify();
                                });
                            }
                        }),
                );
            }

            let secrets = Checkbox::new(("export-include-secrets", 0u64))
                .label("Include secrets")
                .checked(state.include_secrets)
                .on_change({
                    let workspace = workspace.clone();
                    move |checked: &bool, _, cx| {
                        workspace.update(cx, |this, cx| {
                            this.export_state.include_secrets = *checked;
                            cx.notify();
                        });
                    }
                });

            let cancel = Button::new("export-cancel")
                .label("Cancel")
                .ghost()
                .on_click(|_, window, cx| window.close_dialog(cx));
            let go = Button::new("export-confirm")
                .label("Export…")
                .primary()
                .on_click({
                    let workspace = workspace.clone();
                    move |_, window, cx| {
                        workspace.update(cx, |this, cx| this.begin_export(window, cx));
                        window.close_dialog(cx);
                    }
                });

            let mut body = v_flex()
                .gap(px(metrics.space_md))
                .py(px(metrics.space_sm))
                .child(section("Scope", &palette, metrics, scope));
            if let (Scope::Collection, Some((_, name))) = (&state.scope, &state.collection) {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(palette.muted)
                        .child(name.clone()),
                );
            }
            body = body.child(section("Format", &palette, metrics, formats));
            if state.scope == Scope::Everything {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(palette.muted)
                        .child("A bundle is the only format that carries environments."),
                );
            }
            body = body.child(
                v_flex().gap(px(metrics.space_xs)).child(secrets).child(
                    div()
                        .text_xs()
                        .text_color(if state.include_secrets {
                            palette.danger
                        } else {
                            palette.muted
                        })
                        .child(if state.include_secrets {
                            "API keys, tokens and secret variables will be written in plain text."
                        } else {
                            "Secrets are redacted; the file can be shared."
                        }),
                ),
            );

            dialog
                .title("Export")
                .w_112()
                .child(body)
                .footer(DialogFooter::new().child(cancel).child(go))
                .on_cancel(|_, window, cx| {
                    window.close_dialog(cx);
                    true
                })
        });
    }

    fn begin_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.export_state.clone();
        let directory = self
            .workspace_root
            .clone()
            .unwrap_or_else(storage::data_dir);
        let suggested = suggested_file_name(&state, &self.workspace_name);
        let picked = cx.prompt_for_new_path(&directory, Some(&suggested));
        let include_secrets = state.include_secrets;
        let store = self.store.clone();

        cx.spawn_in(window, async move |workspace, cx| {
            let path = match picked.await {
                Ok(Ok(Some(path))) => path,
                // Cancelled: nothing to report.
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    workspace
                        .update_in(cx, |this, _window, cx| {
                            this.notice(
                                &Notice::Failed(
                                    format!("Could not open the save dialog: {error}").into(),
                                ),
                                cx,
                            );
                        })
                        .ok();
                    return;
                }
                Err(_) => return,
            };
            let outcome = cx
                .background_spawn(async move {
                    let text = render(store.as_ref(), &state)?;
                    std::fs::write(&path, text)
                        .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
                    Ok::<PathBuf, String>(path)
                })
                .await;
            workspace
                .update_in(cx, |this, _window, cx| {
                    let notice = match outcome {
                        Ok(path) if include_secrets => Notice::Done(
                            format!("Exported {} (with secrets).", path.display()).into(),
                        ),
                        Ok(path) => Notice::Done(
                            format!("Exported {} (secrets redacted).", path.display()).into(),
                        ),
                        Err(message) => Notice::Failed(message.into()),
                    };
                    this.notice(&notice, cx);
                })
                .ok();
        })
        .detach();
    }
}

fn format_id(format: ExportFormat) -> u64 {
    match format {
        ExportFormat::Postman => 0,
        ExportFormat::Har => 1,
        ExportFormat::Bundle => 2,
    }
}

fn section(
    heading: &str,
    palette: &Palette,
    metrics: Metrics,
    content: impl IntoElement,
) -> AnyElement {
    v_flex()
        .gap(px(metrics.space_xs))
        .child(
            div()
                .text_xs()
                .text_color(palette.muted)
                .child(heading.to_owned()),
        )
        .child(content)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Named imports rather than `super::*`: the parent module glob-imports
    // `gpui_kit::*`, and that macro set shadows `#[test]`.
    use super::{render, ExportState, Scope};
    use restbook_core::import::ExportFormat;
    use restbook_core::{
        Auth, Collection, CollectionItem, Environment, Kv, Method, RequestDef, SavedRequest, Store,
    };
    use restbook_store::MemoryStore;

    fn collection(id: &str, name: &str) -> Collection {
        let mut request = RequestDef::new(Method::Get, "https://example.com");
        request.auth = Auth::Bearer {
            token: "bearer-token".to_owned(),
        };
        Collection {
            items: vec![CollectionItem::Request(SavedRequest {
                id: "r1".to_owned(),
                name: "Login".to_owned(),
                request,
            })],
            ..Collection::new(id, name)
        }
    }

    fn selected(id: &str, name: &str) -> ExportState {
        ExportState {
            scope: Scope::Collection,
            format: ExportFormat::Bundle,
            include_secrets: false,
            collection: Some((id.to_owned(), name.to_owned())),
        }
    }

    #[test]
    fn a_single_collection_export_carries_no_other_collection_or_environment() {
        let store = MemoryStore::new();
        store.save_collection(&collection("col-1", "one")).unwrap();
        store.save_collection(&collection("col-2", "two")).unwrap();
        store
            .save_environment(&Environment {
                name: "dev".to_owned(),
                vars: vec![Kv::new("HOST", "https://example.com")],
            })
            .unwrap();

        let text = render(&store, &selected("col-1", "one")).unwrap();
        assert!(text.contains("one"), "{text}");
        assert!(!text.contains("two"), "{text}");
        assert!(!text.contains("dev"), "{text}");
    }

    #[test]
    fn everything_carries_the_environments_and_redacts_by_default() {
        let store = MemoryStore::new();
        store.save_collection(&collection("col-1", "one")).unwrap();
        store
            .save_environment(&Environment {
                name: "dev".to_owned(),
                vars: Vec::new(),
            })
            .unwrap();

        let state = ExportState {
            scope: Scope::Everything,
            ..ExportState::default()
        };
        let text = render(&store, &state).unwrap();
        assert!(text.contains("dev"), "{text}");
        assert!(!text.contains("bearer-token"), "{text}");
        assert!(text.contains(restbook_core::secrets::REDACTED), "{text}");
    }

    #[test]
    fn including_secrets_writes_the_literal_value() {
        let store = MemoryStore::new();
        store.save_collection(&collection("col-1", "one")).unwrap();

        let mut state = selected("col-1", "one");
        state.include_secrets = true;
        let text = render(&store, &state).unwrap();
        assert!(text.contains("bearer-token"), "{text}");
    }

    #[test]
    fn everything_is_a_bundle_whatever_the_format_button_says() {
        let store = MemoryStore::new();
        store.save_collection(&collection("col-1", "one")).unwrap();

        let state = ExportState {
            scope: Scope::Everything,
            format: ExportFormat::Postman,
            ..ExportState::default()
        };
        let text = render(&store, &state).unwrap();
        // A bundle carries the envelope; a Postman export would start with info.
        assert!(text.contains("\"collections\""), "{text}");
    }
}
