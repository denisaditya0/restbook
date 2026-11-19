//! Batch import and export of workspace data.
//!
//! Every format becomes [`Collection`]s and [`Environment`]s. Parsing is
//! forgiving where a format is sloppy: a bad item is a [`Skip`], not a failed
//! file. Nothing reads cookies, secrets, or the network, and an import never
//! overwrites an existing collection or environment — it reports the collision.

use serde::{Deserialize, Serialize};

use crate::model::{Collection, Environment};

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

/// Write `collections` and `environments` in `format`.
///
/// Postman and HAR hold one collection at a time; the bundle holds them all.
pub fn export(
    format: ExportFormat,
    collections: &[Collection],
    environments: &[Environment],
) -> crate::Result<String> {
    match format {
        ExportFormat::Bundle => Bundle::new(collections.to_vec(), environments.to_vec()).to_json(),
        ExportFormat::Har => har::export(collections),
        ExportFormat::Postman => match collections {
            [one] => postman::export(one),
            [] => Err(crate::Error::Other("there is nothing to export".to_owned())),
            _ => Err(crate::Error::Other(
                "a Postman file holds one collection; export them one at a time".to_owned(),
            )),
        },
    }
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
}

impl ImportFormat {
    /// Detect from a file name, for a file picker that has no other hint.
    pub fn from_file_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        if name.ends_with(".har") {
            return Some(Self::Har);
        }
        if name.ends_with(".yaml") || name.ends_with(".yml") {
            // A YAML file is an OpenAPI document in practice; the parser says
            // so plainly if it is not.
            return Some(Self::OpenApi);
        }
        if name.ends_with(".json") {
            // All three JSON formats are distinguished by content, not suffix.
            return None;
        }
        None
    }

    /// The format whose envelope this text matches, by shape rather than file
    /// name; `curl` is the fallback.
    ///
    /// JSON formats are told apart by the *pair* of keys that makes each valid.
    /// Postman is `info` + `item`, not `info` + `schema`: `schema` is optional,
    /// and a lone `info` used to fall through to the curl parser.
    pub fn detect(text: &str) -> Self {
        let trimmed = text.trim_start();
        if !trimmed.starts_with('{') {
            return Self::Curl;
        }
        if text.contains("\"log\"") && text.contains("\"entries\"") {
            return Self::Har;
        }
        if text.contains("\"info\"") && text.contains("\"item\"") {
            return Self::Postman;
        }
        if text.contains("\"openapi\"") || text.contains("\"swagger\"") {
            return Self::OpenApi;
        }
        Self::Curl
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
    use crate::model::{CollectionItem, Method, RequestDef, SavedRequest};

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

        let bundle = export(ExportFormat::Bundle, &collections, &environments).unwrap();
        assert!(bundle.contains("\"dev\""));

        // A HAR and a Postman collection describe requests only; an
        // environment would have no place to go.
        let har = export(ExportFormat::Har, &collections, &environments).unwrap();
        assert!(!har.contains("\"dev\""));
        let postman = export(ExportFormat::Postman, &collections, &environments).unwrap();
        assert!(!postman.contains("\"dev\""));
    }

    #[test]
    fn postman_export_refuses_several_collections_rather_than_picking_one() {
        let collections = vec![collection_with("a", 1), collection_with("b", 1)];
        let error = export(ExportFormat::Postman, &collections, &[]).unwrap_err();
        assert!(error.to_string().contains("one collection"));
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
            ImportFormat::from_file_name("api.yaml"),
            Some(ImportFormat::OpenApi)
        );
        // JSON could be any of the three, so the name does not decide.
        assert_eq!(ImportFormat::from_file_name("things.json"), None);
    }
}
