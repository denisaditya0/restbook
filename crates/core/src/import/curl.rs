//! curl command-line import.
//!
//! Paste a `curl` line and the editor rebuilds the request: URL, verb, headers,
//! body, form parts, and basic auth. The parser is a leaf (a `str` to a
//! [`RequestDef`]), so it is unit-tested without an `App`. Copied-command noise
//! (`--location`, `-s`, `-i`, …) is ignored, but an unknown option is an error.

use crate::model::{ApiKeyLocation, Auth, Body, FormPart, Kv, Method, RequestDef};

/// Turn a `curl` command line into a request.
///
/// The input may be a full `curl …` line or a bare argument list, and may use
/// single or double quotes and backslash-newline continuations, the way a
/// terminal or a "copy as cURL" button prints it.
pub fn parse(input: &str) -> Result<RequestDef, String> {
    let mut tokens = tokenize(input)?;
    if tokens.first().is_some_and(|token| is_curl_program(token)) {
        tokens.remove(0);
    }

    let mut builder = Builder::default();
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].clone();
        index += 1;

        if token == "--" {
            while index < tokens.len() {
                builder.take_url(&tokens[index]);
                index += 1;
            }
            break;
        }

        if let Some(long) = token.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (long, None),
            };
            builder.long_option(name, inline, &tokens, &mut index)?;
        } else if token.starts_with('-') && token.len() > 1 {
            builder.short_option(&token[1..], &tokens, &mut index)?;
        } else {
            builder.take_url(&token);
        }
    }

    builder.finish()
}

/// Render a request as a `curl` command line, the inverse of [`parse`].
///
/// The command is single-quoted for a POSIX shell; an embedded `'` closes and
/// reopens the quote, which is what a terminal paste needs. Values are emitted
/// as written, so `{{variables}}` survive a round trip and resolve when the
/// pasted command runs in a shell that defines them.
pub fn to_curl(request: &RequestDef) -> String {
    let mut parts = vec!["curl".to_owned()];

    if request.method != Method::Get {
        parts.push("-X".to_owned());
        parts.push(request.method.as_str().to_owned());
    }

    let mut url = request.url.trim().to_owned();
    let mut query: Vec<(String, String)> = request
        .params
        .iter()
        .filter(|param| param.enabled && !param.key.trim().is_empty())
        .map(|param| (param.key.clone(), param.value.clone()))
        .collect();
    if let Auth::ApiKey {
        name,
        value,
        location: ApiKeyLocation::Query,
    } = &request.auth
    {
        query.push((name.clone(), value.clone()));
    }
    if !query.is_empty() {
        let pairs: Vec<String> = query
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&pairs.join("&"));
    }
    parts.push(quote(&url));

    let mut has_content_type = false;
    for header in request.headers.iter().filter(|header| header.enabled) {
        if header.key.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        parts.push("-H".to_owned());
        parts.push(quote(&format!("{}: {}", header.key, header.value)));
    }

    match &request.auth {
        Auth::Basic { username, password } => {
            parts.push("-u".to_owned());
            parts.push(quote(&format!("{username}:{password}")));
        }
        Auth::Bearer { token } => {
            parts.push("-H".to_owned());
            parts.push(quote(&format!("Authorization: Bearer {token}")));
        }
        Auth::ApiKey {
            name,
            value,
            location: ApiKeyLocation::Header,
        } => {
            parts.push("-H".to_owned());
            parts.push(quote(&format!("{name}: {value}")));
        }
        Auth::OAuth2 {
            access_token: Some(token),
            ..
        } => {
            parts.push("-H".to_owned());
            parts.push(quote(&format!("Authorization: Bearer {token}")));
        }
        Auth::None
        | Auth::Inherit
        | Auth::OAuth2 {
            access_token: None, ..
        }
        | Auth::ApiKey {
            location: ApiKeyLocation::Query,
            ..
        } => {}
    }

    match &request.body {
        Body::None => {}
        Body::Raw { mime, text } => {
            if !has_content_type && !mime.trim().is_empty() {
                parts.push("-H".to_owned());
                parts.push(quote(&format!("Content-Type: {mime}")));
            }
            parts.push("--data-raw".to_owned());
            parts.push(quote(text));
        }
        Body::Form(fields) => {
            for field in fields
                .iter()
                .filter(|field| field.enabled && !field.key.trim().is_empty())
            {
                parts.push("--data".to_owned());
                parts.push(quote(&format!("{}={}", field.key, field.value)));
            }
        }
        Body::Multipart(form_parts) => {
            for part in form_parts.iter().filter(|part| part.enabled) {
                let value = match &part.file {
                    Some(path) => format!("{}@{}", part.name, path),
                    None => format!("{}={}", part.name, part.value),
                };
                parts.push("-F".to_owned());
                parts.push(quote(&value));
            }
        }
        Body::Binary { path } => {
            parts.push("--data-binary".to_owned());
            parts.push(quote(&format!("@{path}")));
        }
    }

    if request.insecure {
        parts.push("-k".to_owned());
    }
    if request.follow_redirects.unwrap_or(true) {
        parts.push("-L".to_owned());
    }
    if let Some(ms) = request.timeout_ms.filter(|ms| *ms > 0) {
        parts.push("--max-time".to_owned());
        parts.push(format!("{:.3}", ms as f64 / 1000.0));
    }

    parts.join(" ")
}

/// A single-quoted shell argument. Everything is quoted: a bare `curl` command
/// pasted into a terminal must not lose a `&`, a space, or a `$`.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The command name may be a bare `curl`, a path, or `curl.exe` on Windows.
fn is_curl_program(token: &str) -> bool {
    let name = token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .to_ascii_lowercase();
    name == "curl" || name == "curl.exe"
}

/// Split a command line into arguments, honoring quotes and continuations.
fn tokenize(input: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                // A backslash-newline joins the line and is not an argument
                // character, so it does not start a token by itself.
                Some('\n') => {}
                Some('\r') => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                }
                Some(escaped) => {
                    current.push(escaped);
                    in_token = true;
                }
                None => {
                    current.push('\\');
                    in_token = true;
                }
            },
            '\'' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(each) => current.push(each),
                        None => return Err("unterminated single quote".to_owned()),
                    }
                }
            }
            '"' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('\n') => {}
                            // Inside double quotes only these characters are
                            // escapable; anywhere else the backslash stays,
                            // so a Windows path survives the round trip.
                            Some(escaped @ ('"' | '\\' | '$' | '`')) => current.push(escaped),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => return Err("unterminated double quote".to_owned()),
                        },
                        Some(each) => current.push(each),
                        None => return Err("unterminated double quote".to_owned()),
                    }
                }
            }
            each if each.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            each => {
                current.push(each);
                in_token = true;
            }
        }
    }

    if in_token {
        tokens.push(current);
    }
    Ok(tokens)
}

/// The request being assembled, flag by flag.
#[derive(Default)]
struct Builder {
    url: Option<String>,
    method: Option<Method>,
    headers: Vec<Kv>,
    data: Vec<String>,
    json: Option<String>,
    forms: Vec<FormPart>,
    auth: Auth,
    insecure: bool,
    get: bool,
    head: bool,
}

impl Builder {
    fn long_option(
        &mut self,
        name: &str,
        inline: Option<String>,
        tokens: &[String],
        index: &mut usize,
    ) -> Result<(), String> {
        match name {
            "url" => {
                let value = self.required(inline, tokens, index, "--url")?;
                self.url = Some(value);
            }
            "request" => {
                let value = self.required(inline, tokens, index, "--request")?;
                self.method = Some(parse_method(&value)?);
            }
            "header" => {
                let value = self.required(inline, tokens, index, "--header")?;
                self.headers.push(parse_header(&value));
            }
            "data" | "data-raw" | "data-binary" | "data-ascii" => {
                let value = self.required(inline, tokens, index, "--data")?;
                self.data.push(value);
            }
            "json" => {
                let value = self.required(inline, tokens, index, "--json")?;
                self.json = Some(value);
            }
            "user" => {
                let value = self.required(inline, tokens, index, "--user")?;
                self.auth = parse_basic(&value);
            }
            "form" => {
                let value = self.required(inline, tokens, index, "--form")?;
                self.forms.push(parse_form(&value));
            }
            "insecure" => self.insecure = true,
            "get" => self.get = true,
            "head" => self.head = true,
            "location" | "compressed" | "silent" | "show-error" | "include" | "verbose" => {}
            _ => return Err(format!("unsupported curl option --{name}")),
        }
        Ok(())
    }

    fn short_option(
        &mut self,
        short: &str,
        tokens: &[String],
        index: &mut usize,
    ) -> Result<(), String> {
        let mut chars = short.chars();
        let option = chars
            .next()
            .expect("an empty short option is skipped above");
        let rest: String = chars.collect();
        let inline = (!rest.is_empty()).then_some(rest);

        match option {
            'X' => {
                let value = self.required(inline, tokens, index, "-X")?;
                self.method = Some(parse_method(&value)?);
            }
            'H' => {
                let value = self.required(inline, tokens, index, "-H")?;
                self.headers.push(parse_header(&value));
            }
            'd' => {
                let value = self.required(inline, tokens, index, "-d")?;
                self.data.push(value);
            }
            'u' => {
                let value = self.required(inline, tokens, index, "-u")?;
                self.auth = parse_basic(&value);
            }
            'F' => {
                let value = self.required(inline, tokens, index, "-F")?;
                self.forms.push(parse_form(&value));
            }
            'k' => self.insecure = true,
            'G' => self.get = true,
            'I' => self.head = true,
            'L' | 's' | 'i' | 'v' => {}
            _ => return Err(format!("unsupported curl option -{option}")),
        }
        Ok(())
    }

    /// Read the option's value: the inline `--option=value` half, or the next
    /// token.
    fn required(
        &self,
        inline: Option<String>,
        tokens: &[String],
        index: &mut usize,
        option: &str,
    ) -> Result<String, String> {
        if let Some(value) = inline {
            return Ok(value);
        }
        let value = tokens
            .get(*index)
            .cloned()
            .ok_or_else(|| format!("option {option} needs a value"))?;
        *index += 1;
        Ok(value)
    }

    /// The first positional argument is the URL; a second one is another
    /// request curl would also send, which this app cannot represent.
    fn take_url(&mut self, token: &str) {
        if self.url.is_none() {
            self.url = Some(token.to_owned());
        }
    }

    fn finish(mut self) -> Result<RequestDef, String> {
        let url = self
            .url
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| "missing URL".to_owned())?;

        let mut params = Vec::new();
        let mut body = Body::None;

        if self.get {
            // `-G` turns the payload into a query string and sends no body.
            params = parse_params(&self.data.join("&"));
        } else if let Some(json) = self.json {
            body = Body::Raw {
                mime: "application/json".to_owned(),
                text: json,
            };
        } else if !self.forms.is_empty() {
            body = Body::Multipart(self.forms);
        } else if !self.data.is_empty() {
            let text = self.data.join("&");
            let mime = content_type(&self.headers)
                .unwrap_or_else(|| "application/x-www-form-urlencoded".to_owned());
            body = Body::Raw { mime, text };
            if content_type(&self.headers).is_none() {
                self.headers
                    .push(Kv::new("content-type", "application/x-www-form-urlencoded"));
            }
        }

        let method = match self.method {
            Some(method) => method,
            None if self.head => Method::Head,
            None if !matches!(body, Body::None) => Method::Post,
            None => Method::Get,
        };

        let mut request = RequestDef::new(method, url);
        request.params = params;
        request.headers = self.headers;
        request.body = body;
        request.auth = self.auth;
        request.insecure = self.insecure;
        Ok(request)
    }
}

fn parse_method(value: &str) -> Result<Method, String> {
    Method::parse(value).ok_or_else(|| format!("unsupported method {value}"))
}

/// `Name: value`, with curl's `Name;` spelling for a header with no value.
fn parse_header(token: &str) -> Kv {
    let (key, value) = match token.split_once(':') {
        Some((key, value)) => (key.trim().to_owned(), value.trim().to_owned()),
        None => (token.trim().to_owned(), String::new()),
    };
    let key = if value.is_empty() {
        key.strip_suffix(';').unwrap_or(&key).trim().to_owned()
    } else {
        key
    };
    Kv::new(key, value)
}

/// `user:pass` as basic auth; a missing colon means an empty password.
fn parse_basic(token: &str) -> Auth {
    let (username, password) = match token.split_once(':') {
        Some((username, password)) => (username.to_owned(), password.to_owned()),
        None => (token.to_owned(), String::new()),
    };
    Auth::Basic { username, password }
}

/// `name=value` or `name=@path` as a multipart part.
fn parse_form(token: &str) -> FormPart {
    let (name, value) = match token.split_once('=') {
        Some((name, value)) => (name.to_owned(), value),
        None => (token.to_owned(), ""),
    };
    match value.strip_prefix('@') {
        Some(path) => FormPart {
            name,
            value: String::new(),
            file: Some(path.to_owned()),
            mime: None,
            enabled: true,
        },
        None => FormPart {
            name,
            value: value.to_owned(),
            file: None,
            mime: None,
            enabled: true,
        },
    }
}

/// Split a `-G` payload into query parameters.
fn parse_params(data: &str) -> Vec<Kv> {
    data.split('&')
        .filter(|segment| !segment.is_empty())
        .map(|segment| match segment.split_once('=') {
            Some((key, value)) => Kv::new(key, value),
            None => Kv::new(segment, ""),
        })
        .collect()
}

fn content_type(headers: &[Kv]) -> Option<String> {
    headers
        .iter()
        .find(|header| header.key.eq_ignore_ascii_case("content-type"))
        .map(|header| header.value.clone())
}

#[cfg(test)]
mod tests {
    use super::{parse, to_curl};
    use crate::model::{Auth, Body, Method};

    #[test]
    fn a_request_round_trips_through_curl() {
        let request = parse(
            "curl -X POST 'https://example.com/items?a=1' -H 'X-Trace: abc' -H 'Content-Type: application/json' --data-raw '{\"name\":\"Ada\"}'",
        )
        .expect("the command should parse");

        let again = parse(&to_curl(&request)).expect("the exported command should parse");
        assert_eq!(again.method, Method::Post);
        assert_eq!(again.url, request.url);
        assert_eq!(again.headers.len(), request.headers.len());
        match again.body {
            Body::Raw { mime, text } => {
                assert_eq!(mime, "application/json");
                assert_eq!(text, "{\"name\":\"Ada\"}");
            }
            other => panic!("expected a raw body, got {other:?}"),
        }
    }

    #[test]
    fn an_embedded_quote_survives_the_shell_quoting() {
        let mut request = crate::RequestDef::new(Method::Post, "https://example.com/");
        request.body = Body::Raw {
            mime: "text/plain".to_owned(),
            text: "it's fine".to_owned(),
        };

        let command = to_curl(&request);
        assert!(command.contains("'it'\\''s fine'"));
        let again = parse(&command).expect("the exported command should parse");
        match again.body {
            Body::Raw { text, .. } => assert_eq!(text, "it's fine"),
            other => panic!("expected a raw body, got {other:?}"),
        }
    }

    #[test]
    fn basic_auth_is_exported_as_dash_u() {
        let mut request = crate::RequestDef::new(Method::Get, "https://example.com/");
        request.auth = Auth::Basic {
            username: "user".to_owned(),
            password: "pass".to_owned(),
        };

        let command = to_curl(&request);
        assert!(command.contains("-u 'user:pass'"));
        let again = parse(&command).expect("the exported command should parse");
        assert!(matches!(again.auth, Auth::Basic { .. }));
    }

    #[test]
    fn quoted_headers_are_split_at_the_first_colon() {
        let request = parse(
            "curl -X POST 'https://example.com/api' -H 'Content-Type: application/json' -H \"X-Trace: a:b\"",
        )
        .expect("the command should parse");

        assert_eq!(request.method, Method::Post);
        assert_eq!(request.url, "https://example.com/api");
        assert_eq!(request.headers.len(), 2);
        assert_eq!(request.headers[0].key, "Content-Type");
        assert_eq!(request.headers[0].value, "application/json");
        assert_eq!(request.headers[1].key, "X-Trace");
        assert_eq!(request.headers[1].value, "a:b");
    }

    #[test]
    fn backslash_newlines_join_the_command() {
        let request = parse(
            "curl \\\n  -X PUT \\\n  https://example.com/items/1 \\\n  -H 'Accept: text/plain'",
        )
        .expect("the continued command should parse");

        assert_eq!(request.method, Method::Put);
        assert_eq!(request.url, "https://example.com/items/1");
        assert_eq!(request.headers[0].value, "text/plain");
    }

    #[test]
    fn data_joins_with_ampersands_and_adds_a_form_content_type() {
        let request = parse("curl https://example.com/login -d 'user=me' -d 'pass=secret'")
            .expect("the command should parse");

        assert_eq!(request.method, Method::Post);
        let Body::Raw { mime, text } = &request.body else {
            panic!("expected a raw body, got {:?}", request.body);
        };
        assert_eq!(text, "user=me&pass=secret");
        assert_eq!(mime, "application/x-www-form-urlencoded");
        assert!(request
            .headers
            .iter()
            .any(|header| header.key.eq_ignore_ascii_case("content-type")
                && header.value == "application/x-www-form-urlencoded"));
    }

    #[test]
    fn a_user_supplied_content_type_is_not_duplicated() {
        let request =
            parse("curl https://example.com/json -H 'content-type: text/plain' -d '{\"a\":1}'")
                .expect("the command should parse");

        assert_eq!(
            request
                .headers
                .iter()
                .filter(|header| header.key.eq_ignore_ascii_case("content-type"))
                .count(),
            1
        );
    }

    #[test]
    fn user_and_password_become_basic_auth() {
        let request =
            parse("curl -u alice:s3cret https://example.com").expect("the command should parse");

        assert!(matches!(
            request.auth,
            Auth::Basic { username, password } if username == "alice" && password == "s3cret"
        ));

        let request = parse("curl -u alice https://example.com").expect("the command should parse");
        assert!(matches!(
            request.auth,
            Auth::Basic { username, password } if username == "alice" && password.is_empty()
        ));
    }

    #[test]
    fn a_form_part_with_at_is_a_file_upload() {
        let request =
            parse("curl https://example.com/upload -F 'name=value' -F 'file=@/tmp/report.pdf'")
                .expect("the command should parse");

        let Body::Multipart(parts) = &request.body else {
            panic!("expected a multipart body, got {:?}", request.body);
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "name");
        assert_eq!(parts[0].value, "value");
        assert!(parts[0].file.is_none());
        assert_eq!(parts[1].name, "file");
        assert_eq!(parts[1].file.as_deref(), Some("/tmp/report.pdf"));
    }

    #[test]
    fn json_sets_a_raw_json_body() {
        let request = parse("curl --json '{\"ok\":true}' https://example.com")
            .expect("the command should parse");

        assert_eq!(request.method, Method::Post);
        let Body::Raw { mime, text } = &request.body else {
            panic!("expected a raw body, got {:?}", request.body);
        };
        assert_eq!(mime, "application/json");
        assert_eq!(text, "{\"ok\":true}");
    }

    #[test]
    fn get_moves_data_into_params() {
        let request = parse("curl -G https://example.com/search -d 'q=rust' -d 'page=2'")
            .expect("the command should parse");

        assert_eq!(request.method, Method::Get);
        assert!(matches!(request.body, Body::None));
        assert_eq!(request.params.len(), 2);
        assert_eq!(request.params[0].key, "q");
        assert_eq!(request.params[1].value, "2");
    }

    #[test]
    fn insecure_and_head_flags_are_kept() {
        let request = parse("curl -k -I https://example.com").expect("the command should parse");

        assert!(request.insecure);
        assert_eq!(request.method, Method::Head);
    }

    #[test]
    fn a_header_without_a_value_drops_the_trailing_semicolon() {
        let request =
            parse("curl https://example.com -H 'X-Empty;'").expect("the command should parse");

        assert_eq!(request.headers[0].key, "X-Empty");
        assert_eq!(request.headers[0].value, "");
    }

    #[test]
    fn a_double_quoted_windows_path_keeps_its_backslashes() {
        let request = parse("curl -F \"file=@C:\\Users\\me\\a.txt\" https://example.com")
            .expect("the command should parse");

        let Body::Multipart(parts) = &request.body else {
            panic!("expected a multipart body, got {:?}", request.body);
        };
        assert_eq!(parts[0].file.as_deref(), Some("C:\\Users\\me\\a.txt"));
    }

    #[test]
    fn an_unknown_option_is_an_error() {
        let error = parse("curl --frobnicate https://example.com")
            .expect_err("an unsupported option should not be ignored");

        assert_eq!(error, "unsupported curl option --frobnicate");
    }

    #[test]
    fn a_command_without_a_url_is_an_error() {
        let error = parse("curl -X POST -d 'a=1'").expect_err("a URL is required");

        assert_eq!(error, "missing URL");
    }

    #[test]
    fn an_oauth_token_is_exported_as_a_bearer_header() {
        let mut request = crate::RequestDef::new(Method::Get, "https://example.com/");
        request.auth = Auth::OAuth2 {
            config: crate::oauth::OAuthConfig::default(),
            access_token: Some("abc".to_owned()),
        };

        let command = to_curl(&request);
        assert!(command.contains("-H 'Authorization: Bearer abc'"));
    }

    #[test]
    fn an_oauth_request_without_a_token_exports_no_auth_header() {
        let mut request = crate::RequestDef::new(Method::Get, "https://example.com/");
        request.auth = Auth::OAuth2 {
            config: crate::oauth::OAuthConfig::default(),
            access_token: None,
        };

        assert!(!to_curl(&request).contains("Authorization"));
    }
}
