//! HAR 1.2 import and export.
//!
//! A HAR is a log first and a set of requests second: the same URL repeats,
//! mixed with asset noise. Import filters as it reads (one entry per method,
//! URL, and body; no assets) and groups by origin. Response bodies are logs and
//! are not imported; [`export`] writes stored requests back in the same shape.

use std::collections::HashSet;

use serde_json::Value;

use super::{BulkImport, Skip};
use crate::model::{Collection, CollectionItem, Kv, Method, RequestDef, SavedRequest};

/// File suffixes that are page furniture, not API calls.
const ASSET_SUFFIXES: [&str; 18] = [
    ".js", ".mjs", ".css", ".map", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".ico", ".woff",
    ".woff2", ".ttf", ".eot", ".webp", ".avif", ".mp4", ".webm",
];

/// Content types a browser records for its own assets.
const ASSET_MIME_PREFIXES: [&str; 5] = ["image/", "font/", "text/css", "video/", "audio/"];

/// How many entries are kept when the caller does not say. Enough for a
/// deliberate session, few enough that a long recording does not bury the
/// collection in page noise.
pub const DEFAULT_LIMIT: usize = 200;

/// Import the largest `limit` entries, newest first.
///
/// A real HAR from a few minutes of browsing holds thousands of entries; a
/// limit is not optional. Newest first means the limit keeps the requests the
/// user was just looking at rather than the first ones in the file.
pub fn parse(text: &str, fallback_name: &str, limit: usize) -> crate::Result<BulkImport> {
    let document: Value = serde_json::from_str(text)?;
    let mut result = BulkImport::default();

    let entries = document
        .pointer("/log/entries")
        .and_then(Value::as_array)
        .ok_or_else(|| crate::Error::Other("this is not a HAR: no log.entries".to_owned()))?;

    let mut seen = HashSet::new();
    // Buckets in first-seen order, so a collection does not reshuffle itself
    // between two imports of the same file.
    let mut buckets: Vec<(String, Vec<SavedRequest>)> = Vec::new();
    let mut taken = 0;

    for (index, entry) in entries.iter().enumerate().rev() {
        if taken >= limit {
            break;
        }

        let Some(request) = entry.pointer("/request") else {
            continue;
        };
        let Some(url) = request.get("url").and_then(Value::as_str) else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .and_then(Method::parse)
            .unwrap_or(Method::Get);

        let mime = entry
            .pointer("/response/content/mimeType")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if is_asset(url, mime) {
            continue;
        }

        // The same URL with and without a body are different requests, so the
        // body is part of the identity. Deduping on method and URL alone let a
        // bodyless replay hide the POST worth saving.
        let body = body_of(request);
        let identity = format!("{method} {url} {body:?}");
        if !seen.insert(identity) {
            continue;
        }

        let Some(host) = host_of(url) else {
            result.skipped.push(Skip {
                at: format!("entry {index}"),
                reason: format!("unusable url `{url}`"),
            });
            continue;
        };

        let mut def = RequestDef::new(method, url);
        def.headers = request
            .get("headers")
            .and_then(Value::as_array)
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|header| {
                        let key = header.get("name").and_then(Value::as_str)?;
                        let value = header.get("value").and_then(Value::as_str)?;
                        // Browser-managed and per-connection headers: they are
                        // recomputed by the transport and would be stale or
                        // wrong if replayed.
                        (!is_transport_header(key)).then(|| Kv::new(key, value))
                    })
                    .collect()
            })
            .unwrap_or_default();
        def.body = body;

        let name = format!("{method} {}", path_of(url));
        let saved = SavedRequest {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            request: def,
        };
        taken += 1;

        match buckets.iter_mut().find(|(bucket, _)| bucket == &host) {
            Some((_, bucket)) => bucket.push(saved),
            None => buckets.push((host, vec![saved])),
        }
    }

    if buckets.is_empty() {
        result.skipped.push(Skip {
            at: fallback_name.to_owned(),
            reason: "no importable entries".to_owned(),
        });
        return Ok(result);
    }

    let collections = buckets
        .into_iter()
        .map(|(host, requests)| Collection {
            items: requests.into_iter().map(CollectionItem::Request).collect(),
            ..Collection::new(uuid::Uuid::new_v4().to_string(), host)
        })
        .collect();

    result.collections = collections;
    Ok(result)
}

fn is_asset(url: &str, mime: &str) -> bool {
    if ASSET_MIME_PREFIXES
        .iter()
        .any(|prefix| mime.to_ascii_lowercase().starts_with(prefix))
    {
        return true;
    }
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    ASSET_SUFFIXES.iter().any(|suffix| path.ends_with(suffix))
}

/// Write collections as a HAR 1.2 log.
///
/// A HAR is what a browser and most proxy tools speak, so this is the export
/// for "open it in devtools". The entries carry only what can be replayed —
/// method, URL, headers, body — and no timings or sizes, because inventing
/// them would make the file a lie about a session that never happened.
pub fn export(collections: &[Collection]) -> crate::Result<String> {
    let mut entries = Vec::new();
    for collection in collections {
        push_entries(&collection.items, &mut entries);
    }

    let document = serde_json::json!({
        "log": {
            "version": "1.2",
            "creator": { "name": "restbook", "version": env!("CARGO_PKG_VERSION") },
            "entries": entries,
        }
    });
    Ok(serde_json::to_string_pretty(&document)?)
}

fn push_entries(items: &[CollectionItem], entries: &mut Vec<serde_json::Value>) {
    for item in items {
        match item {
            CollectionItem::Folder(folder) => push_entries(&folder.items, entries),
            CollectionItem::Request(saved) => {
                entries.push(serde_json::json!({
                    "request": entry_request(&saved.request),
                    "response": { "status": 0, "content": { "size": 0 } },
                }));
            }
        }
    }
}

fn entry_request(request: &RequestDef) -> serde_json::Value {
    let mut url = request.url.clone();
    // Query rows are separate in the editor, but a HAR URL is one string.
    let live: Vec<&Kv> = request.params.iter().filter(|row| row.enabled).collect();
    if !live.is_empty() {
        let query = live
            .iter()
            .map(|row| format!("{}={}", row.key, row.value))
            .collect::<Vec<_>>()
            .join("&");
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&query);
    }

    let mut object = serde_json::json!({ "method": request.method.as_str() });
    object["url"] = serde_json::json!(url);
    object["headers"] = serde_json::Value::Array(
        request
            .headers
            .iter()
            .filter(|header| header.enabled)
            .map(|header| serde_json::json!({ "name": header.key, "value": header.value }))
            .collect(),
    );
    if let Some(post) = post_data(&request.body) {
        object["postData"] = post;
    }
    object
}

fn post_data(body: &crate::model::Body) -> Option<serde_json::Value> {
    use crate::model::Body;

    match body {
        Body::None => None,
        Body::Raw { mime, text } => Some(serde_json::json!({ "mimeType": mime, "text": text })),
        Body::Form(rows) => Some(serde_json::json!({
            "mimeType": "application/x-www-form-urlencoded",
            "params": rows
                .iter()
                .filter(|row| row.enabled)
                .map(|row| serde_json::json!({ "name": row.key, "value": row.value }))
                .collect::<Vec<_>>(),
        })),
        Body::Multipart(parts) => Some(serde_json::json!({
            "mimeType": "multipart/form-data",
            "params": parts
                .iter()
                .filter(|part| part.enabled)
                .map(|part| serde_json::json!({
                    "name": part.name,
                    "value": part.file.clone().unwrap_or_else(|| part.value.clone()),
                }))
                .collect::<Vec<_>>(),
        })),
        // A HAR has no way to describe a file body; the path is the best
        // honest record of it.
        Body::Binary { path } => {
            Some(serde_json::json!({ "mimeType": "application/octet-stream", "text": path }))
        }
    }
}

fn is_transport_header(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "host"
            | "connection"
            | "content-length"
            | "accept-encoding"
            | "cookie"
            | "user-agent"
            | "sec-fetch-dest"
            | "sec-fetch-mode"
            | "sec-fetch-site"
    )
}

fn host_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

fn path_of(url: &str) -> String {
    url::Url::parse(url)
        .map(|parsed| {
            let mut path = parsed.path().to_owned();
            if let Some(query) = parsed.query() {
                path.push('?');
                path.push_str(query);
            }
            path
        })
        .unwrap_or_else(|_| url.to_owned())
}

fn body_of(request: &Value) -> crate::model::Body {
    use crate::model::Body;

    let Some(post) = request.get("postData") else {
        return Body::None;
    };
    let mime = post
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    if let Some(rows) = post.get("params").and_then(Value::as_array) {
        if !rows.is_empty() {
            return Body::Form(
                rows.iter()
                    .filter_map(|row| {
                        let name = row.get("name").and_then(Value::as_str)?;
                        let value = row.get("value").and_then(Value::as_str).unwrap_or_default();
                        Some(Kv::new(name, value))
                    })
                    .collect(),
            );
        }
    }

    match post.get("text").and_then(Value::as_str) {
        Some(text) if !text.is_empty() => Body::Raw {
            mime: if mime.is_empty() {
                "application/json".to_owned()
            } else {
                mime
            },
            text: text.to_owned(),
        },
        _ => Body::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = r#"{
      "log": { "entries": [
        { "request": { "method": "GET", "url": "https://app.test/assets/app.js",
                       "headers": [] },
          "response": { "content": { "mimeType": "application/javascript" } } },
        { "request": { "method": "GET", "url": "https://app.test/api/old",
                       "headers": [{ "name": "Accept", "value": "application/json" }] },
          "response": { "content": { "mimeType": "application/json" } } },
        { "request": { "method": "POST", "url": "https://app.test/api/things?page=2",
                       "headers": [{ "name": "Host", "value": "app.test" },
                                   { "name": "Content-Length", "value": "9" },
                                   { "name": "Content-Type", "value": "application/json" }],
                       "postData": { "mimeType": "application/json", "text": "{\"a\":1}" } },
          "response": { "content": { "mimeType": "application/json" } } },
        { "request": { "method": "POST", "url": "https://app.test/api/things?page=2",
                       "headers": [{ "name": "Host", "value": "app.test" },
                                   { "name": "Content-Length", "value": "9" },
                                   { "name": "Content-Type", "value": "application/json" }],
                       "postData": { "mimeType": "application/json", "text": "{\"a\":1}" } },
          "response": { "content": { "mimeType": "application/json" } } },
        { "request": { "method": "GET", "url": "https://cdn.test/logo.png",
                       "headers": [] },
          "response": { "content": { "mimeType": "image/png" } } }
      ] }
    }"#;

    #[test]
    fn assets_are_left_out_and_the_rest_grouped_by_host() {
        let import = parse(LOG, "log", 100).unwrap();
        assert_eq!(import.request_count(), 2);
        assert!(import
            .collections
            .iter()
            .all(|collection| collection.name == "app.test"));
    }

    #[test]
    fn the_same_request_seen_twice_is_imported_once() {
        let import = parse(LOG, "log", 100).unwrap();
        let names: Vec<&str> = import.collections[0]
            .items
            .iter()
            .map(|item| match item {
                CollectionItem::Request(saved) => saved.name.as_str(),
                CollectionItem::Folder(_) => "folder",
            })
            .collect();
        assert_eq!(names, vec!["POST /api/things?page=2", "GET /api/old"]);
    }

    #[test]
    fn a_body_becomes_a_raw_body_and_transport_headers_are_dropped() {
        let import = parse(LOG, "log", 100).unwrap();
        let saved = import.collections[0]
            .items
            .iter()
            .filter_map(|item| match item {
                CollectionItem::Request(saved) => Some(saved),
                CollectionItem::Folder(_) => None,
            })
            .find(|saved| saved.request.method == Method::Post)
            .expect("the POST is imported");
        assert!(matches!(
            saved.request.body,
            crate::model::Body::Raw { ref text, .. } if text == "{\"a\":1}"
        ));
        assert!(saved
            .request
            .headers
            .iter()
            .all(|header| header.key != "Host" && header.key != "Content-Length"));
        assert!(saved
            .request
            .headers
            .iter()
            .any(|header| header.key == "Content-Type"));
    }

    #[test]
    fn the_limit_keeps_the_newest_entries() {
        let import = parse(LOG, "log", 1).unwrap();
        assert_eq!(import.request_count(), 1);
        let CollectionItem::Request(saved) = &import.collections[0].items[0] else {
            panic!("expected a request");
        };
        // Newest first: the POST is the later entry in the log.
        assert_eq!(saved.request.method, Method::Post);
    }

    #[test]
    fn the_same_url_with_a_different_body_is_a_different_request() {
        // Browsers replay a URL with different payloads; collapsing those would
        // silently drop a request the user can see in the panel.
        let log = r#"{ "log": { "entries": [
            { "request": { "method": "POST", "url": "https://app.test/api/x",
                           "postData": { "mimeType": "application/json", "text": "{\"a\":1}" } },
              "response": { "content": { "mimeType": "application/json" } } },
            { "request": { "method": "POST", "url": "https://app.test/api/x",
                           "postData": { "mimeType": "application/json", "text": "{\"a\":2}" } },
              "response": { "content": { "mimeType": "application/json" } } }
        ] } }"#;
        let import = parse(log, "log", 100).unwrap();
        assert_eq!(import.request_count(), 2);
    }

    #[test]
    fn a_document_without_entries_is_an_error_not_an_empty_import() {
        assert!(parse(r#"{"log": {}}"#, "log", 10).is_err());
    }

    #[test]
    fn an_entry_with_no_importable_request_is_reported() {
        let import = parse(r#"{"log": {"entries": [{"request": {}}]}}"#, "log", 10).unwrap();
        assert!(import.collections.is_empty());
        assert!(!import.skipped.is_empty());
    }

    #[test]
    fn an_export_imports_back_as_the_same_requests() {
        let original = parse(LOG, "log", 100).unwrap();
        let text = export(&original.collections).unwrap();
        let reread = parse(&text, "log", 100).unwrap();

        assert_eq!(reread.request_count(), original.request_count());
        assert_eq!(reread.collections[0].name, "app.test");

        // Import reads a log newest-first and export writes in document order,
        // so a round trip reverses the list. The requests are what matter.
        let names = |import: &BulkImport| -> Vec<String> {
            let mut names: Vec<String> = import.collections[0]
                .items
                .iter()
                .filter_map(|item| match item {
                    CollectionItem::Request(saved) => Some(saved.name.clone()),
                    CollectionItem::Folder(_) => None,
                })
                .collect();
            names.sort();
            names
        };
        assert_eq!(names(&reread), names(&original));
    }

    #[test]
    fn an_export_folds_query_rows_into_the_url() {
        let mut request = RequestDef::new(Method::Get, "https://x.test/things");
        request.params = vec![Kv::new("page", "2"), Kv::new("off", "1")];
        request.params[1].enabled = false;
        let collection = Collection {
            items: vec![CollectionItem::Request(SavedRequest {
                id: "r".into(),
                name: "r".into(),
                request,
            })],
            ..Collection::new("c", "c")
        };

        let text = export(&[collection]).unwrap();
        let document: Value = serde_json::from_str(&text).unwrap();
        let url = document.pointer("/log/entries/0/request/url").unwrap();
        // A HAR URL is one string, and a disabled row is not sent.
        assert_eq!(url, "https://x.test/things?page=2");
    }
}
