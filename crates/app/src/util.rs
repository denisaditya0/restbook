//! Formatting helpers shared by the panes and the request views.

/// Bodies larger than this are clipped before rendering. The full bytes stay in
/// the response record; only the viewer is bounded.
pub(crate) const MAX_BODY_BYTES: usize = 256 * 1024;

/// Decode and, when the payload is JSON, pretty-print it. Returns the text to
/// render plus whether the display was clipped.
pub(crate) fn body_text(bytes: &[u8], content_type: Option<&str>) -> (String, bool) {
    let truncated = bytes.len() > MAX_BODY_BYTES;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_BODY_BYTES)]).into_owned();

    let is_json = content_type.is_some_and(|value| value.contains("json"));
    let maybe_json = is_json || {
        let trimmed = text.trim_start();
        trimmed.starts_with('{') || trimmed.starts_with('[')
    };

    if maybe_json {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Ok(pretty) = serde_json::to_string_pretty(&value) {
                return (pretty, truncated);
            }
        }
    }

    (text, truncated)
}

/// A response time for humans, the way a rest client prints it: `812 ms`,
/// `1.24 s`, `1 m 03 s`.
pub(crate) fn duration(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.2} s", ms as f64 / 1_000.0)
    } else {
        let seconds = ms / 1_000;
        format!("{} m {:02} s", seconds / 60, seconds % 60)
    }
}

/// One `Set-Cookie` response header, split for the cookies tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cookie {
    pub name: String,
    pub value: String,
    /// Everything after the value: `Path=/; HttpOnly; SameSite=Lax`.
    pub attributes: String,
}

/// Parse every `Set-Cookie` header. A header without `=`, or with an empty
/// name, is dropped rather than shown as a broken row.
pub(crate) fn cookies(headers: &[(String, String)]) -> Vec<Cookie> {
    headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
        .filter_map(|(_, header)| {
            let mut parts = header.split(';');
            let (name, value) = parts.next()?.split_once('=')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            Some(Cookie {
                name: name.to_owned(),
                value: value.trim().to_owned(),
                attributes: parts
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("; "),
            })
        })
        .collect()
}

pub(crate) fn human_bytes(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// How a response status reads in a header line: `200 OK`, `404 Not Found`.
///
/// Only the codes a rest client meets daily get a phrase. Anything else is
/// shown as its number, which is honest — a wrong phrase is worse than none.
pub(crate) fn status_text(status: u16) -> String {
    let reason = match status {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        418 => "I'm a teapot",
        422 => "Unprocessable Content",
        425 => "Too Early",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => return status.to_string(),
    };
    format!("{status} {reason}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_prints_json_bodies() {
        let (text, truncated) = body_text(br#"{"a":1}"#, Some("application/json"));
        assert_eq!(text, "{\n  \"a\": 1\n}");
        assert!(!truncated);
    }

    #[test]
    fn sniffs_json_without_a_content_type() {
        let (text, _) = body_text(br#" [1, 2]"#, None);
        assert_eq!(text, "[\n  1,\n  2\n]");
    }

    #[test]
    fn leaves_non_json_bodies_alone() {
        let (text, _) = body_text(b"hello", Some("text/plain"));
        assert_eq!(text, "hello");
    }

    #[test]
    fn clips_large_bodies_for_display() {
        let bytes = vec![b'a'; MAX_BODY_BYTES + 10];
        let (text, truncated) = body_text(&bytes, Some("text/plain"));
        assert!(truncated);
        assert_eq!(text.len(), MAX_BODY_BYTES);
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(812), "812 ms");
        assert_eq!(duration(1_240), "1.24 s");
        assert_eq!(duration(63_000), "1 m 03 s");
    }

    #[test]
    fn names_the_statuses_it_knows() {
        assert_eq!(status_text(200), "200 OK");
        assert_eq!(status_text(404), "404 Not Found");
        assert_eq!(status_text(503), "503 Service Unavailable");
    }

    #[test]
    fn leaves_an_unknown_status_as_its_number() {
        assert_eq!(status_text(299), "299");
        assert_eq!(status_text(0), "0");
    }

    #[test]
    fn parses_set_cookie_headers() {
        let headers = vec![
            (
                "Set-Cookie".to_owned(),
                "session=abc; Path=/; HttpOnly".to_owned(),
            ),
            ("Content-Type".to_owned(), "application/json".to_owned()),
            ("set-cookie".to_owned(), "theme=dark".to_owned()),
            ("set-cookie".to_owned(), "broken".to_owned()),
        ];
        let parsed = cookies(&headers);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "session");
        assert_eq!(parsed[0].value, "abc");
        assert_eq!(parsed[0].attributes, "Path=/; HttpOnly");
        assert_eq!(parsed[1].name, "theme");
        assert_eq!(parsed[1].attributes, "");
    }

    #[test]
    fn formats_byte_counts() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}
