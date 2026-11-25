//! Turns a [`RequestDef`] plus a variable context into something sendable.
//!
//! Preparation is pure given the generators: no I/O, no network. The clock and
//! random source are the only non-request inputs, which [`prepare_with`] takes
//! and [`prepare`] supplies. That makes it cheap to test and safe to run while
//! the user types, e.g. for a resolved URL preview.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use url::Url;

use crate::error::{Error, Result};
use crate::generator::Generators;
use crate::model::{ApiKeyLocation, Auth, Body, Method, RequestDef};
use crate::template::{resolve_with, VarContext};

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

/// Headers every request carries unless the request overrides one by name.
/// They live here, not in the transport's client, so the Headers tab can list
/// exactly what the engine sends; `Content-Type` is excluded because the body
/// decides it.
pub fn default_headers() -> Vec<(String, String)> {
    vec![
        ("Accept".to_owned(), "*/*".to_owned()),
        ("Accept-Encoding".to_owned(), "gzip, deflate, br".to_owned()),
        (
            "User-Agent".to_owned(),
            concat!("restbook/", env!("CARGO_PKG_VERSION")).to_owned(),
        ),
    ]
}

pub fn prepare(request: &RequestDef, ctx: &VarContext) -> Result<PreparedRequest> {
    prepare_with(request, ctx, &Generators::now())
}

pub fn prepare_with(
    request: &RequestDef,
    ctx: &VarContext,
    generators: &Generators,
) -> Result<PreparedRequest> {
    let raw_url = resolve_with(&request.url, ctx, generators)?;
    let mut url =
        Url::parse(&raw_url).map_err(|error| Error::InvalidUrl(format!("{raw_url}: {error}")))?;

    let mut headers = Vec::new();
    for header in request.headers.iter().filter(|header| header.enabled) {
        let key = resolve_with(&header.key, ctx, generators)?;
        let value = resolve_with(&header.value, ctx, generators)?;
        check_header(&key, &value)?;
        headers.push((key, value));
    }

    // Defaults come after the user's rows so an override always wins. The
    // comparison is case-insensitive, the way HTTP names are: `user-agent` and
    // `User-Agent` are the same header.
    for (key, value) in default_headers() {
        if !headers
            .iter()
            .any(|(existing, _)| existing.eq_ignore_ascii_case(&key))
        {
            headers.push((key, value));
        }
    }

    let mut query = Vec::new();
    for param in request.params.iter().filter(|param| param.enabled) {
        if param.key.trim().is_empty() {
            continue;
        }
        query.push((
            resolve_with(&param.key, ctx, generators)?,
            resolve_with(&param.value, ctx, generators)?,
        ));
    }

    apply_auth(&request.auth, ctx, generators, &mut headers, &mut query)?;

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
        body: prepare_body(&request.body, ctx, generators)?,
        timeout_ms: request.timeout_ms,
        follow_redirects: request.follow_redirects.unwrap_or(true),
        insecure: request.insecure,
    })
}

fn apply_auth(
    auth: &Auth,
    ctx: &VarContext,
    generators: &Generators,
    headers: &mut Vec<(String, String)>,
    query: &mut Vec<(String, String)>,
) -> Result<()> {
    match auth {
        Auth::None => {}
        Auth::Basic { username, password } => {
            let username = resolve_with(username, ctx, generators)?;
            let password = resolve_with(password, ctx, generators)?;
            let encoded = BASE64.encode(format!("{username}:{password}"));
            headers.push(("authorization".to_owned(), format!("Basic {encoded}")));
        }
        Auth::Bearer { token } => {
            headers.push((
                "authorization".to_owned(),
                format!("Bearer {}", resolve_with(token, ctx, generators)?),
            ));
        }
        Auth::ApiKey {
            name,
            value,
            location,
        } => {
            let name = resolve_with(name, ctx, generators)?;
            let value = resolve_with(value, ctx, generators)?;
            match location {
                ApiKeyLocation::Header => headers.push((name, value)),
                ApiKeyLocation::Query => query.push((name, value)),
            }
        }
        Auth::OAuth2 { access_token, .. } => {
            // Fetching a token is I/O, so it happens before prepare(). Without
            // one there is nothing to send, and a clear error beats an
            // unauthenticated request that fails server-side.
            let Some(token) = access_token else {
                return Err(Error::OAuth(
                    "an OAuth 2.0 access token is required; fetch one first".to_owned(),
                ));
            };
            headers.push((
                "authorization".to_owned(),
                format!("Bearer {}", resolve_with(token, ctx, generators)?),
            ));
        }
    }
    Ok(())
}

fn prepare_body(body: &Body, ctx: &VarContext, generators: &Generators) -> Result<PreparedBody> {
    Ok(match body {
        Body::None => PreparedBody::None,
        Body::Raw { mime, text } => PreparedBody::Bytes {
            mime: mime.clone(),
            data: resolve_with(text, ctx, generators)?.into_bytes(),
        },
        Body::Form(fields) => {
            let mut out = Vec::new();
            for field in fields.iter().filter(|field| field.enabled) {
                if field.key.trim().is_empty() {
                    continue;
                }
                out.push((
                    resolve_with(&field.key, ctx, generators)?,
                    resolve_with(&field.value, ctx, generators)?,
                ));
            }
            PreparedBody::Form(out)
        }
        Body::Multipart(parts) => {
            let mut out = Vec::new();
            for part in parts.iter().filter(|part| part.enabled) {
                let name = resolve_with(&part.name, ctx, generators)?;
                let data = match &part.file {
                    Some(path) => {
                        let path = resolve_with(path, ctx, generators)?;
                        PartData::File {
                            file_name: file_name_of(&path),
                            path,
                            mime: part.mime.clone(),
                        }
                    }
                    None => PartData::Text(resolve_with(&part.value, ctx, generators)?),
                };
                out.push(PreparedPart { name, data });
            }
            PreparedBody::Multipart(out)
        }
        Body::Binary { path } => PreparedBody::File(resolve_with(path, ctx, generators)?),
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
        assert!(prepared
            .headers
            .contains(&("authorization".to_owned(), "Basic dXNlcjpwYXNz".to_owned())));
    }

    #[test]
    fn defaults_fill_in_what_the_user_did_not_override() {
        let request = RequestDef::new(Method::Get, "https://example.com");
        let prepared = prepare(&request, &VarContext::new()).unwrap();

        for (key, value) in default_headers() {
            assert!(
                prepared
                    .headers
                    .iter()
                    .any(|(existing, actual)| existing == &key && actual == &value),
                "missing default {key}",
            );
        }
    }

    #[test]
    fn a_user_header_overrides_a_default_whatever_its_case() {
        let mut request = RequestDef::new(Method::Get, "https://example.com");
        request.headers.push(Kv::new("user-agent", "mine/1.0"));

        let prepared = prepare(&request, &VarContext::new()).unwrap();
        assert_eq!(
            prepared
                .headers
                .iter()
                .filter(|(key, _)| key.eq_ignore_ascii_case("user-agent"))
                .count(),
            1,
        );
        assert!(prepared
            .headers
            .contains(&("user-agent".to_owned(), "mine/1.0".to_owned())));
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

    #[test]
    fn an_oauth_token_becomes_a_bearer_header() {
        let mut request = RequestDef::new(Method::Get, "https://example.com");
        request.auth = Auth::OAuth2 {
            config: crate::oauth::OAuthConfig::default(),
            access_token: Some("{{token}}".to_owned()),
        };

        let ctx = VarContext::new().with_scope([("token", "abc")]);
        let prepared = prepare(&request, &ctx).unwrap();
        assert!(prepared
            .headers
            .contains(&("authorization".to_owned(), "Bearer abc".to_owned())));
    }

    #[test]
    fn oauth_without_a_token_is_a_prepare_time_error() {
        let mut request = RequestDef::new(Method::Get, "https://example.com");
        request.auth = Auth::OAuth2 {
            config: crate::oauth::OAuthConfig::default(),
            access_token: None,
        };

        let error = prepare(&request, &VarContext::new()).unwrap_err();
        assert!(matches!(error, Error::OAuth(_)));
        assert_eq!(
            error.to_string(),
            "oauth: an OAuth 2.0 access token is required; fetch one first"
        );
    }

    #[test]
    fn generator_in_the_url_is_resolved() {
        let request = RequestDef::new(Method::Get, "https://example.com/{{$uuid}}");

        let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let prepared = prepare_with(&request, &VarContext::new(), &Generators::at(now, 7)).unwrap();

        let id = prepared.url.path().trim_start_matches('/');
        assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 4);
    }
}
