//! Plain data shared by the engine, the stores, and the UI.
//!
//! These types are the on-the-wire shape for import/export as well, so they are
//! `serde`-derived and avoid implementation detail.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Head => "HEAD",
            Method::Options => "OPTIONS",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_uppercase().as_str() {
            "GET" => Some(Method::Get),
            "POST" => Some(Method::Post),
            "PUT" => Some(Method::Put),
            "PATCH" => Some(Method::Patch),
            "DELETE" => Some(Method::Delete),
            "HEAD" => Some(Method::Head),
            "OPTIONS" => Some(Method::Options),
            _ => None,
        }
    }
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A name/value row: header, query parameter, form field, or environment entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Kv {
    pub key: String,
    pub value: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Secrets are masked in the UI and, in the app store, never written in
    /// plaintext outside the encrypted database.
    #[serde(default)]
    pub secret: bool,
}

impl Kv {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            enabled: true,
            secret: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub enum Body {
    #[default]
    None,
    Raw {
        mime: String,
        text: String,
    },
    Form(Vec<Kv>),
    Multipart(Vec<FormPart>),
    Binary {
        path: String,
    },
}

// `Form` and `Multipart` hold a sequence, and a `serde` internally tagged enum
// cannot serialize a newtype variant containing one. These mirrors keep the
// stored shape (`{"type": "raw", "mime": …, "text": …}`) and give the row
// lists a name of their own (`rows`, `parts`).
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BodyFields<'a> {
    None,
    Raw { mime: &'a str, text: &'a str },
    Form { rows: &'a [Kv] },
    Multipart { parts: &'a [FormPart] },
    Binary { path: &'a str },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OwnedBodyFields {
    None,
    Raw { mime: String, text: String },
    Form { rows: Vec<Kv> },
    Multipart { parts: Vec<FormPart> },
    Binary { path: String },
}

impl Serialize for Body {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let fields = match self {
            Body::None => BodyFields::None,
            Body::Raw { mime, text } => BodyFields::Raw {
                mime: mime.as_str(),
                text: text.as_str(),
            },
            Body::Form(rows) => BodyFields::Form { rows },
            Body::Multipart(parts) => BodyFields::Multipart { parts },
            Body::Binary { path } => BodyFields::Binary {
                path: path.as_str(),
            },
        };
        fields.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Body {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match OwnedBodyFields::deserialize(deserializer)? {
            OwnedBodyFields::None => Body::None,
            OwnedBodyFields::Raw { mime, text } => Body::Raw { mime, text },
            OwnedBodyFields::Form { rows } => Body::Form(rows),
            OwnedBodyFields::Multipart { parts } => Body::Multipart(parts),
            OwnedBodyFields::Binary { path } => Body::Binary { path },
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FormPart {
    pub name: String,
    #[serde(default)]
    pub value: String,
    /// When set, the part is a file upload and `value` is ignored.
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub mime: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyLocation {
    #[default]
    Header,
    Query,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    #[default]
    None,
    Basic {
        username: String,
        password: String,
    },
    Bearer {
        token: String,
    },
    ApiKey {
        name: String,
        value: String,
        #[serde(default)]
        location: ApiKeyLocation,
    },
    /// An OAuth 2.0 client configuration plus the token fetched for this send.
    #[serde(rename = "oauth2")]
    OAuth2 {
        #[serde(flatten)]
        config: crate::oauth::OAuthConfig,
        /// Supplied by the caller at send time; never persisted.
        #[serde(skip)]
        access_token: Option<String>,
    },
}

/// A request as the user defined it. Contains `{{variables}}`; it is not
/// directly sendable. Call [`crate::prepare`] first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestDef {
    pub id: String,
    pub name: String,
    pub method: Method,
    pub url: String,
    #[serde(default)]
    pub params: Vec<Kv>,
    #[serde(default)]
    pub headers: Vec<Kv>,
    #[serde(default)]
    pub body: Body,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// `None` keeps the default (follow, limited to 10 hops).
    #[serde(default)]
    pub follow_redirects: Option<bool>,
    /// Skip certificate verification. Reflected in the UI as a warning badge.
    #[serde(default)]
    pub insecure: bool,
}

impl RequestDef {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        let url = url.into();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: format!("{method} {url}"),
            method,
            url,
            params: Vec::new(),
            headers: Vec::new(),
            body: Body::None,
            auth: Auth::None,
            timeout_ms: None,
            follow_redirects: None,
            insecure: false,
        }
    }
}

/// One hop of a redirect chain: what was answered, and where it pointed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedirectHop {
    pub status: u16,
    /// Absolute, already resolved against the hop it came from.
    pub location: String,
}

/// What came back. `body` stays raw bytes; text decoding is a view concern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseRecord {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: ResponseBody,
    pub duration_ms: u64,
    pub received_at: chrono::DateTime<chrono::Utc>,
    /// The URL that actually answered, after any redirects. `None` on records
    /// written before this field existed.
    #[serde(default)]
    pub final_url: Option<String>,
    /// Every hop that was followed, oldest first. Empty when nothing redirected
    /// or when redirects were not followed, so an un-followed 3xx is a plain
    /// 3xx and not a one-hop chain.
    #[serde(default)]
    pub redirects: Vec<RedirectHop>,
}

/// Where a response's bytes live. `Memory` is every response today; `File` is
/// the streamed-to-disk shape (B18). `untagged` keeps a memory body serialising
/// as a bare array, so records written before this enum still parse.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseBody {
    Memory(Vec<u8>),
    File(FileBody),
}

/// A body streamed to disk instead of buffered. `preview` is the first few KiB
/// kept so the response pane can render something without a re-read; `etag` and
/// `resume_from` are the resume contract, carried from day one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileBody {
    pub path: String,
    pub len: u64,
    #[serde(default)]
    pub preview: Vec<u8>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub resume_from: u64,
}

impl From<Vec<u8>> for ResponseBody {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Memory(bytes)
    }
}

impl From<String> for ResponseBody {
    fn from(text: String) -> Self {
        Self::Memory(text.into_bytes())
    }
}

impl ResponseBody {
    /// The bytes a reader can render now: the whole body in memory, or the
    /// preview for a file body.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Memory(bytes) => bytes,
            Self::File(file) => &file.preview,
        }
    }

    /// The body's real size: the buffered length, or the file's recorded length
    /// (which can exceed the preview).
    pub fn len(&self) -> usize {
        match self {
            Self::Memory(bytes) => bytes.len(),
            Self::File(file) => file.len as usize,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_file(&self) -> bool {
        matches!(self, Self::File(_))
    }
}

impl ResponseRecord {
    pub fn size_bytes(&self) -> usize {
        self.body.len()
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(self.body.as_bytes())
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Environment {
    pub name: String,
    #[serde(default)]
    pub vars: Vec<Kv>,
}

/// A node in a collection tree: either a folder (which nests further) or a
/// saved request. Tagged so the export shape stays self-describing.
///
/// Variants stay unboxed so the tagged JSON and pattern matches stay direct;
/// a collection tree is small and not a hot path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CollectionItem {
    Folder(Folder),
    Request(SavedRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub items: Vec<CollectionItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedRequest {
    pub id: String,
    pub name: String,
    pub request: RequestDef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub items: Vec<CollectionItem>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::{OAuthConfig, OAuthGrant};

    #[test]
    fn oauth_config_is_flattened_into_the_auth_object() {
        let auth = Auth::OAuth2 {
            config: OAuthConfig {
                client_id: "client-1".to_owned(),
                token_url: "https://auth.example.com/token".to_owned(),
                scopes: vec!["read".to_owned()],
                ..OAuthConfig::default()
            },
            access_token: Some("secret-token".to_owned()),
        };

        let json = serde_json::to_value(&auth).unwrap();
        assert_eq!(json["type"], "oauth2");
        assert_eq!(json["grant"], "authorization_code");
        assert_eq!(json["client_id"], "client-1");
        assert_eq!(json["scopes"][0], "read");
        // The token is supplied per send and must never be exported.
        assert!(json.get("access_token").is_none());

        let back: Auth = serde_json::from_value(json).unwrap();
        match back {
            Auth::OAuth2 {
                config,
                access_token,
            } => {
                assert_eq!(config.grant, OAuthGrant::AuthorizationCode);
                assert_eq!(config.client_id, "client-1");
                assert!(config.pkce, "PKCE is on unless an export says otherwise");
                assert!(access_token.is_none());
            }
            other => panic!("expected OAuth2, got {other:?}"),
        }
    }

    #[test]
    fn an_old_export_without_oauth_fields_still_loads() {
        let auth: Auth =
            serde_json::from_str(r#"{"type":"api_key","name":"X","value":"y"}"#).unwrap();
        assert!(matches!(auth, Auth::ApiKey { .. }));
    }

    #[test]
    fn a_memory_body_serialises_as_a_bare_array() {
        let body = ResponseBody::Memory(b"ok".to_vec());
        let json = serde_json::to_string(&body).unwrap();
        assert_eq!(json, "[111,107]", "the shape older records were saved in");

        // A record written before the enum still parses as Memory.
        let old: ResponseBody = serde_json::from_str("[111,107]").unwrap();
        assert_eq!(old.as_bytes(), b"ok");
        assert!(!old.is_file());
    }

    #[test]
    fn a_file_body_keeps_its_metadata_and_preview() {
        let body = ResponseBody::File(FileBody {
            path: "/tmp/body.bin".to_owned(),
            len: 5_000,
            preview: b"head".to_vec(),
            etag: Some("\"abc\"".to_owned()),
            resume_from: 128,
        });
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["len"], 5_000);
        assert_eq!(json["resume_from"], 128);

        let back: ResponseBody = serde_json::from_value(json).unwrap();
        assert_eq!(back.len(), 5_000, "len is the file's full size");
        assert_eq!(back.as_bytes(), b"head", "the preview is what renders");
        assert!(back.is_file());
    }

    /// A form or multipart body holds a sequence, which an internally tagged
    /// enum cannot write as a newtype variant; the mirrors below keep every
    /// body kind round-tripping through JSON.
    #[test]
    fn every_request_body_kind_roundtrips_through_json() {
        let bodies = [
            Body::None,
            Body::Raw {
                mime: "application/json".to_owned(),
                text: "{}".to_owned(),
            },
            Body::Form(vec![Kv::new("grant_type", "password")]),
            Body::Multipart(vec![FormPart {
                name: "file".to_owned(),
                file: Some("/tmp/a.txt".to_owned()),
                ..FormPart::default()
            }]),
            Body::Binary {
                path: "/tmp/a.bin".to_owned(),
            },
        ];

        for body in bodies {
            let json = serde_json::to_string(&body).unwrap();
            let back: Body = serde_json::from_str(&json).unwrap();
            assert_eq!(
                serde_json::to_string(&back).unwrap(),
                json,
                "a body kind did not survive {json}"
            );
        }
    }

    /// The mirrors must not drift from the shape stored files already use.
    #[test]
    fn a_body_keeps_the_stored_shape() {
        assert_eq!(
            serde_json::to_value(Body::None).unwrap(),
            serde_json::json!({ "type": "none" })
        );
        assert_eq!(
            serde_json::to_value(Body::Raw {
                mime: "text/plain".to_owned(),
                text: "hi".to_owned(),
            })
            .unwrap(),
            serde_json::json!({ "type": "raw", "mime": "text/plain", "text": "hi" })
        );
        assert_eq!(
            serde_json::to_value(Body::Binary {
                path: "a.bin".to_owned(),
            })
            .unwrap(),
            serde_json::json!({ "type": "binary", "path": "a.bin" })
        );
        assert_eq!(
            serde_json::to_value(Body::Form(vec![Kv::new("a", "1")])).unwrap(),
            serde_json::json!({
                "type": "form",
                "rows": [{ "key": "a", "value": "1", "enabled": true, "secret": false }]
            })
        );
    }
}
