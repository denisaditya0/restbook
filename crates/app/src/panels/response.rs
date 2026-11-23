//! The Response pane: a status summary over tabs for the body, headers, and
//! cookies.
//!
//! The pane owns no response; it observes the shared `Document` and redraws when
//! a send finishes. Sending lives entirely in the request pane and the workspace,
//! so this pane only reads.

use gpui_kit::base::input::Rope;
use gpui_kit::base::{
    TextSelection, TextSelectionHandle, TextSelectionRegistration, TextSelectionRun,
};
use gpui_kit::component::button::*;
use gpui_kit::component::highlighter::{HighlightTheme, SyntaxHighlighter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tab::{TabBar, TabVariant};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Disableable as _, WindowExt as _};
use gpui_kit::*;

use restbook_core::{HistoryHit, Origin, ResponseRecord, Store, Transport};

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::actions::{shortcut, SendRequest};
use crate::document::{Document, ResponseState};
// The full Lucide catalog, not the component's default list: `ClipboardCopy` is
// not in the default bundle and would paint nothing.
use crate::icons::IconName;
use crate::theme::{Metrics, Palette};
use crate::util;
use crate::workspace::settings::Settings;

/// Past this many lines the numbered rows stop being worth it: a quarter-megabyte
/// of blank lines is not a line count anyone reads, so the body becomes one
/// plain block instead.
const MAX_NUMBERED_LINES: usize = 3000;

/// How many prior sends of the current request the history sheet lists.
const HISTORY_ROWS: u32 = 20;

/// What the pane asks the workspace to do for it.
///
/// The per-request history is a window-level overlay, and a pane is not allowed
/// to place one — that is the canvas's call. So the pane hands over the rows it
/// already searched and the workspace opens the sheet.
pub(crate) enum Event {
    /// Show the prior sends of the request on screen.
    ShowHistory { hits: Vec<HistoryHit> },
    /// Forget one cookie in the shared jar.
    DeleteCookie { name: String },
    /// Replace one cookie's value in the shared jar.
    UpdateCookie { name: String, value: String },
}

/// One line for the interaction log. Exhaustive, so a new variant must name
/// itself before it can go unlogged.
impl Event {
    pub(crate) fn summary(&self) -> String {
        match self {
            Event::ShowHistory { hits } => {
                format!("event: response history with {} rows", hits.len())
            }
            Event::DeleteCookie { name } => format!("event: response cookie delete {name}"),
            Event::UpdateCookie { name, .. } => {
                format!("event: response cookie update {name}")
            }
        }
    }
}

/// Which view of the response is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseTab {
    Body,
    Headers,
    Cookies,
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

/// One row of the Cookies tab: a cookie the session would send to the URL, or,
/// when the session holds none for it, one the stored response set.
struct CookieRow {
    /// Element identity: index plus name, because two cookies can share a name
    /// across paths.
    key: String,
    name: String,
    value: String,
    attributes: String,
    /// Whether the row is in the live jar, which is what makes it editable.
    live: bool,
}

/// An in-place edit of one cookie's value. The field is dropped with the edit,
/// which also drops its subscription.
struct CookieEdit {
    name: String,
    value: Entity<InputState>,
    _subscription: Subscription,
}

pub(crate) struct ResponsePanel {
    palette: Palette,
    metrics: Metrics,
    document: Entity<Document>,
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
    /// The find query and its field (U56). Empty means the find row is closed.
    find: Entity<InputState>,
    /// Matches for `query` in the body rows, recomputed when the query or the
    /// decoded body changes rather than per frame. `(row, byte offset in row)`
    /// for each match start, plus the index the arrows are centred on.
    finding: Option<Finding>,
    focus_handle: FocusHandle,
}

/// The mark runs for one row: each hit on it, as `start..start + len`.
///
/// `hits` is `(row, byte offset, length)` for the whole body, so the row index
/// selects the ones that belong here. Offsets index the lowercased copy the
/// search scanned; that is only safe for painting, never for slicing.
fn mark_runs(
    row: &str,
    hits: &[(usize, usize, usize)],
    index: usize,
    style: &HighlightStyle,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let width = row.len();
    hits.iter()
        .filter(|(at, _, _)| *at == index)
        .filter_map(|(_, start, len)| {
            let end = (start + len).min(width);
            (*start < end).then_some((*start..end, *style))
        })
        .collect()
}

/// Every match of `query` across `rows`, in reading order.
///
/// Case-insensitive, because a body is text a user is scanning rather than a
/// true identifier they are locating exactly. An empty query matches nothing:
/// the find row still draws (so the field keeps focus while it is cleared) but
/// the count reads "No matches" instead of claiming every byte.
fn find_rows(rows: &[SharedString], query: &str) -> Vec<(usize, usize, usize)> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let haystack = row.to_lowercase();
        let mut from = 0;
        // Deliberately not overlapping: `aa` in `aaa` is one match, not two.
        // The offsets index the lowercased copy, which is only safe because
        // this is used for painting a mark, never for slicing the original.
        while let Some(offset) = haystack[from..].find(&needle) {
            let at = from + offset;
            hits.push((index, at, needle.len()));
            from = at + needle.len();
        }
    }
    hits
}

/// One syntax run: a byte range and the style it paints in.
type HighlightRun = (Range<usize>, HighlightStyle);

/// Matches for one query, flattened across the rows so next/previous is an
/// index rather than a search. Row `None` is the raw block, which is a single
/// line; `Some(n)` is row `n` of the numbered view.
struct Finding {
    /// The query these were computed for, so a stale set is never shown.
    query: String,
    /// `(row index, byte offset, length)` in reading order.
    ///
    /// An `Rc` because `body_view` clones it once per paint to hand it to the
    /// row builder; the hit set can be long on a repetitive body, and a deep
    /// copy per frame is what this exists to avoid.
    hits: Rc<Vec<(usize, usize, usize)>>,
    /// The hit the arrows last moved to; `None` until one is chosen, which is
    /// what makes the count read "3 matches" rather than "1 of 3".
    current: Option<usize>,
}

/// A rendered body, keyed by the record bytes it came from. See
/// [`ResponsePanel::display`].
struct DisplayCache {
    key: (usize, usize, bool),
    /// The decoded text. `SharedString` so a cache hit hands the body back as a
    /// refcount bump rather than a copy of up to `MAX_BODY_BYTES`.
    text: SharedString,
    truncated: bool,
    /// The width of the widest row, measured once in the font the rows paint
    /// in. The numbered rows are fixed-height and nowrap, so without this their
    /// only possible treatment of a long line is clipping at the pane edge.
    widest_line: Pixels,
    /// The body split into rows exactly once, when the cache was filled. A
    /// repaint only borrows this; splitting on every frame would re-allocate
    /// the whole payload each time. `split('\n')` keeps the trailing empty row
    /// a final newline produces, and an empty line becomes `" "` so it still
    /// occupies a row.
    lines: Rc<Vec<SharedString>>,
    /// Syntax runs in byte offsets over the **whole** text. The plain block (raw
    /// and oversized bodies) paints from these directly, so raw keeps its
    /// colours.
    plain: Rc<Vec<HighlightRun>>,
    /// The same runs sliced per line, in byte offsets relative to that line.
    /// Sliced by line at cache time so the virtualized list can hand a row its
    /// own runs without translating global offsets on every frame.
    highlights: Rc<Vec<Vec<HighlightRun>>>,
    /// How many `Set-Cookie` headers the response carried, parsed once here
    /// rather than on every frame just to size the tab's badge.
    cookie_count: usize,
}

/// Identity of a record's body for caching purposes: where the bytes live, how
/// many there are, and how they were displayed. `ResponseRecord` is replaced
/// wholesale on each send, so a stable pointer means the same response.
fn display_key(body: &[u8], pretty: bool) -> (usize, usize, bool) {
    (body.as_ptr() as usize, body.len(), pretty)
}

/// The syntax language for a response body. `format_label` already names the
/// payload from its content type; reusing that guess keeps the dropdown and the
/// highlighter from disagreeing. `body_text` also sniffs JSON that arrives
/// without the header, so the sniff is repeated here for the same reason.
fn response_language(content_type: Option<&str>, text: &str) -> &'static str {
    let content_type = content_type.unwrap_or_default();
    if content_type.contains("json") {
        "json"
    } else if content_type.contains("html") {
        "html"
    } else if text.trim_start().starts_with('{') || text.trim_start().starts_with('[') {
        "json"
    } else {
        "text"
    }
}

/// Parse and style a body once, when the display cache is refreshed. The theme
/// is resolved here too, which is why scrolling does not reparse or recolour.
/// Runs come back in byte offsets over the whole text; [`highlights_by_line`]
/// slices them for the row list.
fn highlight_body(text: &str, language: &str, theme: &HighlightTheme) -> Vec<HighlightRun> {
    if language == "text" {
        return Vec::new();
    }

    let mut highlighter = SyntaxHighlighter::new(language);
    let rope = Rope::from(text);
    highlighter.update(None, &rope, None);
    highlighter.styles(&(0..text.len()), theme)
}

/// Slice whole-document highlight runs into runs relative to each line.
///
/// The body is drawn one row per line, so a run that spans a newline (a block
/// comment, a multi-line string) has to become one run per line with offsets
/// rebased on that line's start. `text.split('\n')` is the renderer's source of
/// truth for line count, so a trailing newline still contributes an empty line.
fn highlights_by_line(text: &str, styles: &[HighlightRun]) -> Vec<Vec<HighlightRun>> {
    let mut starts = vec![0usize];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }

    let mut lines = vec![Vec::new(); starts.len()];
    for (range, style) in styles {
        let style = *style;
        let mut line = starts
            .binary_search(&range.start)
            .unwrap_or_else(|next| next.saturating_sub(1));
        while line < starts.len() {
            let line_start = starts[line];
            let line_end = starts.get(line + 1).map_or(text.len(), |next| next - 1);
            let start = range.start.max(line_start);
            let end = range.end.min(line_end);
            if start < end {
                lines[line].push((start - line_start..end - line_start, style));
            }
            if range.end <= line_end {
                break;
            }
            line += 1;
        }
    }
    lines
}

/// The width of the widest row, shaped in the body's own font and size.
///
/// Called once per decoded body, not per frame: [`ResponsePanel::body_view`]
/// repaints on every scroll, and shaping thousands of lines there would make
/// scrolling drag. The run's colour does not affect advances, so a transparent
/// one is enough to measure with.
fn widest_line(
    lines: &[SharedString],
    family: &SharedString,
    text_size: f32,
    window: &Window,
) -> Pixels {
    let font = gpui_kit::font(family.clone());
    let text_system = window.text_system();
    lines.iter().fold(px(0.), |widest, line| {
        let run = TextRun {
            len: line.len(),
            font: font.clone(),
            color: transparent_black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let width = text_system
            .layout_line(line.as_ref(), px(text_size), &[run], None)
            .width;
        if width > widest {
            width
        } else {
            widest
        }
    })
}

/// Join the summary's parts with the same `·` separator the header has always
/// drawn, returning both the copied string and the per-part highlight runs.
///
/// One function owns the string so the visible line and the Copy button cannot
/// drift: every caller copies the text this returns, not a second `format!`.
fn summary_line(
    segments: impl IntoIterator<Item = (String, HighlightStyle)>,
    separator: HighlightStyle,
) -> (String, Vec<(Range<usize>, HighlightStyle)>) {
    let mut text = String::new();
    let mut spans = Vec::new();
    for (index, (part, style)) in segments.into_iter().enumerate() {
        if index > 0 {
            let start = text.len();
            text.push_str(" · ");
            spans.push((start..text.len(), separator));
        }
        let start = text.len();
        text.push_str(&part);
        spans.push((start..text.len(), style));
    }
    (text, spans)
}

/// The status summary: status, duration, and size joined the way the header
/// draws them. The header paints this string and the pane menu copies it, so
/// there is one owner of the format rather than a second `format!` that could
/// drift from it.
fn summary_for(
    response: &ResponseRecord,
    palette: &Palette,
) -> (String, Vec<(Range<usize>, HighlightStyle)>) {
    summary_line(
        [
            (
                util::status_text(response.status),
                HighlightStyle {
                    color: Some(palette.status(response.status)),
                    font_weight: Some(FontWeight::MEDIUM),
                    ..HighlightStyle::default()
                },
            ),
            (
                util::duration(response.duration_ms),
                HighlightStyle::color(palette.muted),
            ),
            (
                util::human_bytes(response.size_bytes()),
                HighlightStyle::color(palette.muted),
            ),
        ],
        HighlightStyle::color(palette.subtle),
    )
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

impl IntoElement for SelectableLine {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
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
        let hits = self.history.clone();

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
                .disabled(hits.is_empty())
                .on_click(move |_, _, cx| {
                    history.update(cx, |_, cx| {
                        cx.emit(Event::ShowHistory { hits: hits.clone() })
                    });
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
        // Any change to the document — a finished send, an error, a cancel —
        // is a reason to redraw, and a new response or URL is also the moment
        // the history sheet may need new rows.
        cx.observe(&document, |this, _, cx| this.sync_history(cx))
            .detach();
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
        let mut panel = Self {
            palette: Palette::new(cx),
            metrics: Metrics::current(cx),
            document,
            store,
            transport,
            cookie_edit: None,
            history: Vec::new(),
            history_query: String::new(),
            history_origin: Origin::default(),
            history_stamp: None,
            history_revision: 0,
            pretty,
            tab: ResponseTab::Body,
            display: None,
            tabs_scroll: ScrollHandle::new(),
            find,
            finding: None,
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

    /// Refresh the cached history when the current request or its newest
    /// response changed, then redraw. Progress ticks notify many times per
    /// second and must not each search the store, so the store is hit and the
    /// pane redraws only on a real change in `(query, stamp)` — plus every tick
    /// while pending, since the elapsed clock is derived at paint time.
    fn sync_history(&mut self, cx: &mut Context<Self>) {
        let (pending, query, origin, stamp, revision) = {
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
            self.reload_history();
        }

        // While pending the clock has to advance on every heartbeat; once the
        // send resolves, a notify is a one-off, so a redraw with an unchanged
        // key would only burn a frame.
        if pending || changed {
            cx.notify();
        }
    }

    /// Re-read the rows for the current request.
    ///
    /// A saved request or a draft owns its rows, so it is asked for by owner:
    /// two requests with the same URL (a copy, or the same endpoint filed
    /// twice) must not share a send log. `Standalone` has no owner, and a
    /// backend that cannot answer by owner returns `None` (the encrypted store
    /// until origins are persisted), so both fall back to the URL search. An
    /// empty query there means there is no request to scope to at all.
    fn reload_history(&mut self) {
        let scoped = if self.history_origin == Origin::Standalone {
            None
        } else {
            self.store
                .history_for_origin(&self.history_origin, HISTORY_ROWS)
                .unwrap_or(None)
        };

        self.history = match scoped {
            Some(hits) => hits,
            None if self.history_query.is_empty() => Vec::new(),
            None => self
                .store
                .search_history(&self.history_query, HISTORY_ROWS)
                .unwrap_or_default(),
        };
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
        cx: &App,
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
        (text, truncated)
    }

    /// The response header: what was received on the left, how it went on the
    /// right — `Body | Headers 3 | Cookies` … `200 OK · 593 ms · 12.17 KB ·
    /// 🕘 1 minute ago`. One line, because the response is read far more often
    /// than its headers are compared.
    ///
    /// The copy controls are **not** here. They act on the body, which the
    /// Headers and Cookies tabs do not show, so the header was offering a copy
    /// of something absent from the tab it was drawn under. They live in the
    /// body's own toolbar; see [`Self::render_body_toolbar`].
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
            .into_any_element()
    }

    /// The history control: the relative time of the newest send of this
    /// request. Clicking it asks the workspace for a sheet listing the prior
    /// sends; a dropdown would be clipped at the window edge, and the rows need
    /// the full height to be readable. Absent when the request has no history,
    /// so an unsent request keeps the header short.
    fn render_history(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let newest = self.history.first()?;
        let label = relative_time(&newest.at);
        let palette = self.palette.clone();
        let hits = self.history.clone();

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
                            cx.emit(Event::ShowHistory { hits: hits.clone() });
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
                        log::info!(target: "restbook_app::ui", "response format: {label}");
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
                    log::info!(target: "restbook_app::ui", "response tab: index {index}");
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

    /// Recompute the matches when the query or the decoded body changed.
    ///
    /// Called from the input's own change observer and from `body_for_display`,
    /// never from paint: a scroll repaints continuously, and rescanning a
    /// quarter-megabyte body on every frame is the cost this exists to avoid.
    fn refresh_finding(&mut self, cx: &mut Context<Self>) {
        let query = self.find.read(cx).value().to_string();
        let Some(cache) = self.display.as_ref() else {
            self.finding = None;
            return;
        };

        // Same shape rule as the body view, so a match never points at a row
        // the user cannot see.
        let hits = if self.numbered() {
            find_rows(&cache.lines, &query)
        } else {
            find_rows(std::slice::from_ref(&cache.text), &query)
        };

        // Keep the current position only while the query is the same, so
        // typing narrows the set from the top rather than jumping to an index
        // that no longer means anything.
        let current = self
            .finding
            .as_ref()
            .filter(|old| old.query == query)
            .and_then(|old| old.current)
            .filter(|index| *index < hits.len());
        self.finding = Some(Finding {
            query,
            hits: Rc::new(hits),
            current,
        });
    }

    /// Move to the next or previous match and return it, wrapping at the ends.
    ///
    /// The first press after typing picks the first match rather than the
    /// second, which is what makes Enter behave like the field's own "go".
    fn step_match(
        &mut self,
        forward: bool,
        cx: &mut Context<Self>,
    ) -> Option<(usize, usize, usize)> {
        self.refresh_finding(cx);
        let finding = self.finding.as_mut()?;
        if finding.hits.is_empty() {
            return None;
        }
        let total = finding.hits.len();
        let next = match (finding.current, forward) {
            (None, _) => {
                if forward {
                    0
                } else {
                    total - 1
                }
            }
            (Some(current), true) => (current + 1) % total,
            (Some(current), false) => (current + total - 1) % total,
        };
        finding.current = Some(next);
        cx.notify();
        Some(finding.hits[next])
    }

    /// The find row under the body toolbar: the query field, the count, and
    /// next/previous. Absent until the query is non-empty, so the toolbar is not
    /// permanently wider than it needs to be.
    fn render_find_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = &self.palette;
        let metrics = &self.metrics;
        let (hits, current) = self
            .finding
            .as_ref()
            .map(|finding| (finding.hits.len(), finding.current))
            .unwrap_or((0, None));
        let count = match (hits, current) {
            (0, _) => "No matches".to_owned(),
            (total, Some(index)) => format!("{} of {total}", index + 1),
            (total, None) => format!("{total} matches"),
        };

        h_flex()
            .flex_none()
            .items_center()
            .gap(px(metrics.space_xs))
            .px(px(metrics.space_md))
            .py(px(metrics.space_xs))
            .border_b_1()
            .border_color(palette.border)
            .debug_selector(|| "response-find-row".to_owned())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.find).id("response-find")),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(palette.muted)
                    .debug_selector(|| "response-find-count".to_owned())
                    .child(count),
            )
            .child(
                Button::new("find-prev")
                    .icon(IconName::ChevronUp)
                    .ghost()
                    .compact()
                    .debug_selector(|| "response-find-prev".to_owned())
                    .tooltip("Previous match")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.step_match(false, cx);
                    })),
            )
            .child(
                Button::new("find-next")
                    .icon(IconName::ChevronDown)
                    .ghost()
                    .compact()
                    .debug_selector(|| "response-find-next".to_owned())
                    .tooltip("Next match")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.step_match(true, cx);
                    })),
            )
    }

    /// The body's own toolbar: the format switch and both copy controls.
    ///
    /// These describe the payload, so they belong beside it rather than in the
    /// header, which also draws for the Headers and Cookies tabs. Moving them
    /// here is what makes the header one line of facts — status, time, size —
    /// with no controls whose target is off-screen.
    ///
    /// The summary copy stays even though the summary is selectable: a drag is
    /// the fine-grained gesture, and the button is the one-click "give me the
    /// whole thing" that does not depend on hitting a row boundary.
    fn render_body_toolbar(&self, response: &ResponseRecord, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;
        let (summary, _) = summary_for(response, palette);
        let copy_text = summary;

        let find_open = !self.find.read(cx).value().is_empty();
        let find_button = Button::new("find-body")
            .icon(IconName::Search)
            .ghost()
            .compact()
            .debug_selector(|| "response-find-toggle".to_owned())
            .tooltip("Find in the body")
            .on_click(cx.listener(|this, _, window, cx| {
                log::info!(target: "restbook_app::ui", "response: find");
                this.find.update(cx, |state, cx| {
                    state.focus_handle(cx).focus(window, cx);
                });
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
                                log::info!(target: "restbook_app::ui", "response: copy body");
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
                                log::info!(target: "restbook_app::ui", "response: copy summary");
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                            })),
                    ),
            )
            .children(find_open.then(|| self.render_find_row(cx)))
            .into_any_element()
    }

    fn render_body(&mut self, response: &ResponseRecord, window: &Window, cx: &App) -> AnyElement {
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
            // appended after the syntax runs so a highlighted token keeps its
            // colour.
            let match_style = HighlightStyle {
                background_color: Some(self.palette.search_match),
                ..Default::default()
            };
            let hits = self
                .finding
                .as_ref()
                .map(|finding| finding.hits.clone())
                .unwrap_or_default();
            let styled = {
                let mut all: Vec<(Range<usize>, HighlightStyle)> = spans
                    .iter()
                    .map(|(range, style)| (range.clone(), *style))
                    .collect();
                all.extend(mark_runs(&body, &hits, 0, &match_style));
                if all.is_empty() {
                    styled
                } else {
                    styled.with_highlights(all)
                }
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
                .overflow_scroll()
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
        let hits_by_row: Rc<Vec<(usize, usize, usize)>> = self
            .finding
            .as_ref()
            .map(|finding| finding.hits.clone())
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
            .overflow_x_scroll()
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
                                                // The search mark is appended after the syntax
                                                // runs, so a highlighted token keeps its colour
                                                // and only gains a background.
                                                let mut all: Vec<(Range<usize>, HighlightStyle)> =
                                                    spans.to_vec();
                                                all.extend(mark_runs(
                                                    &lines[index],
                                                    &hits_by_row,
                                                    index,
                                                    &match_style,
                                                ));
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
            .overflow_y_scroll()
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

    /// The Cookies tab: what the session would send to this URL, one row
    /// editable and removable at a time. A response restored from history has
    /// no jar entries, so its cookies are listed read-only rather than offering
    /// an edit that would change nothing.
    fn render_cookies(&self, response: &ResponseRecord, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;
        let stored = util::cookies(&response.headers);
        let session = self.transport.cookies(&self.document.read(cx).url());
        let live = !session.is_empty();

        let rows = if live {
            session
                .iter()
                .enumerate()
                .filter_map(|(index, pair)| {
                    let (name, value) = pair.split_once('=')?;
                    Some(CookieRow {
                        key: format!("{index}-{name}"),
                        attributes: stored
                            .iter()
                            .find(|cookie| cookie.name == name)
                            .map(|cookie| cookie.attributes.clone())
                            .unwrap_or_default(),
                        name: name.to_owned(),
                        value: value.to_owned(),
                        live: true,
                    })
                })
                .collect::<Vec<_>>()
        } else {
            stored
                .into_iter()
                .enumerate()
                .map(|(index, cookie)| CookieRow {
                    key: format!("{index}-{}", cookie.name),
                    name: cookie.name,
                    value: cookie.value,
                    attributes: cookie.attributes,
                    live: false,
                })
                .collect()
        };

        if rows.is_empty() {
            return empty_state("No cookies received.", palette);
        }

        let note = if live {
            "The session sends these to this URL."
        } else {
            "This stored response is not in the session, so its cookies cannot be changed."
        };

        v_flex()
            .id("response-cookies")
            .debug_selector(|| "response-cookies".to_owned())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(self.metrics.space_lg))
            .py(px(self.metrics.space_sm))
            .font_family(palette.mono_font.clone())
            .text_size(px(self.metrics.font_small))
            .child(
                div()
                    .pb(px(self.metrics.space_sm))
                    .text_xs()
                    .text_color(palette.muted)
                    .child(note),
            )
            .children(rows.into_iter().map(|row| self.render_cookie_row(row, cx)))
            .into_any_element()
    }

    fn render_cookie_row(&self, row: CookieRow, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;
        let metrics = &self.metrics;
        let editing = self
            .cookie_edit
            .as_ref()
            .is_some_and(|edit| edit.name == row.name);

        let value: AnyElement = match (editing, self.cookie_edit.as_ref()) {
            (true, Some(edit)) => div()
                .flex_1()
                .min_w_0()
                .child(Input::new(&edit.value).id(format!("cookie-value-{}", row.key)))
                .into_any_element(),
            _ => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(row.value.clone())
                .into_any_element(),
        };

        let actions: AnyElement = if editing {
            h_flex()
                .flex_none()
                .gap(px(metrics.space_xs))
                .items_center()
                .child(
                    Button::new(format!("cookie-save-{}", row.key))
                        .icon(IconName::Check)
                        .ghost()
                        .compact()
                        .tooltip("Save cookie value")
                        .on_click(cx.listener(|this, _, _, cx| this.commit_cookie_edit(cx))),
                )
                .child(
                    Button::new(format!("cookie-cancel-{}", row.key))
                        .icon(IconName::X)
                        .ghost()
                        .compact()
                        .tooltip("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_cookie_edit(cx))),
                )
                .into_any_element()
        } else {
            let live = row.live;
            let edit_name = row.name.clone();
            let edit_value = row.value.clone();
            let delete_name = row.name.clone();
            let why = if live {
                "Edit this cookie's value"
            } else {
                "This cookie is not in the session"
            };
            h_flex()
                .flex_none()
                .gap(px(metrics.space_xs))
                .items_center()
                .child(
                    Button::new(format!("cookie-edit-{}", row.key))
                        .icon(IconName::Pencil)
                        .ghost()
                        .compact()
                        .disabled(!live)
                        .tooltip(why)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_cookie_edit(
                                edit_name.clone(),
                                edit_value.clone(),
                                window,
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new(format!("cookie-delete-{}", row.key))
                        .icon(IconName::Trash)
                        .ghost()
                        .compact()
                        .disabled(!live)
                        .tooltip("Delete this cookie")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.cookie_edit = None;
                            cx.emit(Event::DeleteCookie {
                                name: delete_name.clone(),
                            });
                        })),
                )
                .into_any_element()
        };

        h_flex()
            .id(format!("response-cookie-{}", row.key))
            .debug_selector({
                let key = row.key.clone();
                move || format!("response-cookie-{key}")
            })
            .gap(px(metrics.space_md))
            .py(px(metrics.space_xs))
            .items_center()
            .border_b_1()
            .border_color(palette.border)
            .child(
                div()
                    .w(px(160.))
                    .flex_none()
                    .truncate()
                    .text_color(palette.info)
                    .child(row.name),
            )
            .child(value)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(palette.muted)
                    .child(row.attributes),
            )
            .child(actions)
            .into_any_element()
    }

    /// Begin editing one row in place. The field is created here, once, so the
    /// caret survives a repaint; Enter commits through the subscription.
    fn start_cookie_edit(
        &mut self,
        name: String,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("cookie value"));
        input.update(cx, |state, cx| {
            state.set_value(value, window, cx);
            state.focus(window, cx);
        });
        let subscription = cx.subscribe_in(&input, window, {
            move |this, _input, event: &InputEvent, _window, cx| {
                if matches!(
                    event,
                    InputEvent::PressEnter {
                        secondary: false,
                        shift: false
                    }
                ) {
                    this.commit_cookie_edit(cx);
                }
            }
        });
        self.cookie_edit = Some(CookieEdit {
            name,
            value: input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Hand the edited value to the workspace, which owns the jar.
    fn commit_cookie_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.cookie_edit.take() else {
            return;
        };
        let value = edit.value.read(cx).value().to_string();
        cx.emit(Event::UpdateCookie {
            name: edit.name,
            value,
        });
        cx.notify();
    }

    fn cancel_cookie_edit(&mut self, cx: &mut Context<Self>) {
        self.cookie_edit = None;
        cx.notify();
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
            ResponseState::Error { .. } => "The request failed. The error is shown above.",
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

    /// `Idle` means nothing has been sent (or the response was wiped), so
    /// there is no pending branch here: a live send renders through
    /// [`ResponsePanel::render_status`].
    fn render_idle(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(self.metrics.space_xs))
            .debug_selector(|| "response-idle".to_owned())
            .children(self.render_earlier_sends(cx))
            .child(
                div()
                    .text_sm()
                    .text_color(palette.muted)
                    .child("Send a request to see the response."),
            )
            .children(shortcut(&SendRequest, window).map(|key| {
                div()
                    .text_xs()
                    .text_color(palette.muted)
                    .child(format!("{key} sends from anywhere in the request pane."))
            }))
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
        let hits = self.history.clone();

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
                            cx.emit(Event::ShowHistory { hits: hits.clone() });
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_error(&self, message: Option<&str>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let message = message?;
        let palette = &self.palette;

        Some(
            h_flex()
                .px(px(self.metrics.space_lg))
                .py(px(self.metrics.space_sm))
                .gap(px(self.metrics.space_sm))
                .items_start()
                .bg(palette.danger_wash)
                .border_b_1()
                .border_color(palette.danger_border)
                .debug_selector(|| "response-error".to_owned())
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(palette.danger)
                        .child("Request failed"),
                )
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(palette.foreground)
                        .child(message.to_owned()),
                )
                // The evidence for the failure is in the log, and the file is
                // not obvious. Put the copy action where the error is, instead
                // of asking the user to find it in a menu afterwards.
                .child(
                    Button::new("copy-log-on-error")
                        .icon(IconName::ClipboardCopy)
                        .ghost()
                        .compact()
                        .tooltip("Copy the recent log, for a bug report")
                        .on_click(cx.listener(|_, _, window, cx| {
                            match crate::logging::tail(200) {
                                Some(text) if !text.is_empty() => {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                                    window.push_notification(
                                        Notification::success("Copied the recent log"),
                                        cx,
                                    );
                                }
                                _ => window.push_notification(
                                    Notification::error("No log file is being written"),
                                    cx,
                                ),
                            }
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

        v_flex()
            .size_full()
            .bg(palette.background)
            .track_focus(&self.focus_handle)
            // The root selector the element-tree dump reads first; see
            // `preview::tree`.
            .debug_selector(|| "response-pane".to_owned())
            .children(self.render_error(error.as_deref(), cx))
            .child(match (state, &response) {
                // The finished view is unchanged: header, tabs, and body.
                (ResponseState::Completed { .. }, Some(response)) => {
                    self.render_response(response, window, cx)
                }
                (ResponseState::Idle, _) => self.render_idle(window, cx),
                // In flight, cancelled, or failed: there is no status or tab
                // to show, so a summary strip stands in for the header.
                (state, _) => self.render_status(state, cx),
            })
    }
}

#[cfg(test)]
mod tests {
    // Named imports only: `gpui_kit::*` brings in a `test` macro that shadows
    // `#[test]`, and `use super::*` would drag it in.
    use super::{
        display_key, find_rows, highlights_by_line, mark_runs, response_language, summary_line,
    };
    use gpui_kit::HighlightStyle;

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
        let marks = mark_runs("abcdef", &hits, 0, &style);
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].0, 2..5);
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
    use restbook_core::{HistoryEntry, Method, Origin, RequestDef, ResponseRecord, Store};
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

    /// Every body shape `scripts/sample-server.py` serves, so a failure here
    /// can be reproduced by hand against the same route. The old paint test
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
    /// It used to split every `Set-Cookie` header inside `render_tabs`, which
    /// runs on every frame; a scroll alone repaints continuously, so a response
    /// with many cookies paid for the badge on each one. What is asserted is the
    /// cache: the count is stored with the decoded body, so a repaint reads it
    /// instead of recomputing it.
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

    /// Selectors the response pane records for the element-tree dump. The body
    /// shapes are alternatives (numbered rows, or the plain block), so a tree
    /// simply leaves out whichever one the current view did not lay out.
    const TREE_SELECTORS: &[&str] = &[
        "response-pane",
        "response-summary",
        "response-tabs",
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
    /// A real screenshot needs a GPU and a font stack, which the mock window
    /// does not have. What it does record is the bounds of every element that
    /// opted in with `debug_selector`, so the tree is the assertion and the file
    /// under `target/preview/` is the visual diff: two builds can be compared
    /// with `git diff`, and a pane whose layout moved shows up as changed lines.
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
    /// The header also draws for the Headers and Cookies tabs, so a copy button
    /// there offers a copy of something the current tab does not show. They
    /// moved into the body's own toolbar, which only the Body tab draws. The
    /// assertion is positional: each button's top edge is at or below the
    /// toolbar's, and the toolbar is below the header's summary row.
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

    /// Every state the pane has must lay out and paint without panicking. A
    /// body that overflows a fixed-height row, or a virtualized list built at
    /// zero height, is exactly what a plain unit test cannot reach. The shapes
    /// are the ones `scripts/sample-server.py` serves, so a failure here can
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
        let found = view.update(cx, |panel, _| panel.history.len());
        assert_eq!(found, 2, "the reopened tab should see both stored sends");

        draw(cx);
        assert!(
            cx.debug_bounds("response-history-idle-control").is_some(),
            "the idle pane should offer the earlier sends"
        );

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
        assert_eq!(
            other.update(vcx, |panel, _| panel.history.len()),
            0,
            "an unsent request with a shared URL still has no history of its own"
        );

        cx.quit();
    }

    /// A delete made outside this pane (the sidebar, the history sheet) bumps
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
            .history_for_origin(&owner, 10)
            .unwrap()
            .unwrap()
            .first()
            .expect("one seeded send")
            .id;

        let (view, vcx) = open_owned(&mut cx, None, store.clone(), None, owner, jar(&[]));
        assert_eq!(view.update(vcx, |panel, _| panel.history.len()), 1);

        store.delete_history(id).unwrap();
        view.update(vcx, |panel, cx| {
            panel.document.update(cx, |document, cx| {
                document.touch_history();
                cx.notify();
            });
            panel.sync_history(cx);
        });

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
}
