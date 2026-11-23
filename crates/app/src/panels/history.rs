//! The History pane: every request sent, newest first.
//!
//! The pane owns the store search. The workspace tells it to refresh after a
//! send, and the sidebar reloads from the store; it never holds a copy of the
//! document. Picking a row is reported as an [`Event`] and the workspace
//! decides what a chosen row means.

use std::sync::Arc;

use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{h_flex, v_flex, Selectable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use restbook_core::{HistoryFilter, HistoryHit, Method, Origin, Store};

use crate::document::Document;
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

/// Which sends the list shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Scope {
    /// Every send this store holds.
    #[default]
    All,
    /// Only the sends of the request on screen.
    Request,
}

/// How far back the *When* filter reaches. Presets rather than two date
/// pickers: the dock is narrow, and "since" is what a history scan asks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SentWithin {
    #[default]
    Any,
    Last24Hours,
    Last7Days,
    Last30Days,
}

impl SentWithin {
    fn label(self) -> &'static str {
        match self {
            SentWithin::Any => "Any time",
            SentWithin::Last24Hours => "Last 24 hours",
            SentWithin::Last7Days => "Last 7 days",
            SentWithin::Last30Days => "Last 30 days",
        }
    }

    fn since(self, now: chrono::DateTime<chrono::Utc>) -> Option<chrono::DateTime<chrono::Utc>> {
        match self {
            SentWithin::Any => None,
            SentWithin::Last24Hours => Some(now - chrono::Duration::hours(24)),
            SentWithin::Last7Days => Some(now - chrono::Duration::days(7)),
            SentWithin::Last30Days => Some(now - chrono::Duration::days(30)),
        }
    }
}

const METHODS: [Method; 7] = [
    Method::Get,
    Method::Post,
    Method::Put,
    Method::Patch,
    Method::Delete,
    Method::Head,
    Method::Options,
];

/// The status dimension: `None` is any, otherwise the first digit of a code.
const STATUS_CLASSES: [(Option<u16>, &str); 6] = [
    (None, "Any status"),
    (Some(1), "1xx"),
    (Some(2), "2xx"),
    (Some(3), "3xx"),
    (Some(4), "4xx"),
    (Some(5), "5xx"),
];

pub(crate) struct HistoryPanel {
    palette: Palette,
    metrics: Metrics,
    store: Arc<dyn Store>,
    /// The request on screen, so *This request* knows which one that is.
    document: Entity<Document>,
    /// Which rows the list shows.
    scope: Scope,
    /// `(origin, url, revision)` the scoped list was read at, so a document
    /// tick that changes nothing does not run the store query again.
    scope_key: (Origin, String, u64),
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
    /// The filter row's three dimensions. `None` is "any", so a fresh pane
    /// shows every send and menuless state stays the default.
    method: Option<Method>,
    status_class: Option<u16>,
    sent_within: SentWithin,
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
        document: Entity<Document>,
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
        // A different request changes what *This request* means. Progress ticks
        // notify the document many times per second, so the scope key filters
        // those out before the store is read.
        cx.observe(&document, |this, _, cx| this.sync_scope(cx))
            .detach();

        let limit = settings.read(cx).current().history_limit;
        let (hits, error) = search(
            store.as_ref(),
            Scope::All,
            &Origin::default(),
            "",
            "",
            &HistoryFilter::default(),
            limit,
        );
        Self {
            palette: Palette::new(cx),
            metrics: Metrics::current(cx),
            store,
            document,
            scope: Scope::All,
            scope_key: (Origin::default(), String::new(), 0),
            settings,
            hits,
            error,
            reload_epoch: 0,
            query,
            method: None,
            status_class: None,
            sent_within: SentWithin::Any,
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
        let scope = self.scope;
        let filter = self.history_filter();
        let key = self.current_scope_key(cx);
        self.scope_key = key.clone();

        cx.spawn(async move |this, cx| {
            let (hits, error) = cx
                .background_spawn(async move {
                    search(
                        store.as_ref(),
                        scope,
                        &key.0,
                        &key.1,
                        &query,
                        &filter,
                        limit,
                    )
                })
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

    /// Show only the sends of the request on screen. Called when the response
    /// pane asks for its earlier sends: the pane is already open, so the scope
    /// is the whole answer.
    pub(crate) fn scope_to_request(&mut self, cx: &mut Context<Self>) {
        self.scope = Scope::Request;
        self.reload(cx);
        cx.notify();
    }

    /// The store-side shape of the filter row. Built on every search, so a
    /// preset like *Last 7 days* always counts from the moment of the reload.
    fn history_filter(&self) -> HistoryFilter {
        HistoryFilter {
            methods: self
                .method
                .map(|method| vec![method.as_str().to_owned()])
                .unwrap_or_default(),
            status_classes: self.status_class.into_iter().collect(),
            since: self.sent_within.since(chrono::Utc::now()),
            until: None,
        }
    }

    fn filters_active(&self) -> bool {
        self.method.is_some() || self.status_class.is_some() || self.sent_within != SentWithin::Any
    }

    /// What *This request* is scoped to, plus the revision that invalidates it.
    fn current_scope_key(&self, cx: &App) -> (Origin, String, u64) {
        let document = self.document.read(cx);
        (
            document.origin.clone(),
            document.url(),
            document.history_revision,
        )
    }

    /// A document notify is not always a different request; only re-read when
    /// the scope key says it is.
    fn sync_scope(&mut self, cx: &mut Context<Self>) {
        if self.scope != Scope::Request {
            return;
        }
        let key = self.current_scope_key(cx);
        if key != self.scope_key {
            self.scope_key = key;
            self.reload(cx);
        }
    }

    /// The list, virtualized: `history_limit` can reach a thousand rows and
    /// every row is one line, so only the visible slice is built. `uniform_list`
    /// needs a `'static` render closure, so the hits are flattened into rows
    /// here and the per-row clicks reach the panel through an entity handle.
    /// One half of the scope control. Selected styling and the toggle state come
    /// from the button, so it reads as one choice, not two commands.
    fn scope_button(
        &self,
        id: &'static str,
        label: &'static str,
        scope: Scope,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.scope == scope;
        Button::new(id)
            .label(label)
            .compact()
            .ghost()
            .selected(current)
            .toggled(current)
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.scope == scope {
                    return;
                }
                this.scope = scope;
                this.reload(cx);
                cx.notify();
            }))
            .into_any_element()
    }

    /// The filter control. One button, one menu: the dock can be 100 px wide,
    /// so three pickers would not fit, and the checked items are the visible
    /// state. Selecting a row replaces that dimension's choice.
    fn filter_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let panel = cx.entity();
        let method = self.method;
        let status = self.status_class;
        let span = self.sent_within;
        let active = self.filters_active();

        let menu = move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            menu = menu.item(PopupMenuItem::new("Method").disabled(true));
            for choice in [None].into_iter().chain(METHODS.into_iter().map(Some)) {
                let label = choice.map_or("Any method", Method::as_str);
                let panel = panel.clone();
                menu = menu.item(
                    PopupMenuItem::new(label)
                        .checked(method == choice)
                        .on_click(move |_, _, cx| {
                            panel.update(cx, |this, cx| {
                                this.method = choice;
                                this.reload(cx);
                                cx.notify();
                            });
                        }),
                );
            }

            menu = menu.separator();
            menu = menu.item(PopupMenuItem::new("Status").disabled(true));
            for (choice, label) in STATUS_CLASSES {
                let panel = panel.clone();
                menu = menu.item(
                    PopupMenuItem::new(label)
                        .checked(status == choice)
                        .on_click(move |_, _, cx| {
                            panel.update(cx, |this, cx| {
                                this.status_class = choice;
                                this.reload(cx);
                                cx.notify();
                            });
                        }),
                );
            }

            menu = menu.separator();
            menu = menu.item(PopupMenuItem::new("When").disabled(true));
            for choice in [
                SentWithin::Any,
                SentWithin::Last24Hours,
                SentWithin::Last7Days,
                SentWithin::Last30Days,
            ] {
                let panel = panel.clone();
                menu = menu.item(
                    PopupMenuItem::new(choice.label())
                        .checked(span == choice)
                        .on_click(move |_, _, cx| {
                            panel.update(cx, |this, cx| {
                                this.sent_within = choice;
                                this.reload(cx);
                                cx.notify();
                            });
                        }),
                );
            }

            menu.separator().item(
                PopupMenuItem::new("Clear filters")
                    .disabled(!active)
                    .on_click({
                        let panel = panel.clone();
                        move |_, _, cx| {
                            panel.update(cx, |this, cx| {
                                this.method = None;
                                this.status_class = None;
                                this.sent_within = SentWithin::Any;
                                this.reload(cx);
                                cx.notify();
                            });
                        }
                    }),
            )
        };

        Button::new("history-filter")
            .icon(IconName::ListFilter)
            .ghost()
            .compact()
            .selected(active)
            .tooltip("Filter history")
            .dropdown_menu(menu)
            .into_any_element()
    }

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
            let message = if self.scope == Scope::Request && self.document.read(cx).url().is_empty()
            {
                "No request open."
            } else if self.filters_active() {
                "No sends match these filters."
            } else {
                match self.scope {
                    Scope::All => "Nothing sent yet.",
                    Scope::Request => "Nothing sent for this request.",
                }
            };
            return div()
                .px(px(self.metrics.space_md))
                .py(px(self.metrics.space_sm))
                .text_sm()
                .text_color(palette.subtle)
                .child(message)
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
fn search(
    store: &dyn Store,
    scope: Scope,
    origin: &Origin,
    url: &str,
    query: &str,
    filter: &HistoryFilter,
    limit: u32,
) -> (Vec<HistoryHit>, Option<SharedString>) {
    let result = match scope {
        Scope::All => store.search_history(query, filter, limit),
        // The owner's rows are the request's sends; the query box still filters
        // those rows, but it cannot widen the list past the owner.
        Scope::Request => {
            scoped_rows(store, origin, url, filter, limit).map(|hits| filter_by_query(hits, query))
        }
    };
    match result {
        Ok(hits) => (hits, None),
        Err(error) => {
            log::error!("failed to search history: {error}");
            (Vec::new(), Some(error.to_string().into()))
        }
    }
}

/// The rows that belong to one request.
///
/// A saved request or a draft owns its rows, so it is asked for by owner: two
/// requests with the same URL (a copy, or the same endpoint filed twice) must
/// not share a send log. `Standalone` has no owner, so the URL is the best
/// scope available; a backend that cannot answer by owner (`None`) gets the
/// same fallback. One implementation, so the History pane and the response
/// pane's “Earlier sends” count cannot disagree.
pub(crate) fn scoped_rows(
    store: &dyn Store,
    origin: &Origin,
    url: &str,
    filter: &HistoryFilter,
    limit: u32,
) -> restbook_core::Result<Vec<HistoryHit>> {
    let scoped = if *origin == Origin::Standalone {
        None
    } else {
        store.history_for_origin(origin, filter, limit)?
    };
    match scoped {
        Some(hits) => Ok(hits),
        None if url.is_empty() => Ok(Vec::new()),
        None => store.search_history(url, filter, limit),
    }
}

fn filter_by_query(hits: Vec<HistoryHit>, query: &str) -> Vec<HistoryHit> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return hits;
    }
    hits.into_iter()
        .filter(|hit| hit.url.to_lowercase().contains(&query))
        .collect()
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
                v_flex()
                    .flex_none()
                    .gap(px(self.metrics.space_xs))
                    .px(px(self.metrics.space_sm))
                    .py(px(self.metrics.space_xs))
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(self.metrics.space_xs))
                            .child(
                                Input::new(&self.query)
                                    .id("history-search")
                                    .flex_1()
                                    .prefix(icons::quiet(IconName::Search, icons::SMALL, &palette)),
                            )
                            .child(self.filter_button(cx)),
                    )
                    .child(
                        h_flex()
                            .gap(px(self.metrics.space_xs))
                            .child(self.scope_button("history-scope-all", "All", Scope::All, cx))
                            .child(self.scope_button(
                                "history-scope-request",
                                "This request",
                                Scope::Request,
                                cx,
                            )),
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

#[cfg(test)]
mod tests {
    use restbook_core::{
        HistoryEntry, HistoryFilter, Method, Origin, RequestDef, ResponseRecord, Store,
    };
    use restbook_store::MemoryStore;

    use super::scoped_rows;

    fn seed(store: &MemoryStore, url: &str, origin: Origin) {
        store
            .append_history(&HistoryEntry {
                id: 0,
                request: RequestDef::new(Method::Get, url),
                response: ResponseRecord {
                    status: 200,
                    headers: Vec::new(),
                    body: Vec::new().into(),
                    duration_ms: 0,
                    received_at: Default::default(),
                    final_url: None,
                    redirects: Vec::new(),
                },
                url: url.to_owned(),
                origin,
            })
            .expect("the in-memory store accepts history");
    }

    /// Two requests with the same URL — a copy, or the same endpoint filed
    /// twice — must not share a send log. The owner is the key, not the URL.
    #[test]
    fn the_request_scope_lists_only_the_owners_sends() {
        let store = MemoryStore::new();
        let owner = Origin::Saved {
            node: "node-a".to_owned(),
        };
        seed(&store, "https://example.com/a", owner.clone());
        seed(&store, "https://example.com/a", owner.clone());
        seed(
            &store,
            "https://example.com/a",
            Origin::Saved {
                node: "node-b".to_owned(),
            },
        );

        let rows = scoped_rows(
            &store,
            &owner,
            "https://example.com/a",
            &HistoryFilter::default(),
            20,
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
    }

    /// A standalone request has no owner, so the URL is the best scope; no URL
    /// at all means there is nothing to scope to.
    #[test]
    fn a_standalone_request_falls_back_to_its_url() {
        let store = MemoryStore::new();
        seed(&store, "https://example.com/a", Origin::Standalone);
        seed(&store, "https://example.com/b", Origin::Standalone);

        let rows = scoped_rows(
            &store,
            &Origin::Standalone,
            "https://example.com/a",
            &HistoryFilter::default(),
            20,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(scoped_rows(
            &store,
            &Origin::Standalone,
            "",
            &HistoryFilter::default(),
            20,
        )
        .unwrap()
        .is_empty());
    }
}
