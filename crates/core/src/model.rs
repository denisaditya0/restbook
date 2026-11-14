//! Plain data shared by the engine, the stores, and the UI.
//!
//! These types are the on-the-wire shape for import/export as well, so they are
//! `serde`-derived and avoid implementation detail.

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
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

/// What came back. `body` stays raw bytes; text decoding is a view concern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseRecord {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub duration_ms: u64,
    pub received_at: chrono::DateTime<chrono::Utc>,
}

impl ResponseRecord {
    pub fn size_bytes(&self) -> usize {
        self.body.len()
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
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
