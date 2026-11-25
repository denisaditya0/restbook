//! Batch import and export of workspace data.
//!
//! Every format becomes [`Collection`]s and [`Environment`]s. Parsing is
//! forgiving where a format is sloppy: a bad item is a [`Skip`], not a failed
//! file. Nothing reads cookies, secrets, or the network, and an import never
//! overwrites an existing collection or environment — it reports the collision.

use serde::{Deserialize, Serialize};

use crate::model::{Collection, Environment};

pub mod bruno;
pub mod curl;
pub mod har;
pub mod openapi;
pub mod postman;

/// Where a format can be ambiguous, the caller supplies the name: a file name
/// for a file, or a collection name when the text arrives without one.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options<'a> {
    /// Used for the collection name when the document carries none.
    pub fallback_name: &'a str,
    /// HAR only: the newest `limit` entries to keep.
    pub har_limit: Option<usize>,
}

/// Parse `text` as `format`. Prefer the format-specific functions when you know
/// the format; this is the entry point for a file picker that has guessed.
pub fn parse(format: ImportFormat, text: &str, options: Options<'_>) -> crate::Result<BulkImport> {
    match format {
        ImportFormat::Curl => curl::parse(text).map(one_curl).map_err(crate::Error::Other),
        ImportFormat::Postman => postman::parse(text, options.fallback_name),
        ImportFormat::Har => har::parse(
            text,
            options.fallback_name,
            options.har_limit.unwrap_or(har::DEFAULT_LIMIT),
        ),
        ImportFormat::OpenApi => openapi::parse(text, options.fallback_name),
        ImportFormat::Bruno => bruno::parse(text, options.fallback_name),
        ImportFormat::Bundle => {
            let bundle = Bundle::from_json(text)?;
            Ok(BulkImport {
                collections: bundle.collections,
                environments: bundle.environments,
                ..BulkImport::default()
            })
        }
    }
}

/// A pasted command has no collection to belong to, so give it one named for
/// the host it addresses.
fn one_curl(request: crate::RequestDef) -> BulkImport {
    let name = url_host(&request.url).unwrap_or_else(|| "curl".to_owned());
    BulkImport {
        collections: vec![Collection {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.clone(),
            items: vec![crate::CollectionItem::Request(crate::SavedRequest {
                id: uuid::Uuid::new_v4().to_string(),
                name: request.url.clone(),
                request,
            })],
        }],
        ..BulkImport::default()
    }
}

fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| host.to_owned())
}

/// A format that can be written as well as read.
///
/// OpenAPI is not one of them: generating a document from stored requests would
/// be inventing an API description, and a wrong schema is worse than none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// This app's own bundle: everything, including environments.
    Bundle,
    /// One collection, for another rest client.
    Postman,
    /// A HAR log, for a browser or proxy tool.
    Har,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Bundle => "json",
            ExportFormat::Postman => "postman_collection.json",
            ExportFormat::Har => "har",
        }
    }
}

/// How much of the data an exported file may carry.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExportOptions {
    /// Write API keys, tokens, and `secret` variable values in plaintext.
    ///
    /// Off by default: a file leaving the app should be one the user can hand
    /// to someone else. Redaction uses the same slots as the folder workspace
    /// ([`crate::secrets`]), so a secret cannot be hidden in one and shipped in
    /// the other.
    pub include_secrets: bool,
}

/// Write `collections` and `environments` in `format`.
///
/// Postman and HAR hold one collection at a time; the bundle holds them all.
/// Secret values are redacted unless `options` asks for them, and every value
/// stays intact when it is already a `{{variable}}`.
pub fn export(
    format: ExportFormat,
    collections: &[Collection],
    environments: &[Environment],
    options: ExportOptions,
) -> crate::Result<String> {
    let collections = exported_collections(collections, options);
    let environments = exported_environments(environments, options);
    match format {
        ExportFormat::Bundle => Bundle::new(collections, environments).to_json(),
        ExportFormat::Har => har::export(&collections),
        ExportFormat::Postman => match collections.as_slice() {
            [one] => postman::export(one),
            [] => Err(crate::Error::Other("there is nothing to export".to_owned())),
            _ => Err(crate::Error::Other(
                "a Postman file holds one collection; export them one at a time".to_owned(),
            )),
        },
    }
}

fn exported_collections(collections: &[Collection], options: ExportOptions) -> Vec<Collection> {
    let mut collections = collections.to_vec();
    if !options.include_secrets {
        collections
            .iter_mut()
            .for_each(crate::secrets::redact_collection);
    }
    collections
}

fn exported_environments(environments: &[Environment], options: ExportOptions) -> Vec<Environment> {
    let mut environments = environments.to_vec();
    if !options.include_secrets {
        environments
            .iter_mut()
            .for_each(crate::secrets::redact_environment);
    }
    environments
}

/// What an import produced, and what it could not read.
///
/// Every count is a count of *saved* things, so a caller that persists as it
/// walks can report exactly this. A skipped item always carries a reason, which
/// is what makes a partial import explainable instead of silent.
#[derive(Debug, Clone, Default)]
pub struct BulkImport {
    pub collections: Vec<Collection>,
    pub environments: Vec<Environment>,
    /// Names that were already present when the caller started saving.
    pub duplicates: Vec<String>,
    pub skipped: Vec<Skip>,
    /// Things that were imported but need the user's attention — an inherited
    /// auth with nothing to inherit from, for example. Not a skip: the request
    /// is in the result.
    pub notes: Vec<Skip>,
}

/// One item some format described but this one could not turn into a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skip {
    /// Where it was: a collection path, a file name, or a row label.
    pub at: String,
    pub reason: String,
}

impl BulkImport {
    /// The number of saved requests anywhere in the result.
    pub fn request_count(&self) -> usize {
        self.collections
            .iter()
            .map(|collection| count_requests(&collection.items))
            .sum()
    }

    /// A one-line summary fit for a notification.
    pub fn summary(&self) -> String {
        let requests = self.request_count();
        let mut parts = vec![format!("{requests} request{}", plural(requests))];
        if !self.environments.is_empty() {
            parts.push(format!(
                "{} environment{}",
                self.environments.len(),
                plural(self.environments.len())
            ));
        }
        if !self.duplicates.is_empty() {
            parts.push(format!("{} already present", self.duplicates.len()));
        }
        if !self.skipped.is_empty() {
            parts.push(format!("{} skipped", self.skipped.len()));
        }
        if !self.notes.is_empty() {
            parts.push(format!(
                "{} note{}",
                self.notes.len(),
                plural(self.notes.len())
            ));
        }
        format!("Imported {}", parts.join(", "))
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

fn count_requests(items: &[crate::model::CollectionItem]) -> usize {
    items
        .iter()
        .map(|item| match item {
            crate::model::CollectionItem::Request(_) => 1,
            crate::model::CollectionItem::Folder(folder) => count_requests(&folder.items),
        })
        .sum()
}

/// A format this build can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportFormat {
    /// A single `curl` command, as pasted from a browser's devtools.
    Curl,
    /// A Postman v2.1 collection export.
    Postman,
    /// A HAR 1.2 log, as saved by a browser's network panel.
    Har,
    /// An OpenAPI 3 document.
    OpenApi,
    /// A Bruno request file: the YAML shape newer Bruno writes per request.
    Bruno,
    /// This app's own bundle: every collection and environment.
    Bundle,
}

impl ImportFormat {
    /// Detect from a file name, for a file picker that has no other hint.
    pub fn from_file_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        if name.ends_with(".har") {
            return Some(Self::Har);
        }
        if name.ends_with(".bru") {
            return Some(Self::Bruno);
        }
        if name.ends_with(".yaml") || name.ends_with(".yml") {
            // OpenAPI and a Bruno request are both YAML, so the suffix cannot
            // decide; `detect` reads the envelope instead.
            return None;
        }
        if name.ends_with(".json") {
            // All three JSON formats are distinguished by content, not suffix.
            return None;
        }
        None
    }

    /// The format as a person reads it in a sentence ("a HAR log"), not as a
    /// variant name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Curl => "a cURL command",
            Self::Postman => "a Postman collection",
            Self::Har => "a HAR log",
            Self::OpenApi => "an OpenAPI document",
            Self::Bruno => "a Bruno request",
            Self::Bundle => "a restbook bundle",
        }
    }

    /// The format whose envelope this text matches, by shape rather than file
    /// name; `curl` is the fallback.
    ///
    /// JSON formats are told apart by their *top-level* keys: a document can
    /// mention `log` or `entries` in its own schemas, and Postman is `info` +
    /// `item` rather than `info` + the optional `schema`.
    pub fn detect(text: &str) -> Self {
        let trimmed = text.trim_start();
        if !trimmed.starts_with('{') {
            let lower = text.to_ascii_lowercase();
            let has = |key: &str| lower.starts_with(key) || lower.contains(&format!("\n{key}"));
            if has("openapi:") || has("swagger:") || has("paths:") {
                return Self::OpenApi;
            }
            // A Bruno request file is `info` + `http`; an OpenAPI document has
            // `info` + `paths` and was claimed above.
            if has("http:") && has("info:") {
                return Self::Bruno;
            }
            return Self::Curl;
        }
        if let Some(format) = Self::detect_json(text) {
            return format;
        }
        // Truncated or otherwise unparsable JSON still gets the substring
        // guess, so a half-written HAR is reported as a HAR and not as curl.
        if text.contains("\"log\"") && text.contains("\"entries\"") {
            return Self::Har;
        }
        if text.contains("\"info\"") && text.contains("\"item\"") {
            return Self::Postman;
        }
        if text.contains("\"openapi\"") || text.contains("\"swagger\"") {
            return Self::OpenApi;
        }
        if text.contains("\"collections\"") && text.contains("\"version\"") {
            return Self::Bundle;
        }
        Self::Curl
    }

    /// The format a parsable JSON object declares at its root; `curl` when no
    /// envelope of ours claims it. `None` means the text is not a JSON object
    /// at all, and the caller's substring guess still applies.
    fn detect_json(text: &str) -> Option<Self> {
        // Read only the root keys. Parsing the whole document into a `Value`
        // just to look at them cost a second and ~137 MB of heap on a 29 MB
        // OpenAPI file (B30); this visits the root map and skips every value.
        let mut deserializer = serde_json::Deserializer::from_str(text);
        let keys = RootKeys::deserialize(&mut deserializer).ok()?;
        deserializer.end().ok()?;
        // `log` alone is a HAR, even without `entries`, so a malformed log
        // still reaches the HAR parser and says what is wrong with it.
        if keys.log {
            return Some(Self::Har);
        }
        if keys.info && keys.item {
            return Some(Self::Postman);
        }
        if keys.openapi || keys.swagger {
            return Some(Self::OpenApi);
        }
        if keys.collections && keys.version {
            return Some(Self::Bundle);
        }
        Some(Self::Curl)
    }
}

/// The top-level keys that decide a JSON envelope, read without a DOM.
#[derive(Default)]
struct RootKeys {
    log: bool,
    info: bool,
    item: bool,
    openapi: bool,
    swagger: bool,
    collections: bool,
    version: bool,
}

impl<'de> Deserialize<'de> for RootKeys {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(RootKeysVisitor)
    }
}

/// Visits the root map's keys; every value is skipped unread.
struct RootKeysVisitor;

impl<'de> serde::de::Visitor<'de> for RootKeysVisitor {
    type Value = RootKeys;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<RootKeys, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut keys = RootKeys::default();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "log" => keys.log = true,
                "info" => keys.info = true,
                "item" => keys.item = true,
                "openapi" => keys.openapi = true,
                "swagger" => keys.swagger = true,
                "collections" => keys.collections = true,
                "version" => keys.version = true,
                _ => {}
            }
            map.next_value::<serde::de::IgnoredAny>()?;
        }
        Ok(keys)
    }
}

/// The export shape. One file holds everything, so a backup is one file and a
/// diff between two exports is readable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bundle {
    /// `1`, so a later format change can be told apart from this one.
    #[serde(default = "bundle_version")]
    pub version: u32,
    #[serde(default)]
    pub collections: Vec<Collection>,
    #[serde(default)]
    pub environments: Vec<Environment>,
}

fn bundle_version() -> u32 {
    1
}

impl Bundle {
    pub fn new(collections: Vec<Collection>, environments: Vec<Environment>) -> Self {
        Self {
            version: bundle_version(),
            collections,
            environments,
        }
    }

    pub fn to_json(&self) -> crate::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(text: &str) -> crate::Result<Self> {
        let bundle: Self = serde_json::from_str(text)?;
        if bundle.version > bundle_version() {
            return Err(crate::Error::Other(format!(
                "this bundle is version {}, newer than this build understands",
                bundle.version
            )));
        }
        Ok(bundle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CollectionItem, Kv, Method, RequestDef, SavedRequest};

    fn collection_with(name: &str, requests: usize) -> Collection {
        let items = (0..requests)
            .map(|index| {
                CollectionItem::Request(SavedRequest {
                    id: format!("r{index}"),
                    name: format!("Request {index}"),
                    request: RequestDef::new(Method::Get, "https://example.com"),
                })
            })
            .collect();
        Collection {
            id: name.to_owned(),
            name: name.to_owned(),
            items,
        }
    }

    #[test]
    fn a_summary_counts_across_collections() {
        let import = BulkImport {
            collections: vec![collection_with("a", 2), collection_with("b", 1)],
            environments: vec![Environment {
                name: "dev".into(),
                vars: Vec::new(),
            }],
            duplicates: Vec::new(),
            skipped: Vec::new(),
            notes: Vec::new(),
        };
        assert_eq!(import.request_count(), 3);
        assert_eq!(import.summary(), "Imported 3 requests, 1 environment");
    }

    #[test]
    fn a_summary_names_what_was_left_out() {
        let import = BulkImport {
            collections: vec![collection_with("a", 1)],
            environments: Vec::new(),
            duplicates: vec!["a".into()],
            skipped: vec![Skip {
                at: "a/x".into(),
                reason: "no url".into(),
            }],
            notes: Vec::new(),
        };
        assert_eq!(
            import.summary(),
            "Imported 1 request, 1 already present, 1 skipped"
        );
    }

    #[test]
    fn a_bundle_roundtrips() {
        let bundle = Bundle::new(vec![collection_with("a", 2)], Vec::new());
        let json = bundle.to_json().unwrap();
        let loaded = Bundle::from_json(&json).unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.collections.len(), 1);
        assert_eq!(loaded.collections[0].items.len(), 2);
    }

    /// The app's own export must be importable, or *Everything* is a backup
    /// nothing can restore.
    #[test]
    fn a_bundle_is_detected_and_parsed_back() {
        let environments = vec![Environment {
            name: "dev".into(),
            vars: Vec::new(),
        }];
        let json = export(
            ExportFormat::Bundle,
            &[collection_with("a", 2)],
            &environments,
            ExportOptions::default(),
        )
        .unwrap();

        assert_eq!(ImportFormat::detect(&json), ImportFormat::Bundle);
        let import = parse(ImportFormat::Bundle, &json, Options::default()).unwrap();
        assert_eq!(import.collections.len(), 1);
        assert_eq!(import.request_count(), 2);
        assert_eq!(import.environments.len(), 1);
    }

    #[test]
    fn a_newer_bundle_is_refused_rather_than_half_read() {
        let json = r#"{"version": 99, "collections": [], "environments": []}"#;
        assert!(Bundle::from_json(json).is_err());
    }

    #[test]
    fn a_bundle_without_a_version_is_read_as_this_one() {
        let json = r#"{"collections": [], "environments": []}"#;
        assert_eq!(Bundle::from_json(json).unwrap().version, 1);
    }

    #[test]
    fn a_bundle_export_carries_environments_and_the_others_do_not() {
        let collections = vec![collection_with("a", 1)];
        let environments = vec![Environment {
            name: "dev".into(),
            vars: Vec::new(),
        }];

        let bundle = export(
            ExportFormat::Bundle,
            &collections,
            &environments,
            ExportOptions::default(),
        )
        .unwrap();
        assert!(bundle.contains("\"dev\""));

        // A HAR and a Postman collection describe requests only; an
        // environment would have no place to go.
        let har = export(
            ExportFormat::Har,
            &collections,
            &environments,
            ExportOptions::default(),
        )
        .unwrap();
        assert!(!har.contains("\"dev\""));
        let postman = export(
            ExportFormat::Postman,
            &collections,
            &environments,
            ExportOptions::default(),
        )
        .unwrap();
        assert!(!postman.contains("\"dev\""));
    }

    #[test]
    fn postman_export_refuses_several_collections_rather_than_picking_one() {
        let collections = vec![collection_with("a", 1), collection_with("b", 1)];
        let error = export(
            ExportFormat::Postman,
            &collections,
            &[],
            ExportOptions::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("one collection"));
    }

    fn secret(value: &str, key: &str) -> Kv {
        Kv {
            key: key.to_owned(),
            value: value.to_owned(),
            enabled: true,
            secret: true,
        }
    }

    fn request_with(request: RequestDef) -> Collection {
        Collection {
            id: "col-1".to_owned(),
            name: "Acme".to_owned(),
            items: vec![CollectionItem::Request(SavedRequest {
                id: "r1".to_owned(),
                name: "Login".to_owned(),
                request,
            })],
        }
    }

    /// A file leaving the app must not carry a credential unless the caller
    /// asked for one, and it must use the same slots the folder workspace
    /// redacts so the two lists cannot drift.
    #[test]
    fn an_export_redacts_credentials_and_secret_rows_by_default() {
        let mut login = RequestDef::new(Method::Post, "https://example.com/login");
        login.auth = crate::Auth::Bearer {
            token: "bearer-token".to_owned(),
        };
        login.headers.push(secret("header-value", "X-Api-Key"));
        login.params.push(secret("param-value", "token"));
        login.body = crate::Body::Form(vec![secret("form-value", "password")]);

        let environments = vec![Environment {
            name: "dev".to_owned(),
            vars: vec![secret("env-value", "TOKEN")],
        }];

        let bundle = export(
            ExportFormat::Bundle,
            &[request_with(login)],
            &environments,
            ExportOptions::default(),
        )
        .unwrap();
        for leaked in [
            "bearer-token",
            "header-value",
            "param-value",
            "form-value",
            "env-value",
        ] {
            assert!(!bundle.contains(leaked), "`{leaked}` leaked:\n{bundle}");
        }
        assert!(bundle.contains(crate::secrets::REDACTED));
        // The shape survives: a reviewer still sees which row and which auth.
        assert!(bundle.contains("bearer"));
        assert!(bundle.contains("X-Api-Key"));
    }

    #[test]
    fn an_oauth_client_secret_is_redacted_like_any_other_credential() {
        let mut request = RequestDef::new(Method::Post, "https://example.com/token");
        request.auth = crate::Auth::OAuth2 {
            config: crate::OAuthConfig {
                client_id: "client".to_owned(),
                client_secret: "oauth-secret".to_owned(),
                token_url: "https://example.com/token".to_owned(),
                ..crate::OAuthConfig::default()
            },
            access_token: None,
        };

        let bundle = export(
            ExportFormat::Bundle,
            &[request_with(request)],
            &[],
            ExportOptions::default(),
        )
        .unwrap();
        assert!(!bundle.contains("oauth-secret"), "{bundle}");
        assert!(bundle.contains(crate::secrets::REDACTED));
    }

    #[test]
    fn include_secrets_writes_the_literal_values() {
        let mut request = RequestDef::new(Method::Get, "https://example.com");
        request.auth = crate::Auth::Bearer {
            token: "bearer-token".to_owned(),
        };
        let environments = vec![Environment {
            name: "dev".to_owned(),
            vars: vec![secret("env-value", "TOKEN")],
        }];

        let bundle = export(
            ExportFormat::Bundle,
            &[request_with(request)],
            &environments,
            ExportOptions {
                include_secrets: true,
            },
        )
        .unwrap();
        assert!(bundle.contains("bearer-token"));
        assert!(bundle.contains("env-value"));
        assert!(!bundle.contains(crate::secrets::REDACTED));
    }

    #[test]
    fn an_export_leaves_a_variable_reference_alone() {
        let mut request = RequestDef::new(Method::Get, "https://example.com");
        request.auth = crate::Auth::Bearer {
            token: "{{TOKEN}}".to_owned(),
        };

        let postman = export(
            ExportFormat::Postman,
            &[request_with(request)],
            &[],
            ExportOptions::default(),
        )
        .unwrap();
        assert!(postman.contains("{{TOKEN}}"), "{postman}");
        assert!(!postman.contains(crate::secrets::REDACTED));
    }

    #[test]
    fn har_and_postman_are_told_apart_by_shape() {
        assert_eq!(
            ImportFormat::detect(r#"{"log": {"entries": []}}"#),
            ImportFormat::Har
        );
        assert_eq!(
            ImportFormat::detect(r#"{"info": {"schema": "..."}, "item": []}"#),
            ImportFormat::Postman
        );
        assert_eq!(
            ImportFormat::detect(r#"{"openapi": "3.1.0", "paths": {}}"#),
            ImportFormat::OpenApi
        );
        assert_eq!(
            ImportFormat::detect("curl -X POST https://example.com"),
            ImportFormat::Curl
        );
        assert_eq!(
            ImportFormat::detect(
                "info:\n  name: Ping\nhttp:\n  method: GET\n  url: https://x.test\n"
            ),
            ImportFormat::Bruno
        );
        assert_eq!(
            ImportFormat::detect("openapi: 3.0.0\ninfo:\n  title: Acme\npaths: {}\n"),
            ImportFormat::OpenApi
        );
    }

    #[test]
    fn a_nested_log_and_entries_does_not_make_a_spec_a_har() {
        // Cloudflare's 29 MB OpenAPI document mentions both words in its own
        // schemas. The old scan read it as a HAR and refused it with "no
        // log.entries".
        let spec = r#"{
            "openapi": "3.0.3",
            "info": { "title": "Large" },
            "paths": {
                "/logs": {
                    "get": { "responses": { "200": { "schema": { "log": { "entries": [] } } } } }
                }
            }
        }"#;
        assert_eq!(ImportFormat::detect(spec), ImportFormat::OpenApi);
    }

    #[test]
    fn a_log_without_entries_is_still_read_as_a_har() {
        // The HAR parser says which key is missing; curl would turn the JSON
        // into one request named after its first byte.
        assert_eq!(
            ImportFormat::detect(r#"{"log": {"pages": []}}"#),
            ImportFormat::Har
        );
    }

    #[test]
    fn a_postman_export_without_a_schema_is_still_postman() {
        // `schema` is optional across Postman versions. Without this case the
        // old `info` + `schema` test fell through to the curl parser, which
        // accepts anything and turned the JSON into a single request named
        // after its first byte.
        assert_eq!(
            ImportFormat::detect(r#"{"info": {"name": "Acme"}, "item": []}"#),
            ImportFormat::Postman
        );
    }

    #[test]
    fn a_file_name_only_answers_where_the_suffix_is_unambiguous() {
        assert_eq!(
            ImportFormat::from_file_name("session.HAR"),
            Some(ImportFormat::Har)
        );
        assert_eq!(
            ImportFormat::from_file_name("request.bru"),
            Some(ImportFormat::Bruno)
        );
        // YAML is OpenAPI or a Bruno request, so the name does not decide.
        assert_eq!(ImportFormat::from_file_name("api.yaml"), None);
        // JSON could be any of the three, so the name does not decide.
        assert_eq!(ImportFormat::from_file_name("things.json"), None);
    }
}
