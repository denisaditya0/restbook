//! OpenAPI 3 import.
//! A document describes an API, not requests, so "import" generates one request
//! per operation: a collection per document, a folder per path. Each request is
//! the smallest the document accepts — `{{name}}` for required path parameters,
//! required query rows, a body with the first media type's required properties.
//! JSON and YAML are read; only 3.x, and 2.0 (Swagger) is refused by name.

use serde_json::{json, Map, Value};

use super::{BulkImport, Skip};
use crate::model::{
    Body, Collection, CollectionItem, Folder, Kv, Method, RequestDef, SavedRequest,
};

/// The verbs an OpenAPI path item can hold, in the order they are generated.
const OPERATIONS: [(&str, Method); 7] = [
    ("get", Method::Get),
    ("post", Method::Post),
    ("put", Method::Put),
    ("patch", Method::Patch),
    ("delete", Method::Delete),
    ("head", Method::Head),
    ("options", Method::Options),
];

/// Read an OpenAPI 3 document (JSON or YAML) into one collection.
///
/// `fallback_name` names the collection when `info.title` is missing.
pub fn parse(text: &str, fallback_name: &str) -> crate::Result<BulkImport> {
    let document: Value = serde_json::from_str(text).or_else(|_| yaml_to_json(text))?;

    if let Some(swagger) = document.get("swagger").and_then(Value::as_str) {
        return Err(crate::Error::Other(format!(
            "this is a Swagger {swagger} document; export it as OpenAPI 3 first"
        )));
    }

    let host = servers_of(&document).ok_or_else(|| {
        crate::Error::Other(
            "the document has no server URL, so its requests have no host".to_owned(),
        )
    })?;

    let mut result = BulkImport::default();
    let mut collection = Collection::new(
        new_id(),
        document
            .pointer("/info/title")
            .and_then(Value::as_str)
            .unwrap_or(fallback_name),
    );

    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        result.skipped.push(Skip {
            at: collection.name.clone(),
            reason: "no paths".to_owned(),
        });
        return Ok(result);
    };

    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        let mut folder = Folder {
            id: new_id(),
            name: path.clone(),
            items: Vec::new(),
        };

        for (key, method) in OPERATIONS {
            let Some(operation) = item.get(key) else {
                continue;
            };
            let name = operation
                .get("summary")
                .or_else(|| operation.get("operationId"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{method} {path}"));

            match request_of(&host, path, &document, item, operation, method) {
                Ok(request) => folder.items.push(CollectionItem::Request(SavedRequest {
                    id: new_id(),
                    name,
                    request,
                })),
                Err(reason) => result.skipped.push(Skip {
                    at: format!("{path} {key}"),
                    reason,
                }),
            }
        }

        if !folder.items.is_empty() {
            collection.items.push(CollectionItem::Folder(folder));
        }
    }

    if collection.items.is_empty() {
        result.skipped.push(Skip {
            at: collection.name,
            reason: "no operations".to_owned(),
        });
        return Ok(result);
    }

    result.collections.push(collection);
    Ok(result)
}

/// The first server's URL, normalized to have no trailing slash so a path can
/// be appended by concatenation.
fn servers_of(document: &Value) -> Option<String> {
    let server = document.get("servers")?.as_array()?.first()?;
    let url = server
        .get("url")?
        .as_str()?
        .trim_end_matches('/')
        .to_owned();
    // A relative server URL is meaningless on its own.
    url::Url::parse(&url).ok().map(|_| url)
}

fn request_of(
    host: &str,
    path: &str,
    document: &Value,
    item: &Map<String, Value>,
    operation: &Value,
    method: Method,
) -> Result<RequestDef, String> {
    let mut url = path.to_owned();

    let mut params: Vec<Kv> = Vec::new();
    for parameter in parameters(document, item, operation) {
        let Some(name) = parameter.get("name").and_then(Value::as_str) else {
            continue;
        };
        let location = parameter
            .get("in")
            .and_then(Value::as_str)
            .unwrap_or("query");
        match location {
            "path" => {
                // A path parameter with no value in the document becomes a
                // placeholder, which is exactly what `{{name}}` is for.
                url = url.replace(&format!("{{{name}}}"), &format!("{{{{{name}}}}}"));
            }
            "query" => {
                if parameter.get("required").and_then(Value::as_bool) == Some(true) {
                    let value = example_of(&parameter);
                    params.push(Kv::new(name, value));
                }
            }
            "header" => {}
            _ => {}
        }
    }

    // A `{name}` the document never declared cannot be filled; substituting a
    // value would be a guess, so the operation is reported instead. Run after
    // the replacements above, and ignore the `{{name}}` variables they made.
    let declared = url.replace("{{", "").replace("}}", "");
    if declared.contains('{') || declared.contains('}') {
        return Err(format!("undeclared path parameter in `{url}`"));
    }

    let body = body_of(operation);

    let mut request = RequestDef::new(method, format!("{host}{url}"));
    request.params = params;
    request.body = body;
    Ok(request)
}

/// Path-level parameters apply to every operation under them, so both levels
/// are read; an operation-level parameter wins by `(name, in)`.
///
/// A parameter is usually a `$ref` into `components.parameters`, so each entry
/// is resolved before it is read; an unresolved entry made 462 operations look
/// unfillable.
fn parameters(root: &Value, item: &Map<String, Value>, operation: &Value) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    for source in [item.get("parameters"), operation.get("parameters")] {
        let Some(list) = source.and_then(Value::as_array) else {
            continue;
        };
        for parameter in list {
            let Some(parameter) = resolve(root, parameter) else {
                continue;
            };
            let key = (
                parameter.get("name").and_then(Value::as_str),
                parameter.get("in").and_then(Value::as_str),
            );
            if let Some(existing) = merged.iter_mut().find(|existing| {
                (
                    existing.get("name").and_then(Value::as_str),
                    existing.get("in").and_then(Value::as_str),
                ) == key
            }) {
                *existing = parameter.clone();
            } else {
                merged.push(parameter.clone());
            }
        }
    }
    merged
}

/// Follow a local `$ref` (`#/components/...`) to the value it names.
///
/// An external or unresolvable reference yields `None` rather than a guess: a
/// parameter read from the wrong place would put the wrong value in a URL.
fn resolve<'a>(root: &'a Value, value: &'a Value) -> Option<&'a Value> {
    let Some(reference) = value.get("$ref").and_then(Value::as_str) else {
        return Some(value);
    };
    root.pointer(reference.strip_prefix('#')?)
}

/// A value a required parameter can start with: its example, its default, or a
/// typed placeholder.
fn example_of(parameter: &Value) -> String {
    if let Some(example) = parameter.get("example") {
        return scalar(example);
    }
    if let Some(value) = parameter
        .get("schema")
        .and_then(|schema| schema.get("default"))
    {
        return scalar(value);
    }
    let schema = parameter.get("schema").unwrap_or(&Value::Null);
    if let Some(first) = schema.get("enum").and_then(|list| list.get(0)) {
        return scalar(first);
    }
    String::new()
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A body from the first media type that has a schema, preferring JSON.
fn body_of(operation: &Value) -> Body {
    let Some(content) = operation
        .pointer("/requestBody/content")
        .and_then(Value::as_object)
    else {
        return Body::None;
    };

    // JSON is what a rest client shows best, so prefer it when a document
    // offers several representations of the same body.
    let (mime, media) = match content.iter().find(|(mime, _)| mime.contains("json")) {
        Some(found) => found,
        None => match content.iter().next() {
            Some(found) => found,
            None => return Body::None,
        },
    };
    let schema = media.get("schema").cloned().unwrap_or(Value::Null);

    if mime == "application/x-www-form-urlencoded" {
        // The fields are named by the schema; their values are for the user.
        let fields = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|name| Kv::new(name, String::new()))
                    .collect()
            })
            .unwrap_or_default();
        return Body::Form(fields);
    }

    Body::Raw {
        mime: mime.clone(),
        text: serde_json::to_string_pretty(&skeleton(&schema)).unwrap_or_default(),
    }
}

/// The smallest JSON the schema says is valid: every required property, with a
/// value of its type. Deeper `$ref`s are left as `{}` rather than guessed at.
fn skeleton(schema: &Value) -> Value {
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let mut object = Map::new();
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for name in required.iter().filter_map(Value::as_str) {
                    let property = schema
                        .get("properties")
                        .and_then(|properties| properties.get(name))
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    object.insert(name.to_owned(), skeleton(&property));
                }
            }
            Value::Object(object)
        }
        Some("array") => json!([]),
        Some("integer") | Some("number") => json!(0),
        Some("boolean") => json!(false),
        _ => json!(""),
    }
}

/// YAML to the same `Value` a JSON document would produce.
fn yaml_to_json(text: &str) -> crate::Result<Value> {
    let value: yaml_serde::Value = yaml_serde::from_str(text)
        .map_err(|error| crate::Error::Other(format!("openapi: {error}")))?;
    let json = serde_json::to_value(&value)
        .map_err(|error| crate::Error::Other(format!("openapi: {error}")))?;
    Ok(json)
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"{
      "openapi": "3.1.0",
      "info": { "title": "Acme" },
      "servers": [{ "url": "https://api.acme.com/v1/" }],
      "paths": {
        "/projects/{id}/comments": {
          "parameters": [{ "name": "id", "in": "path", "required": true }],
          "get": {
            "summary": "List comments",
            "parameters": [
              { "name": "limit", "in": "query", "required": true, "schema": { "type": "integer", "default": 25 } },
              { "name": "cursor", "in": "query" }
            ]
          },
          "post": {
            "operationId": "createComment",
            "requestBody": {
              "content": {
                "application/json": {
                  "schema": {
                    "type": "object",
                    "required": ["text"],
                    "properties": { "text": { "type": "string" } }
                  }
                }
              }
            }
          }
        },
        "/health": { "get": { "summary": "Ping" } }
      }
    }"#;

    /// Folders arrive in document order, which a JSON object does not promise;
    /// test by name so the assertions say what they mean.
    fn folder<'a>(collection: &'a Collection, path: &str) -> &'a Folder {
        collection
            .items
            .iter()
            .find_map(|item| match item {
                CollectionItem::Folder(folder) if folder.name == path => Some(folder),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no folder {path}"))
    }

    fn request<'a>(folder: &'a Folder, name: &str) -> &'a RequestDef {
        folder
            .items
            .iter()
            .find_map(|item| match item {
                CollectionItem::Request(saved) if saved.name == name => Some(&saved.request),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no request {name}"))
    }

    #[test]
    fn each_path_becomes_a_folder_with_its_operations() {
        let import = parse(SPEC, "fallback").unwrap();
        assert_eq!(import.collections.len(), 1);
        let collection = &import.collections[0];
        assert_eq!(collection.name, "Acme");
        assert_eq!(collection.items.len(), 2);

        let comments = folder(collection, "/projects/{id}/comments");
        assert_eq!(comments.items.len(), 2);
    }

    #[test]
    fn a_path_parameter_becomes_a_variable_and_required_query_rows_are_kept() {
        let import = parse(SPEC, "fallback").unwrap();
        let comments = folder(&import.collections[0], "/projects/{id}/comments");
        let list = request(comments, "List comments");
        assert_eq!(list.url, "https://api.acme.com/v1/projects/{{id}}/comments");
        // Only the required row is generated; `cursor` is left to the user.
        assert_eq!(list.params.len(), 1);
        assert_eq!(list.params[0].key, "limit");
        assert_eq!(list.params[0].value, "25");
    }

    #[test]
    fn a_json_request_body_is_a_skeleton_of_the_required_properties() {
        let import = parse(SPEC, "fallback").unwrap();
        let comments = folder(&import.collections[0], "/projects/{id}/comments");
        let create = request(comments, "createComment");
        assert!(matches!(
            &create.body,
            Body::Raw { mime, text } if mime == "application/json" && text.contains("\"text\"")
        ));
    }

    #[test]
    fn a_trailing_slash_on_the_server_is_not_doubled() {
        let import = parse(SPEC, "fallback").unwrap();
        let health = folder(&import.collections[0], "/health");
        assert_eq!(
            request(health, "Ping").url,
            "https://api.acme.com/v1/health"
        );
    }

    #[test]
    fn a_yaml_document_reads_the_same_way() {
        let yaml = "openapi: 3.0.0\ninfo:\n  title: Tiny\nservers:\n  - url: https://t.test\npaths:\n  /ping:\n    get:\n      summary: Ping\n";
        let import = parse(yaml, "fallback").unwrap();
        assert_eq!(import.collections[0].name, "Tiny");
        assert_eq!(import.request_count(), 1);
    }

    #[test]
    fn a_swagger_document_is_refused_by_name() {
        let error = parse(r#"{"swagger": "2.0", "paths": {}}"#, "x").unwrap_err();
        assert!(error.to_string().contains("Swagger 2.0"));
    }

    #[test]
    fn a_document_without_servers_cannot_build_urls() {
        let error = parse(r#"{"openapi": "3.0.0", "paths": {}}"#, "x").unwrap_err();
        assert!(error.to_string().contains("no server URL"));
    }

    #[test]
    fn a_parameter_that_is_a_ref_is_read_from_components() {
        // Cloudflare's document declares every path parameter this way; without
        // dereferencing, each operation is reported as unfillable instead.
        let import = parse(
            r##"{"openapi":"3.0.0","servers":[{"url":"https://x.test"}],
                "paths":{"/zones/{zone_id}/dns_records":{
                    "get":{"parameters":[{"$ref":"#/components/parameters/zone_id"}]}}},
                "components":{"parameters":{"zone_id":{
                    "name":"zone_id","in":"path","required":true,
                    "schema":{"type":"string"}}}}}"##,
            "x",
        )
        .unwrap();
        assert!(import.skipped.is_empty(), "{:?}", import.skipped);
        let records = folder(&import.collections[0], "/zones/{zone_id}/dns_records");
        assert_eq!(
            request(records, "GET /zones/{zone_id}/dns_records").url,
            "https://x.test/zones/{{zone_id}}/dns_records"
        );
    }

    #[test]
    fn an_unresolvable_ref_is_skipped_rather_than_guessed() {
        let import = parse(
            r#"{"openapi":"3.0.0","servers":[{"url":"https://x.test"}],
                "paths":{"/a":{"get":{"parameters":[{"$ref":"other.json#/p"}]}}}}"#,
            "x",
        )
        .unwrap();
        // The unresolvable parameter is dropped, and the operation still lands
        // because nothing in its path was left unfilled.
        assert_eq!(import.request_count(), 1);
    }

    #[test]
    fn an_unsubstituted_path_parameter_is_skipped_not_sent_literally() {
        // A `{id}` in the path with no parameter definition is not fillable.
        let import = parse(
            r#"{"openapi":"3.0.0","servers":[{"url":"https://x.test"}],
                "paths":{"/a/{id}": {"get": {}}}}"#,
            "x",
        )
        .unwrap();
        assert!(import.collections.is_empty());
        assert!(import.skipped[0]
            .reason
            .contains("undeclared path parameter"));
    }

    /// A document shaped like Cloudflare's: every operation carries a `$ref`
    /// parameter, a required query row, a JSON body and a response schema. One
    /// path with two operations, so `paths * 2` is the request count.
    fn large_document(paths: usize) -> String {
        let mut by_path = Map::new();
        for index in 0..paths {
            by_path.insert(
                format!("/zones/{{zone_id}}/things/{index}"),
                json!({
                    "parameters": [{"$ref": "#/components/parameters/zone_id"}],
                    "get": {
                        "summary": format!("List things {index}"),
                        "parameters": [{
                            "name": "limit", "in": "query", "required": true,
                            "schema": {"type": "integer", "default": 25}
                        }],
                        "responses": {"200": {"description": "ok", "content": {
                            "application/json": {"schema": {
                                "type": "object", "required": ["id", "items"],
                                "properties": {
                                    "id": {"type": "string"},
                                    "items": {"type": "array", "items": {
                                        "type": "object", "required": ["name"],
                                        "properties": {"name": {"type": "string"}}
                                    }}
                                }
                            }}
                        }}}
                    },
                    "post": {
                        "operationId": format!("createThing{index}"),
                        "requestBody": {"content": {"application/json": {"schema": {
                            "type": "object", "required": ["name", "config"],
                            "properties": {
                                "name": {"type": "string"},
                                "config": {"type": "object", "required": ["mode"],
                                    "properties": {"mode": {"type": "string"}}}
                            }
                        }}}}
                    }
                }),
            );
        }

        json!({
            "openapi": "3.1.0",
            "info": {"title": "Big"},
            "servers": [{"url": "https://api.example.com"}],
            "components": {"parameters": {"zone_id": {
                "name": "zone_id", "in": "path", "required": true,
                "schema": {"type": "string"}
            }}},
            "paths": Value::Object(by_path),
        })
        .to_string()
    }

    /// What the app pays for a file: the format guess and the parse, which is
    /// exactly what `import_file` runs off the UI thread.
    fn cost(paths: usize) -> (usize, std::time::Duration) {
        let text = large_document(paths);
        let started = std::time::Instant::now();
        assert_eq!(
            crate::import::ImportFormat::detect(&text),
            crate::import::ImportFormat::OpenApi
        );
        let import = parse(&text, "big.json").unwrap();
        (import.request_count(), started.elapsed())
    }

    /// A Cloudflare-sized document (29 MB, 3,934 requests) is a few seconds of
    /// JSON in a debug build, so the import is a visible wait even off the UI
    /// thread. That cost must stay linear — one pass per document — so four times
    /// the paths may not cost more than eight times the time. The bound is
    /// generous because a debug build is not a benchmark.
    #[test]
    fn a_large_document_parses_in_linear_time() {
        let (some_requests, some) = cost(500);
        let (more_requests, more) = cost(2_000);
        assert_eq!((some_requests, more_requests), (1_000, 4_000));

        let ceiling = some.max(std::time::Duration::from_millis(50)) * 8;
        assert!(
            more < ceiling,
            "4,000 requests took {more:?} against {some:?} for 1,000: the parse is \
             growing faster than the document",
        );
        assert!(
            more < std::time::Duration::from_secs(10),
            "4,000 requests took {more:?}",
        );
    }
}
