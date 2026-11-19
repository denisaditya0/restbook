//! The per-request history sheet: the prior sends of the request on screen,
//! slid in over the window.
//!
//! A canvas-level overlay, not a pane: the response pane reports the request, the
//! workspace opens the sheet, and only the canvas may place a modal. A dropdown
//! would clip its last rows, so the list slides in; a pick restores and closes it.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{h_flex, v_flex, Placement, WindowExt as _};
use gpui_kit::*;

use restbook_core::{HistoryHit, Origin, Store};

use crate::document::Document;
use crate::icons::{self, IconName};
use crate::panels::history_row::{self, HistoryRow};
use crate::theme::{Metrics, Palette};
use crate::workspace::Workspace;

/// Open the history sheet at the window's right edge. The rows are the ones the
/// response pane already searched, so opening it never hits the store.
///
/// `origin` is who owns the rows. A delete made here is reported through the
/// workspace so the sidebar and the response pane re-read the store; the sheet
/// itself closes after a change, since it is a snapshot of the rows it was opened
/// with.
pub(crate) fn open(
    hits: Vec<HistoryHit>,
    origin: Origin,
    document: Entity<Document>,
    store: Arc<dyn Store>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_sheet_at(Placement::Right, cx, move |sheet, _window, cx| {
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);
        let rows = hits
            .iter()
            .map(|hit| row(hit, &palette, &metrics, &document, &store, &workspace))
            .collect::<Vec<_>>();

        // An owned request's rows are a set the workspace can address by owner;
        // a `Standalone` request's rows are only a URL search, so there is no
        // owner to clear and the control is omitted.
        let clear = match &origin {
            Origin::Saved { .. } | Origin::Draft { .. } => {
                let workspace = workspace.clone();
                let origin = origin.clone();
                Some(
                    // A `Button` gives the command an accessible name and a
                    // keyboard path the hand-built glyph never had.
                    Button::new("history-sheet-clear")
                        .icon(IconName::Trash)
                        .ghost()
                        .compact()
                        .tooltip("Clear history")
                        .on_click(move |_, window, cx| {
                            workspace
                                .update(cx, |workspace, cx| {
                                    workspace.confirm_clear_history_for_origin(origin.clone(), cx)
                                })
                                .ok();
                            window.close_sheet(cx);
                        }),
                )
            }
            Origin::Standalone => None,
        };

        sheet
            .title(
                h_flex()
                    .gap(px(metrics.space_sm))
                    .items_center()
                    .child(icons::title(IconName::Clock, "History", &metrics))
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.subtle)
                            .child(hits.len().to_string()),
                    )
                    .children(clear),
            )
            // The sheet's body has no padding of its own: each row carries the
            // same `space_md` inset as the History pane, so the two lists share
            // one leading edge and one vertical rhythm. The width scales with
            // zoom because the row does.
            .size(px(440. * crate::theme::zoom(cx)))
            .px_0()
            .child(v_flex().children(rows))
    });
}

/// One stored send: the same row the History pane draws, with the sheet's
/// restore and delete actions.
fn row(
    hit: &HistoryHit,
    palette: &Palette,
    metrics: &Metrics,
    document: &Entity<Document>,
    store: &Arc<dyn Store>,
    workspace: &WeakEntity<Workspace>,
) -> AnyElement {
    let info = HistoryRow::from_hit(hit);
    let id = info.id;
    let document = document.clone();
    let store = store.clone();

    let delete = {
        let workspace = workspace.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| workspace.confirm_history_send(id, cx))
                .ok();
            window.close_sheet(cx);
        }
    };

    let action = Button::new(format!("history-sheet-delete-{id}"))
        .icon(IconName::Trash)
        .ghost()
        .compact()
        .tooltip("Delete from history")
        .on_click(delete)
        .into_any_element();

    history_row::row(
        format!("history-sheet-{id}"),
        &info,
        palette,
        metrics,
        false,
        action,
        move |_, window, cx| {
            restore(&document, store.as_ref(), id, cx);
            window.close_sheet(cx);
        },
    )
}

/// Show a stored response in place of the live one. The store keeps the
/// response with the request that produced it, so restoring is a document
/// write and no send.
fn restore(document: &Entity<Document>, store: &dyn Store, id: i64, cx: &mut App) {
    log::info!(target: "restbook_app::ui", "response history: restore id={id}");
    match store.get_history(id) {
        Ok(Some(entry)) => {
            log::info!(
                target: "restbook_app::ui",
                "response history: showing {} {} response ({})",
                entry.request.method,
                entry.request.url,
                entry.response.status,
            );
            document.update(cx, |document, cx| {
                document.response = Some(entry.response);
                document.error = None;
                cx.notify();
            });
        }
        Ok(None) => {
            // The row vanished between the search and the click (for example, a
            // store replaced on disk). Nothing to restore.
            log::warn!(target: "restbook_app::ui", "response history: id={id} vanished");
        }
        Err(error) => {
            log::error!(target: "restbook_app::ui", "response history: id={id} failed: {error}");
            document.update(cx, |document, cx| {
                document.error = Some(error.to_string());
                cx.notify();
            });
        }
    }
}
