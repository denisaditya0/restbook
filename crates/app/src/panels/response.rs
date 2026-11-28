//! The Response pane: a status summary over tabs for the body, headers, and
//! cookies.
//!
//! The pane owns no response; it observes the shared `Document` and redraws when
//! a send finishes. Sending lives entirely in the request pane and the workspace,
//! so this pane only reads.

mod cookies;
mod display;
mod failure;
mod find;

use cookies::CookieEdit;
use display::*;

use gpui_kit::base::{
    TextSelection, TextSelectionHandle, TextSelectionRegistration, TextSelectionRun,
};
use gpui_kit::component::button::*;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{TabBar, TabVariant};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _};
use gpui_kit::*;

use restbook_core::{HistoryHit, Origin, ResponseRecord, Store, Transport};

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::actions::{shortcut, FocusUrl, NewTab, SendRequest, ToggleSidebar};
use crate::document::{Document, ResponseState};
// The full Lucide catalog, not the component's default list: `ClipboardCopy` is
// not in the default bundle and would paint nothing.
use crate::icons::IconName;
use crate::theme::{Metrics, Palette};
use crate::util;
use crate::workspace::settings::Settings;

use super::empty::{self, Action};

/// Past this many lines the numbered rows stop being worth it: a quarter-megabyte
/// of blank lines is not a line count anyone reads, so the body becomes one
/// plain block instead.
const MAX_NUMBERED_LINES: usize = 3000;

/// How many prior sends of the current request the Earlier sends control lists.
const HISTORY_ROWS: u32 = 20;

/// What the pane asks the workspace to do for it.
///
/// The per-request history is a window-level overlay, and a pane is not allowed
/// to place one — that is the canvas's call. So the pane reports the intent and
/// the workspace reveals the History pane, scoped to this request.
pub(crate) enum Event {
    /// Show the prior sends of the request on screen: the workspace scopes the
    /// History pane to this request and reveals it.
    ShowHistory,
    /// Send the request on screen again after a failure.
    Retry,
    /// Draw the request and its response as a PNG for a ticket or a chat. The
    /// workspace owns the dialog, so the pane only asks.
    Snapshot,
    /// Forget one cookie in the shared jar.
    DeleteCookie { name: String },
    /// Replace one cookie's value in the shared jar.
    UpdateCookie { name: String, value: String },
    /// Flip the centre between stacked and side by side. The canvas owns the
    /// split and the orientation state, so the pane only reports the intent.
    ToggleSplit,
}

/// One line for the interaction log. Exhaustive, so a new variant must name
/// itself before it can go unlogged.
impl Event {
    pub(crate) fn summary(&self) -> String {
        match self {
            Event::ShowHistory => "event: response history".to_owned(),
            Event::Retry => "event: response retry".to_owned(),
            Event::Snapshot => "event: response snapshot".to_owned(),
            Event::DeleteCookie { name } => format!("event: response cookie delete {name}"),
            Event::UpdateCookie { name, .. } => {
                format!("event: response cookie update {name}")
            }
            Event::ToggleSplit => "event: response toggle split".to_owned(),
        }
    }
}

/// The split toggle's glyph and tooltip name the action the click leads to:
/// showing the current state would offer the wrong verb.
fn split_toggle_action(side_by_side: bool) -> (IconName, &'static str) {
    if side_by_side {
        (IconName::Rows3, "Stack request over response")
    } else {
        (IconName::Columns2, "Split side by side")
    }
}

/// Which view of the response is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseTab {
    Body,
    Headers,
    Cookies,
}

/// Per-tab state the pane restores when a tab comes back: the selected response
/// tab and the first visible body row. Keyed by the tab's document id.
#[derive(Clone, Copy)]
struct ViewState {
    tab: ResponseTab,
    scroll_item: Option<usize>,
}

impl ResponseTab {
    const ALL: [ResponseTab; 3] = [
        ResponseTab::Body,
        ResponseTab::Headers,
        ResponseTab::Cookies,
    ];

    fn label(self) -> &'static str {
        match self {
            ResponseTab::Body => "Body",
            ResponseTab::Headers => "Headers",
            ResponseTab::Cookies => "Cookies",
        }
    }
}

pub(crate) struct ResponsePanel {
    palette: Palette,
    metrics: Metrics,
    document: Entity<Document>,
    /// The document observer, replaced when the active tab changes.
    document_sub: Option<Subscription>,
    /// The history store, so the header can list the prior sends of the
    /// request whose response is showing.
    store: Arc<dyn Store>,
    /// The shared session jar, read for the Cookies tab. Mutations go through
    /// the workspace, which owns the transport.
    transport: Arc<dyn Transport>,
    /// The cookie row whose value is being edited, with its field. `None` when
    /// every row is read-only.
    cookie_edit: Option<CookieEdit>,
    /// The cached rows behind the history control. Refreshed when the request
    /// or the response changes; see [`ResponsePanel::sync_history`].
    history: Vec<HistoryHit>,
    /// The query `history` was searched with, so an unrelated document change
    /// (byte progress, a pending flag) does not hit the store again.
    history_query: String,
    /// The saved request or draft `history` belongs to. `Standalone` means the
    /// request has no owner yet, so the URL is the best scope available.
    history_origin: Origin,
    /// The newest stored response shown, so a completed send of the same URL
    /// still refreshes the list.
    history_stamp: Option<String>,
    /// The document's history revision the cached rows were read at. A delete
    /// made in another pane bumps it, which is the only signal this pane gets
    /// that its list is stale without a send.
    history_revision: u64,
    /// Bumped on every read and on every document switch. A background read is
    /// applied only if this still matches, so a result for a request already
    /// left cannot paint over the new tab's rows.
    history_epoch: u64,
    /// Pretty-print JSON payloads. On by default; the format dropdown toggles
    /// it and names what "pretty" means for this payload.
    pretty: bool,
    tab: ResponseTab,
    /// The last decoded body, with the `(body ptr, len, pretty)` it was derived
    /// from. Decoding pretty-prints JSON, which is far too much work to redo on
    /// every frame — a scroll alone repaints continuously. A new send replaces
    /// the record, so the pointer changes and the cache misses.
    display: Option<DisplayCache>,
    /// The horizontal scroll of the `Body | Headers | Cookies` strip. The tabs
    /// stay inside the header band at a narrow pane instead of pushing the
    /// actions beside them out of the window.
    tabs_scroll: ScrollHandle,
    /// The vertical scroll of the numbered body rows, so stepping to a match can
    /// bring its row into view.
    body_scroll: UniformListScrollHandle,
    /// The find query and its field (U56). The row is open while `find_open`,
    /// or while the field still holds a query after a restore.
    find: Entity<InputState>,
    find_open: bool,
    /// Matches for `query` in the body rows, recomputed when the query or the
    /// decoded body changes rather than per frame. `(row, byte offset in row)`
    /// for each match start, plus the index the arrows are centred on.
    finding: Option<Finding>,
    /// Whether a failure's Details are disclosed, and the log loaded when they
    /// were opened. Loaded on demand, not on every paint.
    failure_details: bool,
    failure_log: Option<String>,
    /// `true` once the failure view has been painted, so the perf probe logs a
    /// frame and not every repaint.
    failure_painted: bool,
    /// The [`Document::paint_key`] the last redraw was given. A send that
    /// resolves without a response leaves the query and the history stamp
    /// untouched, so this is what tells the pane its in-flight strip has
    /// become a failure view.
    painted: (ResponseState, Option<String>),
    /// The selected tab and scroll of each tab, so switching back restores what
    /// the user was looking at instead of resetting to Body at the top.
    view: HashMap<EntityId, ViewState>,
    /// The centre's current split, pushed by the workspace like the tab list:
    /// the pane draws the toggle's next-action glyph from it, but the workspace
    /// owns the orientation and decides what the click does.
    center_side_by_side: bool,
    focus_handle: FocusHandle,
}

/// A single painted line that participates in the window's text selection while
/// keeping the response body's syntax spans.
///
/// [`gpui_kit::base::SelectableText`] owns its `StyledText`, leaving no seam for
/// the highlighter's runs; this recreates just that seam — a retained
/// [`TextSelectionHandle`] and per-frame [`TextSelectionRegistration`] — so the gutter and virtualized rows survive.
struct SelectableLine {
    id: ElementId,
    /// The exact string `styled` was laid out from. Selection indices and the
    /// copied substring both refer to this string, never to the gutter.
    text: SharedString,
    styled: StyledText,
    /// This line's place among the rows of one response. The window selection
    /// orders participants by it, which is what makes a drag across several
    /// lines copy those lines in reading order instead of paint order.
    document_order: u64,
    /// The wash behind the selection. Kept on the element because it is part
    /// of the theme budget the panel cached, not a per-glyph highlight.
    selection_color: Hsla,
}

impl SelectableLine {
    fn new(
        id: impl Into<ElementId>,
        text: SharedString,
        styled: StyledText,
        document_order: u64,
        selection_color: Hsla,
    ) -> Self {
        Self {
            id: id.into(),
            text,
            styled,
            document_order,
            selection_color,
        }
    }
}

impl Element for SelectableLine {
    type RequestLayoutState = TextSelectionHandle;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // One handle per row, retained by the row's element id exactly like
        // `SelectableText` retains its own. A shared handle would work for one
        // line but each row paints its own run, so sharing would leave the
        // last-painted line's text in the handle's copy projection.
        let handle = window.with_element_state(
            global_id.expect("SelectableLine must have a stable element id"),
            |retained: Option<TextSelectionHandle>, _| {
                let handle =
                    retained.unwrap_or_else(|| TextSelectionHandle::new(self.text.clone(), cx));
                (handle.clone(), handle)
            },
        );
        let (layout_id, ()) = self
            .styled
            .request_layout(global_id, inspector_id, window, cx);
        (layout_id, handle)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        handle: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.styled
            .prepaint(global_id, inspector_id, bounds, &mut (), window, cx);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let registration = TextSelectionRegistration::new(hitbox.clone(), bounds)
            .with_document_order(self.document_order)
            .with_text_bounds(vec![bounds])
            .with_rendered_element(handle, window, cx);
        // Pre-registration is what lets the window's gesture code see this row
        // before the frame is painted, so a drag that starts here has geometry
        // to work from on its first move.
        handle.register(registration, window, cx);
        hitbox
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        handle: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.styled.layout().clone();
        // The projection only knows the runs reported this frame. Reporting it
        // before painting also refreshes the selected string the copy action
        // reads, so a copy never lags the visible selection by a frame.
        let before = TextSelection::selected_text(window, cx);
        let projection = handle.update_runs(
            &[
                TextSelectionRun::new(self.text.clone(), layout.clone(), bounds)
                    .with_document_order(self.document_order),
            ],
            cx,
        );
        if before != TextSelection::selected_text(window, cx) {
            window.refresh();
        }

        for range in projection.ranges().iter().flatten().cloned() {
            paint_selection(&layout, range, self.selection_color, window);
        }
        self.styled.paint(
            global_id,
            inspector_id,
            bounds,
            &mut (),
            &mut (),
            window,
            cx,
        );
    }
}

/// Paint the wash behind one selected byte range of a laid-out line.
///
/// A body row is nowrap, so the common case is a single quad; the wrapped case
/// below keeps the summary and any future wrapped caller correct without a
/// second implementation.
fn paint_selection(layout: &TextLayout, range: Range<usize>, color: Hsla, window: &mut Window) {
    let (Some(start), Some(end)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return;
    };
    for bounds in selection_quad_bounds(start, end, layout.bounds(), layout.line_height()) {
        window.paint_quad(PaintQuad {
            bounds,
            background: color.into(),
            corner_radii: Corners::default(),
            border_widths: Edges::default(),
            border_color: transparent_black(),
            border_style: BorderStyle::default(),
        });
    }
}

fn selection_quad_bounds(
    start: Point<Pixels>,
    end: Point<Pixels>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Vec<Bounds<Pixels>> {
    if start.y == end.y {
        return vec![Bounds::from_corners(
            start,
            Point::new(end.x, end.y + line_height),
        )];
    }

    let mut quads = vec![Bounds::from_corners(
        start,
        Point::new(bounds.right(), start.y + line_height),
    )];
    if end.y > start.y + line_height {
        quads.push(Bounds::from_corners(
            Point::new(bounds.left(), start.y + line_height),
            Point::new(bounds.right(), end.y),
        ));
    }
    quads.push(Bounds::from_corners(
        Point::new(bounds.left(), end.y),
        Point::new(end.x, end.y + line_height),
    ));
    quads
}

impl EventEmitter<Event> for ResponsePanel {}
impl EventEmitter<gpui_kit::component::dock::PanelEvent> for ResponsePanel {}

impl gpui_kit::component::dock::BasePanel for ResponsePanel {
    fn panel_name(&self) -> &'static str {
        "response"
    }
}

/// Presentation half of the dock seam. The summary line is this pane's own
/// header, so the dock adds no title bar of its own.
impl gpui_kit::component::dock::Panel for ResponsePanel {
    fn tab_name(&self, _cx: &App) -> Option<SharedString> {
        Some("Response".into())
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child("Response")
    }

    fn title_bar(&self, _cx: &App) -> bool {
        false
    }

    /// The pane's own header is the top edge of the response; the group's
    /// padding would leave a band of empty space above it.
    fn inner_padding(&self, _cx: &App) -> bool {
        false
    }

    /// The pane's own items in the dock's ellipsis menu: the two things the
    /// header's copy buttons do, and the earlier-sends control. Each one is
    /// disabled until there is something to act on, so the menu only ever
    /// offers a copy of a response that exists.
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let panel = cx.entity();
        let response = self.document.read(cx).response.clone();
        let summary = response
            .as_ref()
            .map(|response| summary_for(response, &self.palette).0);
        // The cached body is built in the pane's own pretty mode, so the menu
        // copies what the Body tab shows, not the raw bytes.
        let body = response
            .as_ref()
            .map(|response| self.body_for_display(response, window, cx).0);
        let has_history = !self.history.is_empty();

        let copy_body = panel.clone();
        let mut menu = menu.item(
            PopupMenuItem::new("Copy body as displayed")
                .icon(IconName::ClipboardCopy)
                .disabled(body.is_none())
                .on_click(move |_, _, cx| {
                    if let Some(body) = body.clone() {
                        copy_body.update(cx, |_, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(String::from(body)))
                        });
                    }
                }),
        );

        let copy_summary = panel.clone();
        menu = menu.item(
            PopupMenuItem::new("Copy status, time, and size")
                .icon(IconName::Copy)
                .disabled(summary.is_none())
                .on_click(move |_, _, cx| {
                    if let Some(summary) = summary.clone() {
                        copy_summary.update(cx, |_, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(summary))
                        });
                    }
                }),
        );

        menu = menu.separator();
        let history = panel;
        menu.item(
            PopupMenuItem::new("Earlier sends\u{2026}")
                .icon(IconName::Clock)
                .disabled(!has_history)
                .on_click(move |_, _, cx| {
                    history.update(cx, |_, cx| cx.emit(Event::ShowHistory));
                }),
        )
    }
}

impl Focusable for ResponsePanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ResponsePanel {
    pub(crate) fn new(
        document: Entity<Document>,
        store: Arc<dyn Store>,
        transport: Arc<dyn Transport>,
        settings: Entity<Settings>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // A new response or URL is also the moment the Earlier sends count may
        // change.
        let document_sub = Some(cx.observe(&document, |this, _, cx| this.sync_history(cx)));
        // The pane's pretty-print start state is an app-level preference. It is
        // only a *start* state: the format dropdown below still overrides it per
        // response, so a settings change flips what the next body opens as, not
        // a body the user already chose a view for.
        cx.observe(&settings, |this, settings, cx| {
            this.pretty = settings.read(cx).current().pretty_responses;
            this.display = None;
            cx.notify();
        })
        .detach();

        let pretty = settings.read(cx).current().pretty_responses;
        let find = cx.new(|cx| InputState::new(window, cx).placeholder("Find in response"));
        // The field notifies its own state, not this pane, so the matches have
        // to be recomputed once per keystroke rather than on every paint.
        cx.observe(&find, |this, _, cx| {
            this.refresh_finding(cx);
            cx.notify();
        })
        .detach();
        // Enter and Shift+Enter are the field's own gestures: the input emits
        // PressEnter before anything outside it sees the key, so the step is
        // handled here rather than through a global binding that would fight the
        // input for it.
        cx.subscribe(&find, |this, _, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { shift, .. } = event {
                this.step_match(!shift, cx);
            }
        })
        .detach();
        let mut panel = Self {
            palette: Palette::new(cx),
            metrics: Metrics::current(cx),
            document,
            document_sub,
            store,
            transport,
            cookie_edit: None,
            history: Vec::new(),
            history_query: String::new(),
            history_origin: Origin::default(),
            history_stamp: None,
            history_revision: 0,
            history_epoch: 0,
            pretty,
            tab: ResponseTab::Body,
            display: None,
            tabs_scroll: ScrollHandle::new(),
            body_scroll: UniformListScrollHandle::new(),
            find,
            find_open: false,
            finding: None,
            failure_details: false,
            failure_log: None,
            failure_painted: false,
            painted: (ResponseState::Idle, None),
            view: HashMap::new(),
            center_side_by_side: false,
            focus_handle: cx.focus_handle(),
        };
        panel.sync_history(cx);
        panel
    }

    /// Rebuild the cached palette after a theme or zoom change.
    pub(crate) fn refresh_theme(&mut self, cx: &mut App) {
        self.palette = Palette::new(cx);
        self.metrics = Metrics::current(cx);
        // Highlight colours were resolved once into the display cache; a theme
        // change has to discard them so the next draw uses the new syntax theme.
        self.display = None;
    }

    /// Take the centre's orientation from the workspace, which owns it. The
    /// header's toggle glyph names the state the click leads to, so it has to
    /// follow every flip, install, and reset.
    pub(crate) fn set_center_side_by_side(&mut self, side: bool, cx: &mut Context<Self>) {
        if self.center_side_by_side != side {
            self.center_side_by_side = side;
            cx.notify();
        }
    }

    /// Refresh the cached history when the current request or its newest
    /// response changed, then redraw. Progress ticks notify many times per
    /// second and must not each search the store, so the store is hit only on a
    /// real change in `(query, stamp)` — plus every tick while pending, since
    /// the elapsed clock is derived at paint time.
    fn sync_history(&mut self, cx: &mut Context<Self>) {
        let (pending, query, origin, stamp, revision, painted) = {
            let document = self.document.read(cx);
            let query = document
                .last_url
                .clone()
                .unwrap_or_else(|| document.request.url.clone());
            let stamp = document
                .response
                .as_ref()
                .map(|response| response.received_at.to_rfc3339());
            (
                document.is_pending(),
                query.trim().to_owned(),
                document.origin.clone(),
                stamp,
                document.history_revision,
                document.paint_key(),
            )
        };

        let changed = query != self.history_query
            || stamp != self.history_stamp
            || origin != self.history_origin
            || revision != self.history_revision;
        if changed {
            self.history_query = query;
            self.history_origin = origin;
            self.history_stamp = stamp;
            self.history_revision = revision;
            self.reload_history(cx);
        }

        // The redraw has its own reason. A failure or a cancel leaves the query
        // and the stamp exactly as they were, so `changed` stays false while the
        // pane is still painting the screen the in-flight send put there.
        let repaint = painted != self.painted;
        self.painted = painted;

        // While pending the clock has to advance on every heartbeat; once the
        // send resolves, a notify is a one-off, so a redraw with an unchanged
        // key would only burn a frame.
        if pending || changed || repaint {
            cx.notify();
        }
    }

    /// Re-point the pane at the active tab's document. The cached display and
    /// history belong to the old tab, so they are dropped and rebuilt from the
    /// new one, and the new tab's selected view and scroll are restored.
    pub(crate) fn set_document(&mut self, document: Entity<Document>, cx: &mut Context<Self>) {
        if self.document == document {
            return;
        }
        self.view
            .insert(self.document.entity_id(), self.capture_view());
        self.document = document.clone();
        self.document_sub = Some(cx.observe(&document, |this, _, cx| this.sync_history(cx)));
        self.display = None;
        self.finding = None;
        self.failure_details = false;
        self.failure_log = None;
        self.failure_painted = false;
        // An edit belongs to the tab it was started in; committing it against the
        // tab just entered would write one request's cookie into another's jar.
        self.cookie_edit = None;
        self.history_query = String::new();
        self.history_origin = Origin::default();
        self.history_stamp = None;
        self.history_revision = 0;
        // The rows belong to the document being left. Clearing the key alone
        // is not enough: the empty document reports exactly these defaults, so
        // `sync_history` would see no change and keep the old tab's sends. The
        // epoch bump drops a read already in flight for the document being left.
        self.history.clear();
        self.history_epoch = self.history_epoch.wrapping_add(1);
        self.painted = (ResponseState::Idle, None);
        let view = self
            .view
            .get(&document.entity_id())
            .copied()
            .unwrap_or(ViewState {
                tab: ResponseTab::Body,
                scroll_item: None,
            });
        self.tab = view.tab;
        if let Some(item) = view.scroll_item {
            self.body_scroll.scroll_to_item(item, ScrollStrategy::Top);
        }
        self.sync_history(cx);
        cx.notify();
    }

    /// The selected tab and first visible body row, for the tab being left.
    fn capture_view(&self) -> ViewState {
        let scroll_item = {
            let state = self.body_scroll.0.borrow();
            state.last_item_size.and_then(|size| {
                let height = f32::from(size.item.height);
                (height > 0.0)
                    .then(|| (f32::from(state.base_handle.offset().y).max(0.0) / height) as usize)
            })
        };
        ViewState {
            tab: self.tab,
            scroll_item,
        }
    }

    /// Re-read the rows for the current request, through the same scope the
    /// History pane uses.
    ///
    /// The read runs on the background executor behind the store's connection
    /// mutex, so an encrypted query cannot stall a paint. The epoch makes the
    /// last read win: a result for a superseded request is dropped rather than
    /// flashed on.
    fn reload_history(&mut self, cx: &mut Context<Self>) {
        self.history_epoch = self.history_epoch.wrapping_add(1);
        let epoch = self.history_epoch;
        let store = self.store.clone();
        let origin = self.history_origin.clone();
        let query = self.history_query.clone();
        let started = std::time::Instant::now();

        cx.spawn(async move |this, cx| {
            let rows = cx
                .background_spawn(async move {
                    crate::panels::history::scoped_rows(
                        store.as_ref(),
                        &origin,
                        &query,
                        &restbook_core::HistoryFilter::default(),
                        HISTORY_ROWS,
                    )
                    .unwrap_or_default()
                })
                .await;
            log::debug!(
                target: "restbook_app::perf",
                "response history reload took {}ms",
                started.elapsed().as_millis(),
            );
            this.update(cx, |panel, cx| {
                if panel.history_epoch == epoch {
                    panel.history = rows;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// What kind of text the payload is, for the format dropdown. Only the
    /// label is a guess; the body renderer still sniffs before reformatting.
    fn format_label(&self, response: &ResponseRecord) -> &'static str {
        let content_type = response.header("content-type").unwrap_or_default();
        if content_type.contains("json") {
            "JSON"
        } else if content_type.contains("html") {
            "HTML"
        } else if content_type.contains("xml") {
            "XML"
        } else {
            "Text"
        }
    }

    /// The body as it should be shown, decoding and pretty-printing only when
    /// the cached copy does not already match the record. The returned
    /// `SharedString` on a cache hit is a refcount bump of `DisplayCache::text`,
    /// not a copy of the payload.
    fn body_for_display(
        &mut self,
        response: &ResponseRecord,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> (SharedString, bool) {
        let body = response.body.as_bytes();
        let key = display_key(body, self.pretty);
        if let Some(cache) = &self.display {
            if cache.key == key {
                return (cache.text.clone(), cache.truncated);
            }
        }

        let content_type = response.header("content-type");
        let (text, truncated) = if self.pretty {
            util::body_text(body, content_type)
        } else {
            let shown = &body[..body.len().min(util::MAX_BODY_BYTES)];
            (
                String::from_utf8_lossy(shown).into_owned(),
                body.len() > util::MAX_BODY_BYTES,
            )
        };

        // Highlighting joins the cache for the same reason decoding does:
        // parsing a large payload on every scroll frame would make scrolling
        // drag. See `refresh_theme` for why the theme is captured here.
        let language = response_language(content_type, &text);
        let plain = Rc::new(highlight_body(
            &text,
            language,
            cx.theme().highlight_theme.as_ref(),
        ));
        let highlights = Rc::new(highlights_by_line(&text, &plain));

        let text = SharedString::from(text);
        // The rows are built here, once, alongside the decoded text: `body_view`
        // repaints continuously while scrolling, and re-splitting the whole body
        // there would allocate a fresh `Vec<SharedString>` per frame. The empty
        // line becomes a space so it still occupies a row, matching what the
        // per-frame split used to produce.
        let lines: Rc<Vec<SharedString>> = Rc::new(
            text.split('\n')
                .map(|line| {
                    if line.is_empty() {
                        SharedString::from(" ")
                    } else {
                        SharedString::from(line.to_owned())
                    }
                })
                .collect(),
        );

        // Only the numbered rows need a width: the plain block is already one
        // flex child sized by its content. Measure once here, where the body is
        // decoded, because `body_view` repaints continuously while scrolling.
        let widest_line = if self.pretty && lines.len() <= MAX_NUMBERED_LINES {
            widest_line(
                &lines,
                &self.palette.mono_font,
                self.metrics.font_small,
                window,
            )
        } else {
            px(0.)
        };

        self.display = Some(DisplayCache {
            key,
            text: text.clone(),
            truncated,
            widest_line,
            lines,
            plain,
            highlights,
            // The tab badge is the only reader, and it needs a number, not the
            // parsed cookies. Counting here means the badge costs nothing on a
            // repaint; splitting every `Set-Cookie` per frame was the whole
            // cost the badge imposed.
            cookie_count: util::cookies(&response.headers).len(),
        });
        // A query typed before the body finished decoding was scanned against
        // nothing. Recompute now that the rows exist; an empty query is a no-op.
        self.refresh_finding(cx);
        (text, truncated)
    }

    /// The response header: what was received on the left, how it went on the right —
    /// `Body | Headers 3 | Cookies` … `200 OK · 593 ms · 12.17 KB · 🕘 1 minute ago`.
    ///
    /// The copy controls live in the body's own toolbar instead, because they act on
    /// the body, which the Headers and Cookies tabs do not show; see
    /// [`Self::render_body_toolbar`].
    fn render_header(&self, response: &ResponseRecord, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;
        // One selectable run, not three spans of chrome: the summary the user
        // copies is exactly the one they see, and the role colors ride along as
        // highlight runs so nothing about the strip's look changes.
        let (summary, spans) = summary_for(response, &self.palette);

        h_flex()
            .h(px(self.metrics.header_height))
            .flex_none()
            .items_center()
            .gap(px(self.metrics.space_sm))
            .px(px(self.metrics.space_md))
            .bg(palette.tab_bar)
            .border_b_1()
            .border_color(palette.border)
            .child(
                div()
                    // The tabs take the free width and may shrink; the actions to
                    // their right keep their own. Without this the strip lays out
                    // at content width and paints over the rest of the header.
                    .flex_1()
                    .min_w_0()
                    // Recorded so the element-tree dump (see
                    // `preview::tree`) can name the tabs. No-op outside tests.
                    .debug_selector(|| "response-tabs".to_owned())
                    .child(self.render_tabs(response, cx)),
            )
            .child(
                div()
                    .flex_none()
                    .text_sm()
                    .debug_selector(|| "response-summary".to_owned())
                    .child(SelectableLine::new(
                        "response-summary",
                        SharedString::from(summary.clone()),
                        StyledText::new(SharedString::from(summary)).with_highlights(spans),
                        0,
                        palette.selection,
                    )),
            )
            .children(self.render_history(cx))
            .child(self.render_split_toggle(cx))
            .into_any_element()
    }

    /// The centre-split toggle, at the header's right edge. It sits with the
    /// response because the split is Request-over-Response; the glyph names the
    /// action the click leads to (two columns to go side by side, three rows to
    /// stack), and the workspace performs the flip. Shown here rather than in a
    /// global bar so the control sits beside the pane it divides.
    fn render_split_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        let (icon, tooltip) = split_toggle_action(self.center_side_by_side);
        Button::new("response-split-toggle")
            .icon(icon)
            .ghost()
            .compact()
            .tooltip(tooltip)
            .debug_selector(|| "response-split-toggle".to_owned())
            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::ToggleSplit)))
            .into_any_element()
    }

    /// The history control: the relative time of the newest send of this
    /// request. Clicking it asks the workspace to reveal the History pane
    /// already scoped to this request. Absent when the request has no history,
    /// so an unsent request keeps the header short.
    fn render_history(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let newest = self.history.first()?;
        let label = relative_time(&newest.at);
        let palette = self.palette.clone();

        Some(
            h_flex()
                .flex_none()
                .items_center()
                .gap(px(self.metrics.space_sm))
                .child(div().flex_none().text_color(palette.subtle).child("·"))
                .child(
                    Button::new("response-history")
                        .icon(IconName::Clock)
                        .label(label)
                        .ghost()
                        .compact()
                        .tooltip("Show earlier responses for this request")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(Event::ShowHistory);
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_format_switch(
        &self,
        response: &ResponseRecord,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let format = self.format_label(response);
        let pretty = self.pretty;
        let panel = cx.entity().downgrade();
        let menu = move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            for (label, checked, pretty) in [(format, pretty, true), ("Raw", !pretty, false)] {
                let panel = panel.clone();
                menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                    move |_, _, cx: &mut App| {
                        log::debug!(target: "restbook_app::ui", "response format: {label}");
                        // A menu item has no entity handle of its own, so the
                        // weak handle captured here is how the click reaches
                        // the panel that owns the mode.
                        panel
                            .update(cx, |panel, cx| {
                                panel.pretty = pretty;
                                cx.notify();
                            })
                            .ok();
                    },
                ));
            }
            menu
        };

        Button::new("response-format")
            .label(if pretty { format } else { "Raw" })
            .ghost()
            .compact()
            .dropdown_caret(true)
            .dropdown_menu(menu)
            .into_any_element()
    }

    fn render_tabs(&self, response: &ResponseRecord, cx: &mut Context<Self>) -> AnyElement {
        // From the display cache, which parsed it once, rather than splitting
        // every `Set-Cookie` header again for each repaint.
        let cookie_count = self
            .display
            .as_ref()
            .map(|cache| cache.cookie_count)
            .unwrap_or_else(|| util::cookies(&response.headers).len());
        let counts = [0, response.headers.len(), cookie_count];
        let labels = ResponseTab::ALL
            .iter()
            .zip(counts)
            .map(|(tab, count)| match count {
                0 => tab.label().to_owned(),
                count => format!("{} {count}", tab.label()),
            })
            .collect::<Vec<_>>();

        TabBar::new("response-tabs")
            .with_variant(TabVariant::Underline)
            .underline()
            // Same contract as the request pane's strips: a bounded width plus a
            // handle to reveal into, so a narrow pane scrolls the tabs and offers
            // the rest from the overflow menu rather than clipping them.
            .track_scroll(&self.tabs_scroll)
            .menu(true)
            .selected_index(
                ResponseTab::ALL
                    .iter()
                    .position(|each| *each == self.tab)
                    .unwrap_or(0),
            )
            .on_click(cx.listener(|this, index, _, cx| {
                if let Some(&tab) = ResponseTab::ALL.get(*index) {
                    log::debug!(target: "restbook_app::ui", "response tab: index {index}");
                    this.tab = tab;
                    cx.notify();
                }
            }))
            .children(labels)
            .into_any_element()
    }

    /// Whether the body is drawn as numbered rows or as the raw block. Both
    /// shapes get find marks, so this decides which rows the matches are
    /// computed against.
    fn numbered(&self) -> bool {
        self.pretty
            && self
                .display
                .as_ref()
                .map(|cache| cache.lines.len())
                .unwrap_or(0)
                <= MAX_NUMBERED_LINES
    }

    /// The body's own toolbar: the format switch and both copy controls.
    ///
    /// These describe the payload, so they belong beside it rather than in the header,
    /// which also draws for the Headers and Cookies tabs. The summary copy stays even
    /// though the summary is selectable: the button is the one-click "give me the whole
    /// thing" that needs no row boundary.
    fn render_body_toolbar(&self, response: &ResponseRecord, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;
        let (summary, _) = summary_for(response, palette);
        let copy_text = summary;

        let find_open = self.find_open || !self.find.read(cx).value().is_empty();
        let find_button = Button::new("find-body")
            .icon(IconName::Search)
            .ghost()
            .compact()
            .debug_selector(|| "response-find-toggle".to_owned())
            .tooltip("Find in the body")
            .on_click(cx.listener(|this, _, window, cx| {
                log::debug!(target: "restbook_app::ui", "response: find");
                this.toggle_find(window, cx);
            }));

        v_flex()
            .flex_none()
            .debug_selector(|| "response-body-toolbar".to_owned())
            .child(
                h_flex()
                    .items_center()
                    .gap(px(self.metrics.space_xs))
                    .px(px(self.metrics.space_md))
                    .py(px(self.metrics.space_xs))
                    .border_b_1()
                    .border_color(palette.border)
                    .child(self.render_format_switch(response, cx))
                    .child(div().flex_1())
                    .child(find_button)
                    .child(
                        Button::new("copy-body")
                            .icon(IconName::ClipboardCopy)
                            .ghost()
                            .compact()
                            .debug_selector(|| "response-copy-body".to_owned())
                            .tooltip("Copy the body as displayed")
                            .on_click(cx.listener(|this, _, window, cx| {
                                log::debug!(target: "restbook_app::ui", "response: copy body");
                                let response = this.document.read(cx).response.clone();
                                if let Some(response) = response {
                                    let (body, _) = this.body_for_display(&response, window, cx);
                                    // The cache hands back a `SharedString`; the
                                    // clipboard wants an owned string, and a copy is a
                                    // user gesture so paying for it here is fine.
                                    cx.write_to_clipboard(ClipboardItem::new_string(String::from(
                                        body,
                                    )));
                                }
                            })),
                    )
                    .child(
                        Button::new("copy-summary")
                            .icon(IconName::Copy)
                            .ghost()
                            .compact()
                            .debug_selector(|| "response-copy-summary".to_owned())
                            .tooltip("Copy the status, time, and size")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                log::debug!(target: "restbook_app::ui", "response: copy summary");
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                            })),
                    )
                    .child(
                        Button::new("snapshot-image")
                            .icon(IconName::Image)
                            .ghost()
                            .compact()
                            .debug_selector(|| "response-snapshot".to_owned())
                            .tooltip("Snapshot the request and response as an image")
                            .on_click(cx.listener(|_, _, _, cx| {
                                log::debug!(target: "restbook_app::ui", "response: snapshot");
                                cx.emit(Event::Snapshot);
                            })),
                    ),
            )
            .children(find_open.then(|| self.render_find_row(cx)))
            .into_any_element()
    }

    fn render_body(
        &mut self,
        response: &ResponseRecord,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = self.palette.clone();
        let (body, truncated) = self.body_for_display(response, window, cx);
        // The number of rows `body_view` will draw, so the notice names what is
        // on screen instead of a fixed phrase.
        let shown = self
            .display
            .as_ref()
            .map(|cache| cache.lines.len())
            .unwrap_or(0);

        v_flex()
            .flex_1()
            .min_h_0()
            .size_full()
            .children(truncated.then(|| {
                div()
                    .px(px(self.metrics.space_lg))
                    .pt(px(self.metrics.space_sm))
                    .text_xs()
                    .text_color(palette.danger)
                    .debug_selector(|| "response-body-truncated".to_owned())
                    .child(format!("Showing the first {shown} lines."))
            }))
            .child(self.body_view(body, &palette))
            .into_any_element()
    }

    /// The scrollable body. Virtualized, so a large payload only ever builds the
    /// rows that are on screen: one element per line without that is thousands of
    /// layouts and paints on every frame, which is what makes scrolling drag.
    fn body_view(&self, body: SharedString, palette: &Palette) -> AnyElement {
        // Rows and runs were both built once when the body was decoded, so a
        // repaint only clones two `Rc`s instead of re-splitting the payload into
        // fresh `SharedString`s per frame.
        let (lines, highlights) = match &self.display {
            Some(cache) => (cache.lines.clone(), cache.highlights.clone()),
            // `body_for_display` fills the cache before this is called, so this
            // arm is only a guard: it renders the whole body as one row rather
            // than nothing.
            None => (Rc::new(vec![body.clone()]), Rc::new(Vec::new())),
        };
        let line_count = lines.len().max(1);

        // Raw bodies and anything too large to number skip the numbered rows:
        // the wire body is usually one long line and a row is nowrap, so it
        // clipped at the pane edge, and a quarter-megabyte of blank lines is not
        // worth the count. Both become one selectable block that scrolls both
        // ways and keeps its syntax runs; the horizontal half needs a real flex
        // row with a non-shrinking child, or there is nothing to scroll to.
        if !self.pretty || line_count > MAX_NUMBERED_LINES {
            let spans = self
                .display
                .as_ref()
                .map(|cache| cache.plain.clone())
                .unwrap_or_default();
            let styled = StyledText::new(body.clone()).with_highlights(spans.iter().cloned());
            // The raw block is one row, so every hit is on row 0. The marks are
            // merged into the syntax runs, not appended after them: `StyledText`
            // walks ranges in order and a mark inside a run would otherwise be
            // painted past the end of the text.
            let match_style = HighlightStyle {
                background_color: Some(self.palette.search_match),
                ..Default::default()
            };
            let current_style = HighlightStyle {
                background_color: Some(self.palette.search_match_current),
                ..Default::default()
            };
            let (hits, current) = self
                .finding
                .as_ref()
                .map(|finding| {
                    (
                        finding.hits.clone(),
                        finding
                            .current
                            .and_then(|index| finding.hits.get(index).copied()),
                    )
                })
                .unwrap_or_default();
            let marks = mark_runs(&body, &hits, 0, &match_style, current, &current_style);
            let all = merge_highlight_runs(body.len(), &spans, &marks);
            let styled = if all.is_empty() {
                styled
            } else {
                styled.with_highlights(all)
            };
            return div()
                .id("response-body")
                // Read by the render test to compare the content width against
                // the viewport; `debug_selector` is a no-op outside test builds.
                .debug_selector(|| "response-body-viewport".to_owned())
                // `flex` is what sets `display: flex`; a plain div is a block
                // container, where the child stretches to the pane's width and
                // `flex_none` is ignored. The direction is a row so the nowrap
                // text is sized by its content, not by the scrollport.
                .flex()
                .flex_row()
                .flex_1()
                .min_h_0()
                // Stays on the hidden variant: horizontal scroll relies on the
                // child keeping its max-content width (see the UI guide's gotcha),
                // and `Scrollable`'s wrapper caps the content at the viewport.
                // `a_raw_body_is_wider_than_the_viewport` pins this.
                .overflow_scroll()
                // Lock to the gesture's dominant axis for its whole duration. The
                // default picks it per event, so a diagonal two-finger scroll
                // alternates sideways/down; this holds one axis until the flick ends.
                .restrict_scroll_to_axis()
                .items_start()
                .px(px(self.metrics.space_lg))
                .py(px(self.metrics.space_sm))
                .font_family(palette.mono_font.clone())
                .text_size(px(self.metrics.font_small))
                .whitespace_nowrap()
                .child(
                    div()
                        // `flex_none` is the half that makes the block wider than
                        // the pane: as a row flex child its main size only reaches
                        // max-content when it may not shrink. With the default
                        // `flex-shrink: 1` the nowrap text is laid out in full but
                        // the box stops at the scrollport's width, so there is
                        // nothing to scroll horizontally.
                        .flex_none()
                        .debug_selector(|| "response-body-content".to_owned())
                        .child(SelectableLine::new(
                            "response-body-plain",
                            body,
                            styled,
                            // The summary owns order 0, so the body starts at 1 here
                            // too, exactly as the numbered rows do.
                            1,
                            palette.selection,
                        )),
                )
                .into_any_element();
        }

        // The number column is the edge gutter plus one monospace digit
        // (roughly 0.6 em) per place, so it tracks zoom instead of freezing.
        let digits = line_count.to_string().len() as f32;
        let gutter = px(self.metrics.gutter + self.metrics.font_small * 0.6 * digits);
        let widest = self
            .display
            .as_ref()
            .map(|cache| cache.widest_line)
            .unwrap_or_default();
        // The gutter, the widest row, and a little breathing room so the longest
        // line does not sit flush against the scrollport's edge.
        let content_width = gutter + widest + px(self.metrics.gutter * 2.);
        let row_height = self.metrics.body_row_height;
        let number_pad = px(self.metrics.space_sm);
        let subtle = palette.subtle;
        let selection = palette.selection;
        // Resolved once per frame: the match style is the same for every row, and
        // the set of matching rows is small even when the body is not.
        let match_style = HighlightStyle {
            background_color: Some(palette.search_match),
            ..Default::default()
        };
        let current_style = HighlightStyle {
            background_color: Some(palette.search_match_current),
            ..Default::default()
        };
        let (hits_by_row, current_hit) = self
            .finding
            .as_ref()
            .map(|finding| {
                (
                    finding.hits.clone(),
                    finding
                        .current
                        .and_then(|index| finding.hits.get(index).copied()),
                )
            })
            .unwrap_or_default();

        div()
            .id("response-body")
            // Horizontal scroll for the numbered rows. `uniform_list` cannot
            // grow wider than its viewport, so it sits in a block that is at
            // least as wide as the widest line; the scrollport is the outer
            // element, so the line numbers scroll with the text.
            .debug_selector(|| "response-body-numbered-viewport".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            // As the pretty view above: the raw line must keep its max-content
            // width to scroll sideways, which the `Scrollable` wrapper caps.
            .overflow_x_scroll()
            // Without the axis lock a vertical flick nudges the small residual
            // `x` delta and the rows drift sideways. The `uniform_list` inside
            // reads only `y`, so this outer axis is the only one that can drift.
            .restrict_scroll_to_axis()
            .py(px(self.metrics.space_sm))
            .font_family(palette.mono_font.clone())
            .text_size(px(self.metrics.font_small))
            .child(
                div()
                    .min_w(content_width)
                    .debug_selector(|| "response-body-numbered-content".to_owned())
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list("response-body-lines", lines.len(), move |range, _, _| {
                            range
                                .map(|index| {
                                    h_flex()
                                        .h(px(row_height))
                                        .items_start()
                                        .child(
                                            div()
                                                .w(gutter)
                                                .flex_none()
                                                .pr(number_pad)
                                                .text_right()
                                                .text_color(subtle)
                                                .child((index + 1).to_string()),
                                        )
                                        .child(div().flex_1().min_w_0().whitespace_nowrap().child(
                                            {
                                                let spans = highlights
                                                    .get(index)
                                                    .map(Vec::as_slice)
                                                    .unwrap_or(&[]);
                                                let styled = StyledText::new(lines[index].clone());
                                                // A match can sit inside a syntax run, so
                                                // the two sets are merged into one sorted,
                                                // non-overlapping list before they reach
                                                // `StyledText`.
                                                let marks = mark_runs(
                                                    &lines[index],
                                                    &hits_by_row,
                                                    index,
                                                    &match_style,
                                                    current_hit,
                                                    &current_style,
                                                );
                                                let all = merge_highlight_runs(
                                                    lines[index].len(),
                                                    spans,
                                                    &marks,
                                                );
                                                let styled = if all.is_empty() {
                                                    styled
                                                } else {
                                                    styled.with_highlights(all)
                                                };
                                                // The gutter is a sibling, not part of this
                                                // run, so a copied drag contains body text
                                                // only — never line numbers.
                                                // The summary above the body owns order 0, so
                                                // body rows start at 1: a drag that spans both
                                                // then copies the summary before line 1.
                                                SelectableLine::new(
                                                    ("response-line", index),
                                                    lines[index].clone(),
                                                    styled,
                                                    index as u64 + 1,
                                                    selection,
                                                )
                                                .into_any_element()
                                            },
                                        ))
                                })
                                .collect::<Vec<_>>()
                        })
                        .track_scroll(&self.body_scroll)
                        .size_full(),
                    ),
            )
            .into_any_element()
    }

    fn render_headers(&self, response: &ResponseRecord) -> AnyElement {
        let palette = &self.palette;
        if response.headers.is_empty() {
            return empty_state("No headers received.", palette);
        }

        div()
            .id("response-headers")
            .debug_selector(|| "response-headers".to_owned())
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .px(px(self.metrics.space_lg))
            .py(px(self.metrics.space_sm))
            .font_family(palette.mono_font.clone())
            .text_size(px(self.metrics.font_small))
            .children(
                response
                    .headers
                    .iter()
                    .enumerate()
                    .map(|(index, (name, value))| {
                        h_flex()
                            .id(("response-header", index))
                            .gap(px(self.metrics.space_md))
                            .py(px(self.metrics.space_xs))
                            .items_start()
                            .border_b_1()
                            .border_color(palette.border)
                            .child(
                                div()
                                    .w(px(200.))
                                    .flex_none()
                                    .text_color(palette.info)
                                    .child(name.clone()),
                            )
                            .child(div().flex_1().min_w_0().child(value.clone()))
                    }),
            )
            .into_any_element()
    }

    fn render_response(
        &mut self,
        response: &ResponseRecord,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .size_full()
            .child(self.render_header(response, cx))
            .child(match self.tab {
                ResponseTab::Body => v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_body_toolbar(response, cx))
                    .child(self.render_body(response, window, cx))
                    .into_any_element(),
                ResponseTab::Headers => self.render_headers(response),
                ResponseTab::Cookies => self.render_cookies(response, cx),
            })
            .into_any_element()
    }

    fn render_status(&self, state: ResponseState, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;
        let (elapsed, bytes_received) = state.stats().unwrap_or_default();
        // Role colours, not literals: the theme already names what a working,
        // interrupted, or failed request should look like.
        let color = match state {
            ResponseState::Fetching { .. } => palette.info,
            ResponseState::Canceled { .. } => palette.warning,
            ResponseState::Error { .. } => palette.danger,
            _ => palette.muted,
        };
        let message = match state {
            ResponseState::Fetching { .. } => "Waiting for the response…",
            ResponseState::Canceled { .. } => "The request was canceled.",
            ResponseState::Error { .. } => "",
            _ => "",
        };

        // Same selectable run as the finished header. The strip ticks while a
        // request is in flight, so the string is rebuilt each tick; selection
        // is window-level and survives the new text, which is what lets a user
        // copy a snapshot of a transfer that is still moving.
        let (summary, spans) = summary_line(
            [
                (
                    state.label().to_owned(),
                    HighlightStyle {
                        color: Some(color),
                        font_weight: Some(FontWeight::MEDIUM),
                        ..HighlightStyle::default()
                    },
                ),
                (
                    util::duration(elapsed.as_millis() as u64),
                    HighlightStyle::color(palette.muted),
                ),
                (
                    util::human_bytes(bytes_received as usize),
                    HighlightStyle::color(palette.muted),
                ),
            ],
            HighlightStyle::color(palette.subtle),
        );
        let copy_text = summary.clone();

        v_flex()
            .size_full()
            .debug_selector(|| "response-status".to_owned())
            .child(
                // Same height and surface as the finished response header, so
                // the switch from in-flight to arrived does not shift the body.
                h_flex()
                    .h(px(self.metrics.header_height))
                    .flex_none()
                    .items_center()
                    .gap(px(self.metrics.space_sm))
                    .px(px(self.metrics.space_md))
                    .bg(palette.tab_bar)
                    .border_b_1()
                    .border_color(palette.border)
                    .child(
                        div().flex_none().text_sm().child(SelectableLine::new(
                            "response-status-summary",
                            SharedString::from(summary),
                            StyledText::new(SharedString::from(copy_text.clone()))
                                .with_highlights(spans),
                            0,
                            palette.selection,
                        )),
                    )
                    .child(
                        Button::new("copy-status-summary")
                            .icon(IconName::Copy)
                            .ghost()
                            .compact()
                            .tooltip("Copy the status, time, and size")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                            })),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child(div().text_sm().text_color(palette.muted).child(message)),
            )
            .into_any_element()
    }

    /// `Idle` means nothing has been sent (or the response was wiped), so there is no
    /// pending branch here; a live send renders through [`ResponsePanel::render_status`].
    ///
    /// The empty state names the next actions and their live shortcuts, the same rows
    /// the shortcuts sheet draws, so the keys read per OS instead of a hardcoded glyph.
    fn render_idle(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let actions = vec![
            Action::new("Send request", shortcut(&SendRequest, window)),
            Action::new("New tab", shortcut(&NewTab, window)),
            Action::new("Focus URL", shortcut(&FocusUrl, window)),
            Action::new("Toggle sidebar", shortcut(&ToggleSidebar, window)),
        ];

        div()
            .size_full()
            .debug_selector(|| "response-idle".to_owned())
            .child(empty::empty_state(
                "Nothing sent yet",
                Some("Send a request to see the response."),
                actions,
                self.render_earlier_sends(cx),
                &self.palette,
                &self.metrics,
            ))
            .into_any_element()
    }

    /// The way back to a request's stored sends when this tab holds no
    /// response: a tab that was closed and reopened, or one whose response was
    /// never kept. Without this the rows are still in the store but nothing on
    /// screen can reach them — the header's history control only exists next to
    /// a response.
    fn render_earlier_sends(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let newest = self.history.first()?;
        let label = format!(
            "Earlier sends ({}) · last {} {}",
            self.history.len(),
            util::status_text(newest.status),
            relative_time(&newest.at),
        );

        Some(
            div()
                .pb(px(self.metrics.space_sm))
                // Read by the render test; a no-op outside test builds.
                .debug_selector(|| "response-history-idle-control".to_owned())
                .child(
                    Button::new("response-history-idle")
                        .icon(IconName::Clock)
                        .label(label)
                        .ghost()
                        .compact()
                        .tooltip("Show earlier responses for this request")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(Event::ShowHistory);
                        })),
                )
                .into_any_element(),
        )
    }
}

/// How long ago a stored hit happened, in the shape Insomnia uses. The store
/// keeps RFC 3339; chunks coarser than a week read better as a count than a
/// date here.
pub(crate) fn relative_time(at: &str) -> String {
    let Some(then) = epoch_seconds(at) else {
        return String::new();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(then);
    let elapsed = (now - then).max(0);

    match elapsed {
        0..=59 => "just now".to_owned(),
        60..=3_599 => ago(elapsed / 60, "minute"),
        3_600..=86_399 => ago(elapsed / 3_600, "hour"),
        86_400..=604_799 => ago(elapsed / 86_400, "day"),
        _ => ago(elapsed / 604_800, "week"),
    }
}

fn ago(count: i64, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{count} {unit}s ago")
    }
}

/// Seconds since the Unix epoch for an RFC 3339 timestamp. Only the fields the
/// history rows carry are read; a malformed or partial stamp yields `None`.
fn epoch_seconds(stamp: &str) -> Option<i64> {
    if stamp.len() < 19 {
        return None;
    }

    let year: i64 = stamp.get(0..4)?.parse().ok()?;
    let month: i64 = stamp.get(5..7)?.parse().ok()?;
    let day: i64 = stamp.get(8..10)?.parse().ok()?;
    let hour: i64 = stamp.get(11..13)?.parse().ok()?;
    let minute: i64 = stamp.get(14..16)?.parse().ok()?;
    let second: i64 = stamp.get(17..19)?.parse().ok()?;
    let mut seconds =
        days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second;

    // Skip a fractional part, if one is present, before reading the offset.
    let rest = &stamp[19..];
    let rest = match rest.strip_prefix('.') {
        Some(fraction) => {
            let end = fraction.find(['Z', '+', '-'])?;
            &fraction[end..]
        }
        None => rest,
    };
    let offset = if rest.is_empty() || rest.starts_with('Z') {
        0
    } else {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let hours: i64 = rest.get(1..3)?.parse().ok()?;
        let minutes: i64 = rest.get(4..6)?.parse().ok()?;
        sign * (hours * 3_600 + minutes * 60)
    };
    seconds -= offset;
    Some(seconds)
}

/// Days from 1970-01-01 to `year-month-day`. Howard Hinnant's civil-from-days
/// inverse, which needs no table or leap-year branch beyond the month offset.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = (month + 9) % 12;
    let day_of_year = (153 * month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn empty_state(message: &str, palette: &Palette) -> AnyElement {
    v_flex()
        .flex_1()
        .size_full()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_sm()
                .text_color(palette.muted)
                .child(message.to_owned()),
        )
        .into_any_element()
}

impl Render for ResponsePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Read the shared document only long enough to derive the state and
        // copy what the finished view needs; holding the borrow through the
        // render calls below would keep `cx` borrowed.
        let state = self.document.read(cx).response_state();
        let response = self.document.read(cx).response.clone();
        let error = self.document.read(cx).error.clone();
        let palette = self.palette.clone();
        // A disclosure belongs to one failure; a later success must not inherit
        // an open Details section.
        if !matches!(state, ResponseState::Error { .. }) {
            self.failure_details = false;
            self.failure_log = None;
            self.failure_painted = false;
        }

        v_flex()
            .size_full()
            .bg(palette.background)
            .track_focus(&self.focus_handle)
            // The root selector the element-tree dump reads first; see
            // `preview::tree`.
            .debug_selector(|| "response-pane".to_owned())
            .child(match (state, &response) {
                // The finished view is unchanged: header, tabs, and body.
                (ResponseState::Completed { .. }, Some(response)) => {
                    self.render_response(response, window, cx)
                }
                (ResponseState::Idle, _) => self.render_idle(window, cx),
                // A failure owns the whole pane: cause, attempt, and actions.
                (ResponseState::Error { .. }, _) => self.render_failure(state, error.as_ref(), cx),
                // In flight or cancelled: there is no status or tab to show, so
                // a summary strip stands in for the header.
                (state, _) => self.render_status(state, cx),
            })
    }
}

#[cfg(test)]
mod tests {
    // Named imports only: `gpui_kit::*` brings in a `test` macro that shadows
    // `#[test]`, and `use super::*` would drag it in.
    use super::failure::failure_hint;
    use super::{
        display_key, find_rows, highlights_by_line, mark_runs, response_language,
        split_toggle_action, summary_line,
    };
    use gpui_kit::HighlightStyle;
    use restbook_core::FailureKind;

    /// The header puts the toggle's glyph and tooltip on the *next* action, not
    /// the current state: a side-by-side centre offers "stack", a stacked one
    /// offers "split". Reading the current state would name the opposite verb.
    #[test]
    fn the_split_toggle_names_the_next_action() {
        let (icon, tooltip) = split_toggle_action(false);
        assert_eq!(tooltip, "Split side by side");
        assert!(matches!(icon, crate::icons::IconName::Columns2));

        let (icon, tooltip) = split_toggle_action(true);
        assert_eq!(tooltip, "Stack request over response");
        assert!(matches!(icon, crate::icons::IconName::Rows3));
    }

    #[test]
    fn the_summary_copy_string_joins_the_parts_and_styles_every_byte() {
        let parts = [
            ("200 OK".to_owned(), HighlightStyle::default()),
            ("593 ms".to_owned(), HighlightStyle::default()),
            ("12.17 KB".to_owned(), HighlightStyle::default()),
        ];
        let (text, spans) = summary_line(parts, HighlightStyle::default());
        assert_eq!(text, "200 OK · 593 ms · 12.17 KB");
        // Two separators mean five styled runs, each starting where the last
        // stopped: the whole string is painted and none of it is uncoloured.
        assert_eq!(spans.len(), 5);
        assert_eq!(spans.first().unwrap().0.start, 0);
        assert_eq!(spans.last().unwrap().0.end, text.len());
        for pair in spans.windows(2) {
            assert_eq!(pair[0].0.end, pair[1].0.start);
        }
    }

    /// A find is case-insensitive and reports every row, in reading order, so
    /// next/previous can walk one flat list instead of searching per row.
    #[test]
    fn find_reports_every_match_across_rows_in_order() {
        let rows: Vec<gpui_kit::SharedString> =
            vec!["{\"Status\": \"OK\"}".into(), "  \"status\": \"ok\"".into()];
        let hits = find_rows(&rows, "status");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0], (0, 2, 6));
        assert_eq!(hits[1], (1, 3, 6));
    }

    /// An empty or whitespace-only query matches nothing. Otherwise clearing
    /// the field would claim to match every position, and the count would read
    /// "412 matches" for a query the user just deleted.
    #[test]
    fn an_empty_query_finds_nothing() {
        let rows: Vec<gpui_kit::SharedString> = vec!["abc".into()];
        assert!(find_rows(&rows, "").is_empty());
        assert!(find_rows(&rows, "   ").is_empty());
    }

    /// `aaa` searched for `aa` is one match, not two: matches do not overlap,
    /// which is what a reader expects from a highlight that walks forwards.
    #[test]
    fn matches_do_not_overlap() {
        let rows: Vec<gpui_kit::SharedString> = vec!["aaa".into()];
        assert_eq!(find_rows(&rows, "aa"), vec![(0, 0, 2)]);
    }

    /// A mark covers exactly the matched bytes, and only on its own row.
    #[test]
    fn a_mark_covers_the_match_on_its_row_only() {
        let hits = vec![(0, 2, 3), (1, 0, 3)];
        let style = HighlightStyle::default();
        let marks = mark_runs("abcdef", &hits, 0, &style, None, &style);
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].0, 2..5);
    }

    /// A mark inside a syntax run has to split the run. Appending it instead
    /// put the mark after the full-line run, where `StyledText` painted it past
    /// the end of the text and the match stayed unhighlighted.
    #[test]
    fn a_match_inside_a_syntax_run_splits_the_run() {
        use super::merge_highlight_runs;

        let base = vec![(0..6, HighlightStyle::default())];
        let active = HighlightStyle {
            background_color: Some(gpui_kit::hsla(0.0, 1.0, 0.5, 1.0)),
            ..Default::default()
        };
        let marks = vec![(2..5, active)];
        let merged = merge_highlight_runs(6, &base, &marks);

        let ranges: Vec<_> = merged.iter().map(|(range, _)| range.clone()).collect();
        assert_eq!(ranges, [0..2, 2..5, 5..6]);
        assert_eq!(merged[1].1.background_color, active.background_color);
    }

    /// The arrows have to mark the hit they are on differently from the rest,
    /// otherwise stepping through a list only moves the count.
    #[test]
    fn the_current_match_uses_the_stronger_style() {
        let hits = vec![(0, 1, 2), (0, 4, 2)];
        let normal = HighlightStyle::default();
        let active = HighlightStyle {
            background_color: Some(gpui_kit::hsla(0.0, 1.0, 0.5, 1.0)),
            ..Default::default()
        };
        let marks = mark_runs("abcdef", &hits, 0, &normal, Some((0, 4, 2)), &active);

        assert_eq!(marks.len(), 2);
        assert_eq!(marks[0].1.background_color, None);
        assert_eq!(marks[1].1.background_color, active.background_color);
    }

    #[test]
    fn the_display_key_is_stable_for_the_same_body() {
        let body = b"{\"a\":1}";
        assert_eq!(display_key(body, true), display_key(body, true));
    }

    #[test]
    fn the_display_key_changes_when_pretty_is_toggled() {
        let body = b"{\"a\":1}";
        assert_ne!(display_key(body, true), display_key(body, false));
    }

    #[test]
    fn the_language_follows_the_content_type_and_falls_back_to_a_json_sniff() {
        assert_eq!(response_language(Some("application/json"), "{}"), "json");
        assert_eq!(
            response_language(Some("text/html; charset=utf-8"), ""),
            "html"
        );
        // A body the pane chose to pretty-print should still be highlighted when
        // the header is missing or wrong.
        assert_eq!(response_language(None, "  {\"a\":1}"), "json");
        assert_eq!(response_language(Some("text/plain"), "hello"), "text");
    }

    #[test]
    fn a_run_spanning_lines_is_rebased_on_each_line() {
        // Covers `aa\nbb\ncc` without the newlines; each line should get 0..2.
        let lines = highlights_by_line("aa\nbb\ncc", &[(0..8, HighlightStyle::default())]);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], vec![(0..2, HighlightStyle::default())]);
        assert_eq!(lines[1], vec![(0..2, HighlightStyle::default())]);
        assert_eq!(lines[2], vec![(0..2, HighlightStyle::default())]);
    }

    #[test]
    fn the_display_key_changes_with_the_body() {
        // Different lengths, so the keys differ regardless of where the
        // allocator happened to place the bytes.
        assert_ne!(
            display_key(b"{\"a\":1}", true),
            display_key(b"{\"bb\":2}", true)
        );
    }

    /// A refusal on a plain-HTTP address is the classic “the server only speaks
    /// HTTPS” case, so the hint has to name it.
    #[test]
    fn a_refused_http_url_suggests_https() {
        let hint = failure_hint(FailureKind::Connect, "http://api.test/items");
        assert!(hint.contains("https://"), "{hint}");
    }

    #[test]
    fn a_timeout_points_at_the_timeout_setting() {
        let hint = failure_hint(FailureKind::Timeout, "https://api.test/items");
        assert!(hint.contains("Settings"), "{hint}");
    }

    #[test]
    fn the_kind_carries_the_title() {
        assert_eq!(FailureKind::Other.title(), "Request failed");
        assert_eq!(FailureKind::Connect.title(), "Couldn't connect");
        assert!(!failure_hint(FailureKind::Other, "https://api.test").is_empty());
    }
}

/// Headless render tests.
///
/// `TestAppContext` swaps GPUI's real platform for a mock one, so a window can be
/// laid out and painted with no display or GPU. The window is the real
/// [`ResponsePanel`], so these draw the actual pane, but they cannot see pixels
/// (recorded, not compared) — a guard against a panic, not a screenshot diff.
#[cfg(test)]
mod render_tests {
    // Named imports only: `use super::*` would drag in GPUI's `test` macro and
    // shadow the built-in `#[test]`.
    use std::path::PathBuf;
    use std::sync::Arc;

    use gpui_kit::{AppContext as _, Entity, TestAppContext, VisualTestContext};
    use restbook_core::testing::{FakeTransport, Reply};
    use restbook_core::{
        Failure, FailureKind, HistoryEntry, Method, Origin, RequestDef, ResponseRecord, Store,
    };
    use restbook_store::MemoryStore;

    use crate::document::Document;
    use crate::preview::tree;
    use crate::workspace::settings::Settings;

    use super::{ResponsePanel, ResponseTab};

    /// A record with a JSON content type, so the pane pretty-prints and
    /// highlights it the way it would a real API reply.
    fn json_response(body: impl Into<String>) -> ResponseRecord {
        response_with(200, Some("application/json"), body.into())
    }

    /// A record with an explicit status, content type, and body. `None` leaves
    /// the header off, which is the `/sniff` shape: JSON bytes with nothing to
    /// say they are JSON.
    fn response_with(
        status: u16,
        content_type: Option<&str>,
        body: impl Into<Vec<u8>>,
    ) -> ResponseRecord {
        ResponseRecord {
            status,
            headers: content_type
                .map(|content_type| vec![("content-type".to_owned(), content_type.to_owned())])
                .unwrap_or_default(),
            body: restbook_core::ResponseBody::Memory(body.into()),
            duration_ms: 42,
            // The epoch is fine: only formatting reads this.
            received_at: Default::default(),
            final_url: None,
            redirects: Vec::new(),
        }
    }

    /// One minified line: the shape a real endpoint returns, and the one that
    /// made the plain block scroll horizontally.
    fn one_long_line() -> String {
        let records: Vec<String> = (0..25)
            .map(|index| {
                format!(
                    "{{\"productUuid\":\"6f1c{index:04x}-9a2b-4c3d-8e4f-5a6b7c8d9e0f\",\
                     \"testCase\":\"purchase bronet when balance is sufficient\"}}"
                )
            })
            .collect();
        format!("{{\"status\":\"OK\",\"data\":[{}]}}", records.join(","))
    }

    /// Enough lines to pass `MAX_NUMBERED_LINES`, so the unnumbered fallback is
    /// what renders.
    fn many_lines() -> String {
        let items: Vec<String> = (0..3600)
            .map(|index| format!("{{\"id\":{index},\"name\":\"item {index}\"}}"))
            .collect();
        format!("{{\"items\":[{}]}}", items.join(","))
    }

    /// Past `MAX_BODY_BYTES`, so the display is truncated.
    fn huge() -> String {
        let padding = "x".repeat(crate::util::MAX_BODY_BYTES + 1024);
        format!("{{\"status\":\"OK\",\"padding\":\"{padding}\"}}")
    }

    /// The `/long-string` shape: one ~2.7 KB string inside otherwise indented
    /// JSON, so pretty-printing does not break the row that is too wide.
    fn long_string() -> String {
        let value = "x".repeat(2_700);
        format!("{{\"status\":\"OK\",\"value\":\"{value}\"}}")
    }

    /// The `/html` shape, scaled to `rows` table lines plus one long tag row.
    /// Multi-line with an over-wide row, so the numbered view has to place
    /// every line and the plain view has to scroll both ways.
    fn html_page(rows: usize) -> String {
        let mut page = String::from(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <title>restbook sample</title>\n</head>\n<body>\n  <table>\n",
        );
        for index in 0..rows {
            page.push_str(&format!(
                "    <tr><td>{index}</td><td class=\"name\">6f1c{index:04x}-9a2b-4c3d-8e4f-5a6b7c8d9e0f</td></tr>\n"
            ));
        }
        page.push_str("  </table>\n  <div class=\"tags\">");
        for index in 0..20 {
            page.push_str(&format!("<span class=\"tag\">6f1c{index:04x}</span>"));
        }
        page.push_str("</div>\n</body>\n</html>\n");
        page
    }

    /// The `/xml` shape: one attribute row long enough to clip in the numbered
    /// view.
    fn xml_doc() -> String {
        let attributes = (0..20)
            .map(|index| {
                format!("attribute{index}=\"6f1c{index:04x}-9a2b-4c3d-8e4f-5a6b7c8d9e0f\"")
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<catalog>\n  <product id=\"one\" {attributes}>\n    <name>bronet</name>\n  </product>\n</catalog>\n"
        )
    }

    /// The `/csv` shape: commas and doubled quotes inside quoted fields.
    fn csv_text() -> String {
        let mut rows = vec!["id,name,note".to_owned()];
        rows.extend(
            (0..12).map(|index| {
                format!("{index},\"item {index}\",\"comma, and a \"\"quoted\"\" word\"")
            }),
        );
        rows.join("\n") + "\n"
    }

    /// The `/text` shape: no syntax to label, and one line past any window.
    fn plain_text() -> String {
        let words = (0..400)
            .map(|index| format!("word{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        format!("restbook sample text\nThis body has no syntax.\n\n{words}\n")
    }

    /// The `/unicode` shape: keys and values wider and narrower than ASCII, so
    /// the highlighter and the gutter both see them.
    fn unicode_json() -> String {
        "{\n  \"日本語\": \"こんにちは\",\n  \"العربية\": \"السلام\",\n  \"emoji\": \"🦀🚀\",\n  \"combining\": \"e\u{301}\",\n  \"run\": \"日本語日本語日本語\"\n}".to_owned()
    }

    /// The `/deep` shape: indentation deeper than the gutter is wide.
    fn deep_json() -> String {
        let depth = 120;
        format!("{}null{}", "[".repeat(depth), "]".repeat(depth))
    }

    /// Every body shape `sample-server` serves, so a failure here can be
    /// reproduced by hand against the same route. The old paint test
    /// covered JSON only; the viewer has to label and survive the rest too.
    fn every_shape() -> Vec<(&'static str, ResponseRecord)> {
        vec![
            (
                "one-line",
                response_with(200, Some("application/json"), one_long_line()),
            ),
            (
                "indented-json",
                response_with(200, Some("application/json"), "{\n  \"status\": \"OK\"\n}"),
            ),
            (
                "long-string",
                response_with(200, Some("application/json"), long_string()),
            ),
            (
                "many-lines",
                response_with(200, Some("application/json"), many_lines()),
            ),
            ("huge", response_with(200, Some("application/json"), huge())),
            (
                "html",
                response_with(200, Some("text/html; charset=utf-8"), html_page(40)),
            ),
            (
                "xml",
                response_with(200, Some("application/xml; charset=utf-8"), xml_doc()),
            ),
            (
                "csv",
                response_with(200, Some("text/csv; charset=utf-8"), csv_text()),
            ),
            (
                "text",
                response_with(200, Some("text/plain; charset=utf-8"), plain_text()),
            ),
            (
                "binary",
                response_with(
                    200,
                    Some("application/octet-stream"),
                    vec![0xff, 0xfe, 0x00, 0x80],
                ),
            ),
            ("empty", response_with(204, None, "")),
            (
                "array",
                response_with(200, Some("application/json"), "[1, 2, 3]"),
            ),
            (
                "scalar",
                response_with(200, Some("application/json"), "null"),
            ),
            (
                "invalid-json",
                response_with(200, Some("application/json"), "{\"oops\":"),
            ),
            ("sniff", response_with(200, None, "{\"a\": 1}")),
            (
                "unicode",
                response_with(200, Some("application/json"), unicode_json()),
            ),
            (
                "deep",
                response_with(200, Some("application/json"), deep_json()),
            ),
        ]
    }

    /// Open a window whose root view is the pane, wired to an empty store and a
    /// settings file that does not exist (so the defaults load and nothing is
    /// written). The response, if any, is already in the document, which puts
    /// the pane in its completed state before the first frame.
    fn open(
        cx: &mut TestAppContext,
        response: Option<ResponseRecord>,
    ) -> (Entity<ResponsePanel>, &mut VisualTestContext) {
        let url = "http://sample/one-line".to_owned();
        open_with(cx, response, Arc::new(MemoryStore::new()), Some(url))
    }

    /// [`open`], but with a store that already holds history and control over
    /// the document's `last_url` and owner — the things a reopened tab differs
    /// in.
    fn open_with(
        cx: &mut TestAppContext,
        response: Option<ResponseRecord>,
        store: Arc<dyn Store>,
        last_url: Option<String>,
    ) -> (Entity<ResponsePanel>, &mut VisualTestContext) {
        open_owned(
            cx,
            response,
            store,
            last_url,
            Origin::default(),
            FakeTransport::always(Reply::Status(200)),
        )
    }

    /// A session jar seeded with `cookies`, for the Cookies tab.
    fn jar(cookies: &[(&str, &str)]) -> Arc<dyn restbook_core::Transport> {
        let transport = FakeTransport::always(Reply::Status(200));
        transport.set_cookies(
            cookies
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
        );
        transport
    }

    /// [`open_with`] plus the owner the document reports, so the history scope
    /// can be tested without a saved tree.
    fn open_owned(
        cx: &mut TestAppContext,
        response: Option<ResponseRecord>,
        store: Arc<dyn Store>,
        last_url: Option<String>,
        origin: Origin,
        transport: Arc<dyn restbook_core::Transport>,
    ) -> (Entity<ResponsePanel>, &mut VisualTestContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let document = cx.new(|_| {
                let mut document = Document::new();
                let url = "http://sample/one-line".to_owned();
                document.request = RequestDef::new(Method::Get, url);
                document.last_url = last_url;
                document.response = response;
                document.origin = origin;
                document
            });
            let settings = cx.new(|_| Settings::new(PathBuf::from("restbook-render-test.json")));
            ResponsePanel::new(document, store, transport, settings, window, cx)
        });
        (view, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    /// The tab badge's count is parsed once, when the body is decoded.
    ///
    /// Recomputing it in `render_tabs` would split every `Set-Cookie` header on every
    /// frame, and a scroll repaints continuously. The count is cached with the decoded
    /// body, so a repaint reads it instead of recomputing it.
    #[test]
    fn the_cookie_count_is_cached_with_the_body() {
        let mut record = json_response("{\"a\":1}");
        record
            .headers
            .push(("set-cookie".to_owned(), "sid=abc; Path=/".to_owned()));
        record
            .headers
            .push(("set-cookie".to_owned(), "theme=dark".to_owned()));
        // A header that parses to nothing must not inflate the badge.
        record
            .headers
            .push(("set-cookie".to_owned(), "nonsense".to_owned()));
        assert_eq!(crate::util::cookies(&record.headers).len(), 2);

        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let (view, cx) = open(&mut cx, Some(record.clone()));
        draw(cx);

        let cached = cx.update(|_, cx| {
            let panel = view.read(cx);
            panel.display.as_ref().map(|cache| cache.cookie_count)
        });
        assert_eq!(cached, Some(2), "the count is stored with the decoded body");
        cx.quit();
    }

    /// Each tab keeps its selected response tab, so a switch back shows what was
    /// left instead of resetting to Body. The scroll rides along in the same
    /// `ViewState`; the tab is the deterministic half to pin.
    #[test]
    fn switching_tabs_restores_the_selected_response_tab() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let first = cx.update(|cx| cx.new(|_| Document::new()));
        let second = cx.update(|cx| cx.new(|_| Document::new()));
        let (view, cx) = cx.add_window_view({
            let first = first.clone();
            move |window, cx| {
                let settings =
                    cx.new(|_| Settings::new(PathBuf::from("restbook-render-test.json")));
                ResponsePanel::new(
                    first,
                    Arc::new(MemoryStore::new()),
                    FakeTransport::always(Reply::Status(200)),
                    settings,
                    window,
                    cx,
                )
            }
        });

        // Leave the first tab on Headers.
        cx.update(|_, cx| {
            view.update(cx, |panel, _| panel.tab = ResponseTab::Headers);
        });

        // A tab never visited starts on the body.
        cx.update(|_, cx| {
            view.update(cx, |panel, cx| panel.set_document(second.clone(), cx));
        });
        let on_second = cx.update(|_, cx| view.read(cx).tab);
        assert_eq!(
            on_second,
            ResponseTab::Body,
            "a fresh tab starts on the body"
        );

        // Leave the second on Cookies, then switch back to the first.
        cx.update(|_, cx| {
            view.update(cx, |panel, _| panel.tab = ResponseTab::Cookies);
        });
        cx.update(|_, cx| {
            view.update(cx, |panel, cx| panel.set_document(first.clone(), cx));
        });
        let back = cx.update(|_, cx| view.read(cx).tab);
        assert_eq!(
            back,
            ResponseTab::Headers,
            "switching back must restore the response tab it was left on"
        );
        cx.quit();
    }

    /// Selectors the response pane records for the element-tree dump. The body
    /// shapes are alternatives (numbered rows, or the plain block), so a tree
    /// simply leaves out whichever one the current view did not lay out.
    const TREE_SELECTORS: &[&str] = &[
        "response-pane",
        "response-summary",
        "response-tabs",
        "response-split-toggle",
        "response-body-toolbar",
        "response-copy-body",
        "response-copy-summary",
        "response-body-numbered-viewport",
        "response-body-numbered-content",
        "response-body-viewport",
        "response-body-content",
        "response-body-truncated",
        "response-headers",
        "response-cookies",
        "response-error",
        "response-idle",
        "response-status",
    ];

    /// 6.2: the recorded bounds as a deterministic text tree.
    ///
    /// A real screenshot needs a GPU and a font stack, which the mock window lacks. It
    /// records the bounds of every element that opted in with `debug_selector`, so the
    /// tree is the assertion and the file under `target/preview/` is the visual diff: a
    /// pane whose layout moved shows up as changed lines.
    #[test]
    fn a_pane_has_a_deterministic_element_tree() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let (_view, cx) = open(&mut cx, Some(json_response("{\n  \"status\": \"OK\"\n}")));
        draw(cx);
        let first = tree::bounds_tree(cx, "response-numbered", TREE_SELECTORS);

        // A second draw of the same fixture must produce byte-identical text;
        // anything else would make the file useless for a diff.
        draw(cx);
        let second = tree::bounds_tree(cx, "response-numbered", TREE_SELECTORS);
        assert_eq!(first, second, "the element tree is not deterministic");
        assert!(
            first.contains("response-body-numbered-viewport"),
            "the numbered body should be in the tree:\n{first}"
        );
        assert!(
            first.contains("response-summary"),
            "the summary strip should be in the tree:\n{first}"
        );

        let path = tree::write_tree(cx, "response-numbered", TREE_SELECTORS)
            .expect("the tree dump should be writable");
        assert!(path.exists(), "{}", path.display());

        cx.quit();
    }

    /// The copy controls belong to the body, not the header strip.
    ///
    /// The header also draws for the Headers and Cookies tabs, so a copy button there
    /// offers something the current tab does not show; the toolbar only the Body tab
    /// draws does. The assertion is positional: each button's top edge is at or below
    /// the toolbar's, which is below the header's summary row.
    #[test]
    fn the_copy_controls_live_in_the_body_toolbar() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let (_view, cx) = open(&mut cx, Some(json_response("{\"status\": \"OK\"}")));
        draw(cx);

        let text = tree::bounds_tree(cx, "response-toolbar", TREE_SELECTORS);
        let top_of = |selector: &str| -> f32 {
            text.lines()
                .filter(|line| line.contains(selector))
                .find(|line| line.contains('@'))
                .unwrap_or_else(|| panic!("{selector} should be in the tree:\n{text}"))
                .split('@')
                .nth(1)
                .and_then(|rest| rest.split(',').nth(1))
                .and_then(|top| top.split_whitespace().next())
                .unwrap_or_else(|| panic!("a recorded top in {selector}"))
                .parse()
                .expect("a numeric top")
        };

        let summary = top_of("response-summary");
        let toolbar = top_of("response-body-toolbar");
        let copy_body = top_of("response-copy-body");
        let copy_summary = top_of("response-copy-summary");

        assert!(
            toolbar > summary,
            "the body toolbar should sit below the header summary: \
             toolbar at {toolbar}, summary at {summary}"
        );
        for (name, top) in [("copy body", copy_body), ("copy summary", copy_summary)] {
            assert!(
                top >= toolbar,
                "the {name} button should be inside the body toolbar: \
                 button at {top}, toolbar at {toolbar}"
            );
        }

        cx.quit();
    }

    /// The centre-split toggle belongs to the response header, not the body's
    /// toolbar. It divides Request from Response, so it sits beside the status
    /// summary and stays put on the Headers and Cookies tabs too. The assertion
    /// is positional: the toggle's top is above the body toolbar, which only the
    /// Body tab draws.
    #[test]
    fn the_split_toggle_lives_in_the_response_header() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let (_view, cx) = open(&mut cx, Some(json_response("{\"status\": \"OK\"}")));
        draw(cx);

        let text = tree::bounds_tree(cx, "response-split", TREE_SELECTORS);
        let top_of = |selector: &str| -> f32 {
            text.lines()
                .filter(|line| line.contains(selector))
                .find(|line| line.contains('@'))
                .unwrap_or_else(|| panic!("{selector} should be in the tree:\n{text}"))
                .split('@')
                .nth(1)
                .and_then(|rest| rest.split(',').nth(1))
                .and_then(|top| top.split_whitespace().next())
                .unwrap_or_else(|| panic!("a recorded top in {selector}"))
                .parse()
                .expect("a numeric top")
        };

        let toggle = top_of("response-split-toggle");
        let toolbar = top_of("response-body-toolbar");
        assert!(
            toggle < toolbar,
            "the split toggle belongs to the header, above the body toolbar: \
             toggle at {toggle}, toolbar at {toolbar}"
        );

        cx.quit();
    }

    /// Every state the pane has must lay out and paint without panicking. A
    /// body that overflows a fixed-height row, or a virtualized list built at
    /// zero height, is exactly what a plain unit test cannot reach. The shapes
    /// are the ones `sample-server` serves, so a failure here can
    /// be reproduced by hand against the same route.
    #[test]
    fn the_pane_paints_every_response_shape() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        for (_name, response) in every_shape() {
            let (_view, cx) = open(&mut cx, Some(response));
            draw(cx);
        }

        // The error state, which renders the summary strip instead of the tabs.
        let (_view, cx) = open(&mut cx, None);
        draw(cx);

        cx.quit();
    }

    /// Every shape has to survive the Raw switch too: it renders one plain
    /// block instead of the numbered rows, with its own layout. `/huge` is
    /// skipped because laying out a quarter-megabyte string in the test window
    /// costs more than the coverage is worth; its truncation flag is asserted
    /// by `a_body_past_the_cap_is_marked_truncated`.
    #[test]
    fn every_shape_survives_the_raw_switch() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        for (name, response) in every_shape() {
            if name == "huge" {
                continue;
            }
            let (view, cx) = open(&mut cx, Some(response));
            // Raw is the plain block with `pretty` off. The cache is keyed on
            // that flag, so clearing it is what forces the different branch.
            view.update(cx, |panel, cx| {
                panel.pretty = false;
                panel.display = None;
                cx.notify();
            });
            draw(cx);
        }

        cx.quit();
    }

    /// Raw keeps every line break, so a multi-line body has to be taller than
    /// its scrollport. A `nowrap` style that collapsed the breaks into spaces
    /// would leave one line and nothing to scroll vertically, which is exactly
    /// the failure this pins down.
    #[test]
    fn a_raw_body_is_taller_than_the_viewport() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let (view, cx) = open(
            &mut cx,
            Some(response_with(
                200,
                Some("text/html; charset=utf-8"),
                html_page(200),
            )),
        );
        view.update(cx, |panel, cx| {
            panel.pretty = false;
            panel.display = None;
            cx.notify();
        });
        draw(cx);

        let viewport = cx
            .debug_bounds("response-body-viewport")
            .expect("the body container should be recorded");
        let content = cx
            .debug_bounds("response-body-content")
            .expect("the plain body block should be recorded");
        assert!(
            content.size.height > viewport.size.height,
            "a multi-line body should be taller than the pane, but content was {:?} and viewport {:?}",
            content.size,
            viewport.size
        );

        cx.quit();
    }

    /// Both secondary tabs, with rows and with the empty state. The headers
    /// tab shows every header the wire sent; the cookies tab filters to
    /// `Set-Cookie`, so one response can populate one and not the other.
    #[test]
    fn the_headers_and_cookies_tabs_paint_rows_and_empty_states() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let mut populated = response_with(200, Some("application/json"), "{}");
        populated
            .headers
            .extend((0..40).map(|index| (format!("x-sample-{index}"), format!("value-{index}"))));
        populated.headers.push((
            "set-cookie".to_owned(),
            "sid=abc; Path=/; HttpOnly".to_owned(),
        ));
        populated.headers.push((
            "set-cookie".to_owned(),
            "theme=dark; Path=/; SameSite=Lax".to_owned(),
        ));

        for response in [populated, response_with(204, None, "")] {
            let (view, cx) = open(&mut cx, Some(response));
            for tab in [ResponseTab::Headers, ResponseTab::Cookies] {
                view.update(cx, |panel, cx| {
                    panel.tab = tab;
                    cx.notify();
                });
                draw(cx);
            }
        }

        cx.quit();
    }

    /// The Cookies tab draws the session jar when it has entries for this URL,
    /// because those are the cookies a send would actually carry. With no jar
    /// entries — a response restored from history — it falls back to the rows
    /// that response set, read-only.
    #[test]
    fn the_cookies_tab_draws_the_session_before_the_stored_response() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let mut stored = response_with(200, Some("application/json"), "{}");
        stored.headers.push((
            "set-cookie".to_owned(),
            "sid=abc; Path=/; HttpOnly".to_owned(),
        ));
        stored
            .headers
            .push(("set-cookie".to_owned(), "theme=dark; Path=/".to_owned()));

        fn open_session(
            cx: &mut TestAppContext,
            response: ResponseRecord,
            transport: Arc<dyn restbook_core::Transport>,
        ) -> (Entity<ResponsePanel>, &mut VisualTestContext) {
            open_owned(
                cx,
                Some(response),
                Arc::new(MemoryStore::new()),
                Some("http://sample/one-line".to_owned()),
                Origin::default(),
                transport,
            )
        }

        let (view, vcx) = open_session(
            &mut cx,
            stored.clone(),
            jar(&[("sid", "xyz"), ("theme", "light")]),
        );
        view.update(vcx, |panel, cx| {
            panel.tab = ResponseTab::Cookies;
            cx.notify();
        });
        draw(vcx);
        assert!(vcx.debug_bounds("response-cookie-0-sid").is_some());
        assert!(vcx.debug_bounds("response-cookie-1-theme").is_some());

        let (view, vcx) = open_session(&mut cx, stored.clone(), jar(&[]));
        view.update(vcx, |panel, cx| {
            panel.tab = ResponseTab::Cookies;
            cx.notify();
        });
        draw(vcx);
        assert!(
            vcx.debug_bounds("response-cookie-0-sid").is_some(),
            "a stored response's cookies should still be listed without a jar"
        );
        assert!(vcx.debug_bounds("response-cookie-1-theme").is_some());

        cx.quit();
    }
    fn seed_history(store: &MemoryStore, url: &str, status: u16) {
        seed_owned_history(store, url, status, Origin::default());
    }

    /// [`seed_history`] with an owner, so scoping can be exercised.
    fn seed_owned_history(store: &MemoryStore, url: &str, status: u16, origin: Origin) {
        store
            .append_history(&HistoryEntry {
                id: 0,
                request: RequestDef::new(Method::Get, url),
                response: ResponseRecord {
                    status,
                    headers: Vec::new(),
                    body: b"{}".to_vec().into(),
                    duration_ms: 5,
                    received_at: Default::default(),
                    final_url: None,
                    redirects: Vec::new(),
                },
                url: url.to_owned(),
                origin,
            })
            .expect("the in-memory store accepts history");
    }

    /// A saved request that was closed and reopened: the request is back in the
    /// document, but this tab holds no response, so there is no header and the
    /// history control that lives next to it is gone. The pane still has to find
    /// the store's rows — from the request's URL, since the tab never sent
    /// anything — and the idle state has to offer the way back to them.
    #[test]
    fn a_reopened_request_finds_its_earlier_sends() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let store = Arc::new(MemoryStore::new());
        let url = "http://sample/one-line";
        seed_history(&store, url, 200);
        seed_history(&store, url, 201);

        let (view, cx) = open_with(&mut cx, None, store, None);
        cx.run_until_parked();
        let found = view.update(cx, |panel, _| panel.history.len());
        assert_eq!(found, 2, "the reopened tab should see both stored sends");

        draw(cx);
        assert!(
            cx.debug_bounds("response-history-idle-control").is_some(),
            "the idle pane should offer the earlier sends"
        );

        cx.quit();
    }

    /// Closing the last tab re-points every pane at an empty document. The rows
    /// cached for the closed tab must not survive that: otherwise the idle pane
    /// offers "Earlier sends" for a request that is no longer open, and the
    /// history scope that control asks for has nothing to show.
    #[test]
    fn a_closed_request_keeps_no_earlier_sends() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let store = Arc::new(MemoryStore::new());
        seed_history(&store, "http://sample/one-line", 403);

        let (view, cx) = open_with(&mut cx, None, store, None);
        cx.run_until_parked();
        let before = view.update(cx, |panel, _| panel.history.len());
        assert_eq!(before, 1, "the tab starts with the rows for its URL");

        // A read for this request is in flight when the last tab closes. It must
        // not land after the switch and re-offer rows for a request that is gone.
        let empty = cx.update(|_, cx| cx.new(|_| Document::new()));
        view.update(cx, |panel, cx| {
            panel
                .document
                .update(cx, |document, _| document.touch_history());
            panel.sync_history(cx);
            panel.set_document(empty, cx);
        });
        cx.run_until_parked();

        let after = view.update(cx, |panel, _| panel.history.len());
        assert_eq!(after, 0, "a closed request has no earlier sends to offer");

        draw(cx);
        assert!(
            cx.debug_bounds("response-history-idle-control").is_none(),
            "the idle pane must not offer history for a request that is not open"
        );

        cx.quit();
    }

    /// A fresh pane is the app's first ten minutes: nothing sent, so the empty
    /// state has to name the next actions rather than show one sentence. Each
    /// row is painted with a live shortcut, so the tree carries one selector per
    /// action and the labels are the ones the shortcuts sheet uses.
    #[test]
    fn the_idle_pane_names_the_next_actions() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let (_view, cx) = open(&mut cx, None);
        draw(cx);

        let selectors = [
            "response-idle",
            "empty-action-Send request",
            "empty-action-New tab",
            "empty-action-Focus URL",
            "empty-action-Toggle sidebar",
        ];
        let idle = tree::bounds_tree(cx, "response-idle", &selectors);
        for selector in selectors {
            assert!(
                idle.contains(selector),
                "the idle empty state should record {selector}:\n{idle}"
            );
        }

        cx.quit();
    }

    /// A saved request owns its sends. Two requests with the same URL — a copy,
    /// or the same endpoint filed twice — used to share a send log because the
    /// only key was the URL text. The header must list the owner's rows only, and
    /// an owner with no sends must show none even though the URL has plenty.
    #[test]
    fn history_belongs_to_the_saved_request_not_the_url() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let store = Arc::new(MemoryStore::new());
        let url = "http://sample/one-line";
        let first = Origin::Saved {
            node: "node-a".to_owned(),
        };
        seed_owned_history(&store, url, 200, first.clone());
        seed_owned_history(&store, url, 201, first.clone());
        seed_owned_history(
            &store,
            url,
            500,
            Origin::Saved {
                node: "node-b".to_owned(),
            },
        );

        let (view, vcx) = open_owned(&mut cx, None, store.clone(), None, first, jar(&[]));
        vcx.run_until_parked();
        assert_eq!(
            view.update(vcx, |panel, _| panel.history.len()),
            2,
            "only node-a's two sends belong to this request"
        );

        // A third request with the same URL and no sends of its own must not
        // inherit anyone else's rows.
        let (other, vcx) = open_owned(
            &mut cx,
            None,
            store,
            None,
            Origin::Saved {
                node: "node-c".to_owned(),
            },
            jar(&[]),
        );
        vcx.run_until_parked();
        assert_eq!(
            other.update(vcx, |panel, _| panel.history.len()),
            0,
            "an unsent request with a shared URL still has no history of its own"
        );

        cx.quit();
    }

    /// A delete made outside this pane (the sidebar, the History pane) bumps
    /// the document's history revision. Without that signal the pane keeps its
    /// cached rows and shows a send that no longer exists until the next send.
    #[test]
    fn a_history_deletion_refreshes_the_cached_rows() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let store = Arc::new(MemoryStore::new());
        let url = "http://sample/one-line";
        let owner = Origin::Saved {
            node: "node-a".to_owned(),
        };
        seed_owned_history(&store, url, 200, owner.clone());
        let id = store
            .history_for_origin(&owner, &restbook_core::HistoryFilter::default(), 10)
            .unwrap()
            .unwrap()
            .first()
            .expect("one seeded send")
            .id;

        let (view, vcx) = open_owned(&mut cx, None, store.clone(), None, owner, jar(&[]));
        vcx.run_until_parked();
        assert_eq!(view.update(vcx, |panel, _| panel.history.len()), 1);

        store.delete_history(id).unwrap();
        view.update(vcx, |panel, cx| {
            panel.document.update(cx, |document, cx| {
                document.touch_history();
                cx.notify();
            });
            panel.sync_history(cx);
        });
        vcx.run_until_parked();

        assert_eq!(
            view.update(vcx, |panel, _| panel.history.len()),
            0,
            "the deleted send must not linger in the cached list"
        );

        cx.quit();
    }

    #[test]
    fn a_raw_body_is_wider_than_the_viewport() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let (view, cx) = open(&mut cx, Some(json_response(one_long_line())));
        // Raw is the plain block with `pretty` off; a minified body is one row
        // in either view, but only Raw is guaranteed not to go through the
        // fixed-height numbered rows.
        view.update(cx, |panel, cx| {
            panel.pretty = false;
            panel.display = None;
            cx.notify();
        });
        draw(cx);

        let viewport = cx
            .debug_bounds("response-body-viewport")
            .expect("the body container should be recorded");
        let content = cx
            .debug_bounds("response-body-content")
            .expect("the plain body block should be recorded");
        assert!(
            content.size.width > viewport.size.width,
            "a minified body should be wider than the pane, but content was {:?} and viewport {:?}",
            content.size,
            viewport.size
        );

        cx.quit();
    }

    /// The numbered view scrolls sideways too (U45). The content block has to
    /// be wider than the scrollport, or a long line is clipped exactly as it
    /// was before, and the gutter and text both have to ride the same scroll.
    #[test]
    fn a_numbered_body_is_wider_than_the_viewport() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        // Pretty is the default, so this body takes the numbered branch. The
        // body has to stay indented JSON with one over-wide row: the minified
        // fixture above is one long line on the wire, but pretty-printing it
        // breaks the row into pieces that fit.
        let (_view, cx) = open(&mut cx, Some(json_response(long_string())));
        draw(cx);

        let viewport = cx
            .debug_bounds("response-body-numbered-viewport")
            .expect("the numbered scrollport should be recorded");
        let content = cx
            .debug_bounds("response-body-numbered-content")
            .expect("the numbered content block should be recorded");
        assert!(
            content.size.width > viewport.size.width,
            "a long line should be wider than the pane, but content was {:?} and viewport {:?}",
            content.size,
            viewport.size
        );

        cx.quit();
    }

    /// A body past the display cap is truncated and the pane knows it, which is
    /// what shows the banner.
    #[test]
    fn a_body_past_the_cap_is_marked_truncated() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);

        let (view, cx) = open(&mut cx, Some(json_response(huge())));
        view.update(cx, |panel, cx| {
            panel.pretty = false;
            cx.notify();
        });
        draw(cx);

        let truncated = view.read_with(cx, |panel, _| {
            panel.display.as_ref().map(|cache| cache.truncated)
        });
        assert_eq!(truncated, Some(true), "the banner would not be drawn");

        cx.quit();
    }

    /// The toolbar button used to focus an input that was not rendered until
    /// the query was non-empty, so clicking Find showed nothing at all. The
    /// row now opens before focus is handed to it.
    #[gpui_kit::test]
    fn the_find_button_opens_the_row(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_view, cx) = open(cx, Some(json_response("{\"a\":1}")));
        draw(cx);
        assert!(
            cx.debug_bounds("response-find-row").is_none(),
            "the row starts closed"
        );

        let target = cx
            .debug_bounds("response-find-toggle")
            .expect("the find button is in the body toolbar")
            .center();
        cx.simulate_mouse_down(target, gpui_kit::MouseButton::Left, Default::default());
        cx.simulate_mouse_up(target, gpui_kit::MouseButton::Left, Default::default());
        cx.run_until_parked();
        draw(cx);

        assert!(
            cx.debug_bounds("response-find-row").is_some(),
            "clicking Find must render the row it focuses"
        );
        cx.quit();
    }

    /// The arrows have to move the current hit, not only the count. This pins
    /// the first press, the wrap at both ends, and the backward step, which the
    /// UI shows through the stronger match style and the scroll to the row.
    #[test]
    fn stepping_moves_the_current_match_and_wraps() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let (view, cx) = open(
            &mut cx,
            Some(json_response("{\"status\":\"a\",\"message\":\"status b\"}")),
        );
        draw(cx);

        cx.update(|window, cx| {
            view.update(cx, |panel, cx| {
                panel
                    .find
                    .update(cx, |state, cx| state.set_value("status", window, cx));
                panel.refresh_finding(cx);
                assert_eq!(
                    panel.finding.as_ref().map(|finding| finding.hits.len()),
                    Some(2),
                    "the pretty-printed body has two matches",
                );

                assert!(panel.step_match(true, cx).is_some());
                assert_eq!(panel.finding.as_ref().and_then(|f| f.current), Some(0));
                panel.step_match(true, cx);
                assert_eq!(panel.finding.as_ref().and_then(|f| f.current), Some(1));
                panel.step_match(true, cx);
                assert_eq!(
                    panel.finding.as_ref().and_then(|f| f.current),
                    Some(0),
                    "next wraps at the end",
                );
                panel.step_match(false, cx);
                assert_eq!(
                    panel.finding.as_ref().and_then(|f| f.current),
                    Some(1),
                    "previous wraps at the start",
                );
            });
        });
        cx.quit();
    }

    /// A failure is a state of its own: the cause, the attempted request, and a
    /// retry, not just a red line above a blank pane.
    #[gpui_kit::test]
    fn a_failure_renders_its_cause_and_a_retry(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, cx) = open(cx, None);
        view.update(cx, |panel, cx| {
            panel.document.update(cx, |document, cx| {
                document.last_url = Some("http://api.test/items".to_owned());
                document.error = Some(Failure::new(
                    FailureKind::Connect,
                    "transport error: error sending request: tcp connect error: \
                     Connection refused",
                ));
                cx.notify();
            });
        });
        draw(cx);

        assert!(cx.debug_bounds("response-error").is_some());
        assert!(cx.debug_bounds("response-error-hint").is_some());
        assert!(
            cx.debug_bounds("response-error-retry").is_some(),
            "a failure must offer the way out"
        );
        assert!(
            cx.debug_bounds("response-error-detail").is_none(),
            "the technical detail starts behind Details"
        );

        let target = cx
            .debug_bounds("response-error-details")
            .expect("the Details control is laid out")
            .center();
        cx.simulate_mouse_down(target, gpui_kit::MouseButton::Left, Default::default());
        cx.simulate_mouse_up(target, gpui_kit::MouseButton::Left, Default::default());
        cx.run_until_parked();
        draw(cx);
        assert!(cx.debug_bounds("response-error-detail").is_some());
        assert!(cx.debug_bounds("response-error-log").is_some());
        cx.quit();
    }
}
