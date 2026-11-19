//! The History pane: every request sent, newest first.
//!
//! The pane owns the store search. The workspace tells it to refresh after a
//! send, and the sidebar reloads from the store; it never holds a copy of the
//! document. Picking a row is reported as an [`Event`] and the workspace
//! decides what a chosen row means.

use std::sync::Arc;

use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use restbook_core::{HistoryHit, Store};

use crate::icons::{self, IconName};
use crate::panels::history_row::{self, HistoryRow};
use crate::theme::{Metrics, Palette};
use crate::workspace::settings::Settings;

pub(crate) enum Event {
    /// The user clicked a history row. The workspace loads the full entry, so
    /// the request comes back with its params, headers, body, and auth intact.
    Picked { id: i64 },
    /// The user asked to forget one send.
    Delete { id: i64 },
    /// The user asked to forget every send.
    ClearAll,
}

/// One line for the interaction log. Exhaustive, so a new variant must name
/// itself before it can go unlogged.
impl Event {
    pub(crate) fn summary(&self) -> String {
        match self {
            Event::Picked { id } => format!("event: history pick id={id}"),
            Event::Delete { id } => format!("event: history delete id={id}"),
            Event::ClearAll => "event: history clear all".to_owned(),
        }
    }
}

pub(crate) struct HistoryPanel {
    palette: Palette,
    metrics: Metrics,
    store: Arc<dyn Store>,
    /// The shared preferences, read for the search limit on every reload. As a
    /// handle, not a copied number, so a change in the settings dialog reaches
    /// the next search without the pane being rebuilt.
    settings: Entity<Settings>,
    hits: Vec<HistoryHit>,
    /// The store's message when the last search failed. The empty state is a
    /// claim — "Nothing sent yet." — that a failed read cannot make, so it is
    /// shown as a failure instead.
    error: Option<SharedString>,
    /// Bumped on every reload request. A search runs on the background executor
    /// and its result is applied only if this still matches the value captured
    /// when the search started, so a slow one cannot paint over a newer query.
    reload_epoch: u64,
    query: Entity<InputState>,
    /// The row the user picked, kept highlighted so the list reads as a list
    /// of choices rather than a wall of rows.
    selected: Option<i64>,
    focus_handle: FocusHandle,
}

impl EventEmitter<Event> for HistoryPanel {}
impl EventEmitter<gpui_kit::component::dock::PanelEvent> for HistoryPanel {}

/// How the dock sees this pane. Behavior only; the tab title is a fmt string.
impl gpui_kit::component::dock::BasePanel for HistoryPanel {
    fn panel_name(&self) -> &'static str {
        "history"
    }
}

/// Presentation half of the dock seam. The sidebar draws its own header, so
/// the dock does not put a one-panel title bar (or a fake tab) above it.
impl gpui_kit::component::dock::Panel for HistoryPanel {
    fn tab_name(&self, _cx: &App) -> Option<SharedString> {
        Some("History".into())
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // A clock, not a gallery: the pane is ordered by when each request was
        // sent, which is the one thing a history row is about.
        icons::title(IconName::Clock, "History", &self.metrics)
    }

    fn title_bar(&self, _cx: &App) -> bool {
        false
    }

    fn inner_padding(&self, _cx: &App) -> bool {
        false
    }

    /// The pane's own items in the dock's ellipsis menu: re-read the log, then
    /// forget one send or all of them. Each verb is the same event the header
    /// and the row buttons emit, so the workspace still owns the confirmation.
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let panel = cx.entity();
        let selected = self.selected;
        let has_hits = !self.hits.is_empty();

        let reload = panel.clone();
        let mut menu = menu.item(PopupMenuItem::new("Refresh").on_click(move |_, _, cx| {
            reload.update(cx, |this, cx| this.reload(cx));
        }));

        menu = menu.separator();
        let delete = panel.clone();
        menu = menu.item(
            PopupMenuItem::new("Delete send")
                .icon(IconName::Trash)
                .disabled(selected.is_none())
                .on_click(move |_, _, cx| {
                    if let Some(id) = selected {
                        delete.update(cx, |_, cx| cx.emit(Event::Delete { id }));
                    }
                }),
        );

        let clear = panel;
        menu.item(
            PopupMenuItem::new("Clear history")
                .icon(IconName::Trash)
                .disabled(!has_hits)
                .on_click(move |_, _, cx| {
                    clear.update(cx, |_, cx| cx.emit(Event::ClearAll));
                }),
        )
    }
}

impl Focusable for HistoryPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl HistoryPanel {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        settings: Entity<Settings>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search history"));
        // The search box notifies its own state, not this pane, so the list
        // has to be told to re-read once per keystroke.
        cx.observe(&query, |this, _, cx| this.reload(cx)).detach();
        // A new search limit only shows on the next search, which is what a
        // settings change should do — it re-caps the list, it does not reorder
        // what is already on screen.
        cx.observe(&settings, |this, _, cx| this.reload(cx))
            .detach();

        let limit = settings.read(cx).current().history_limit;
        let (hits, error) = search(store.as_ref(), "", limit);
        Self {
            palette: Palette::new(cx),
            metrics: Metrics::current(cx),
            store,
            settings,
            hits,
            error,
            reload_epoch: 0,
            query,
            selected: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Rebuild the cached palette and metrics after a theme or zoom change.
    pub(crate) fn refresh_theme(&mut self, cx: &App) {
        self.palette = Palette::new(cx);
        self.metrics = Metrics::current(cx);
    }

    /// Re-read the store and redraw. Called after every successful send, and on
    /// every search-box notify.
    ///
    /// The query is read here, but the search runs on the background executor
    /// behind the store's connection mutex. The epoch makes the last request
    /// win: a result for a superseded query is dropped rather than flashed on.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self.reload_epoch = self.reload_epoch.wrapping_add(1);
        let epoch = self.reload_epoch;
        let query = self.query.read(cx).value().to_string();
        let limit = self.settings.read(cx).current().history_limit;
        let store = self.store.clone();

        cx.spawn(async move |this, cx| {
            let (hits, error) = cx
                .background_spawn(async move { search(store.as_ref(), &query, limit) })
                .await;
            this.update(cx, |panel, cx| {
                if panel.reload_epoch == epoch {
                    panel.hits = hits;
                    panel.error = error;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The list, virtualized: `history_limit` can reach a thousand rows and
    /// every row is one line, so only the visible slice is built. `uniform_list`
    /// needs a `'static` render closure, so the hits are flattened into rows
    /// here and the per-row clicks reach the panel through an entity handle.
    fn render_hits(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;

        // A failed search is not an empty log, and saying "Nothing sent yet."
        // would be a lie the user cannot act on.
        if let Some(error) = &self.error {
            return div()
                .px(px(self.metrics.space_md))
                .py(px(self.metrics.space_sm))
                .text_sm()
                .text_color(palette.danger)
                .child(format!("Couldn’t search history. {error}"))
                .into_any_element();
        }

        if self.hits.is_empty() {
            return div()
                .px(px(self.metrics.space_md))
                .py(px(self.metrics.space_sm))
                .text_sm()
                .text_color(palette.subtle)
                .child("Nothing sent yet.")
                .into_any_element();
        }

        let rows: Vec<HistoryRow> = self.hits.iter().map(HistoryRow::from_hit).collect();

        let palette = palette.clone();
        let panel = cx.entity();
        let metrics = self.metrics;
        let selected = self.selected;

        uniform_list("history-rows", rows.len(), move |range, _, _| {
            range
                .map(|index| {
                    let row = &rows[index];
                    let id = row.id;
                    // Identity and time share the pane: the two-line block keeps
                    // the full stamp and the relative time visible instead of
                    // truncating them against the URL.
                    let delete = Button::new(format!("history-delete-{id}"))
                        .icon(IconName::Trash)
                        .ghost()
                        .compact()
                        .tooltip("Delete from history")
                        .on_click({
                            let panel = panel.clone();
                            move |_, _, app| {
                                app.stop_propagation();
                                panel.update(app, |_, cx| cx.emit(Event::Delete { id }));
                            }
                        })
                        .into_any_element();
                    let panel = panel.clone();
                    history_row::row(
                        format!("history-{id}"),
                        row,
                        &palette,
                        &metrics,
                        selected == Some(id),
                        delete,
                        move |_, _, app| {
                            panel.update(app, |this, cx| {
                                this.selected = Some(id);
                                cx.emit(Event::Picked { id });
                                cx.notify();
                            });
                        },
                    )
                })
                .collect::<Vec<_>>()
        })
        .size_full()
        .into_any_element()
    }
}

/// Search the store, keeping a failure instead of swallowing it: the pane has
/// to know whether an empty list means "no sends" or "the read failed". The
/// log line stays for a bug report; the returned message is what the list
/// shows in the empty state's place.
fn search(store: &dyn Store, query: &str, limit: u32) -> (Vec<HistoryHit>, Option<SharedString>) {
    match store.search_history(query, limit) {
        Ok(hits) => (hits, None),
        Err(error) => {
            log::error!("failed to search history: {error}");
            (Vec::new(), Some(error.to_string().into()))
        }
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette.clone();

        v_flex()
            .size_full()
            .bg(palette.sidebar)
            .track_focus(&self.focus_handle)
            .child(
                h_flex()
                    .h(px(self.metrics.header_height))
                    .flex_none()
                    .items_center()
                    .gap(px(self.metrics.space_xs))
                    .pl(px(self.metrics.space_md))
                    .pr(px(self.metrics.space_sm))
                    .border_b_1()
                    .border_color(palette.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("History"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.subtle)
                            .child(self.hits.len().to_string()),
                    )
                    .when(!self.hits.is_empty(), |style| {
                        style.child(
                            // One click forgets the whole log. It is the only
                            // destructive control here without a dialog, so it
                            // carries a tooltip and sits outside the row flow.
                            Button::new("history-clear-all")
                                .icon(IconName::Trash)
                                .ghost()
                                .compact()
                                .tooltip("Clear all history")
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(Event::ClearAll);
                                })),
                        )
                    }),
            )
            .child(
                h_flex()
                    .flex_none()
                    .px(px(self.metrics.space_sm))
                    .py(px(self.metrics.space_xs))
                    .child(
                        Input::new(&self.query)
                            .id("history-search")
                            .flex_1()
                            .prefix(icons::quiet(IconName::Search, icons::SMALL, &palette)),
                    ),
            )
            .child(
                // `uniform_list` scrolls itself, so this box only owns the
                // pane's share of the height and the padding above the first
                // row.
                v_flex()
                    .id("history-list")
                    .flex_1()
                    .min_h_0()
                    .py(px(self.metrics.space_xs))
                    .child(self.render_hits(cx)),
            )
    }
}
