//! Turns a [`RequestDef`] plus a variable context into something sendable.
//!
//! Preparation is pure: no I/O, no clock, no network. That makes it cheap to
//! unit-test and safe to call while the user types (e.g. to show a "resolved"
//! preview of the URL).

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use url::Url;

use crate::error::{Error, Result};
use crate::model::{ApiKeyLocation, Auth, Body, Method, RequestDef};
use crate::template::{resolve, VarContext};

#[derive(Debug, Clone)]
pub struct PreparedRequest {
    pub method: Method,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: PreparedBody,
    pub timeout_ms: Option<u64>,
    pub follow_redirects: bool,
    pub insecure: bool,
}

#[derive(Debug, Clone)]
pub enum PreparedBody {
    None,
    Bytes { mime: String, data: Vec<u8> },
    Form(Vec<(String, String)>),
    Multipart(Vec<PreparedPart>),
    File(String),
}

#[derive(Debug, Clone)]
pub struct PreparedPart {
    pub name: String,
    pub data: PartData,
}

#[derive(Debug, Clone)]
pub enum PartData {
    Text(String),
    File {
        path: String,
        mime: Option<String>,
        file_name: String,
    },
}

pub fn prepare(request: &RequestDef, ctx: &VarContext) -> Result<PreparedRequest> {
    let raw_url = resolve(&request.url, ctx)?;
    let mut url =
        Url::parse(&raw_url).map_err(|error| Error::InvalidUrl(format!("{raw_url}: {error}")))?;

    let mut headers = Vec::new();
    for header in request.headers.iter().filter(|header| header.enabled) {
        let key = resolve(&header.key, ctx)?;
        let value = resolve(&header.value, ctx)?;
        check_header(&key, &value)?;
        headers.push((key, value));
    }

    let mut query = Vec::new();
    for param in request.params.iter().filter(|param| param.enabled) {
        if param.key.trim().is_empty() {
            continue;
        }
        query.push((resolve(&param.key, ctx)?, resolve(&param.value, ctx)?));
    }

    apply_auth(&request.auth, ctx, &mut headers, &mut query)?;

    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(&key, &value);
        }
    }

    Ok(PreparedRequest {
        method: request.method,
        url,
        headers,
        body: prepare_body(&request.body, ctx)?,
        timeout_ms: request.timeout_ms,
        follow_redirects: request.follow_redirects.unwrap_or(true),
        insecure: request.insecure,
    })
}

fn apply_auth(
    auth: &Auth,
    ctx: &VarContext,
    headers: &mut Vec<(String, String)>,
    query: &mut Vec<(String, String)>,
) -> Result<()> {
    match auth {
        Auth::None => {}
        Auth::Basic { username, password } => {
            let username = resolve(username, ctx)?;
            let password = resolve(password, ctx)?;
            let encoded = BASE64.encode(format!("{username}:{password}"));
            headers.push(("authorization".to_owned(), format!("Basic {encoded}")));
        }
        Auth::Bearer { token } => {
            headers.push((
                "authorization".to_owned(),
                format!("Bearer {}", resolve(token, ctx)?),
            ));
        }
        Auth::ApiKey {
            name,
            value,
            location,
        } => {
            let name = resolve(name, ctx)?;
            let value = resolve(value, ctx)?;
            match location {
                ApiKeyLocation::Header => headers.push((name, value)),
                ApiKeyLocation::Query => query.push((name, value)),
            }
        }
    }
    Ok(())
}

fn prepare_body(body: &Body, ctx: &VarContext) -> Result<PreparedBody> {
    Ok(match body {
        Body::None => PreparedBody::None,
        Body::Raw { mime, text } => PreparedBody::Bytes {
            mime: mime.clone(),
            data: resolve(text, ctx)?.into_bytes(),
        },
        Body::Form(fields) => {
            let mut out = Vec::new();
            for field in fields.iter().filter(|field| field.enabled) {
                if field.key.trim().is_empty() {
                    continue;
                }
                out.push((resolve(&field.key, ctx)?, resolve(&field.value, ctx)?));
            }
            PreparedBody::Form(out)
        }
        Body::Multipart(parts) => {
            let mut out = Vec::new();
            for part in parts.iter().filter(|part| part.enabled) {
                let name = resolve(&part.name, ctx)?;
                let data = match &part.file {
                    Some(path) => {
                        let path = resolve(path, ctx)?;
                        PartData::File {
                            file_name: file_name_of(&path),
                            path,
                            mime: part.mime.clone(),
                        }
                    }
                    None => PartData::Text(resolve(&part.value, ctx)?),
                };
                out.push(PreparedPart { name, data });
            }
            PreparedBody::Multipart(out)
        }
        Body::Binary { path } => PreparedBody::File(resolve(path, ctx)?),
    })
}

fn file_name_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file")
        .to_owned()
}

/// Header injection guard. A newline in a header value is a request-splitting
/// bug waiting to happen, so it is rejected at prepare time.
fn check_header(key: &str, value: &str) -> Result<()> {
    let bad = |text: &str| text.contains(['\r', '\n']);
    if key.is_empty() || key.contains(':') || bad(key) || bad(value) {
        return Err(Error::InvalidHeader(key.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kv;

    #[test]
    fn interpolates_and_appends_params() {
        let mut request = RequestDef::new(Method::Get, "https://{{host}}/v1/items");
        request.params.push(Kv::new("page", "{{page}}"));
        request.params.push(Kv {
            key: "skipped".into(),
            value: "1".into(),
            enabled: false,
            secret: false,
        });

        let ctx = VarContext::new()
            .with_scope([("host", "api.example.com")])
            .with_scope([("page", "2")]);

        let prepared = prepare(&request, &ctx).unwrap();
        assert_eq!(
            prepared.url.as_str(),
            "https://api.example.com/v1/items?page=2"
        );
    }

    #[test]
    fn basic_auth_becomes_a_header() {
        let mut request = RequestDef::new(Method::Get, "https://example.com");
        request.auth = Auth::Basic {
            username: "user".into(),
            password: "pass".into(),
        };

        let prepared = prepare(&request, &VarContext::new()).unwrap();
        assert_eq!(
            prepared.headers,
            vec![("authorization".to_owned(), "Basic dXNlcjpwYXNz".to_owned())]
        );
    }

    #[test]
    fn rejects_header_injection() {
        let mut request = RequestDef::new(Method::Get, "https://example.com");
        request
            .headers
            .push(Kv::new("x-test", "value\r\nx-evil: 1"));

        let error = prepare(&request, &VarContext::new()).unwrap_err();
        assert!(matches!(error, Error::InvalidHeader(_)));
    }

    #[test]
    fn missing_variable_fails_before_sending() {
        let request = RequestDef::new(Method::Get, "https://{{nope}}/");
        assert!(prepare(&request, &VarContext::new()).is_err());
    }
}
