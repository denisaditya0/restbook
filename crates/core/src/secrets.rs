//! Which values are credentials, and what a file puts in their place.
//!
//! One definition, used by both the folder workspace and [`crate::import`]: a
//! tracked workspace file redacts a secret because the value stays in
//! `restbook.local.json`, an export because the file leaves the machine.

use crate::model::{Auth, Body, Collection, CollectionItem, Environment, Kv, RequestDef};

/// What a secret value is replaced with. It is written literally so a reviewer
/// sees the shape of the request and knows a real value is needed locally.
pub const REDACTED: &str = "<restbook:redacted>";

/// One value that is a credential, plus the names a local workspace uses to
/// find it again.
///
/// `kind` is the source file (`"req"`, `"env"`, or a [`Kv`] family like
/// `"header"`), `owner` the collection id or environment name, and `row` the
/// auth field or key. The tuple must survive a rename: an id or name, not a path.
pub struct SecretSlot<'a> {
    pub kind: &'static str,
    pub owner: &'a str,
    pub row: &'a str,
    pub value: &'a mut String,
}

/// Visit every credential in `collection`, recursing into folders.
///
/// The owner is the *collection* id, not the request id: the collection is the
/// file that gets renamed and copied, so its id is the stable handle.
pub fn visit_collection_secrets(collection: &mut Collection, f: &mut impl FnMut(SecretSlot<'_>)) {
    let owner = collection.id.clone();
    visit_items(&mut collection.items, &owner, f);
}

/// Visit every `secret` row in `environment`.
pub fn visit_environment_secrets(
    environment: &mut Environment,
    f: &mut impl FnMut(SecretSlot<'_>),
) {
    visit_kvs(&mut environment.vars, "env", &environment.name, f);
}

/// Replace every credential in `collection` with [`REDACTED`].
pub fn redact_collection(collection: &mut Collection) {
    visit_collection_secrets(collection, &mut |slot| *slot.value = REDACTED.to_owned());
}

/// Replace every `secret` row in `environment` with [`REDACTED`].
pub fn redact_environment(environment: &mut Environment) {
    visit_environment_secrets(environment, &mut |slot| *slot.value = REDACTED.to_owned());
}

fn visit_items(items: &mut [CollectionItem], owner: &str, f: &mut impl FnMut(SecretSlot<'_>)) {
    for item in items.iter_mut() {
        match item {
            CollectionItem::Folder(folder) => visit_items(&mut folder.items, owner, f),
            CollectionItem::Request(saved) => visit_request(&mut saved.request, owner, f),
        }
    }
}

fn visit_request(request: &mut RequestDef, owner: &str, f: &mut impl FnMut(SecretSlot<'_>)) {
    visit_auth(&mut request.auth, owner, f);
    visit_kvs(&mut request.headers, "header", owner, f);
    visit_kvs(&mut request.params, "param", owner, f);
    if let Body::Form(rows) = &mut request.body {
        visit_kvs(rows, "form", owner, f);
    }
}

fn visit_auth(auth: &mut Auth, owner: &str, f: &mut impl FnMut(SecretSlot<'_>)) {
    let value = match auth {
        Auth::Basic { password, .. } => password,
        Auth::Bearer { token } => token,
        Auth::ApiKey { value, .. } => value,
        Auth::OAuth2 { config, .. } => &mut config.client_secret,
        Auth::None => return,
    };
    if value.is_empty() || is_template(value) {
        return;
    }
    f(SecretSlot {
        kind: "req",
        owner,
        row: "auth",
        value,
    });
}

fn visit_kvs(kvs: &mut [Kv], kind: &'static str, owner: &str, f: &mut impl FnMut(SecretSlot<'_>)) {
    for kv in kvs.iter_mut() {
        if !kv.secret || kv.value.is_empty() || is_template(&kv.value) {
            continue;
        }
        f(SecretSlot {
            kind,
            owner,
            row: &kv.key,
            value: &mut kv.value,
        });
    }
}

/// True when a value is already indirection — a whole-string `{{variable}}`.
/// Redacting one of these would break the sharing story it exists to support:
/// the token lives in a local variable, and the tracked file names it.
fn is_template(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("{{") && trimmed.ends_with("}}") && trimmed.len() > 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Method, SavedRequest};

    fn secret(key: &str, value: &str) -> Kv {
        Kv {
            key: key.to_owned(),
            value: value.to_owned(),
            enabled: true,
            secret: true,
        }
    }

    fn collection_with(request: RequestDef) -> Collection {
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

    fn slot_rows(collection: &mut Collection) -> Vec<String> {
        let mut rows = Vec::new();
        visit_collection_secrets(collection, &mut |slot| {
            rows.push(format!("{}:{}", slot.kind, slot.row));
        });
        rows
    }

    /// Both a plain row and a form field can hold a credential; the row family
    /// is part of the key, so a header and a body field with the same name do
    /// not collide.
    #[test]
    fn every_secret_holding_row_is_a_slot() {
        let mut request = RequestDef::new(Method::Post, "https://example.com");
        request.auth = Auth::Basic {
            username: "user".to_owned(),
            password: "pass".to_owned(),
        };
        request.headers = vec![secret("X-Key", "header-value"), Kv::new("Accept", "json")];
        request.params = vec![secret("token", "param-value")];
        request.body = Body::Form(vec![secret("password", "form-value")]);
        let mut collection = collection_with(request);

        assert_eq!(
            slot_rows(&mut collection),
            ["req:auth", "header:X-Key", "param:token", "form:password"]
        );

        redact_collection(&mut collection);
        let CollectionItem::Request(saved) = &collection.items[0] else {
            unreachable!();
        };
        assert_eq!(saved.request.headers[0].value, REDACTED);
        assert_eq!(
            saved.request.headers[1].value, "json",
            "a plain row is kept"
        );
        assert_eq!(saved.request.params[0].value, REDACTED);
        let Body::Form(rows) = &saved.request.body else {
            unreachable!();
        };
        assert_eq!(rows[0].value, REDACTED);
    }

    #[test]
    fn an_oauth_client_secret_is_a_slot() {
        let mut request = RequestDef::new(Method::Post, "https://example.com/token");
        request.auth = Auth::OAuth2 {
            config: crate::OAuthConfig {
                client_id: "client".to_owned(),
                client_secret: "oauth-secret".to_owned(),
                token_url: "https://example.com/token".to_owned(),
                ..crate::OAuthConfig::default()
            },
            access_token: None,
        };
        let mut collection = collection_with(request);

        assert_eq!(slot_rows(&mut collection), ["req:auth"]);
        redact_collection(&mut collection);
        let CollectionItem::Request(saved) = &collection.items[0] else {
            unreachable!();
        };
        let Auth::OAuth2 { config, .. } = &saved.request.auth else {
            unreachable!();
        };
        assert_eq!(config.client_secret, REDACTED);
    }

    /// A `{{var}}` is indirection, not a credential: the token lives in a local
    /// variable and the file names it.
    #[test]
    fn a_variable_reference_is_not_a_slot() {
        let mut request = RequestDef::new(Method::Get, "https://example.com");
        request.auth = Auth::Bearer {
            token: "{{TOKEN}}".to_owned(),
        };
        request.headers = vec![secret("X-Key", "{{KEY}}")];
        let mut collection = collection_with(request);

        assert!(slot_rows(&mut collection).is_empty());
    }

    #[test]
    fn a_secret_row_with_no_value_is_not_a_slot() {
        let mut environment = Environment {
            name: "dev".to_owned(),
            vars: vec![secret("TOKEN", ""), Kv::new("HOST", "https://example.com")],
        };
        let mut rows = Vec::new();
        visit_environment_secrets(&mut environment, &mut |slot| {
            rows.push(slot.row.to_owned());
        });
        assert!(rows.is_empty());
    }
}
