//! Environments, globals, and the variable scopes a send resolves against.
//! One `Variables` per window owns the environment and global rows, so the
//! store is written about them in exactly one place. Every mutation is
//! store-first — persist, then mirror the store's answer — so the UI can never
//! show a value the store rejected. Loading never panics: an unreadable store
//! is logged and treated as empty, so the window still opens.

use std::sync::Arc;

use gpui_kit::{Context, EventEmitter};

use restbook_core::{
    Auth, Body, Environment, Kv, RequestDef, ResolvedVar, ScopedVars, Store, VarScope,
};

use crate::notice::Notice;

pub(crate) struct Variables {
    store: Arc<dyn Store>,
    environments: Vec<Environment>,
    globals: Vec<Kv>,
    /// The variable rows of the collection the active tab belongs to. The
    /// canvas resolves the owner and sets this; a draft or an orphan has none.
    collection: Vec<Kv>,
    /// The name of the environment the send path resolves against, if any.
    active: Option<String>,
}

/// A write the store refused. `Variables` has no window and no toast of its
/// own, so a failure is emitted for the workspace, which owns the one flash
/// every pane's notice reaches.
impl EventEmitter<Notice> for Variables {}

impl Variables {
    /// Read the store once. `_cx` is unused today; it keeps the constructor in
    /// the shape `cx.new(|cx| Variables::new(store, cx))` expects.
    pub(crate) fn new(store: Arc<dyn Store>, _cx: &mut Context<Self>) -> Self {
        let mut variables = Self {
            store,
            environments: Vec::new(),
            globals: Vec::new(),
            collection: Vec::new(),
            active: None,
        };
        variables.load();
        variables
    }

    fn load(&mut self) {
        self.environments = self.store.environments().unwrap_or_else(|error| {
            log::error!("failed to load environments: {error}");
            Vec::new()
        });
        self.globals = self.store.globals().unwrap_or_else(|error| {
            log::error!("failed to load globals: {error}");
            Vec::new()
        });
        self.active = self.store.active_environment().unwrap_or_else(|error| {
            log::error!("failed to load the active environment: {error}");
            None
        });
    }

    /// Re-read after another writer touched the store, so an imported
    /// environment shows up without reopening the window.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self.load();
        cx.notify();
    }

    pub(crate) fn environments(&self) -> &[Environment] {
        &self.environments
    }

    pub(crate) fn globals(&self) -> &[Kv] {
        &self.globals
    }

    pub(crate) fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// Replace the collection-scope rows, which the canvas resolved from the
    /// active tab's node. Called on a tab switch and after a collection edit,
    /// so a send and the inspector agree on what the Collection tier holds.
    pub(crate) fn set_collection(&mut self, vars: Vec<Kv>, cx: &mut Context<Self>) {
        if self.collection == vars {
            return;
        }
        self.collection = vars;
        cx.notify();
    }

    pub(crate) fn set_active(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        if let Err(error) = self.store.set_active_environment(name.as_deref()) {
            log::error!("failed to set the active environment: {error}");
            return;
        }
        self.active = name;
        cx.notify();
    }

    pub(crate) fn save_environment(&mut self, environment: Environment, cx: &mut Context<Self>) {
        if let Err(error) = self.store.save_environment(&environment) {
            log::error!("failed to save environment: {error}");
            cx.emit(Notice::Failed(error.to_string().into()));
            return;
        }
        match self
            .environments
            .iter_mut()
            .find(|each| each.name == environment.name)
        {
            Some(existing) => *existing = environment,
            None => self.environments.push(environment),
        }
        cx.notify();
    }

    pub(crate) fn delete_environment(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Err(error) = self.store.delete_environment(name) {
            log::error!("failed to delete environment: {error}");
            cx.emit(Notice::Failed(error.to_string().into()));
            return;
        }
        self.environments.retain(|each| each.name != name);

        // An active environment that no longer exists would resolve to nothing,
        // so the selection is cleared with it.
        if self.active.as_deref() == Some(name) {
            self.active = None;
            if let Err(error) = self.store.set_active_environment(None) {
                log::error!("failed to clear the active environment: {error}");
            }
        }
        cx.notify();
    }

    pub(crate) fn save_globals(&mut self, vars: Vec<Kv>, cx: &mut Context<Self>) {
        if let Err(error) = self.store.save_globals(&vars) {
            log::error!("failed to save globals: {error}");
            cx.emit(Notice::Failed(error.to_string().into()));
            return;
        }
        self.globals = vars;
        cx.notify();
    }

    /// The scopes a request resolves against. `_request` has no producer yet: a
    /// `RequestDef` carries no variables, so only Globals, the active
    /// collection, and the active environment are filled.
    pub(crate) fn scoped(&self, _request: &RequestDef) -> ScopedVars {
        resolve_scopes(&self.globals, &self.collection, self.active_environment())
    }

    /// The names a `{{name}}` can resolve to right now, winner per name, for
    /// the completion popup. The collection tier is included because the canvas
    /// sets it for the active tab; the request tier has no producer.
    pub(crate) fn scoped_names(&self) -> Vec<ResolvedVar> {
        resolve_scopes(&self.globals, &self.collection, self.active_environment()).names()
    }

    fn active_environment(&self) -> Option<&Environment> {
        let active = self.active.as_deref()?;
        self.environments.iter().find(|each| each.name == active)
    }
}

/// Globals, the active collection, then the active environment. Precedence is
/// decided by `vars.rs`; this only fills each tier the canvas can name.
fn resolve_scopes(
    globals: &[Kv],
    collection: &[Kv],
    environment: Option<&Environment>,
) -> ScopedVars {
    let mut scoped = ScopedVars::new();
    scoped.set_scope(VarScope::Global, globals.iter().cloned());
    scoped.set_scope(VarScope::Collection, collection.iter().cloned());
    if let Some(environment) = environment {
        scoped.set_scope(VarScope::Environment, environment.vars.iter().cloned());
    }
    scoped
}

/// Whether the row that won `name` in `scope` is marked secret, so the
/// inspector and the completion popup can mask it instead of printing the
/// value.
pub(crate) fn secret_of(variables: &Variables, name: &str, scope: Option<VarScope>) -> bool {
    let found = match scope {
        Some(VarScope::Environment) => variables
            .active()
            .and_then(|active| {
                variables
                    .environments()
                    .iter()
                    .find(|environment| environment.name == active)
            })
            .and_then(|environment| {
                environment
                    .vars
                    .iter()
                    .find(|kv| kv.enabled && kv.key == name)
            }),
        Some(VarScope::Collection) => variables
            .collection
            .iter()
            .find(|kv| kv.enabled && kv.key == name),
        Some(VarScope::Global) => variables
            .globals()
            .iter()
            .find(|kv| kv.enabled && kv.key == name),
        _ => None,
    };

    found.is_some_and(|kv| kv.secret)
}

/// Every string in a request that can carry a `{{name}}`, in the order the
/// inspector walks them. Disabled rows are skipped, matching resolution.
pub(crate) fn reference_fields(request: &RequestDef) -> Vec<String> {
    let mut fields = vec![request.url.clone()];

    for kv in request.params.iter().filter(|kv| kv.enabled) {
        fields.push(kv.key.clone());
        fields.push(kv.value.clone());
    }
    for kv in request.headers.iter().filter(|kv| kv.enabled) {
        fields.push(kv.key.clone());
        fields.push(kv.value.clone());
    }
    if let Body::Raw { text, .. } = &request.body {
        fields.push(text.clone());
    }
    match &request.auth {
        Auth::Basic { username, password } => {
            fields.push(username.clone());
            fields.push(password.clone());
        }
        Auth::Bearer { token } => fields.push(token.clone()),
        Auth::ApiKey { name, value, .. } => {
            fields.push(name.clone());
            fields.push(value.clone());
        }
        // The token is resolved at send time, but every client setting may hold
        // a `{{var}}` too; the redirect URI is the one that usually does.
        Auth::OAuth2 {
            config,
            access_token,
        } => {
            for value in [
                &config.client_id,
                &config.client_secret,
                &config.auth_url,
                &config.token_url,
                &config.redirect_uri,
            ] {
                fields.push(value.clone());
            }
            fields.extend(config.scopes.iter().cloned());
            if let Some(token) = access_token {
                fields.push(token.clone());
            }
        }
        Auth::None | Auth::Inherit => {}
    }

    fields
}

/// The distinct `{{name}}`s a request references, first occurrence winning.
///
/// `$`-prefixed tokens are generators, not scope lookups: `template` renders
/// them through the generator at send time, so they can never be unresolved
/// and must not be reported as names the inspector is missing.
pub(crate) fn references(request: &RequestDef) -> Vec<String> {
    // The parser is the one previews and sends use, so the two agree on what
    // counts as a variable (trimmed name, unterminated braces left literal).
    let parser = ScopedVars::new();
    let mut names = Vec::new();

    for field in reference_fields(request) {
        for var in parser.preview(&field).vars {
            if var.name.is_empty() || var.name.starts_with('$') || names.contains(&var.name) {
                continue;
            }
            names.push(var.name);
        }
    }

    names
}

#[cfg(test)]
mod tests {
    // Named imports, not `super::*`: the supermodule imports gpui-kit, whose
    // `#[test]` macro would otherwise shadow the builtin.
    use super::{reference_fields, references, resolve_scopes, secret_of, Variables};
    use restbook_core::{Auth, Body, Environment, Kv, Method, RequestDef, VarScope};

    fn var(key: &str, value: &str) -> Kv {
        Kv::new(key, value)
    }

    /// An empty window: no globals, no environments, no active collection. Tests
    /// fill only the tier they exercise.
    fn variables() -> Variables {
        Variables {
            store: std::sync::Arc::new(restbook_store::MemoryStore::new()),
            environments: Vec::new(),
            globals: Vec::new(),
            collection: Vec::new(),
            active: None,
        }
    }

    /// The Collection tier is the producer this lane adds: a name only the
    /// collection holds resolves there, and a secret row masks instead of
    /// printing its value.
    #[test]
    fn a_collection_variable_resolves_and_a_secret_masks() {
        let mut vars = variables();
        let mut secret_row = var("token", "t0k");
        secret_row.secret = true;
        vars.collection = vec![var("base", "https://collection.test"), secret_row];

        let request = RequestDef::new(Method::Get, "https://{{base}}/things");
        let resolved = vars
            .scoped(&request)
            .lookup("base")
            .expect("the collection answers for base");
        assert_eq!(resolved.value, "https://collection.test");
        assert_eq!(resolved.scope, VarScope::Collection);

        let names = vars.scoped_names();
        assert!(names
            .iter()
            .any(|each| each.name == "base" && each.scope == VarScope::Collection));

        assert!(secret_of(&vars, "token", Some(VarScope::Collection)));
        assert!(!secret_of(&vars, "base", Some(VarScope::Collection)));
    }

    #[test]
    fn environment_beats_global() {
        let globals = [var("host", "global")];
        let environment = Environment {
            name: "dev".to_owned(),
            vars: vec![var("host", "env")],
        };

        let resolved = resolve_scopes(&globals, &[], Some(&environment))
            .lookup("host")
            .expect("host should resolve");

        assert_eq!(resolved.value, "env");
        assert_eq!(resolved.scope, VarScope::Environment);
    }

    #[test]
    fn environment_beats_collection_beats_global() {
        let globals = [var("host", "global"), var("only_global", "g")];
        let collection = [var("host", "collection"), var("only_collection", "c")];
        let environment = Environment {
            name: "dev".to_owned(),
            vars: vec![var("host", "env")],
        };

        let scoped = resolve_scopes(&globals, &collection, Some(&environment));
        assert_eq!(
            scoped.lookup("host").map(|each| (each.value, each.scope)),
            Some(("env".to_owned(), VarScope::Environment))
        );
        assert_eq!(
            scoped
                .lookup("only_collection")
                .map(|each| (each.value, each.scope)),
            Some(("c".to_owned(), VarScope::Collection))
        );
        assert_eq!(
            scoped
                .lookup("only_global")
                .map(|each| (each.value, each.scope)),
            Some(("g".to_owned(), VarScope::Global))
        );
    }

    #[test]
    fn with_no_active_environment_only_globals_answer() {
        let globals = [var("token", "abc")];

        let scoped = resolve_scopes(&globals, &[], None);
        assert_eq!(
            scoped.lookup("token").map(|each| each.value),
            Some("abc".into())
        );
        assert!(scoped.lookup("missing").is_none());
    }

    #[test]
    fn references_are_collected_in_first_seen_order() {
        let mut request = RequestDef::new(Method::Post, "https://{{base}}/users/{{id}}");
        request.params = vec![var("q", "{{term}}"), {
            let mut disabled = var("page", "{{ignored}}");
            disabled.enabled = false;
            disabled
        }];
        request.headers = vec![var("Authorization", "Bearer {{token}}")];
        request.body = Body::Raw {
            mime: "application/json".to_owned(),
            text: "{\"name\": \"{{user}}\"}".to_owned(),
        };
        request.auth = Auth::Bearer {
            token: "{{secret}}".to_owned(),
        };

        assert_eq!(
            references(&request),
            vec!["base", "id", "term", "token", "user", "secret"]
        );
    }

    #[test]
    fn a_disabled_row_contributes_nothing() {
        let mut request = RequestDef::new(Method::Get, "https://example.com");
        let mut disabled = var("{{hidden}}", "{{also_hidden}}");
        disabled.enabled = false;
        request.headers = vec![disabled, var("Accept", "{{wanted}}")];

        assert_eq!(references(&request), vec!["wanted"]);
    }

    #[test]
    fn a_repeated_name_is_reported_once() {
        let mut request = RequestDef::new(Method::Get, "https://{{host}}/{{host}}");
        request.params = vec![var("host", "{{host}}")];

        assert_eq!(references(&request), vec!["host"]);
    }

    #[test]
    fn auth_fields_are_walked_by_scheme() {
        let mut basic = RequestDef::new(Method::Get, "https://example.com");
        basic.auth = Auth::Basic {
            username: "{{user}}".to_owned(),
            password: "{{pass}}".to_owned(),
        };
        assert_eq!(references(&basic), vec!["user", "pass"]);

        let mut api_key = RequestDef::new(Method::Get, "https://example.com");
        api_key.auth = Auth::ApiKey {
            name: "{{header}}".to_owned(),
            value: "{{key}}".to_owned(),
            location: restbook_core::ApiKeyLocation::Header,
        };
        assert_eq!(references(&api_key), vec!["header", "key"]);
    }

    #[test]
    fn generators_are_not_variables() {
        let request = RequestDef::new(Method::Get, "https://api.test/{{base}}/{{$randomInt}}");
        let mut request = request;
        request.body = Body::Raw {
            mime: "application/json".to_owned(),
            text: "{\"token\": \"{{$uuid}}\"}".to_owned(),
        };

        assert_eq!(references(&request), vec!["base"]);
    }

    #[test]
    fn the_field_list_keeps_both_halves_of_a_row() {
        let mut request = RequestDef::new(Method::Get, "https://example.com");
        request.headers = vec![var("X-{{key}}", "{{value}}")];

        assert!(reference_fields(&request).contains(&"X-{{key}}".to_owned()));
        assert!(reference_fields(&request).contains(&"{{value}}".to_owned()));
    }
}
