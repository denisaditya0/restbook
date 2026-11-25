//! The deterministic bodies `sample-server` serves.
//!
//! Every body here is a pure function of its arguments: two runs produce the
//! same bytes, so "Copy body" is diffable and a capture stays valid. That is
//! why the ids are a scramble (`mix`) rather than a counter and why the gzip
//! and base64 encodings are fixed.
//!
//! The shapes are load-bearing for the response pane's tests, so keep them as
//! they are and add a route rather than editing one.

use std::io::Write;

use base64::Engine as _;

pub const JSON: &str = "application/json; charset=utf-8";
pub const TEXT: &str = "text/plain; charset=utf-8";
pub const HTML: &str = "text/html; charset=utf-8";
pub const XML: &str = "application/xml; charset=utf-8";
pub const CSV: &str = "text/csv; charset=utf-8";
pub const BINARY: &str = "application/octet-stream";

/// 256 KiB, matching `util::MAX_BODY_BYTES` on the app side.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// SplitMix64: a stable scramble, so no id reads as a counter.
fn mix(value: u64) -> u64 {
    let value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

/// A stable uuid-shaped string, so repeated runs are comparable.
pub fn uuid(seed: u64) -> String {
    let high = mix(seed);
    let low = mix(high ^ seed);
    let hexed = format!("{high:016x}{low:016x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hexed[0..8],
        &hexed[8..12],
        &hexed[12..16],
        &hexed[16..20],
        &hexed[20..]
    )
}

/// A JSON string literal. Non-ASCII stays raw, matching the fixtures that
/// serve `ensure_ascii=False`; every value here is plain text anyway.
pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn test_case(index: u64) -> &'static str {
    if index % 2 == 1 {
        "purchase bronet when balance is sufficient via modchan"
    } else {
        "purchase bronet when single active time yes and any offer \
         validity will increase the active period of the card"
    }
}

/// One record of the shape most APIs actually send, with long ids, in the
/// order the response pane's fixtures expect.
fn record_fields(index: u64) -> [(&'static str, String); 10] {
    [
        ("productUuid", json_string(&uuid(index))),
        ("poId", json_string(&uuid(index + 1000))),
        ("testCase", json_string(test_case(index))),
        ("uniqueIdDev", json_string(&uuid(index + 2000))),
        ("statusDev", "null".to_owned()),
        ("uniqueIdPrd", json_string(&uuid(index + 3000))),
        (
            "statusPrd",
            json_string(if index % 3 == 0 { "ERROR" } else { "OK" }),
        ),
        (
            "serviceId",
            json_string(&(2_000_004_190 + index).to_string()),
        ),
        ("scheduleId", json_string(&(2_814_256 + index).to_string())),
        (
            "mpeNumber",
            json_string(&format!("MPEAXR{}", 265_359 + index)),
        ),
    ]
}

/// A record on one line.
pub fn record_compact(index: u64) -> String {
    let mut out = String::from("{");
    for (position, (name, value)) in record_fields(index).iter().enumerate() {
        if position > 0 {
            out.push(',');
        }
        out.push_str(&json_string(name));
        out.push(':');
        out.push_str(value);
    }
    out.push('}');
    out
}

/// A record pretty-printed with two spaces per level, nested under `level`.
fn record_pretty(index: u64, level: usize) -> String {
    let inner = " ".repeat(2 * (level + 1));
    let fields = record_fields(index);
    let mut out = String::from("{\n");
    for (position, (name, value)) in fields.iter().enumerate() {
        out.push_str(&inner);
        out.push_str(&json_string(name));
        out.push_str(": ");
        out.push_str(value);
        if position + 1 < fields.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str(&" ".repeat(2 * level));
    out.push('}');
    out
}

/// The shape that exposed the clipping: minified, one very long line.
pub fn one_line() -> String {
    let mut out =
        String::from("{\"status\":\"OK\",\"code\":\"00\",\"message\":\"Success\",\"data\":[");
    for index in 0..25 {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&record_compact(index));
    }
    out.push_str("]}");
    out
}

/// Indented JSON with one enormous string value.
///
/// Pretty-printing shortens every normal line, but a token has no inside to
/// break: this is the row that the numbered view cannot show in full.
pub fn long_string() -> String {
    let raw: Vec<u8> = (0u8..=255).cycle().take(2048).collect();
    let token = base64::engine::general_purpose::URL_SAFE.encode(&raw);
    let mut out = String::from("{\n");
    out.push_str("  \"status\": \"OK\",\n");
    out.push_str(&format!("  \"access_token\": {},\n", json_string(&token)));
    out.push_str("  \"token_type\": \"Bearer\",\n");
    out.push_str("  \"expires_in\": 3600\n");
    out.push('}');
    out
}

/// Enough lines that numbering them is not worth it.
pub fn many_lines() -> String {
    let mut out = String::from("{\n  \"items\": [");
    for index in 0..1200 {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n    {\n");
        out.push_str(&format!("      \"id\": {index},\n"));
        out.push_str(&format!(
            "      \"name\": {}\n",
            json_string(&format!("item {index}"))
        ));
        out.push_str("    }");
    }
    out.push_str("\n  ]\n}");
    out
}

/// Past the display cap, so the truncation banner has to appear.
pub fn huge() -> String {
    let mut out = String::from("{\"items\":[");
    for index in 0..12000u64 {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"id\":{index},\"name\":{},\"uuid\":{}}}",
            json_string(&format!("item {index}")),
            json_string(&uuid(index))
        ));
    }
    out.push_str("]}");
    debug_assert!(out.len() > MAX_BODY_BYTES, "the huge sample must not fit");
    out
}

/// A minified body of roughly the requested size, still valid JSON.
///
/// One padded string is the cheapest way to any size: it keeps the payload
/// parseable, so the JSON view has something to pretty-print, and it makes the
/// long-line case appear at every scale rather than only at 10 KB.
pub fn body_of_kilobytes(kilobytes: i64) -> String {
    let target = kilobytes.max(1).saturating_mul(1024);
    // Room for the wrapper keys and the uuid, so the total lands near `target`.
    let padding = usize::try_from(target.saturating_sub(96)).unwrap_or(0);
    format!(
        "{{\"status\":\"OK\",\"id\":{},\"padding\":\"{}\"}}",
        json_string(&uuid(7)),
        "x".repeat(padding)
    )
}

pub fn error_body(status: i64) -> String {
    let mut out = String::from("{\n");
    out.push_str("  \"status\": \"ERROR\",\n");
    out.push_str(&format!(
        "  \"code\": {},\n",
        json_string(&status.to_string())
    ));
    out.push_str(&format!(
        "  \"message\": {}\n",
        json_string(&format!("sample server: refusing with {status}"))
    ));
    out.push('}');
    out
}

/// A top-level array: the other root a JSON body can have.
pub fn json_array() -> String {
    let mut out = String::from("[");
    for index in 0..6 {
        if index > 0 {
            out.push(',');
        }
        out.push('\n');
        // The element itself sits one level in, so its brace is indented too.
        out.push_str("  ");
        out.push_str(&record_pretty(index, 1));
    }
    out.push_str("\n]");
    out
}

/// The smallest valid JSON. `null` is the case most likely to be mistaken for
/// an empty body, which is why it is the one served.
pub fn json_scalar() -> &'static str {
    "null"
}

/// `application/json` that does not parse: the viewer must fall back to the
/// raw bytes instead of pretending the pretty printer succeeded.
pub fn invalid_json() -> &'static str {
    "{\"status\": \"OK\", \"data\": [1, 2, 3,"
}

/// Text the width and selection code cannot assume is ASCII: wide CJK glyphs, a
/// right-to-left script, a ZWJ emoji sequence, combining marks, and one long run
/// with no space to break at.
pub fn unicode_json() -> String {
    let mut out = String::from("{\n");
    out.push_str("  \"status\": \"OK\",\n");
    out.push_str(&format!(
        "  \"japanese\": {},\n",
        json_string("請求書を発行します")
    ));
    out.push_str(&format!(
        "  \"arabic\": {},\n",
        json_string("فاتورة الضريبة")
    ));
    out.push_str(&format!("  \"emoji\": {},\n", json_string("🧾👨‍👩‍👧‍👦")));
    out.push_str(&format!(
        "  \"combining\": {},\n",
        json_string("e\u{301}e\u{301}e\u{301}")
    ));
    out.push_str(&format!(
        "  \"long_cjk\": {}\n",
        json_string(&"請求書".repeat(60))
    ));
    out.push('}');
    out
}

/// Nesting deep enough that the indentation, not the content, is the widest
/// thing on the last line.
pub fn deep() -> String {
    let mut out = String::from("{\n");
    for level in 0..120 {
        out.push_str(&" ".repeat(2 * (level + 1)));
        out.push_str(&format!("\"level{}\": {{\n", 119 - level));
    }
    out.push_str(&" ".repeat(2 * 121));
    out.push_str("\"end\": true\n");
    for level in (1..=120).rev() {
        out.push_str(&" ".repeat(2 * level));
        out.push_str("}\n");
    }
    out.push('}');
    out
}

/// Indented XML with one long attribute row, so the XML label and the
/// unhighlighted long line are visible in the same body.
pub fn xml_doc() -> String {
    let attributes: Vec<String> = (0..12)
        .map(|index| format!("data-{index}=\"{}\"", uuid(index)))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<catalog>\n  <product id=\"{}\" sku=\"MPEAXR265359\" {}>\n    <name>bronet</name>\n    <price currency='IDR'>15000</price>\n  </product>\n</catalog>\n",
        uuid(0),
        attributes.join(" ")
    )
}

/// An HTML page with one row of tags long enough to need the same horizontal
/// treatment as a minified JSON line.
pub fn html_page() -> String {
    let mut rows = String::new();
    for index in 0..8 {
        rows.push_str(&format!(
            "<tr><td>{index}</td><td class=\"name\">{}</td></tr>\n",
            uuid(index)
        ));
    }
    let tags: String = (0..20)
        .map(|index| format!("<span class=\"tag\">{}</span>", uuid(index)))
        .collect();
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <title>restbook sample</title>\n</head>\n<body>\n  <table>\n{rows}  </table>\n  <div class=\"tags\">{tags}</div>\n</body>\n</html>\n"
    )
}

/// No structure to label or highlight, and one line past any window.
pub fn plain_text() -> String {
    let words: Vec<String> = (0..400).map(|index| format!("word{index}")).collect();
    format!(
        "restbook sample text\nThis body has no syntax, so the viewer must not colour it.\n\n{}\nend of sample\n",
        words.join(" ")
    )
}

/// Commas inside quoted fields and a doubled quote: a body a naive
/// one-line-per-row viewer misreads.
pub fn csv_text() -> String {
    let mut rows = vec![String::from("id,name,note")];
    for index in 0..12 {
        rows.push(format!(
            "{index},\"item {index}\",\"comma, and a \"\"quoted\"\" word\""
        ));
    }
    rows.join("\n") + "\n"
}

/// Every byte value, four times: not valid UTF-8 anywhere, so the lossy decode
/// has to produce replacement characters rather than an error.
pub fn binary() -> Vec<u8> {
    (0u8..=255).cycle().take(1024).collect()
}

/// A gzip body. `reqwest` decodes it and drops `Content-Encoding`, so the pane
/// should show plain JSON with only `Content-Type` left to label it. The header
/// carries no timestamp, so the bytes are the same on every run.
pub fn gzip(body: &str) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(body.as_bytes())
        .expect("gzip into a Vec cannot fail");
    encoder.finish().expect("gzip into a Vec cannot fail")
}

/// The menu `/` serves: every route and what it is for.
pub const INDEX: &str = r#"restbook sample server

  Bodies the viewer has to survive
  /one-line              minified JSON on one line: both views scroll sideways
  /long-string           indented JSON with a ~2.7 KB string: the numbered view scrolls that row
  /many-lines            indented JSON past 3000 lines: unnumbered fallback
  /huge                  minified JSON past 256 KiB: "truncated for display"
  /body?kb=2048          a body of about that size, any size
  /stream?chunks=10      chunked JSON, one record per write
  /abort?chunks=2        chunked JSON cut off mid-body: a short read
  /empty                 204 with no body
  /binary                bytes that are not UTF-8
  /gzip                  gzip-compressed JSON, decompressed by the client

  Formats
  /array                 JSON with a top-level array
  /scalar                JSON `null`
  /invalid-json          application/json that does not parse
  /sniff                 JSON with no Content-Type at all
  /xml                   XML with one long attribute row
  /html                  HTML with one long tag row
  /text                  plain text with one very long line
  /csv                   quoted CSV
  /unicode               CJK, Arabic, emoji, combining marks
  /deep                  JSON nested ~120 levels

  Transport, headers, cookies
  /error?status=503      that status with a JSON body
  /headers?count=40      that many X-Sample-<n> response headers
  /cookies               five Set-Cookie headers, one name repeated
  /redirect?hops=3       a 302 chain ending at /one-line

  Requests
  /echo                  any method, reflected: body, query, headers

  add ?delay=<ms> to any path to hold the response open before it starts
  (?delay=600000 with / is "never answering", for Cancel)."#;
