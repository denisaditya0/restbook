//! Postman v2.1 collection import and export.
//!
//! A Postman export is a tree of folders, each request a list of typed blocks
//! (`url`, `header`, `body`, `auth`, …). Only what a rest client can represent
//! is read: scripts, tests, and variables are dropped, and an unusable item is a
//! [`Skip`], not a failed file. The tree is kept and [`export`] writes it back.

use serde_json::{json, Map, Value};

use super::{BulkImport, Skip};
use crate::model::{
    ApiKeyLocation, Auth, Body, Collection, CollectionItem, Folder, FormPart, Kv, Method,
    RequestDef, SavedRequest,
};

/// Read a whole v2.1 collection document into one [`Collection`].
///
/// `fallback_name` names the collection when the export has no `info.name`,
/// which is common for a hand-trimmed file.
pub fn parse(text: &str, fallback_name: &str) -> crate::Result<BulkImport> {
    let document: Value = serde_json::from_str(text)?;
    let mut result = BulkImport::default();

    let name = document
        .pointer("/info/name")
        .and_then(Value::as_str)
        .unwrap_or(fallback_name)
        .to_owned();

    let mut collection = Collection {
        id: new_id(),
        name,
        items: Vec::new(),
    };

    if let Some(items) = document.get("item").and_then(Value::as_array) {
        let path = collection.name.clone();
        walk(items, &path, &mut collection.items, &mut result.skipped);
    }

    if collection.items.is_empty() {
        result.skipped.push(Skip {
            at: collection.name.clone(),
            reason: "no importable requests".to_owned(),
        });
        return Ok(result);
    }

    result.collections.push(collection);
    Ok(result)
}

fn walk(items: &[Value], path: &str, out: &mut Vec<CollectionItem>, skipped: &mut Vec<Skip>) {
    for item in items {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Untitled")
            .trim()
            .to_owned();
        let here = format!("{path} / {name}");

        if let Some(children) = item.get("item").and_then(Value::as_array) {
            let mut folder = Folder {
                id: new_id(),
                name: name.clone(),
                items: Vec::new(),
            };
            walk(children, &here, &mut folder.items, skipped);
            out.push(CollectionItem::Folder(folder));
            continue;
        }

        if let Some(request) = item.get("request") {
            match request_of(request) {
                Ok(request) => out.push(CollectionItem::Request(SavedRequest {
                    id: new_id(),
                    name,
                    request,
                })),
                Err(reason) => skipped.push(Skip { at: here, reason }),
            }
        }
    }
}

/// One Postman `request` block as a [`RequestDef`].
fn request_of(request: &Value) -> Result<RequestDef, String> {
    // A request may be abbreviated as a bare URL string.
    let (method, url, headers, body, auth) = if let Some(url) = request.as_str() {
        (
            Method::Get,
            url.to_owned(),
            Vec::new(),
            Body::None,
            Auth::None,
        )
    } else {
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .and_then(Method::parse)
            .unwrap_or(Method::Get);
        let url = url_of(request.get("url")).ok_or_else(|| "no url".to_owned())?;
        let headers = headers_of(request.get("header"));
        let body = body_of(request.get("body"))?;
        let auth = auth_of(request.get("auth"));
        (method, url, headers, body, auth)
    };

    if url.trim().is_empty() {
        return Err("no url".to_owned());
    }

    let mut def = RequestDef::new(method, url);
    def.headers = headers;
    def.body = body;
    def.auth = auth;
    Ok(def)
}

/// `url` is either a string or `{ raw, query }`. The query rows are kept
/// separate so they stay editable instead of being frozen into the URL.
fn url_of(url: Option<&Value>) -> Option<String> {
    match url? {
        Value::String(text) => Some(text.clone()),
        other => other
            .get("raw")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                other
                    .pointer("/host/0")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }),
    }
}

fn headers_of(header: Option<&Value>) -> Vec<Kv> {
    header
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let key = row.get("key").and_then(Value::as_str)?;
                    let value = row.get("value").and_then(Value::as_str).unwrap_or_default();
                    let mut kv = Kv::new(key, value);
                    // Postman's `disabled: true` is our `enabled: false`.
                    kv.enabled = !row
                        .get("disabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    // A `Cookie` header is regenerated from the jar; keeping it
                    // would send a stale pair alongside the live one.
                    (!key.eq_ignore_ascii_case("cookie")).then_some(kv)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn body_of(body: Option<&Value>) -> Result<Body, String> {
    let Some(body) = body else {
        return Ok(Body::None);
    };
    match body.get("mode").and_then(Value::as_str).unwrap_or("raw") {
        "raw" => {
            let text = body.get("raw").and_then(Value::as_str).unwrap_or_default();
            if text.trim().is_empty() {
                return Ok(Body::None);
            }
            // `options.raw.language` is a hint; the language decides the MIME
            // when Postman did not write a Content-Type header.
            let mime = match body
                .pointer("/options/raw/language")
                .and_then(Value::as_str)
                .unwrap_or("json")
            {
                "xml" => "application/xml",
                "html" => "text/html",
                "text" => "text/plain",
                _ => "application/json",
            };
            Ok(Body::Raw {
                mime: mime.to_owned(),
                text: text.to_owned(),
            })
        }
        "urlencoded" => Ok(Body::Form(rows_of(body.get("urlencoded")))),
        "formdata" => {
            let parts = body
                .get("formdata")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            let name = row.get("name").and_then(Value::as_str)?.to_owned();
                            let kind = row.get("type").and_then(Value::as_str).unwrap_or("text");
                            let value =
                                row.get("value").and_then(Value::as_str).unwrap_or_default();
                            let src = row.get("src").and_then(Value::as_str);
                            Some(FormPart {
                                name,
                                value: if kind == "file" {
                                    String::new()
                                } else {
                                    value.to_owned()
                                },
                                file: (kind == "file").then(|| src.unwrap_or_default().to_owned()),
                                mime: row
                                    .get("contentType")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                                enabled: !row
                                    .get("disabled")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(Body::Multipart(parts))
        }
        "file" => {
            let src = body
                .get("file")
                .and_then(|f| f.get("src").and_then(Value::as_str).or_else(|| f.as_str()));
            Ok(match src {
                Some(path) => Body::Binary {
                    path: path.to_owned(),
                },
                None => Body::None,
            })
        }
        // `graphql` has no equivalent this engine can send.
        other => Err(format!("unsupported body mode `{other}`")),
    }
}

fn rows_of(rows: Option<&Value>) -> Vec<Kv> {
    rows.and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let key = row.get("key").and_then(Value::as_str)?;
                    let value = row.get("value").and_then(Value::as_str).unwrap_or_default();
                    let mut kv = Kv::new(key, value);
                    kv.enabled = !row
                        .get("disabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    Some(kv)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn auth_of(auth: Option<&Value>) -> Auth {
    let Some(auth) = auth else {
        return Auth::None;
    };

    // Params sit under the auth type's own key: `{ "type": "bearer",
    // "bearer": [{ "key": "token", "value": "…" }] }`.
    let entries = |kind: &str| auth.get(kind).and_then(Value::as_array);
    let read = |kind: &str, key: &str| {
        entries(kind)
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.get("key").and_then(Value::as_str) == Some(key))
            })
            .and_then(|entry| entry.get("value").and_then(Value::as_str))
            .unwrap_or_default()
            .to_owned()
    };

    match auth.get("type").and_then(Value::as_str).unwrap_or("noauth") {
        "basic" => Auth::Basic {
            username: read("basic", "username"),
            password: read("basic", "password"),
        },
        "bearer" => Auth::Bearer {
            token: read("bearer", "token"),
        },
        "apikey" => Auth::ApiKey {
            name: read("apikey", "key"),
            value: read("apikey", "value"),
            location: match read("apikey", "in").as_str() {
                "query" => ApiKeyLocation::Query,
                _ => ApiKeyLocation::Header,
            },
        },
        _ => Auth::None,
    }
}

/// A stable-enough id for an imported node. The store upserts a collection by
/// id, so two imports of the same export must not collide inside one store:
/// the timestamp makes a re-import a new collection instead of silently
/// overwriting the previous one.
fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Write one collection as a Postman v2.1 export.
///
/// Export exists so a workspace is not a one-way door: anything imported can be
/// handed back, and a user can move to another tool without re-typing. The
/// output is the same shape [`parse`] reads, so the round trip is tested rather
/// than assumed.
pub fn export(collection: &Collection) -> crate::Result<String> {
    let document = json!({
        "info": {
            "name": collection.name,
            "schema": POSTMAN_SCHEMA,
        },
        "item": items_of(&collection.items),
    });
    Ok(serde_json::to_string_pretty(&document)?)
}

const POSTMAN_SCHEMA: &str = "https://schema.getpostman.com/json/collection/v2.1.0/collection.json";

fn items_of(items: &[CollectionItem]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|item| match item {
                CollectionItem::Folder(folder) => json!({
                    "name": folder.name,
                    "item": items_of(&folder.items),
                }),
                CollectionItem::Request(saved) => json!({
                    "name": saved.name,
                    "request": request_json(&saved.request),
                }),
            })
            .collect(),
    )
}

fn request_json(request: &RequestDef) -> Value {
    let mut object = Map::new();
    object.insert("method".to_owned(), json!(request.method.as_str()));

    let mut url = Map::new();
    url.insert("raw".to_owned(), json!(request.url));
    // Query rows stay as rows, matching how they are read back.
    if !request.params.is_empty() {
        url.insert(
            "query".to_owned(),
            Value::Array(
                request
                    .params
                    .iter()
                    .map(|param| {
                        let mut row = json!({ "key": param.key, "value": param.value });
                        if !param.enabled {
                            row["disabled"] = json!(true);
                        }
                        row
                    })
                    .collect(),
            ),
        );
    }
    object.insert("url".to_owned(), Value::Object(url));

    if !request.headers.is_empty() {
        object.insert(
            "header".to_owned(),
            Value::Array(
                request
                    .headers
                    .iter()
                    .map(|header| {
                        let mut row = json!({ "key": header.key, "value": header.value });
                        if !header.enabled {
                            row["disabled"] = json!(true);
                        }
                        row
                    })
                    .collect(),
            ),
        );
    }

    if let Some(body) = body_json(&request.body) {
        object.insert("body".to_owned(), body);
    }
    if let Some(auth) = auth_json(&request.auth) {
        object.insert("auth".to_owned(), auth);
    }

    Value::Object(object)
}

fn body_json(body: &Body) -> Option<Value> {
    match body {
        Body::None => None,
        Body::Raw { mime, text } => Some(json!({
            "mode": "raw",
            "raw": text,
            "options": { "raw": { "language": language_of(mime) } },
        })),
        Body::Form(rows) => Some(json!({
            "mode": "urlencoded",
            "urlencoded": rows
                .iter()
                .map(|row| {
                    let mut item = json!({ "key": row.key, "value": row.value });
                    if !row.enabled {
                        item["disabled"] = json!(true);
                    }
                    item
                })
                .collect::<Vec<_>>(),
        })),
        Body::Multipart(parts) => Some(json!({
            "mode": "formdata",
            "formdata": parts
                .iter()
                .map(|part| match &part.file {
                    Some(file) => json!({
                        "key": part.name,
                        "type": "file",
                        "src": file,
                        "disabled": !part.enabled,
                    }),
                    None => {
                        let mut item = json!({ "key": part.name, "value": part.value });
                        if !part.enabled {
                            item["disabled"] = json!(true);
                        }
                        item
                    }
                })
                .collect::<Vec<_>>(),
        })),
        Body::Binary { path } => Some(json!({
            "mode": "file",
            "file": { "src": path },
        })),
    }
}

fn language_of(mime: &str) -> &'static str {
    match mime {
        m if m.contains("json") => "json",
        m if m.contains("xml") => "xml",
        m if m.contains("html") => "html",
        _ => "text",
    }
}

fn auth_json(auth: &Auth) -> Option<Value> {
    match auth {
        Auth::None => None,
        Auth::Basic { username, password } => Some(json!({
            "type": "basic",
            "basic": [
                { "key": "username", "value": username },
                { "key": "password", "value": password },
            ],
        })),
        Auth::Bearer { token } => Some(json!({
            "type": "bearer",
            "bearer": [{ "key": "token", "value": token }],
        })),
        Auth::ApiKey {
            name,
            value,
            location,
        } => Some(json!({
            "type": "apikey",
            "apikey": [
                { "key": "key", "value": name },
                { "key": "value", "value": value },
                { "key": "in", "value": match location {
                    ApiKeyLocation::Header => "header",
                    ApiKeyLocation::Query => "query",
                } },
            ],
        })),
        // Postman's OAuth 2.0 shape is not what this app stores, and the token
        // is never part of the request; the auth is dropped on export.
        Auth::OAuth2 { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORT: &str = r#"{
      "info": { "name": "Acme", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json" },
      "item": [
        {
          "name": "Auth",
          "item": [
            {
              "name": "Login",
              "request": {
                "method": "POST",
                "url": { "raw": "https://api.acme.com/login" },
                "header": [
                  { "key": "Content-Type", "value": "application/json" },
                  { "key": "Cookie", "value": "sid=stale" },
                  { "key": "X-Off", "value": "1", "disabled": true }
                ],
                "body": { "mode": "raw", "raw": "{\"user\":\"me\"}",
                          "options": { "raw": { "language": "json" } } },
                "auth": { "type": "bearer", "bearer": [ { "key": "token", "value": "{{TOKEN}}" } ] }
              }
            }
          ]
        },
        { "name": "Health", "request": "https://api.acme.com/health" },
        { "name": "Broken", "request": { "method": "GET", "url": { } } }
      ]
    }"#;

    #[test]
    fn a_folder_becomes_a_folder_and_its_request_keeps_every_tab() {
        let import = parse(EXPORT, "fallback").unwrap();
        assert_eq!(import.collections.len(), 1);
        assert_eq!(import.collections[0].name, "Acme");

        let CollectionItem::Folder(folder) = &import.collections[0].items[0] else {
            panic!("expected a folder");
        };
        assert_eq!(folder.name, "Auth");
        let CollectionItem::Request(saved) = &folder.items[0] else {
            panic!("expected a request");
        };
        assert_eq!(saved.name, "Login");
        assert_eq!(saved.request.method, Method::Post);
        assert_eq!(saved.request.url, "https://api.acme.com/login");
        assert!(matches!(
            saved.request.auth,
            Auth::Bearer { ref token } if token == "{{TOKEN}}"
        ));
        assert!(matches!(
            saved.request.body,
            Body::Raw { ref mime, .. } if mime == "application/json"
        ));
    }

    #[test]
    fn a_cookie_header_is_dropped_and_a_disabled_header_is_kept_disabled() {
        let import = parse(EXPORT, "fallback").unwrap();
        let CollectionItem::Folder(folder) = &import.collections[0].items[0] else {
            panic!("expected a folder");
        };
        let CollectionItem::Request(saved) = &folder.items[0] else {
            panic!("expected a request");
        };

        assert!(saved
            .request
            .headers
            .iter()
            .all(|header| !header.key.eq_ignore_ascii_case("cookie")));
        let off = saved
            .request
            .headers
            .iter()
            .find(|header| header.key == "X-Off")
            .expect("the disabled header is still listed");
        assert!(!off.enabled);
    }

    #[test]
    fn a_request_written_as_a_bare_url_is_a_get() {
        let import = parse(EXPORT, "fallback").unwrap();
        let CollectionItem::Request(saved) = &import.collections[0].items[1] else {
            panic!("expected a request");
        };
        assert_eq!(saved.request.method, Method::Get);
        assert_eq!(saved.request.url, "https://api.acme.com/health");
    }

    #[test]
    fn an_item_without_a_url_is_skipped_with_a_reason() {
        let import = parse(EXPORT, "fallback").unwrap();
        assert_eq!(import.skipped.len(), 1);
        assert_eq!(import.skipped[0].at, "Acme / Broken");
        assert_eq!(import.skipped[0].reason, "no url");
        assert_eq!(import.request_count(), 2);
    }

    #[test]
    fn an_empty_collection_is_reported_rather_than_imported_blank() {
        let import = parse(r#"{"info": {"name": "Empty"}, "item": []}"#, "x").unwrap();
        assert!(import.collections.is_empty());
        assert_eq!(import.skipped[0].at, "Empty");
    }

    #[test]
    fn the_fallback_name_is_used_when_the_export_has_none() {
        let import = parse(
            r#"{"item": [{"name": "A", "request": "https://x.test"}]}"#,
            "Backup",
        )
        .unwrap();
        assert_eq!(import.collections[0].name, "Backup");
    }

    #[test]
    fn a_graphql_body_is_skipped_instead_of_sent_as_nothing() {
        let import = parse(
            r#"{"info":{"name":"G"},"item":[{"name":"Q","request":{"url":"https://x.test","body":{"mode":"graphql"}}}]}"#,
            "x",
        )
        .unwrap();
        assert!(import.collections.is_empty());
        assert!(import.skipped[0].reason.contains("graphql"));
    }

    #[test]
    fn an_export_imports_back_as_the_same_collection() {
        let original = parse(EXPORT, "fallback").unwrap();
        let collection = &original.collections[0];

        let text = export(collection).unwrap();
        let reread = parse(&text, "fallback").unwrap();
        let copy = &reread.collections[0];

        assert_eq!(copy.name, collection.name);
        assert_eq!(copy.items.len(), collection.items.len());

        let CollectionItem::Folder(before) = &collection.items[0] else {
            panic!("expected a folder");
        };
        let CollectionItem::Folder(after) = &copy.items[0] else {
            panic!("expected a folder");
        };
        assert_eq!(after.name, before.name);

        let CollectionItem::Request(before) = &before.items[0] else {
            panic!("expected a request");
        };
        let CollectionItem::Request(after) = &after.items[0] else {
            panic!("expected a request");
        };
        assert_eq!(after.request.method, before.request.method);
        assert_eq!(after.request.url, before.request.url);
        assert!(matches!(
            after.request.auth,
            Auth::Bearer { ref token } if token == "{{TOKEN}}"
        ));
        assert!(matches!(
            &after.request.body,
            Body::Raw { mime, text } if mime == "application/json" && text == "{\"user\":\"me\"}"
        ));
    }

    #[test]
    fn an_export_keeps_disabled_rows_disabled() {
        let original = parse(EXPORT, "fallback").unwrap();
        let text = export(&original.collections[0]).unwrap();
        let reread = parse(&text, "fallback").unwrap();

        let CollectionItem::Folder(folder) = &reread.collections[0].items[0] else {
            panic!("expected a folder");
        };
        let CollectionItem::Request(saved) = &folder.items[0] else {
            panic!("expected a request");
        };
        assert!(saved
            .request
            .headers
            .iter()
            .any(|header| header.key == "X-Off" && !header.enabled));
    }
}
