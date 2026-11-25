//! OAuth 2.0 shapes: configuration, PKCE, authorization and token requests.
//!
//! Nothing here performs I/O: [`OAuthConfig`] builds the authorization URL and
//! token-request forms, and [`parse_token_response`] reads the reply. The
//! transport implements [`OAuthTransport`], so `prepare()` stays pure.

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::error::{Error, Result};

/// Which OAuth 2.0 flow a request uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthGrant {
    /// The browser opens the authorization URL and the app exchanges the code.
    #[default]
    AuthorizationCode,
    /// The app fetches a token directly; no browser, no redirect URI.
    ClientCredentials,
}

/// An OAuth 2.0 client's settings, persisted with the request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthConfig {
    #[serde(default)]
    pub grant: OAuthGrant,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    /// Only the authorization-code flow opens this URL.
    #[serde(default)]
    pub auth_url: String,
    pub token_url: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub redirect_uri: String,
    #[serde(default = "default_true")]
    pub pkce: bool,
}

impl Default for OAuthConfig {
    fn default() -> Self {
        Self {
            grant: OAuthGrant::default(),
            client_id: String::new(),
            client_secret: String::new(),
            auth_url: String::new(),
            token_url: String::new(),
            scopes: Vec::new(),
            redirect_uri: String::new(),
            pkce: true,
        }
    }
}

fn default_true() -> bool {
    true
}

/// A PKCE verifier and the S256 challenge derived from it.
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// A fresh verifier: 64 hex characters, inside RFC 7636's 43–128 range.
    pub fn generate() -> Self {
        let verifier = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        Self::from_verifier(&verifier)
    }

    pub fn from_verifier(verifier: &str) -> Self {
        Self {
            verifier: verifier.to_owned(),
            challenge: challenge_for(verifier),
        }
    }
}

/// `base64url(no padding)` of SHA-256, as RFC 7636 section 4.2 defines it.
fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// What the browser should open, plus the verifier the exchange will need.
#[derive(Debug, Clone)]
pub struct AuthorizationRequest {
    pub url: String,
    pub state: String,
    pub verifier: String,
}

/// A token endpoint call: a form, optionally with HTTP basic client auth.
#[derive(Debug, Clone)]
pub struct TokenRequest {
    pub token_url: String,
    pub form: Vec<(String, String)>,
    pub basic_auth: Option<(String, String)>,
}

impl TokenRequest {
    fn new(config: &OAuthConfig) -> Result<Self> {
        require("token URL", &config.token_url)?;
        require("client id", &config.client_id)?;
        Ok(Self {
            token_url: config.token_url.clone(),
            form: Vec::new(),
            basic_auth: None,
        })
    }

    /// A secret goes in the `Authorization: Basic` header; without one the
    /// client id travels in the form instead. RFC 6749 allows either, and a
    /// public client has no secret to send.
    fn apply_client_auth(mut self, config: &OAuthConfig) -> Self {
        if config.client_secret.is_empty() {
            self.form
                .push(("client_id".to_owned(), config.client_id.clone()));
        } else {
            self.basic_auth = Some((config.client_id.clone(), config.client_secret.clone()));
        }
        self
    }
}

impl OAuthConfig {
    /// Build the authorization URL and the state/PKCE values to keep until the
    /// redirect comes back.
    pub fn authorize(&self) -> Result<AuthorizationRequest> {
        require("authorization URL", &self.auth_url)?;
        require("client id", &self.client_id)?;
        require("redirect URI", &self.redirect_uri)?;
        let mut url = Url::parse(&self.auth_url)
            .map_err(|error| Error::OAuth(format!("invalid authorization URL: {error}")))?;

        let state = uuid::Uuid::new_v4().to_string();
        let mut verifier = String::new();
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("response_type", "code");
            pairs.append_pair("client_id", &self.client_id);
            pairs.append_pair("redirect_uri", &self.redirect_uri);
            if !self.scopes.is_empty() {
                pairs.append_pair("scope", &self.scopes.join(" "));
            }
            pairs.append_pair("state", &state);
            if self.pkce {
                let pkce = Pkce::generate();
                pairs.append_pair("code_challenge", &pkce.challenge);
                pairs.append_pair("code_challenge_method", "S256");
                verifier = pkce.verifier;
            }
        }

        Ok(AuthorizationRequest {
            url: url.to_string(),
            state,
            verifier,
        })
    }

    /// The form for a client-credentials grant. No browser is involved.
    pub fn client_credentials_request(&self) -> Result<TokenRequest> {
        let mut request = TokenRequest::new(self)?;
        request
            .form
            .push(("grant_type".to_owned(), "client_credentials".to_owned()));
        if !self.scopes.is_empty() {
            request
                .form
                .push(("scope".to_owned(), self.scopes.join(" ")));
        }
        Ok(request.apply_client_auth(self))
    }

    /// The form that turns an authorization code into a token.
    pub fn code_exchange_request(&self, code: &str, verifier: &str) -> Result<TokenRequest> {
        require("authorization code", code)?;
        require("redirect URI", &self.redirect_uri)?;
        let mut request = TokenRequest::new(self)?;
        request
            .form
            .push(("grant_type".to_owned(), "authorization_code".to_owned()));
        request.form.push(("code".to_owned(), code.to_owned()));
        request
            .form
            .push(("redirect_uri".to_owned(), self.redirect_uri.clone()));
        if self.pkce {
            require("PKCE verifier", verifier)?;
            request
                .form
                .push(("code_verifier".to_owned(), verifier.to_owned()));
        }
        Ok(request.apply_client_auth(self))
    }
}

fn require(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::OAuth(format!("a {name} is required")));
    }
    Ok(())
}

/// A token endpoint's success body. Every field but `access_token` is optional,
/// because servers disagree about what they return.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// Parse a token response. An OAuth error body (`{"error": "…"}`) becomes an
/// [`Error::OAuth`] with the server's description, not a serde message about a
/// missing field.
pub fn parse_token_response(body: &[u8]) -> Result<TokenResponse> {
    match serde_json::from_slice::<TokenResponse>(body) {
        Ok(token) => Ok(token),
        Err(parse_error) => {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) {
                if let Some(error) = value.get("error").and_then(|value| value.as_str()) {
                    let message = match value
                        .get("error_description")
                        .and_then(|value| value.as_str())
                    {
                        Some(description) => format!("{error}: {description}"),
                        None => error.to_owned(),
                    };
                    return Err(Error::OAuth(message));
                }
            }
            Err(Error::Json(parse_error))
        }
    }
}

/// The seam between `core` and whatever actually talks to the token endpoint.
/// Implementations live outside this crate; nothing in `prepare()` calls it.
#[async_trait]
pub trait OAuthTransport: Send + Sync {
    async fn fetch_token(&self, request: TokenRequest) -> Result<TokenResponse>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> OAuthConfig {
        OAuthConfig {
            client_id: "client-1".to_owned(),
            client_secret: "secret".to_owned(),
            auth_url: "https://auth.example.com/authorize".to_owned(),
            token_url: "https://auth.example.com/token".to_owned(),
            redirect_uri: "http://127.0.0.1:7777/callback".to_owned(),
            scopes: vec!["read".to_owned(), "write".to_owned()],
            ..OAuthConfig::default()
        }
    }

    fn query_of(url: &str) -> Vec<(String, String)> {
        Url::parse(url)
            .unwrap()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }

    fn value_of<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn form_value<'a>(request: &'a TokenRequest, name: &str) -> Option<&'a str> {
        request
            .form
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn rfc_7636_appendix_b_challenge_matches() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorize_carries_every_required_parameter() {
        let request = config().authorize().unwrap();
        let pairs = query_of(&request.url);

        assert_eq!(value_of(&pairs, "response_type"), Some("code"));
        assert_eq!(value_of(&pairs, "client_id"), Some("client-1"));
        assert_eq!(
            value_of(&pairs, "redirect_uri"),
            Some("http://127.0.0.1:7777/callback")
        );
        assert_eq!(value_of(&pairs, "scope"), Some("read write"));
        assert_eq!(value_of(&pairs, "state"), Some(request.state.as_str()));
        assert_eq!(value_of(&pairs, "code_challenge_method"), Some("S256"));

        let challenge = value_of(&pairs, "code_challenge").expect("a S256 challenge");
        assert_eq!(
            challenge,
            Pkce::from_verifier(&request.verifier).challenge,
            "the returned verifier must produce the URL's challenge"
        );
        assert!(!request.verifier.is_empty());
    }

    #[test]
    fn authorization_urls_differ_by_a_fresh_state() {
        let first = config().authorize().unwrap();
        let second = config().authorize().unwrap();
        assert_ne!(first.state, second.state);
        assert_ne!(first.verifier, second.verifier);
    }

    #[test]
    fn pkce_can_be_turned_off() {
        let mut config = config();
        config.pkce = false;

        let request = config.authorize().unwrap();
        let pairs = query_of(&request.url);
        assert!(value_of(&pairs, "code_challenge").is_none());
        assert!(value_of(&pairs, "code_challenge_method").is_none());
        assert!(request.verifier.is_empty());
        assert!(value_of(&pairs, "state").is_some());
    }

    #[test]
    fn authorize_requires_the_browser_facing_settings() {
        let mut missing = config();
        missing.auth_url.clear();
        assert!(matches!(missing.authorize(), Err(Error::OAuth(_))));

        let mut missing = config();
        missing.redirect_uri.clear();
        assert!(matches!(missing.authorize(), Err(Error::OAuth(_))));

        let mut missing = config();
        missing.client_id.clear();
        assert!(matches!(missing.authorize(), Err(Error::OAuth(_))));
    }

    #[test]
    fn client_credentials_form_uses_the_body_without_a_secret() {
        let mut config = config();
        config.client_secret.clear();

        let request = config.client_credentials_request().unwrap();
        assert_eq!(
            form_value(&request, "grant_type"),
            Some("client_credentials")
        );
        assert_eq!(form_value(&request, "scope"), Some("read write"));
        assert_eq!(form_value(&request, "client_id"), Some("client-1"));
        assert!(request.basic_auth.is_none());
    }

    #[test]
    fn client_credentials_form_uses_basic_auth_with_a_secret() {
        let request = config().client_credentials_request().unwrap();
        assert_eq!(
            form_value(&request, "grant_type"),
            Some("client_credentials")
        );
        assert_eq!(form_value(&request, "client_id"), None);
        assert_eq!(
            request.basic_auth,
            Some(("client-1".to_owned(), "secret".to_owned()))
        );
    }

    #[test]
    fn code_exchange_form_carries_the_code_and_verifier() {
        let request = config().code_exchange_request("the-code", "v").unwrap();
        assert_eq!(
            form_value(&request, "grant_type"),
            Some("authorization_code")
        );
        assert_eq!(form_value(&request, "code"), Some("the-code"));
        assert_eq!(
            form_value(&request, "redirect_uri"),
            Some("http://127.0.0.1:7777/callback")
        );
        assert_eq!(form_value(&request, "code_verifier"), Some("v"));
        assert!(request.basic_auth.is_some());
    }

    #[test]
    fn code_exchange_without_pkce_omits_the_verifier() {
        let mut config = config();
        config.pkce = false;

        let request = config.code_exchange_request("the-code", "").unwrap();
        assert_eq!(form_value(&request, "code_verifier"), None);
    }

    #[test]
    fn a_success_body_parses_every_optional_field() {
        let body = br#"{
            "access_token": "abc",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "r",
            "scope": "read"
        }"#;
        let token = parse_token_response(body).unwrap();
        assert_eq!(token.access_token, "abc");
        assert_eq!(token.token_type.as_deref(), Some("Bearer"));
        assert_eq!(token.expires_in, Some(3600));
        assert_eq!(token.refresh_token.as_deref(), Some("r"));
        assert_eq!(token.scope.as_deref(), Some("read"));
    }

    #[test]
    fn a_bare_access_token_is_enough() {
        let token = parse_token_response(br#"{"access_token":"abc"}"#).unwrap();
        assert_eq!(token.access_token, "abc");
        assert!(token.token_type.is_none());
    }

    #[test]
    fn an_error_body_becomes_an_oauth_error() {
        let body = br#"{"error":"invalid_grant","error_description":"the code has expired"}"#;
        let error = parse_token_response(body).unwrap_err();
        assert!(
            matches!(&error, Error::OAuth(message) if message == "invalid_grant: the code has expired")
        );
        assert_eq!(
            error.to_string(),
            "oauth: invalid_grant: the code has expired"
        );
    }

    #[test]
    fn nonsense_is_json_error_not_an_oauth_error() {
        let error = parse_token_response(b"not json").unwrap_err();
        assert!(matches!(error, Error::Json(_)));
    }
}
