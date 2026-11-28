//! Body decoding, syntax highlighting, find marks, and the [`DisplayCache`] the
//! Response pane keeps per record.
//!
//! These are the pure helpers behind the body view: none of them holds panel
//! state or touches a `Context`. The panel owns the cache and calls into here.

use gpui_kit::base::input::Rope;
use gpui_kit::component::highlighter::{HighlightTheme, SyntaxHighlighter};
use gpui_kit::*;

use restbook_core::ResponseRecord;

use std::ops::Range;
use std::rc::Rc;

use crate::theme::Palette;
use crate::util;

/// The mark runs for one row: each hit on it, as `start..start + len`.
///
/// `hits` is `(row, byte offset, length)` for the whole body, so the row index
/// selects the ones that belong here. Offsets index the lowercased copy the
/// search scanned; that is only safe for painting, never for slicing.
pub(super) fn mark_runs(
    row: &str,
    hits: &[(usize, usize, usize)],
    index: usize,
    style: &HighlightStyle,
    current: Option<(usize, usize, usize)>,
    current_style: &HighlightStyle,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let width = row.len();
    hits.iter()
        .filter(|(at, _, _)| *at == index)
        .filter_map(|hit @ (_, start, len)| {
            let end = (start + len).min(width);
            (*start < end).then_some((
                *start..end,
                if current == Some(*hit) {
                    *current_style
                } else {
                    *style
                },
            ))
        })
        .collect()
}

/// Merge syntax runs and search marks into one sorted, non-overlapping list.
///
/// `StyledText::with_highlights` walks ranges in order and assumes they do not
/// overlap; a mark inside a syntax run was painted past the text end. Splitting
/// the run around the mark keeps the token's colour on either side and gives the
/// mark its background.
pub(super) fn merge_highlight_runs(
    text_len: usize,
    base: &[(Range<usize>, HighlightStyle)],
    marks: &[(Range<usize>, HighlightStyle)],
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut boundaries: Vec<usize> = base
        .iter()
        .flat_map(|(range, _)| [range.start, range.end])
        .chain(marks.iter().flat_map(|(range, _)| [range.start, range.end]))
        .filter(|offset| *offset <= text_len)
        .collect();
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut runs = Vec::new();
    for pair in boundaries.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end {
            continue;
        }
        // The mark wins where the two overlap; the syntax run supplies what is
        // left on either side. A gap between runs is left out, and `StyledText`
        // paints its default style there.
        if let Some((_, style)) = marks
            .iter()
            .rev()
            .find(|(range, _)| range.start <= start && end <= range.end)
        {
            runs.push((start..end, *style));
        } else if let Some((_, style)) = base
            .iter()
            .find(|(range, _)| range.start <= start && end <= range.end)
        {
            runs.push((start..end, *style));
        }
    }
    runs
}

/// Every match of `query` across `rows`, in reading order.
///
/// Case-insensitive, because a body is text a user is scanning rather than a
/// true identifier they are locating exactly. An empty query matches nothing:
/// the find row still draws (so the field keeps focus while it is cleared) but
/// the count reads "No matches" instead of claiming every byte.
pub(super) fn find_rows(rows: &[SharedString], query: &str) -> Vec<(usize, usize, usize)> {
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
pub(super) type HighlightRun = (Range<usize>, HighlightStyle);

/// Matches for one query, flattened across the rows so next/previous is an
/// index rather than a search. Row `None` is the raw block, which is a single
/// line; `Some(n)` is row `n` of the numbered view.
pub(super) struct Finding {
    /// The query these were computed for, so a stale set is never shown.
    pub(super) query: String,
    /// `(row index, byte offset, length)` in reading order.
    ///
    /// An `Rc` because `body_view` clones it once per paint to hand it to the
    /// row builder; the hit set can be long on a repetitive body, and a deep
    /// copy per frame is what this exists to avoid.
    pub(super) hits: Rc<Vec<(usize, usize, usize)>>,
    /// The hit the arrows last moved to; `None` until one is chosen, which is
    /// what makes the count read "3 matches" rather than "1 of 3".
    pub(super) current: Option<usize>,
}

/// A rendered body, keyed by the record bytes it came from. See
/// [`ResponsePanel::display`].
pub(super) struct DisplayCache {
    pub(super) key: (usize, usize, bool),
    /// The decoded text. `SharedString` so a cache hit hands the body back as a
    /// refcount bump rather than a copy of up to `MAX_BODY_BYTES`.
    pub(super) text: SharedString,
    pub(super) truncated: bool,
    /// The width of the widest row, measured once in the font the rows paint
    /// in. The numbered rows are fixed-height and nowrap, so without this their
    /// only possible treatment of a long line is clipping at the pane edge.
    pub(super) widest_line: Pixels,
    /// The body split into rows exactly once, when the cache was filled. A
    /// repaint only borrows this; splitting on every frame would re-allocate
    /// the whole payload each time. `split('\n')` keeps the trailing empty row
    /// a final newline produces, and an empty line becomes `" "` so it still
    /// occupies a row.
    pub(super) lines: Rc<Vec<SharedString>>,
    /// Syntax runs in byte offsets over the **whole** text. The plain block (raw
    /// and oversized bodies) paints from these directly, so raw keeps its
    /// colours.
    pub(super) plain: Rc<Vec<HighlightRun>>,
    /// The same runs sliced per line, in byte offsets relative to that line.
    /// Sliced by line at cache time so the virtualized list can hand a row its
    /// own runs without translating global offsets on every frame.
    pub(super) highlights: Rc<Vec<Vec<HighlightRun>>>,
    /// How many `Set-Cookie` headers the response carried, parsed once here
    /// rather than on every frame just to size the tab's badge.
    pub(super) cookie_count: usize,
}

/// Identity of a record's body for caching purposes: where the bytes live, how
/// many there are, and how they were displayed. `ResponseRecord` is replaced
/// wholesale on each send, so a stable pointer means the same response.
pub(super) fn display_key(body: &[u8], pretty: bool) -> (usize, usize, bool) {
    (body.as_ptr() as usize, body.len(), pretty)
}

/// The syntax language for a response body. `format_label` already names the
/// payload from its content type; reusing that guess keeps the dropdown and the
/// highlighter from disagreeing. `body_text` also sniffs JSON that arrives
/// without the header, so the sniff is repeated here for the same reason.
pub(super) fn response_language(content_type: Option<&str>, text: &str) -> &'static str {
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
pub(super) fn highlight_body(
    text: &str,
    language: &str,
    theme: &HighlightTheme,
) -> Vec<HighlightRun> {
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
pub(super) fn highlights_by_line(text: &str, styles: &[HighlightRun]) -> Vec<Vec<HighlightRun>> {
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
pub(super) fn widest_line(
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
pub(super) fn summary_line(
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
pub(super) fn summary_for(
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
