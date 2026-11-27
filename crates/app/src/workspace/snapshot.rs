//! Snapshot: the request and its response as a shareable PNG.
//!
//! The image is rasterized from the SVG that [`crate::snapshot`] draws, because a
//! release build cannot read pixels back from its own window. Secrets are redacted
//! unless ticked, and the tick is deliberately not remembered: a remembered
//! "include secrets" is a silent leak on the next snapshot.

use std::path::PathBuf;

use gpui_kit::component::button::*;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::{v_flex, WindowExt as _};
use gpui_kit::*;
use restbook_core::{Auth, RequestDef, ResponseRecord};

use crate::notice::Notice;
use crate::snapshot::{self, clipboard, png, svg};
use crate::theme::{Metrics, Palette};

use super::{export::slug, storage, Workspace};

/// The image is drawn at twice its logical size, so it stays crisp in a chat
/// client that upscales it and in a ticket where it is zoomed.
const SNAPSHOT_SCALE: f32 = 2.0;

/// The scale a tall image falls back to. Doubling a tall response is four times
/// the pixels for no extra legibility, so past the budget it is drawn at 1x.
const SNAPSHOT_TALL_SCALE: f32 = 1.0;

/// The doubled height past which the 2x raster is no longer worth its size.
const SNAPSHOT_MAX_PIXEL_HEIGHT: f32 = 3000.0;

/// A compact snapshot is drawn at 1x and clipped to this box, so its file and
/// clipboard bitmap stay small. The numbers are the PNG's own pixels.
const SNAPSHOT_COMPACT_WIDTH: f32 = 1200.0;
const SNAPSHOT_COMPACT_HEIGHT: f32 = 1200.0;

impl Workspace {
    /// Open the snapshot dialog, or say why there is nothing to snapshot.
    ///
    /// A missing response is not an error state: the request alone is still a
    /// useful thing to attach, so only a request with neither a response nor a
    /// failure is refused.
    pub(crate) fn open_snapshot_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_response = {
            let document = self.document.read(cx);
            document.response.is_some() || document.error.is_some()
        };
        if !has_response {
            self.notice(&Notice::Failed("Send the request first.".into()), cx);
            return;
        }

        self.snapshot_include_secrets = false;

        let workspace = cx.entity();
        let metrics = Metrics::current(cx);
        let palette = Palette::new(cx);

        window.open_dialog(cx, move |dialog, _window, cx| {
            let include_secrets = workspace.read(cx).snapshot_include_secrets;
            let compact = workspace.read(cx).snapshot_compact;

            let secrets = Checkbox::new(("snapshot-include-secrets", 0u64))
                .label("Include secrets")
                .checked(include_secrets)
                .on_click({
                    let workspace = workspace.clone();
                    move |checked, _, cx| {
                        workspace.update(cx, |this, cx| {
                            this.snapshot_include_secrets = *checked;
                            cx.notify();
                        });
                    }
                });

            let hint = div()
                .text_xs()
                .text_color(if include_secrets {
                    palette.danger
                } else {
                    palette.muted
                })
                .child(if include_secrets {
                    "API keys, tokens and secret headers will be drawn in plain text."
                } else {
                    "Secrets are redacted; the image can be shared."
                });

            let compact_toggle = Checkbox::new(("snapshot-compact", 0u64))
                .label("Compact (fits 1200 × 1200 px)")
                .checked(compact)
                .on_click({
                    let workspace = workspace.clone();
                    move |checked, _, cx| {
                        workspace.update(cx, |this, cx| {
                            this.snapshot_compact = *checked;
                            cx.notify();
                        });
                    }
                });

            let cancel = Button::new("snapshot-cancel")
                .label("Cancel")
                .ghost()
                .on_click(|_, window, cx| window.close_dialog(cx));

            let save = Button::new("snapshot-save")
                .label("Save PNG…")
                .ghost()
                .on_click({
                    let workspace = workspace.clone();
                    move |_, window, cx| {
                        workspace.update(cx, |this, cx| this.save_snapshot(window, cx));
                        window.close_dialog(cx);
                    }
                });

            let copy = Button::new("snapshot-copy")
                .label("Copy image")
                .primary()
                .on_click({
                    let workspace = workspace.clone();
                    move |_, window, cx| {
                        workspace.update(cx, |this, cx| this.copy_snapshot(cx));
                        window.close_dialog(cx);
                    }
                });

            dialog
                .title("Snapshot")
                .w_112()
                .child(
                    v_flex()
                        .gap(px(metrics.space_md))
                        .py(px(metrics.space_sm))
                        .child(
                            div()
                                .text_sm()
                                .text_color(palette.foreground)
                                .child("The request as a cURL command, then the response body."),
                        )
                        .child(
                            v_flex()
                                .gap(px(metrics.space_xs))
                                .child(secrets)
                                .child(compact_toggle)
                                .child(hint),
                        ),
                )
                .footer(DialogFooter::new().child(cancel).child(save).child(copy))
                .on_cancel(|_, window, cx| {
                    window.close_dialog(cx);
                    true
                })
        });
    }

    /// Draw the current send and put it on the clipboard as an image.
    pub(crate) fn copy_snapshot(&mut self, cx: &mut Context<Self>) {
        let png = match self.snapshot_png(cx) {
            Ok(png) => png,
            Err(message) => {
                self.notice(&Notice::Failed(message.into()), cx);
                return;
            }
        };
        let notice = match clipboard::copy_image(cx, png) {
            Ok(()) => Notice::Done("Snapshot copied.".into()),
            Err(message) => Notice::Failed(message.into()),
        };
        self.notice(&notice, cx);
    }

    /// Draw the current send and ask where to write the PNG.
    pub(crate) fn save_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let png = match self.snapshot_png(cx) {
            Ok(png) => png,
            Err(message) => {
                self.notice(&Notice::Failed(message.into()), cx);
                return;
            }
        };

        let directory = self
            .workspace_root
            .clone()
            .unwrap_or_else(storage::data_dir);
        let suggested = {
            let document = self.document.read(cx);
            suggested_file_name(
                &document.request,
                document.response.as_ref(),
                chrono::Utc::now(),
            )
        };
        let picked = cx.prompt_for_new_path(&directory, Some(&suggested));

        cx.spawn_in(window, async move |workspace, cx| {
            let path = match picked.await {
                Ok(Ok(Some(path))) => path,
                // Cancelled, or the picker failed: nothing the user asked for
                // happened, so there is nothing worth a notice.
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(_)) => return,
            };
            let outcome = cx
                .background_spawn(async move {
                    std::fs::write(&path, png)
                        .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
                    Ok::<PathBuf, String>(path)
                })
                .await;
            workspace
                .update_in(cx, |this, _window, cx| {
                    let notice = match outcome {
                        Ok(path) => Notice::Done(format!("Saved {}.", path.display()).into()),
                        Err(message) => Notice::Failed(message.into()),
                    };
                    this.notice(&notice, cx);
                })
                .ok();
        })
        .detach();
    }

    /// The PNG bytes for what is on screen right now.
    fn snapshot_png(&self, cx: &App) -> Result<Vec<u8>, String> {
        let document = self.document.read(cx);
        let request = &document.request;
        let mask_values = if self.snapshot_include_secrets {
            Vec::new()
        } else {
            secret_values(request)
        };
        let environment = self
            .variables
            .read(cx)
            .active()
            .map(str::to_owned)
            .unwrap_or_default();

        let built = snapshot::build(
            request,
            document.response.as_ref(),
            document.error.as_ref(),
            &snapshot::Options {
                mask_values: &mask_values,
                environment: &environment,
            },
        );

        let svg = svg::render(&built, &style(cx, self.snapshot_compact));
        let scale = if self.snapshot_compact {
            SNAPSHOT_TALL_SCALE
        } else {
            scale_for(&svg)
        };
        png::render(&svg, scale)
    }
}

/// The name the save prompt starts with: what was requested and when the
/// snapshot was taken, so a folder of snapshots sorts and reads as a log. The
/// status and timing are in the image itself; the file name carries the moment.
fn suggested_file_name(
    request: &RequestDef,
    response: Option<&ResponseRecord>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let mut stem = slug(&format!(
        "{} {}",
        request.method.as_str(),
        request.url.trim()
    ));
    // Enough to name the endpoint, short enough to stay a file name a person
    // can read; `slug` only emits ASCII, so the cut cannot split a character.
    stem.truncate(72);
    let stem = stem.trim_end_matches('-');
    match response {
        Some(_) => format!("{stem}-{}.png", now.format("%Y%m%d-%H%M%S")),
        None => format!("{stem}-failed.png"),
    }
}

/// The values the snapshot must hide. The request keeps `{{variables}}`
/// unresolved, so an environment secret never reaches the image; what can leak
/// is a literal in a secret-flagged row, and the auth block.
fn secret_values(request: &RequestDef) -> Vec<String> {
    let mut values = Vec::new();
    for kv in request
        .params
        .iter()
        .chain(request.headers.iter())
        .filter(|kv| kv.enabled && kv.secret)
    {
        values.push(kv.value.clone());
    }
    match &request.auth {
        Auth::Basic { password, .. } => values.push(password.clone()),
        Auth::Bearer { token } => values.push(token.clone()),
        Auth::ApiKey { value, .. } => values.push(value.clone()),
        // The OAuth token is supplied at send time and never persisted, so
        // there is nothing in the request to hide here.
        Auth::None | Auth::Inherit | Auth::OAuth2 { .. } => {}
    }
    values.retain(|value| !value.is_empty());
    values
}

/// The raster scale for a drawn snapshot: 2x while the doubled image stays
/// within the budget, else 1x. Decided from the SVG's own declared size.
fn scale_for(svg: &str) -> f32 {
    let (_, height) = svg::extent(svg);
    if height * SNAPSHOT_SCALE > SNAPSHOT_MAX_PIXEL_HEIGHT {
        SNAPSHOT_TALL_SCALE
    } else {
        SNAPSHOT_SCALE
    }
}

fn style(cx: &App, compact: bool) -> svg::Style {
    let palette = Palette::new(cx);
    let metrics = Metrics::current(cx);
    let font_size = metrics.font_small.max(11.0);
    svg::Style {
        background: snapshot::hex(
            palette.background.h,
            palette.background.s,
            palette.background.l,
        ),
        foreground: snapshot::hex(
            palette.foreground.h,
            palette.foreground.s,
            palette.foreground.l,
        ),
        muted: snapshot::hex(palette.muted.h, palette.muted.s, palette.muted.l),
        border: snapshot::hex(palette.border.h, palette.border.s, palette.border.l),
        mono: palette.mono_font.to_string(),
        width: 760.0,
        max_width: if compact {
            SNAPSHOT_COMPACT_WIDTH
        } else {
            svg::MAX_WIDTH
        },
        max_height: compact.then_some(SNAPSHOT_COMPACT_HEIGHT),
        padding: 20.0,
        font_size,
        line_height: (font_size * 1.5).round(),
        title_size: metrics.font_body,
    }
}

#[cfg(test)]
mod tests {
    use super::{scale_for, suggested_file_name};
    use chrono::{TimeZone as _, Utc};
    use restbook_core::{Method, RequestDef, ResponseRecord};

    fn captured_at() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 24, 15, 30, 45).unwrap()
    }

    fn responded(status: u16, duration_ms: u64) -> ResponseRecord {
        ResponseRecord {
            status,
            headers: Vec::new(),
            body: Vec::new().into(),
            duration_ms,
            received_at: Default::default(),
            final_url: None,
            redirects: Vec::new(),
        }
    }

    /// The name says what was requested and when it was captured, with nothing
    /// a file system would refuse.
    #[test]
    fn the_file_name_names_the_request_and_the_capture_time() {
        let request = RequestDef::new(Method::Post, "https://api.example.com/users?id=42");
        assert_eq!(
            suggested_file_name(&request, Some(&responded(201, 137)), captured_at()),
            "post-https-api-example-com-users-id-42-20261124-153045.png"
        );
    }

    /// A failed send still gets a name, so its snapshot does not arrive under
    /// the same name as the last one that worked.
    #[test]
    fn a_failed_send_is_named_failed() {
        let request = RequestDef::new(Method::Get, "https://example.com");
        assert_eq!(
            suggested_file_name(&request, None, captured_at()),
            "get-https-example-com-failed.png"
        );
    }

    /// An image that stays small is drawn at 2x, which is what keeps text crisp.
    #[test]
    fn a_small_image_is_drawn_at_twice_the_scale() {
        assert_eq!(scale_for("<svg width=\"760\" height=\"1200\"></svg>"), 2.0);
    }

    /// A tall response falls back to 1x rather than quadrupling its pixels.
    #[test]
    fn a_tall_image_is_drawn_at_half_the_scale() {
        assert_eq!(scale_for("<svg width=\"760\" height=\"4000\"></svg>"), 1.0);
    }

    /// A long endpoint is cut short, and the cut cannot leave the doubled dash
    /// or half-word that a raw `truncate` would.
    #[test]
    fn a_long_url_is_shortened() {
        let request = RequestDef::new(
            Method::Get,
            format!("https://example.com/{}", "a".repeat(200)),
        );
        let name = suggested_file_name(&request, Some(&responded(200, 5)), captured_at());
        assert!(name.len() < 100, "{name}");
        assert!(!name.contains("--"), "{name}");
        assert!(name.ends_with("-20261124-153045.png"), "{name}");
    }
}
