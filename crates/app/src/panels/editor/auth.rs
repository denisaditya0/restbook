//! The Auth tab: none, basic, bearer, or an API key.

use gpui_kit::component::button::*;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, Selectable as _};
use gpui_kit::*;

use restbook_core::{ApiKeyLocation, Auth, OAuthConfig, OAuthGrant};

use super::{AuthKind, EditorEvent, RequestEditor, ALL_AUTH_KINDS, AUTH_LABELS};

impl RequestEditor {
    pub(crate) fn render_auth_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let metrics = self.metrics;
        let strip = h_flex()
            .gap(px(metrics.space_xs))
            .pb(px(metrics.space_sm))
            .children(
                AUTH_LABELS
                    .into_iter()
                    .zip(ALL_AUTH_KINDS)
                    .map(|(label, kind)| {
                        let current = kind == self.auth_kind;
                        // Keyed on the variant, not its label, so a relabel
                        // cannot re-key the control.
                        Button::new(format!("auth-kind-{kind:?}"))
                            .label(label)
                            .compact()
                            .ghost()
                            .selected(current)
                            .toggled(current)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.auth_kind = kind;
                                cx.notify();
                            }))
                            .into_any_element()
                    }),
            );

        let fields: Vec<AnyElement> = match self.auth_kind {
            AuthKind::None | AuthKind::Inherit => Vec::new(),
            AuthKind::Basic => vec![
                Input::new(&self.auth_a)
                    .id("auth-username")
                    .flex_1()
                    .into_any_element(),
                Input::new(&self.auth_b)
                    .id("auth-password")
                    .flex_1()
                    .into_any_element(),
            ],
            AuthKind::Bearer => vec![Input::new(&self.auth_a)
                .id("auth-token")
                .flex_1()
                .into_any_element()],
            AuthKind::ApiKey => vec![
                Input::new(&self.auth_a)
                    .id("auth-key-name")
                    .flex_1()
                    .into_any_element(),
                Input::new(&self.auth_b)
                    .id("auth-key-value")
                    .flex_1()
                    .into_any_element(),
            ],
            // OAuth has its own layout below, not the two shared fields.
            AuthKind::OAuth2 => Vec::new(),
        };

        let key_location = (self.auth_kind == AuthKind::ApiKey).then(|| {
            h_flex()
                .gap(px(metrics.space_sm))
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(self.palette.muted)
                        .child("Add to"),
                )
                .child(
                    Checkbox::new("api-key-location")
                        .label("query string")
                        .checked(self.api_key_in_query)
                        .on_change(cx.listener(|this, checked, _, cx| {
                            this.api_key_in_query = *checked;
                            cx.notify();
                        })),
                )
        });

        let hint = match self.auth_kind {
            AuthKind::Basic => {
                Some("Username and password, sent as an Authorization: Basic header.")
            }
            AuthKind::Bearer => Some("Token, sent as an Authorization: Bearer header."),
            AuthKind::ApiKey => Some(
                "Header name and key value, sent as a header unless \"query string\" is checked.",
            ),
            AuthKind::OAuth2 => None,
            AuthKind::None | AuthKind::Inherit => None,
        };

        let oauth = (self.auth_kind == AuthKind::OAuth2).then(|| self.render_oauth_fields(cx));

        v_flex()
            .gap(px(metrics.space_sm))
            .child(strip)
            .child(h_flex().gap(px(metrics.space_sm)).children(fields))
            .children(key_location)
            .children(oauth)
            .children(hint.map(|hint| div().text_xs().text_color(self.palette.muted).child(hint)))
            .into_any_element()
    }

    /// The OAuth 2.0 form: a flow selector, the client settings, and the
    /// fetch action. The token itself is not typed here; it arrives from the
    /// fetch (or from an imported request's in-memory token).
    fn render_oauth_fields(&self, cx: &mut Context<Self>) -> AnyElement {
        let metrics = self.metrics;
        let muted = self.palette.muted;
        let labelled = |label: &'static str, input: &Entity<InputState>, id: &'static str| {
            v_flex()
                .gap(px(metrics.space_xs))
                .flex_1()
                .child(div().text_xs().text_color(muted).child(label))
                .child(Input::new(input).id(id))
        };

        let grant = |kind: OAuthGrant, label: &'static str| {
            Button::new(format!("oauth-grant-{kind:?}"))
                .label(label)
                .compact()
                .ghost()
                .selected(self.oauth_grant == kind)
                .toggled(self.oauth_grant == kind)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.oauth_grant = kind;
                    cx.notify();
                }))
                .into_any_element()
        };

        let status = self
            .oauth_status
            .clone()
            .map(|status| div().text_xs().text_color(muted).child(status));

        v_flex()
            .gap(px(metrics.space_sm))
            .child(
                h_flex()
                    .gap(px(metrics.space_sm))
                    .items_center()
                    .child(div().text_xs().text_color(muted).child("Grant"))
                    .child(grant(OAuthGrant::ClientCredentials, "Client credentials"))
                    .child(grant(OAuthGrant::AuthorizationCode, "Authorization code")),
            )
            .child(h_flex().gap(px(metrics.space_sm)).children([
                labelled("Client id", &self.oauth_client_id, "oauth-client-id"),
                labelled(
                    "Client secret",
                    &self.oauth_client_secret,
                    "oauth-client-secret",
                ),
            ]))
            .child(h_flex().gap(px(metrics.space_sm)).children([
                labelled("Authorization URL", &self.oauth_auth_url, "oauth-auth-url"),
                labelled("Token URL", &self.oauth_token_url, "oauth-token-url"),
            ]))
            .child(h_flex().gap(px(metrics.space_sm)).children([
                labelled("Scopes", &self.oauth_scopes, "oauth-scopes"),
                labelled(
                    "Redirect URI",
                    &self.oauth_redirect_uri,
                    "oauth-redirect-uri",
                ),
            ]))
            .child(
                h_flex()
                    .gap(px(metrics.space_md))
                    .items_center()
                    .child(
                        Checkbox::new("oauth-pkce")
                            .label("PKCE")
                            .checked(self.oauth_pkce)
                            .on_change(cx.listener(|this, checked, _, cx| {
                                this.oauth_pkce = *checked;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("oauth-fetch-token")
                            .label("Fetch token")
                            .compact()
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(EditorEvent::FetchToken);
                            })),
                    ),
            )
            .children(status)
            .into_any_element()
    }

    /// The Auth tab's settings as an `OAuthConfig`. The workspace sends this to
    /// the transport, and `request()` embeds it in the `Auth`.
    pub(crate) fn oauth_config(&self, cx: &App) -> OAuthConfig {
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let scopes = value(&self.oauth_scopes)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        OAuthConfig {
            grant: self.oauth_grant,
            client_id: value(&self.oauth_client_id),
            client_secret: value(&self.oauth_client_secret),
            auth_url: value(&self.oauth_auth_url),
            token_url: value(&self.oauth_token_url),
            scopes,
            redirect_uri: value(&self.oauth_redirect_uri),
            pkce: self.oauth_pkce,
        }
    }

    /// Record the result of a fetch. Called by the workspace after the
    /// transport answers; no window is needed because the token is not a widget.
    pub(crate) fn set_oauth_token(
        &mut self,
        token: Option<String>,
        status: String,
        cx: &mut Context<Self>,
    ) {
        if token.is_some() {
            self.oauth_access_token = token;
        }
        self.oauth_status = Some(status);
        cx.notify();
    }

    /// Reduce the Auth tab's state to an `Auth`. The two shared fields cover
    /// the simple schemes; OAuth folds its own settings.
    pub(crate) fn auth(&self, cx: &App) -> Auth {
        if self.auth_kind == AuthKind::OAuth2 {
            return Auth::OAuth2 {
                config: self.oauth_config(cx),
                access_token: self.oauth_access_token.clone(),
            };
        }
        let a = self.auth_a.read(cx).value().to_string();
        let b = self.auth_b.read(cx).value().to_string();
        auth_from(self.auth_kind, self.api_key_in_query, &a, &b)
    }
}

pub(crate) fn auth_from(kind: AuthKind, api_key_in_query: bool, first: &str, second: &str) -> Auth {
    match kind {
        AuthKind::None => Auth::None,
        AuthKind::Inherit => Auth::Inherit,
        AuthKind::Basic => Auth::Basic {
            username: first.to_owned(),
            password: second.to_owned(),
        },
        AuthKind::Bearer => Auth::Bearer {
            token: first.to_owned(),
        },
        AuthKind::ApiKey => Auth::ApiKey {
            name: first.to_owned(),
            value: second.to_owned(),
            location: if api_key_in_query {
                ApiKeyLocation::Query
            } else {
                ApiKeyLocation::Header
            },
        },
        // Callers go through `RequestEditor::auth`, which folds the OAuth
        // widgets itself; a bare two-field form cannot express a flow.
        AuthKind::OAuth2 => Auth::OAuth2 {
            config: OAuthConfig::default(),
            access_token: None,
        },
    }
}

/// The widget state for a stored `Auth`: the scheme plus the two shared fields
/// and the API-key location flag. The inverse of [`auth_from`].
pub(crate) fn auth_fields(auth: &Auth) -> (AuthKind, String, String, bool) {
    match auth {
        Auth::None => (AuthKind::None, String::new(), String::new(), false),
        Auth::Inherit => (AuthKind::Inherit, String::new(), String::new(), false),
        Auth::Basic { username, password } => {
            (AuthKind::Basic, username.clone(), password.clone(), false)
        }
        Auth::Bearer { token } => (AuthKind::Bearer, token.clone(), String::new(), false),
        Auth::ApiKey {
            name,
            value,
            location,
        } => (
            AuthKind::ApiKey,
            name.clone(),
            value.clone(),
            *location == ApiKeyLocation::Query,
        ),
        Auth::OAuth2 { .. } => (AuthKind::OAuth2, String::new(), String::new(), false),
    }
}

#[cfg(test)]
mod tests {
    // Named imports, not `super::*`: the supermodule glob-imports gpui-kit,
    // which would bring its `#[test]` macro into scope and shadow the builtin.
    use super::{auth_fields, auth_from, AuthKind};
    // `Auth` has no `PartialEq`, so the tests destructure rather than compare.
    use restbook_core::{ApiKeyLocation, Auth};

    #[test]
    fn none_sends_nothing() {
        assert!(matches!(
            auth_from(AuthKind::None, false, "a", "b"),
            Auth::None
        ));
    }

    #[test]
    fn basic_takes_username_then_password() {
        match auth_from(AuthKind::Basic, false, "user", "pass") {
            Auth::Basic { username, password } => {
                assert_eq!(username, "user");
                assert_eq!(password, "pass");
            }
            other => panic!("expected Basic, got {other:?}"),
        }
    }

    #[test]
    fn bearer_uses_the_first_field_only() {
        match auth_from(AuthKind::Bearer, false, "token", "ignored") {
            Auth::Bearer { token } => assert_eq!(token, "token"),
            other => panic!("expected Bearer, got {other:?}"),
        }
    }

    #[test]
    fn api_key_location_follows_the_checkbox() {
        let location = |query| match auth_from(AuthKind::ApiKey, query, "X-Key", "secret") {
            Auth::ApiKey {
                name,
                value,
                location,
            } => {
                assert_eq!(name, "X-Key");
                assert_eq!(value, "secret");
                location
            }
            other => panic!("expected ApiKey, got {other:?}"),
        };

        assert!(matches!(location(false), ApiKeyLocation::Header));
        assert!(matches!(location(true), ApiKeyLocation::Query));
    }

    #[test]
    fn oauth_selects_its_own_scheme_and_leaves_the_shared_fields_empty() {
        let auth = Auth::OAuth2 {
            config: restbook_core::OAuthConfig {
                client_id: "client".to_owned(),
                token_url: "https://auth.test/token".to_owned(),
                ..restbook_core::OAuthConfig::default()
            },
            access_token: None,
        };
        let (kind, first, second, in_query) = auth_fields(&auth);
        assert_eq!(kind, AuthKind::OAuth2);
        assert!(first.is_empty());
        assert!(second.is_empty());
        assert!(!in_query);
    }
}
