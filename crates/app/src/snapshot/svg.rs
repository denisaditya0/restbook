//! The SVG a snapshot is drawn from.
//!
//! Text is one `<text>` per line, since the layout already knows where every
//! line goes. A block is a single column and the canvas widens to hold the
//! longest line up to [`MAX_WIDTH`]; only a line past that cap wraps, so a
//! pretty-printed body keeps the lines it was written with.

use super::Snapshot;

/// Everything the drawing needs, resolved from the theme by the caller so this
/// module never reads a palette.
pub(crate) struct Style {
    pub(crate) background: String,
    pub(crate) foreground: String,
    pub(crate) muted: String,
    pub(crate) border: String,
    pub(crate) mono: String,
    /// The narrowest the canvas gets. A wide block widens past this up to
    /// [`Self::max_width`], so a long code line stays on one line instead of
    /// wrapping into a narrow column.
    pub(crate) width: f32,
    /// The widest the canvas grows before a line wraps.
    pub(crate) max_width: f32,
    /// When set, the body stops once the image would grow past this height and
    /// the rest is counted, not drawn. `None` draws the whole body.
    pub(crate) max_height: Option<f32>,
    pub(crate) padding: f32,
    pub(crate) font_size: f32,
    pub(crate) line_height: f32,
    pub(crate) title_size: f32,
}

/// The widest the canvas grows before a line wraps, for an image that is not
/// compacted. Wide enough for ordinary pretty-printed JSON, bounded so a
/// minified body does not make a mile-wide image.
pub(crate) const MAX_WIDTH: f32 = 2400.0;

/// A monospace advance is a little over half the font size; the estimate only
/// has to keep a line inside the canvas, so it is deliberately conservative.
const ADVANCE: f32 = 0.62;

impl Style {
    /// How many characters of `size` text fit the canvas `width`.
    fn chars_for(&self, width: f32, size: f32) -> usize {
        let inner = (width - self.padding * 2.0).max(1.0);
        ((inner / (size * ADVANCE)).floor() as usize).max(16)
    }
}

/// Draw the whole image and return the SVG document.
pub(crate) fn render(snapshot: &Snapshot, style: &Style) -> String {
    let width = canvas_width(snapshot, style);
    let block_chars = style.chars_for(width, style.font_size);
    let head_chars = style.chars_for(width, style.title_size);

    let head_lines = wrap_line(&snapshot.head.request, head_chars);
    let blocks: Vec<Vec<String>> = snapshot
        .blocks
        .iter()
        .map(|block| wrap_all(&block.text, block_chars))
        .collect();
    let total_lines: usize = blocks.iter().map(Vec::len).sum();

    // The last baseline a body line may sit on and still leave room for the
    // omitted-lines footer and the bottom padding inside a compact image.
    let limit = style
        .max_height
        .map(|max| max - style.padding * 2.0 - style.line_height * 2.0);

    let mut body = String::new();
    let mut y = style.padding;

    y += style.title_size;
    for line in &head_lines {
        body.push_str(&text(
            style.padding,
            y,
            style.title_size,
            &style.foreground,
            line,
            style,
        ));
        y += style.line_height;
    }
    body.push_str(&text(
        style.padding,
        y,
        style.font_size,
        &style.muted,
        &snapshot.head.outcome,
        style,
    ));
    y += style.line_height + style.padding * 0.5;
    body.push_str(&format!(
        "<line x1=\"{x1}\" y1=\"{y}\" x2=\"{x2}\" y2=\"{y}\" stroke=\"{stroke}\" stroke-width=\"1\"/>",
        x1 = style.padding,
        x2 = width - style.padding,
        stroke = style.border,
    ));

    let mut drawn = 0;
    let mut clipped = false;
    'blocks: for (block, lines) in snapshot.blocks.iter().zip(&blocks) {
        if let Some(limit) = limit {
            // A label with no line under it is not worth the space, so a block
            // only starts if the label and its first line both fit.
            let label = style.padding * 0.5 + style.font_size + style.line_height * 0.75;
            if y + label + style.line_height > limit {
                clipped = true;
                break;
            }
        }
        y += style.padding * 0.5 + style.font_size;
        body.push_str(&text(
            style.padding,
            y,
            style.font_size,
            &style.muted,
            block.label,
            style,
        ));
        y += style.line_height * 0.75;
        for line in lines {
            if let Some(limit) = limit {
                if y + style.line_height > limit {
                    clipped = true;
                    break 'blocks;
                }
            }
            y += style.line_height;
            body.push_str(&text(
                style.padding,
                y,
                style.font_size,
                &style.foreground,
                line,
                style,
            ));
            drawn += 1;
        }
    }

    // A clipped image says how much it left out, so it does not read as if the
    // body ended there.
    if clipped {
        y += style.line_height;
        body.push_str(&text(
            style.padding,
            y,
            style.font_size,
            &style.muted,
            &format!("\u{2026} {} more lines", total_lines - drawn),
            style,
        ));
    }

    let height = (y + style.padding).ceil();
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\">\
         <rect width=\"{width}\" height=\"{height}\" fill=\"{background}\"/>{body}</svg>",
        width = width,
        height = height,
        background = style.background,
        body = body,
    )
}

/// The canvas width: the narrowest that holds the longest line without wrapping,
/// clamped between the style's floor and [`MAX_WIDTH`]. Widening beats wrapping
/// for code, where a wrapped pretty-printed line reads worse than a wider image.
fn canvas_width(snapshot: &Snapshot, style: &Style) -> f32 {
    let mut block = 0;
    for text_block in &snapshot.blocks {
        for line in text_block.text.lines() {
            block = block.max(line.chars().count());
        }
    }
    let head = snapshot
        .head
        .request
        .chars()
        .count()
        .max(snapshot.head.outcome.chars().count());
    // One extra character of slack so the longest line always fits after the
    // char-count is floored; the extra is a hair of right margin, not a wrap.
    let block_px = style.padding * 2.0 + (block as f32 + 1.0) * style.font_size * ADVANCE;
    let head_px = style.padding * 2.0 + (head as f32 + 1.0) * style.title_size * ADVANCE;
    block_px.max(head_px).clamp(style.width, style.max_width)
}

fn wrap_all(text: &str, chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for source in text.lines() {
        lines.extend(wrap_line(source, chars));
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Soft-wrap one source line to `chars`, preferring the last space so words and
/// XML attributes stay whole; a long token with no space breaks hard. A
/// continuation keeps the indent of the line it came from, so a wrapped JSON
/// value still sits inside the object that holds it.
pub(crate) fn wrap_line(line: &str, chars: usize) -> Vec<String> {
    let chars = chars.max(1);
    if line.chars().count() <= chars {
        return vec![line.to_owned()];
    }
    let indent: String = line.chars().take_while(|ch| *ch == ' ').collect();
    // The indent is drawn again on every continuation, so it comes out of
    // their width rather than pushing them past the column.
    let width = chars.saturating_sub(indent.chars().count()).max(1);

    let mut out = Vec::new();
    let mut rest = line;
    let mut first = true;
    loop {
        let limit = if first { chars } else { width };
        if rest.chars().count() <= limit {
            out.push(if first {
                rest.to_owned()
            } else {
                format!("{indent}{rest}")
            });
            break;
        }
        let boundary = rest
            .char_indices()
            .nth(limit)
            .map(|(at, _)| at)
            .unwrap_or(rest.len());
        let cut = match rest[..boundary].rfind(' ') {
            Some(at) if at > 0 => at,
            _ => boundary,
        };
        out.push(if first {
            rest[..cut].to_owned()
        } else {
            format!("{indent}{}", &rest[..cut])
        });
        rest = rest[cut..].trim_start_matches(' ');
        first = false;
    }
    out
}

/// The canvas size an SVG document declares, so the caller can pick a raster
/// scale from how large the image actually came out.
pub(crate) fn extent(svg: &str) -> (f32, f32) {
    (attribute(svg, "width=\""), attribute(svg, "height=\""))
}

fn attribute(svg: &str, prefix: &str) -> f32 {
    svg.find(prefix)
        .and_then(|at| {
            let rest = &svg[at + prefix.len()..];
            rest.find('"').and_then(|end| rest[..end].parse().ok())
        })
        .unwrap_or(0.0)
}

fn text(x: f32, y: f32, size: f32, fill: &str, value: &str, style: &Style) -> String {
    format!(
        "<text x=\"{x}\" y=\"{y}\" font-family=\"{mono}\" font-size=\"{size}\" \
         fill=\"{fill}\" xml:space=\"preserve\">{value}</text>",
        mono = style.mono,
        value = escape(value),
    )
}

/// The five characters that would otherwise end the element or the attribute.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{extent, render, wrap_line, Style, MAX_WIDTH};
    use crate::snapshot::{Block, Head, Snapshot};

    fn style() -> Style {
        Style {
            background: "#111111".to_owned(),
            foreground: "#eeeeee".to_owned(),
            muted: "#999999".to_owned(),
            border: "#333333".to_owned(),
            mono: "monospace".to_owned(),
            width: 760.0,
            max_width: MAX_WIDTH,
            max_height: None,
            padding: 20.0,
            font_size: 12.0,
            line_height: 18.0,
            title_size: 15.0,
        }
    }

    fn compact_style(max_height: f32) -> Style {
        Style {
            max_height: Some(max_height),
            ..style()
        }
    }

    fn snapshot(text: &str) -> Snapshot {
        Snapshot {
            head: Head {
                request: "GET https://api.test".to_owned(),
                outcome: "200 · 1 B · 1 ms".to_owned(),
            },
            blocks: vec![Block {
                label: "Response",
                text: text.to_owned(),
            }],
        }
    }

    /// The drawn text, by font size, so a test can tell the head from the body.
    fn lines_at(svg: &str, size: f32) -> Vec<&str> {
        let needle = format!("font-size=\"{size}\"");
        svg.split("<text")
            .filter(|chunk| chunk.contains(&needle))
            .filter_map(|chunk| chunk.split("xml:space=\"preserve\">").nth(1))
            .filter_map(|rest| rest.split("</text>").next())
            .collect()
    }

    /// A short body keeps the narrowest canvas, so a small snapshot stays small.
    #[test]
    fn a_short_block_keeps_the_floor_width() {
        let (width, _) = extent(&render(&snapshot("one\ntwo"), &style()));
        assert_eq!(width, 760.0);
    }

    /// A long line widens the canvas rather than wrapping, and is still drawn
    /// once: a wide image beats a wrapped pretty-printed line.
    #[test]
    fn a_long_line_widens_the_canvas_instead_of_wrapping() {
        let line = "a".repeat(200);
        let svg = render(&snapshot(&line), &style());
        let (width, _) = extent(&svg);
        assert!(width > 760.0, "{width}");
        let body = lines_at(&svg, 12.0);
        assert_eq!(
            body.iter().filter(|text| **text == line).count(),
            1,
            "{body:?}"
        );
    }

    /// A line longer than any useful image still fits: the canvas stops at the
    /// cap and only then does the line wrap.
    #[test]
    fn the_canvas_never_widens_past_the_cap() {
        let svg = render(&snapshot(&"x".repeat(4000)), &style());
        let (width, _) = extent(&svg);
        assert_eq!(width, MAX_WIDTH);
        assert!(lines_at(&svg, 12.0).len() > 1, "the line wrapped");
    }

    /// A continuation keeps the indent of the line it came from, so a wrapped
    /// JSON value still sits inside the object that holds it.
    #[test]
    fn a_wrapped_line_keeps_its_indent() {
        let wrapped = wrap_line("    \"url\": \"https://example.com/a/b/c\"", 20);
        assert!(wrapped.len() > 1);
        for line in &wrapped {
            assert!(line.starts_with("    "), "{line}");
            assert!(line.chars().count() <= 20, "{line}");
        }
    }

    /// A head wider than the cap wraps at the cap instead of running off it.
    #[test]
    fn a_long_head_wraps_at_the_cap() {
        let mut long = snapshot("one");
        long.head.request = format!("GET https://example.com/{}", "a".repeat(400));
        let svg = render(&long, &style());

        let (width, _) = extent(&svg);
        assert_eq!(width, MAX_WIDTH);
        let head = lines_at(&svg, 15.0);
        assert!(head.len() > 1, "{head:?}");
        for line in &head {
            assert!(line.chars().count() <= 253, "{line}");
        }
    }

    #[test]
    fn a_line_wider_than_the_column_wraps_at_a_space() {
        let wrapped = wrap_line("alpha beta gamma delta", 11);
        assert_eq!(wrapped, vec!["alpha beta", "gamma delta"]);
    }

    #[test]
    fn a_token_with_no_space_breaks_hard() {
        assert_eq!(wrap_line("abcdefghijkl", 5), vec!["abcde", "fghij", "kl"]);
    }

    #[test]
    fn the_document_escapes_markup_in_the_body() {
        let svg = render(&snapshot("{\"a\": \"<b> & c\"}"), &style());
        assert!(svg.contains("&lt;b&gt; &amp; c"), "{svg}");
        assert!(!svg.contains("<b>"), "{svg}");
    }

    #[test]
    fn the_document_carries_its_own_size() {
        let svg = render(&snapshot("one\ntwo"), &style());
        assert!(
            svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"760\""),
            "{svg}"
        );
        assert!(svg.ends_with("</svg>"), "{svg}");
        assert!(svg.contains("fill=\"#111111\""), "{svg}");
    }

    /// The size the reader parses is the size the document declares, so the
    /// caller can choose a raster scale from it.
    #[test]
    fn the_extent_matches_the_declared_size() {
        let svg = render(&snapshot("one\ntwo"), &style());
        let (width, height) = extent(&svg);
        assert_eq!(width, 760.0);
        assert!(height > 0.0, "{height}");
        assert!(svg.contains(&format!("height=\"{height}\"")), "{svg}");
    }

    fn long_body() -> String {
        (1..=25)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Compact mode stops the body at the height budget, keeps the image inside
    /// it, and says how many lines it did not draw.
    #[test]
    fn a_compact_block_clips_the_rest() {
        let svg = render(&snapshot(&long_body()), &compact_style(400.0));
        let (_, height) = extent(&svg);
        assert!(height <= 400.0, "{height}");
        assert!(svg.contains("more lines"), "{svg}");
        assert!(!svg.contains(">line 25</text>"), "line 25 was drawn");
    }

    /// A body that already fits the budget is left whole, with no footer.
    #[test]
    fn a_body_that_fits_is_not_clipped() {
        let svg = render(&snapshot("one\ntwo"), &compact_style(4000.0));
        assert!(!svg.contains("more lines"), "{svg}");
    }

    /// The footer counts exactly the lines that were left out.
    #[test]
    fn the_footer_counts_the_lines_it_omitted() {
        let svg = render(&snapshot(&long_body()), &compact_style(400.0));
        let drawn = svg.matches(">line ").count();
        assert_eq!(
            drawn + omitted(&svg),
            25,
            "{drawn} drawn + omitted should be 25",
        );
    }

    /// The count in "… N more lines".
    fn omitted(svg: &str) -> usize {
        let before = svg
            .split("more lines")
            .next()
            .unwrap_or_default()
            .trim_end();
        let digits: String = before
            .chars()
            .rev()
            .take_while(|ch| ch.is_ascii_digit())
            .collect();
        digits
            .chars()
            .rev()
            .collect::<String>()
            .parse()
            .unwrap_or(0)
    }

    #[test]
    fn every_line_of_a_long_body_is_drawn_once() {
        let text = (1..=25)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let svg = render(&snapshot(&text), &style());
        for n in 1..=25 {
            assert_eq!(
                svg.matches(&format!(">line {n}</text>")).count(),
                1,
                "line {n} is drawn exactly once",
            );
        }
    }
}
