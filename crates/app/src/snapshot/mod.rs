//! A shareable image of one send: the request as cURL, the response as it reads.
//!
//! This is a report, not a screenshot: a release build cannot read pixels back
//! from its own window, so the image is composed from [`RequestDef`] and
//! [`ResponseRecord`]. That also makes it deterministic, which is what tests
//! compare.

use restbook_core::{Failure, RequestDef, ResponseBody, ResponseRecord};

pub(crate) mod clipboard;
pub(crate) mod png;
pub(crate) mod svg;

/// What a hidden value becomes. The same string the environment inspector
/// draws, so "hidden" reads the same everywhere.
pub(crate) const MASK: &str = "••••••";

/// The line above the blocks: what was sent, and how it went.
pub(crate) struct Head {
    pub(crate) request: String,
    pub(crate) outcome: String,
}

/// One labelled monospace block. `label` is `&'static str` because the image
/// has three of them and none is user text.
pub(crate) struct Block {
    pub(crate) label: &'static str,
    pub(crate) text: String,
}

pub(crate) struct Snapshot {
    pub(crate) head: Head,
    pub(crate) blocks: Vec<Block>,
}

pub(crate) struct Options<'a> {
    /// Values replaced wherever they appear. The caller decides what counts as
    /// secret; this module only knows how to hide it.
    pub(crate) mask_values: &'a [String],
    /// The active environment, named in the head. Empty means none.
    pub(crate) environment: &'a str,
}

/// Compose the image's values. `response` and `failure` are exclusive in
/// practice; when neither is present the head says so and only the request
/// block is drawn.
pub(crate) fn build(
    request: &RequestDef,
    response: Option<&ResponseRecord>,
    failure: Option<&Failure>,
    options: &Options<'_>,
) -> Snapshot {
    let mut blocks = vec![Block {
        label: "Request",
        text: redact(
            &restbook_core::import::curl::to_curl(request),
            options.mask_values,
        ),
    }];

    let outcome = match (response, failure) {
        (Some(response), _) => {
            blocks.push(Block {
                label: "Response",
                text: redact(
                    &pretty(&response.body, content_type(response)),
                    options.mask_values,
                ),
            });
            outcome_line(response, options.environment)
        }
        (None, Some(failure)) => {
            blocks.push(Block {
                label: "Failure",
                text: redact(&failure.message, options.mask_values),
            });
            outcome_line_for_failure(options.environment)
        }
        (None, None) => options.environment.to_owned(),
    };

    Snapshot {
        head: Head {
            request: format!("{} {}", request.method.as_str(), request.url.trim()),
            outcome,
        },
        blocks,
    }
}

/// Everything after the status, separated by the same middot the panes use.
fn outcome_line(response: &ResponseRecord, environment: &str) -> String {
    let mut parts = vec![
        response.status.to_string(),
        human_bytes(response.body.len()),
        format!("{} ms", response.duration_ms),
        response
            .received_at
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string(),
    ];
    if !environment.is_empty() {
        parts.push(environment.to_owned());
    }
    parts.join(" · ")
}

fn outcome_line_for_failure(environment: &str) -> String {
    if environment.is_empty() {
        "Failed".to_owned()
    } else {
        format!("Failed · {environment}")
    }
}

/// Replace every occurrence of any masked value. Longest first, so one secret
/// that contains another is hidden whole rather than half-replaced.
pub(crate) fn redact(text: &str, values: &[String]) -> String {
    let mut sorted: Vec<&String> = values.iter().filter(|value| !value.is_empty()).collect();
    sorted.sort_by_key(|value| std::cmp::Reverse(value.len()));
    let mut out = text.to_owned();
    for value in sorted {
        if out.contains(value.as_str()) {
            out = out.replace(value.as_str(), MASK);
        }
    }
    out
}

/// The body as a reader should see it: JSON re-indented, text untouched, and a
/// body that is not text named by its size instead of rendered as mojibake.
fn pretty(body: &ResponseBody, content_type: Option<&str>) -> String {
    if body.is_empty() {
        return "(empty body)".to_owned();
    }
    let Ok(text) = std::str::from_utf8(body.as_bytes()) else {
        return format!("[binary, {}]", human_bytes(body.len()));
    };
    let mut out = if looks_like_json(content_type, text) {
        match serde_json::from_str::<serde_json::Value>(text) {
            // A signature-checked payload that merely resembles JSON must go out
            // byte for byte, so a failed parse keeps the original text.
            Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_owned()),
            Err(_) => text.to_owned(),
        }
    } else {
        text.to_owned()
    };
    // A streamed body keeps only its preview, and the image must say so rather
    // than read as if the payload ended there.
    if let ResponseBody::File(file) = body {
        if file.len as usize > file.preview.len() {
            out.push_str(&format!(
                "\n\n[preview: first {} of {}]",
                human_bytes(file.preview.len()),
                human_bytes(file.len as usize)
            ));
        }
    }
    out
}

fn content_type(response: &ResponseRecord) -> Option<&str> {
    response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.as_str())
}

fn looks_like_json(content_type: Option<&str>, text: &str) -> bool {
    if let Some(content_type) = content_type {
        let lowered = content_type.to_ascii_lowercase();
        return lowered.contains("json");
    }
    let head = text.trim_start();
    head.starts_with('{') || head.starts_with('[')
}

/// Bytes as a person reads them: one decimal, and no trailing `.0`.
pub(crate) fn human_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let size = bytes as f64;
    let (value, unit) = if size >= MB {
        (size / MB, "MB")
    } else if size >= KB {
        (size / KB, "KB")
    } else {
        return format!("{bytes} B");
    };
    let rounded = (value * 10.0).round() / 10.0;
    if (rounded - rounded.trunc()).abs() < f64::EPSILON {
        format!("{:.0} {unit}", rounded)
    } else {
        format!("{rounded:.1} {unit}")
    }
}

/// `Hsla` in the form the SVG needs. The pure function takes the channels so
/// this module does not depend on the theme or on GPUI's color types.
pub(crate) fn hex(h: f32, s: f32, l: f32) -> String {
    let (r, g, b) = hsl_to_rgb(h, s, l);
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let h = hue.rem_euclid(1.0);
    let s = saturation.clamp(0.0, 1.0);
    let l = lightness.clamp(0.0, 1.0);

    let channel = |offset: f32| {
        let k = (offset + h * 12.0) % 12.0;
        let a = s * l.min(1.0 - l);
        let value = l - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0);
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    (channel(0.0), channel(8.0), channel(4.0))
}

#[cfg(test)]
mod tests {
    use super::{build, hex, human_bytes, looks_like_json, redact, Options, MASK};
    use restbook_core::{Body, Method, RequestDef, ResponseBody, ResponseRecord};

    fn request() -> RequestDef {
        RequestDef::new(Method::Post, "https://api.test/v1/orders")
    }

    fn response(body: &str) -> ResponseRecord {
        ResponseRecord {
            status: 201,
            headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
            body: ResponseBody::Memory(body.as_bytes().to_vec()),
            duration_ms: 120,
            received_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            final_url: None,
            redirects: Vec::new(),
        }
    }

    #[test]
    fn a_secret_is_hidden_everywhere_it_appears() {
        let values = vec!["hunter2".to_owned()];
        assert_eq!(
            redact("Authorization: hunter2 and hunter2 again", &values),
            format!("Authorization: {MASK} and {MASK} again")
        );
    }

    #[test]
    fn an_empty_value_never_masks_the_whole_text() {
        // An empty string is contained by everything, so replacing it would
        // put a mask between every character.
        assert_eq!(redact("abc", &[String::new()]), "abc");
    }

    #[test]
    fn the_longer_of_two_overlapping_values_wins() {
        let values = vec!["abc".to_owned(), "abcdef".to_owned()];
        assert_eq!(redact("abcdef", &values), MASK);
    }

    #[test]
    fn json_is_pretty_printed_and_plain_text_is_left_alone() {
        let body = r#"{"a":1}"#;
        let built = build(
            &request(),
            Some(&response(body)),
            None,
            &Options {
                mask_values: &[],
                environment: "",
            },
        );
        assert!(
            built.blocks[1].text.contains("\n  \"a\": 1"),
            "{:?}",
            built.blocks[1].text
        );
    }

    #[test]
    fn a_body_that_is_not_json_keeps_its_bytes() {
        let record = response("<ok/>");
        let mut record = record;
        record.headers = vec![("Content-Type".to_owned(), "application/xml".to_owned())];
        let built = build(
            &request(),
            Some(&record),
            None,
            &Options {
                mask_values: &[],
                environment: "",
            },
        );
        assert_eq!(built.blocks[1].text, "<ok/>");
    }

    #[test]
    fn a_binary_body_is_named_by_its_size() {
        let mut record = response("");
        record.headers = vec![("Content-Type".to_owned(), "image/png".to_owned())];
        record.body = ResponseBody::Memory(vec![0xff, 0xfe, 0x00]);
        let built = build(
            &request(),
            Some(&record),
            None,
            &Options {
                mask_values: &[],
                environment: "",
            },
        );
        assert_eq!(built.blocks[1].text, "[binary, 3 B]");
    }

    #[test]
    fn the_head_names_the_method_the_url_and_the_outcome() {
        let built = build(
            &request(),
            Some(&response("{}")),
            None,
            &Options {
                mask_values: &[],
                environment: "SIT",
            },
        );
        assert_eq!(built.head.request, "POST https://api.test/v1/orders");
        assert!(
            built.head.outcome.starts_with("201 · 2 B · 120 ms"),
            "{}",
            built.head.outcome
        );
        assert!(
            built.head.outcome.ends_with("· SIT"),
            "{}",
            built.head.outcome
        );
    }

    #[test]
    fn a_failure_is_its_own_block() {
        let failure = restbook_core::Failure {
            kind: restbook_core::FailureKind::Timeout,
            message: "timed out after 30s".to_owned(),
        };
        let built = build(
            &request(),
            None,
            Some(&failure),
            &Options {
                mask_values: &[],
                environment: "",
            },
        );
        assert_eq!(built.blocks.len(), 2);
        assert_eq!(built.blocks[1].label, "Failure");
        assert_eq!(built.blocks[1].text, "timed out after 30s");
        assert_eq!(built.head.outcome, "Failed");
    }

    #[test]
    fn a_request_body_reaches_the_curl_command() {
        let mut request = request();
        request.body = Body::Raw {
            mime: "application/json".to_owned(),
            text: r#"{"sku":"A-1"}"#.to_owned(),
        };
        let built = build(
            &request,
            None,
            None,
            &Options {
                mask_values: &[],
                environment: "",
            },
        );
        assert!(
            built.blocks[0].text.contains("sku"),
            "{}",
            built.blocks[0].text
        );
    }

    #[test]
    fn sizes_read_the_way_the_panes_write_them() {
        assert_eq!(human_bytes(12), "12 B");
        assert_eq!(human_bytes(1024), "1 KB");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5 MB");
    }

    #[test]
    fn a_json_content_type_decides_over_the_body_shape() {
        assert!(looks_like_json(Some("application/json; charset=utf-8"), ""));
        assert!(!looks_like_json(Some("text/plain"), r#"{"a":1}"#));
        assert!(looks_like_json(None, r#"{"a":1}"#));
        assert!(looks_like_json(None, "  [1]"));
    }

    #[test]
    fn hex_matches_the_hsl_primaries() {
        assert_eq!(hex(0.0, 0.0, 0.0), "#000000");
        assert_eq!(hex(0.0, 0.0, 1.0), "#ffffff");
        assert_eq!(hex(0.0, 1.0, 0.5), "#ff0000");
        assert_eq!(hex(1.0 / 3.0, 1.0, 0.5), "#00ff00");
        assert_eq!(hex(2.0 / 3.0, 1.0, 0.5), "#0000ff");
    }
}
