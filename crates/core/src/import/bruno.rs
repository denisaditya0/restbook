//! Bruno request import.
//!
//! Bruno writes one YAML file per request: `info` names it, `http` holds method,
//! URL, headers, query rows, auth, and body, and `settings` the timeout and
//! redirect choice. `auth: inherit` imports as the `Inherit` state.

use serde_json::Value;

use super::{BulkImport, Skip};
use crate::model::{Auth, Body, Collection, CollectionItem, Kv, Method, RequestDef, SavedRequest};

/// Read one Bruno request file into a one-request collection.
///
/// `fallback_name` names the request when `info.name` is missing, which is what
/// the file picker passes.
pub fn parse(text: &str, fallback_name: &str) -> crate::Result<BulkImport> {
    // `.bru` is the older text shape, not YAML. Name it so the report says what
    // to do instead of turning it into an empty or wrong request.
    if text.contains("meta {") {
        return Err(crate::Error::Other(
            "bruno: the .bru text format is not supported; export the request as YAML".to_owned(),
        ));
    }

    let document: Value = serde_json::from_str(text).or_else(|_| yaml_to_json(text))?;
    let Some(http) = document.get("http").and_then(Value::as_object) else {
        return Err(crate::Error::Other(
            "bruno: the file has no http request".to_owned(),
        ));
    };

    let name = document
        .pointer("/info/name")
        .and_then(Value::as_str)
        .unwrap_or(fallback_name)
        .to_owned();

    let mut result = BulkImport::default();
    let Some(method) = http
        .get("method")
        .and_then(Value::as_str)
        .and_then(Method::parse)
    else {
        result.skipped.push(Skip {
            at: name,
            reason: "no supported HTTP method".to_owned(),
        });
        return Ok(result);
    };
    let Some(raw_url) = http.get("url").and_then(Value::as_str) else {
        result.skipped.push(Skip {
            at: name,
            reason: "no URL".to_owned(),
        });
        return Ok(result);
    };

    let mut request = RequestDef::new(method, raw_url.to_owned());
    request.name = name.clone();

    // Bruno repeats the query in both `url` and `params`. The params are the
    // editable copy (they carry the disabled rows), so they win; anything in the
    // URL that they do not name is appended so no part of the address is lost.
    let mut params = query_rows(http.get("params"));
    if let Ok(mut parsed) = url::Url::parse(raw_url) {
        let pairs = parsed
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        parsed.set_query(None);
        parsed.set_fragment(None);
        request.url = parsed.to_string();
        for (key, value) in pairs {
            if !params.iter().any(|row| row.key == key) {
                params.push(Kv::new(key, value));
            }
        }
    }
    request.params = params;
    request.headers = kv_rows(http.get("headers"));

    if let Some(body) = http.get("body") {
        request.body = body_of(body);
    }

    if let Some(auth) = http.get("auth") {
        if auth
            .as_str()
            .is_some_and(|mode| mode.eq_ignore_ascii_case("inherit"))
        {
            request.auth = Auth::Inherit;
        }
    }

    if let Some(settings) = document.get("settings").and_then(Value::as_object) {
        request.timeout_ms = settings
            .get("timeout")
            .and_then(Value::as_u64)
            .filter(|timeout| *timeout > 0);
        request.follow_redirects = settings.get("followRedirects").and_then(Value::as_bool);
    }

    result.collections.push(Collection {
        items: vec![CollectionItem::Request(SavedRequest {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.clone(),
            request,
        })],
        ..Collection::new(uuid::Uuid::new_v4().to_string(), name)
    });
    Ok(result)
}

/// Query rows only. A `path` row is already part of the URL and would become a
/// second, wrong `?key=value` if it were copied into `params`.
fn query_rows(value: Option<&Value>) -> Vec<Kv> {
    let Some(rows) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let row = row.as_object()?;
            let kind = row.get("type").and_then(Value::as_str).unwrap_or("query");
            if !kind.eq_ignore_ascii_case("query") {
                return None;
            }
            let key = row.get("name").and_then(Value::as_str)?;
            let value = row.get("value").and_then(Value::as_str).unwrap_or("");
            let mut kv = Kv::new(key, value);
            kv.enabled = !row
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(kv)
        })
        .collect()
}

fn kv_rows(value: Option<&Value>) -> Vec<Kv> {
    let Some(rows) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let row = row.as_object()?;
            let key = row.get("name").and_then(Value::as_str)?;
            let value = row.get("value").and_then(Value::as_str).unwrap_or("");
            let mut kv = Kv::new(key, value);
            kv.enabled = !row
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(kv)
        })
        .collect()
}

fn body_of(value: &Value) -> Body {
    let Some(object) = value.as_object() else {
        return Body::None;
    };
    let kind = object
        .get("type")
        .or_else(|| object.get("mode"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match kind.as_str() {
        "json" => Body::Raw {
            mime: "application/json".to_owned(),
            text: text_of(object.get("json")),
        },
        "xml" => Body::Raw {
            mime: "application/xml".to_owned(),
            text: text_of(object.get("xml")),
        },
        "text" | "raw" => Body::Raw {
            mime: "text/plain".to_owned(),
            text: text_of(object.get("text").or_else(|| object.get("raw"))),
        },
        _ => Body::None,
    }
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(value) => serde_json::to_string_pretty(value).unwrap_or_default(),
        None => String::new(),
    }
}

fn yaml_to_json(text: &str) -> crate::Result<Value> {
    let value: yaml_serde::Value = yaml_serde::from_str(text)
        .map_err(|error| crate::Error::Other(format!("bruno: {error}")))?;
    serde_json::to_value(&value).map_err(|error| crate::Error::Other(format!("bruno: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Method;

    /// A redacted take on the shape Bruno writes for one request.
    const REQUEST: &str = r#"
info:
  name: Get Subscription New SIT
  type: http
  seq: 16

http:
  method: GET
  url: http://api.test/ott/subscription?msisdn=628&partner=DISNEY
  headers:
    - name: accept
      value: application/json
  params:
    - name: msisdn
      value: "628"
      type: query
    - name: partner
      value: DISNEY
      type: query
    - name: skuGroup
      value: PREMIUM
      type: query
      disabled: true
  auth: inherit

settings:
  encodeUrl: true
  timeout: 0
  followRedirects: true
  maxRedirects: 5
"#;

    #[test]
    fn a_single_request_file_becomes_one_request() {
        let import = parse(REQUEST, "file.yml").unwrap();
        let collection = import.collections.first().expect("one collection");
        assert_eq!(collection.name, "Get Subscription New SIT");
        let CollectionItem::Request(saved) = &collection.items[0] else {
            panic!("the collection holds the request");
        };
        assert_eq!(saved.request.method, Method::Get);
        assert_eq!(saved.request.url, "http://api.test/ott/subscription");
        assert_eq!(saved.request.follow_redirects, Some(true));
        assert_eq!(saved.request.timeout_ms, None, "zero means no timeout");
        assert_eq!(saved.request.headers[0].key, "accept");
    }

    /// The URL and `params` both list the query, so merging must not send every
    /// row twice; a disabled row stays present but off.
    #[test]
    fn the_query_is_merged_and_disabled_rows_stay_disabled() {
        let import = parse(REQUEST, "file.yml").unwrap();
        let CollectionItem::Request(saved) = &import.collections[0].items[0] else {
            panic!("the collection holds the request");
        };
        assert_eq!(saved.request.params.len(), 3);
        let msisdn = saved
            .request
            .params
            .iter()
            .find(|row| row.key == "msisdn")
            .expect("msisdn");
        assert_eq!(msisdn.value, "628");
        let sku = saved
            .request
            .params
            .iter()
            .find(|row| row.key == "skuGroup")
            .expect("skuGroup");
        assert!(!sku.enabled);
    }

    #[test]
    fn inherited_auth_imports_as_an_inherit_state() {
        let import = parse(REQUEST, "file.yml").unwrap();
        let CollectionItem::Request(saved) = &import.collections[0].items[0] else {
            panic!("the collection holds the request");
        };
        assert!(matches!(saved.request.auth, Auth::Inherit));
        assert!(import.notes.is_empty());
    }

    #[test]
    fn the_old_text_format_names_what_to_do() {
        let error = parse(
            "meta {\n  name: Ping\n}\nget {\n  url: https://x.test\n}\n",
            "ping.bru",
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("export the request as YAML"),
            "{error}"
        );
    }

    #[test]
    fn a_body_is_read_from_its_type() {
        let yaml = r#"
info: { name: Create }
http:
  method: POST
  url: https://api.test/items
  body:
    type: json
    json:
      name: widget
"#;
        let import = parse(yaml, "create.yml").unwrap();
        let CollectionItem::Request(saved) = &import.collections[0].items[0] else {
            panic!("the collection holds the request");
        };
        match &saved.request.body {
            Body::Raw { mime, text } => {
                assert_eq!(mime, "application/json");
                assert!(text.contains("\"widget\""), "{text}");
            }
            other => panic!("expected a raw body, got {other:?}"),
        }
    }
}
