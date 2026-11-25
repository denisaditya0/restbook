//! Wire-form text for a list of [`Kv`] rows.
//!
//! A `Form` body and the query-parameter list are both a `Vec<Kv>` and both one
//! `a=1&b=2` string on the wire; `parse` decodes it, `render` encodes it, and a
//! leading `#` disables a row (a real `#` is written `%23`). An incomplete
//! escape stays literal, and a parsed row is never `secret` — no wire form.

use crate::model::Kv;

/// Read wire-form text into rows, in order. Repeated keys stay separate rows.
pub fn parse(text: &str) -> Vec<Kv> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        let (line, enabled) = match line.strip_prefix('#') {
            Some(rest) => (rest.trim_start(), false),
            None => (line, true),
        };
        for segment in line.split('&') {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }
            let (key, value) = segment.split_once('=').unwrap_or((segment, ""));
            let key = decode(key);
            if key.is_empty() {
                continue;
            }
            rows.push(Kv {
                key,
                value: decode(value),
                enabled,
                secret: false,
            });
        }
    }
    rows
}

/// Write rows as canonical wire-form text. Blank rows are omitted: they have no
/// representation and would not survive a parse.
pub fn render(rows: &[Kv]) -> String {
    rows.iter()
        .filter(|row| !row.key.is_empty())
        .map(|row| {
            let pair = format!("{}={}", encode(&row.key), encode(&row.value));
            if row.enabled {
                pair
            } else {
                format!("# {pair}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push_str("%20"),
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                match (hex_value(bytes[index + 1]), hex_value(bytes[index + 2])) {
                    (Some(high), Some(low)) => {
                        out.push(high << 4 | low);
                        index += 3;
                    }
                    _ => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    // A `%XX` escape can name a byte that is not valid UTF-8 on its own.
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, value: &str) -> Kv {
        Kv::new(key, value)
    }

    fn disabled(key: &str, value: &str) -> Kv {
        Kv {
            enabled: false,
            ..Kv::new(key, value)
        }
    }

    #[test]
    fn repeated_keys_stay_separate_rows() {
        let rows = parse("a=1&a=2");
        assert_eq!(rows, [row("a", "1"), row("a", "2")]);
    }

    #[test]
    fn percent_escapes_and_plus_decode() {
        let rows = parse("q=hello%20world&x=a+b&y=%E2%9C%93");
        assert_eq!(
            rows,
            [row("q", "hello world"), row("x", "a b"), row("y", "✓")]
        );
    }

    #[test]
    fn render_escapes_reserved_characters() {
        let text = render(&[row("a&b", "c=d e%f")]);
        assert_eq!(text, "a%26b=c%3Dd%20e%25f");
        assert_eq!(parse(&text), [row("a&b", "c=d e%f")]);
    }

    #[test]
    fn a_key_without_an_equals_sign_has_an_empty_value() {
        assert_eq!(parse("flag"), [row("flag", "")]);
        assert_eq!(render(&[row("flag", "")]), "flag=");
    }

    #[test]
    fn blank_and_keyless_segments_are_dropped() {
        assert_eq!(parse("=x&& \n\na=1&"), [row("a", "1")]);
        assert_eq!(render(&[row("", "orphan"), row("a", "1")]), "a=1");
    }

    #[test]
    fn a_commented_line_is_a_disabled_row() {
        let rows = parse("# a=1\nb=2");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], disabled("a", "1"));
        assert_eq!(rows[1], row("b", "2"));
        assert_eq!(render(&rows), "# a=1\nb=2");
    }

    #[test]
    fn a_literal_hash_key_must_be_escaped() {
        assert_eq!(parse("%23a=1"), [row("#a", "1")]);
    }

    #[test]
    fn an_incomplete_escape_stays_literal() {
        assert_eq!(parse("a=100%&b=%zz"), [row("a", "100%"), row("b", "%zz")]);
    }

    #[test]
    fn unicode_survives_a_round_trip() {
        let rows = parse("name=Jos%C3%A9&note=%E2%9C%93+ok");
        assert_eq!(rows, [row("name", "José"), row("note", "✓ ok")]);
        assert_eq!(parse(&render(&rows)), rows);
    }

    #[test]
    fn render_is_the_canonical_form_a_parse_returns() {
        let rows = vec![row("a", "1"), disabled("off", "x y"), row("a", "2")];
        assert_eq!(parse(&render(&rows)), rows);
    }
}
