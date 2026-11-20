//! The Body tab: none, raw, form, multipart, or binary.

use gpui_kit::component::button::*;
use gpui_kit::component::input::{Editor, Input};
use gpui_kit::component::{h_flex, v_flex, Selectable as _};
use gpui_kit::*;

use restbook_core::Body;

use crate::theme::Metrics;

use super::kv_table::{self, RowAction};
use super::parts_table::{self, PartAction};
use super::{language_for_mime, BodyKind, RequestEditor};

/// Raw-body presets. The first entry is the default for a new request. The
/// label is the language Postman-style, not the MIME string: the header is
/// generated from the choice and listed under the Headers tab's default rows,
/// so the body tab does not ask the user to think in header syntax.
pub(crate) const MIME_TYPES: [&str; 4] = [
    "application/json",
    "application/xml",
    "text/plain",
    "text/html",
];

/// The language name shown on a raw-body preset button.
pub(crate) fn preset_label(mime: &str) -> &'static str {
    match mime {
        "application/json" => "JSON",
        "application/xml" => "XML",
        "text/html" => "HTML",
        _ => "Text",
    }
}

impl RequestEditor {
    pub(crate) fn render_body_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let metrics = Metrics::current(cx);
        let kinds = [
            (BodyKind::None, "None"),
            (BodyKind::Raw, "Raw"),
            (BodyKind::Form, "Form"),
            (BodyKind::Multipart, "Multipart"),
            (BodyKind::Binary, "Binary"),
        ];

        let kind_strip = h_flex()
            .gap(px(metrics.space_xs))
            .pb(px(metrics.space_sm))
            .children(kinds.into_iter().map(|(kind, label)| {
                let current = kind == self.body_kind;
                // Keyed on the variant, not its label, so a relabel cannot
                // re-key the control.
                Button::new(format!("body-kind-{kind:?}"))
                    .label(label)
                    .compact()
                    .ghost()
                    .selected(current)
                    .toggled(current)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.body_kind = kind;
                        cx.notify();
                    }))
                    .into_any_element()
            }));

        let content = match self.body_kind {
            BodyKind::None => div()
                .text_sm()
                .text_color(self.palette.muted)
                .child("This request sends no body.")
                .into_any_element(),
            BodyKind::Raw => {
                let mimes = MIME_TYPES.into_iter().map(|mime| {
                    let current = mime == self.raw_mime;
                    Button::new(format!("mime-{mime}"))
                        .label(preset_label(mime))
                        .tooltip(mime)
                        .compact()
                        .ghost()
                        .selected(current)
                        .toggled(current)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.raw_mime = mime.to_owned();
                            // The editor is the same widget across mime types;
                            // only the grammar behind it changes.
                            let language = language_for_mime(&this.raw_mime);
                            this.raw_text
                                .update(cx, |state, cx| state.set_highlighter(language, cx));
                            cx.notify();
                        }))
                        .into_any_element()
                });

                v_flex()
                    .gap(px(metrics.space_sm))
                    .child(h_flex().gap(px(metrics.space_xs)).flex_wrap().children(mimes))
                    .child(
                        div()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(format!(
                                "Sent as {}. The Content-Type is listed under Headers → Default headers.",
                                self.raw_mime
                            )),
                    )
                    // Enter is intercepted by the editor itself (see
                    // `RequestEditor::new`): the kit's binding would insert a
                    // newline before any key-down listener could accept.
                    .child(Editor::new(&self.raw_text).h(px(metrics.body_row_height * 10.)))
                    .into_any_element()
            }
            BodyKind::Form => {
                let on_action = cx.listener(|this, action: &RowAction, window, cx| {
                    let variables = this.variables.clone();
                    this.form.apply(*action, &variables, window, cx);
                    cx.notify();
                });

                v_flex()
                    .gap(px(metrics.space_sm))
                    .child(
                        div()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child("Sent as application/x-www-form-urlencoded."),
                    )
                    .child(kv_table::render(&self.form, &self.palette, cx, on_action))
                    .into_any_element()
            }
            BodyKind::Multipart => {
                let on_action = cx.listener(|this, action: &PartAction, window, cx| {
                    this.parts.apply(*action, window, cx);
                    cx.notify();
                });
                let on_choose = cx.listener(|this, id: &u64, window, cx| {
                    this.choose_part_file(*id, window, cx);
                });

                v_flex()
                    .gap(px(metrics.space_sm))
                    .child(div().text_xs().text_color(self.palette.muted).child(
                        "A part with a file path is a file upload; without one it is a text field.",
                    ))
                    .child(parts_table::render(
                        &self.parts,
                        &self.palette,
                        cx,
                        on_action,
                        on_choose,
                    ))
                    .into_any_element()
            }
            BodyKind::Binary => {
                let on_choose = cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.choose_binary_file(window, cx);
                });

                v_flex()
                    .gap(px(metrics.space_sm))
                    .child(
                        div()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child("Sent as the raw bytes of the file."),
                    )
                    .child(
                        h_flex()
                            .gap(px(metrics.space_sm))
                            .child(Input::new(&self.binary_path).id("binary-path").flex_1())
                            .child(
                                Button::new("binary-choose")
                                    .label("Choose…")
                                    .ghost()
                                    .compact()
                                    .on_click(on_choose),
                            ),
                    )
                    .into_any_element()
            }
        };

        v_flex()
            .gap(px(metrics.space_sm))
            .child(kind_strip)
            .child(content)
            .into_any_element()
    }

    /// Reduce the Body tab's state to a `Body`. The raw text's fallback mime
    /// is applied here, at send time, so the buttons and the payload cannot
    /// disagree.
    pub(crate) fn body(&self, cx: &App) -> Body {
        match self.body_kind {
            BodyKind::None => Body::None,
            BodyKind::Raw => {
                let mime = self.raw_mime.trim();
                Body::Raw {
                    mime: if mime.is_empty() {
                        MIME_TYPES[0].to_owned()
                    } else {
                        mime.to_owned()
                    },
                    text: self.raw_text.read(cx).value().to_string(),
                }
            }
            BodyKind::Form => Body::Form(self.form.to_kv(cx)),
            BodyKind::Multipart => Body::Multipart(self.parts.to_parts(cx)),
            BodyKind::Binary => Body::Binary {
                path: self.binary_path.read(cx).value().trim().to_owned(),
            },
        }
    }

    fn choose_part_file(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        Self::pick_file(window, cx, move |this, path, window, cx| {
            this.parts.set_path(id, path, window, cx);
        });
    }

    fn choose_binary_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        Self::pick_file(window, cx, |this, path, window, cx| {
            this.binary_path
                .update(cx, |state, cx| state.set_value(path, window, cx));
        });
    }

    /// Show the native file picker and write the chosen path through `apply`.
    /// The picker hands back a future, so it is awaited off the click in a task;
    /// the editor is upgraded there, and a released editor has nothing to do.
    fn pick_file(
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut Self, &str, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a file".into()),
        });

        cx.spawn_in(window, async move |editor, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let path = path.to_string_lossy().into_owned();
            editor
                .update_in(cx, |this, window, cx| {
                    apply(this, &path, window, cx);
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }
}
