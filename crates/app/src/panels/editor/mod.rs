//! The request editor: one `RequestDef`, four tab bodies.
//!
//! The editor keeps each tab's widgets and folds them into a `RequestDef` only
//! in [`Self::request`], so switching away from a body or auth scheme is
//! lossless: the inputs stay put and only the selected kind is read.

mod auth;
mod body;
mod headers;
mod kv_raw;
mod kv_table;
mod params;
mod parts_table;
mod settings;

use gpui_kit::component::input::{EditorState, InputState};
use gpui_kit::*;

use restbook_core::{Auth, Body, Method, OAuthGrant, RequestDef};

use crate::completion::{accept_body_completion, VariableCompletions};
use crate::theme::{Metrics, Palette};
use crate::variables::Variables;

pub(crate) use body::MIME_TYPES;

const METHODS: [Method; 7] = [
    Method::Get,
    Method::Post,
    Method::Put,
    Method::Patch,
    Method::Delete,
    Method::Head,
    Method::Options,
];

pub(crate) const AUTH_LABELS: [&str; 5] = ["None", "Basic", "Bearer", "API key", "OAuth 2.0"];

/// Which editor tab is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorTab {
    Params,
    Headers,
    Body,
    Auth,
    Settings,
}

impl EditorTab {
    pub(crate) const ALL: [EditorTab; 5] = [
        EditorTab::Params,
        EditorTab::Headers,
        EditorTab::Body,
        EditorTab::Auth,
        EditorTab::Settings,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            EditorTab::Params => "Params",
            EditorTab::Headers => "Headers",
            EditorTab::Body => "Body",
            EditorTab::Auth => "Auth",
            EditorTab::Settings => "Settings",
        }
    }
}

/// Which body kind the Body tab is editing. Every kind's widgets are kept
/// while the user switches away and back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BodyKind {
    #[default]
    None,
    Raw,
    Form,
    Multipart,
    Binary,
}

/// Which auth scheme the Auth tab is editing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum AuthKind {
    #[default]
    None,
    /// Take the collection's auth. Exposed in the Auth tab when a request
    /// sits in a collection; the model carries the state already.
    Inherit,
    Basic,
    Bearer,
    ApiKey,
    OAuth2,
}

/// Same order as [`AUTH_LABELS`].
pub(crate) const ALL_AUTH_KINDS: [AuthKind; 5] = [
    AuthKind::None,
    AuthKind::Basic,
    AuthKind::Bearer,
    AuthKind::ApiKey,
    AuthKind::OAuth2,
];

/// A request from the Editor to its pane that is not just "a field changed".
#[derive(Debug, Clone)]
pub(crate) enum EditorEvent {
    /// Fetch an OAuth token with the Auth tab's current settings. The workspace
    /// owns the transport, so the editor only asks.
    FetchToken,
}

impl EventEmitter<EditorEvent> for RequestEditor {}

pub(crate) struct RequestEditor {
    pub(crate) palette: Palette,
    /// The spacing and type the tab bodies draw with, re-read on a theme or
    /// zoom change alongside the palette.
    metrics: Metrics,
    /// Read by the value-field completion lists, so an environment edit shows
    /// up in the next popup with nothing to re-install.
    pub(crate) variables: Entity<Variables>,
    method: Method,
    pub(crate) tab: EditorTab,
    pub(crate) url: Entity<InputState>,
    params: kv_table::Rows,
    /// The Params tab as wire-form text, when the user chose Raw. `None` is the
    /// table; the text is parsed back on the way out and at send time.
    pub(crate) params_raw: Option<kv_raw::RawRows>,
    headers: kv_table::Rows,
    /// Whether the Headers tab shows the generated default headers. Collapsed
    /// by default, like a web client's "hidden headers": they are not part of
    /// the request, so they should not compete with the rows the user wrote.
    pub(crate) show_default_headers: bool,
    pub(crate) body_kind: BodyKind,
    pub(crate) raw_mime: String,
    /// A code editor rather than a plain textarea: the raw body is the one
    /// input whose syntax matters, and only `EditorState` can highlight it.
    pub(crate) raw_text: Entity<EditorState>,
    form: kv_table::Rows,
    /// The same switch for a `Form` body.
    pub(crate) form_raw: Option<kv_raw::RawRows>,
    parts: parts_table::PartRows,
    pub(crate) binary_path: Entity<InputState>,
    pub(crate) auth_kind: AuthKind,
    pub(crate) auth_a: Entity<InputState>,
    pub(crate) auth_b: Entity<InputState>,
    pub(crate) api_key_in_query: bool,
    /// The OAuth 2.0 settings, kept apart from the shared two fields because a
    /// flow has more than two. Every field is a widget so switching schemes
    /// stays lossless, like the rest of the editor.
    pub(crate) oauth_grant: OAuthGrant,
    pub(crate) oauth_pkce: bool,
    pub(crate) oauth_client_id: Entity<InputState>,
    pub(crate) oauth_client_secret: Entity<InputState>,
    pub(crate) oauth_auth_url: Entity<InputState>,
    pub(crate) oauth_token_url: Entity<InputState>,
    pub(crate) oauth_scopes: Entity<InputState>,
    pub(crate) oauth_redirect_uri: Entity<InputState>,
    /// The access token obtained by a fetch. It is not part of `OAuthConfig`
    /// (`Auth` keeps it `serde(skip)`), so it lives beside the widgets and is
    /// handed to `prepare()` through `request()`.
    pub(crate) oauth_access_token: Option<String>,
    /// A line under the OAuth fields: "fetching", an error, or an expiry note.
    pub(crate) oauth_status: Option<String>,
    /// Request timeout in milliseconds as typed. Empty means "use the default",
    /// which is why it is a string field and not an `Option<u64>`: a text input
    /// has to represent "nothing entered" somewhere, and parsing at `request()`
    /// time keeps the widget dumb.
    pub(crate) timeout_ms: Entity<InputState>,
    /// Whether to follow redirects. `None` is the transport default (follow up
    /// to 10 hops); this is tri-state so "leave it alone" is representable.
    pub(crate) follow_redirects: bool,
    /// Skip TLS certificate verification. Off by default; the request pane
    /// badges it when on, because it is a footgun.
    pub(crate) insecure: bool,
    /// Keeps the Enter interceptor registered for as long as the editor lives.
    /// The raw body is multi-line, so the kit's `enter` binding would insert a
    /// newline before any key-down listener could accept a completion; this is
    /// the one layer that runs ahead of the binding.
    _keystroke_guard: Subscription,
    /// A URL waiting to be written into the input. `InputState::set_value`
    /// needs a window, which an observer does not have, so the panel applies
    /// this at the top of its next render.
    pending_url: Option<String>,
    /// A whole stored request waiting to be written into the widgets. Same
    /// reason as `pending_url`: the writes need a window.
    pending_request: Option<RequestDef>,
}

impl RequestEditor {
    pub(crate) fn new(
        variables: Entity<Variables>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("https://httpbin.org/get"));
        // The highlighter language follows the selected mime; JSON is the
        // default body type, so a new request starts highlighted.
        let raw_text = cx.new(|cx| {
            EditorState::new(window, cx)
                .placeholder("{\n  \"hello\": \"world\"\n}")
                .language(language_for_mime(MIME_TYPES[0]))
        });
        // The body is the request's only `EditorState`, and therefore the only
        // field that can host the kit's completion popover. The provider holds
        // the variables entity, so it reads whatever the Environment pane last
        // wrote instead of a copy taken here.
        raw_text.update(cx, |state, _| {
            state.lsp_mut().completion_provider = Some(VariableCompletions::new(variables.clone()));
        });
        // Accepting the raw body's completion must happen before the kit's
        // `enter` binding: the body is multi-line, so that binding inserts a
        // newline first and a key-down fallback would accept over a body the user
        // just split. An interceptor runs ahead of bindings and may consume the
        // keystroke; it stays out of the way unless this editor owns the caret and
        // its popover is open, leaving Enter and Shift+Enter untouched.
        let accept_body = raw_text.clone();
        let _keystroke_guard = cx.intercept_keystrokes(move |event, window, cx| {
            if !plain_enter(&event.keystroke) {
                return;
            }
            if !accept_body.read(cx).focus_handle(cx).is_focused(window) {
                return;
            }
            if accept_body.update(cx, |state, cx| accept_body_completion(state, window, cx)) {
                // The accept closed the popover; swallow the key so the
                // binding cannot also insert its newline.
                cx.stop_propagation();
            }
        });
        let binary_path = cx.new(|cx| InputState::new(window, cx).placeholder("/path/to/file"));
        // The fields are shared by every scheme, so the placeholders stay
        // neutral; the per-scheme hint under them says how they are used.
        let auth_a = cx.new(|cx| InputState::new(window, cx).placeholder("first field"));
        let auth_b = cx.new(|cx| InputState::new(window, cx).placeholder("second field"));
        let oauth_client_id = cx.new(|cx| InputState::new(window, cx).placeholder("client id"));
        let oauth_client_secret =
            cx.new(|cx| InputState::new(window, cx).placeholder("client secret"));
        let oauth_auth_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://…/authorize"));
        let oauth_token_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://…/token"));
        let oauth_scopes =
            cx.new(|cx| InputState::new(window, cx).placeholder("read write (space separated)"));
        let oauth_redirect_uri =
            cx.new(|cx| InputState::new(window, cx).placeholder("http://127.0.0.1/callback"));
        let timeout_ms = cx.new(|cx| InputState::new(window, cx).placeholder("default"));

        Self {
            palette: Palette::new(cx),
            metrics: Metrics::current(cx),
            variables,
            method: Method::Get,
            tab: EditorTab::Params,
            url,
            params: kv_table::Rows::default(),
            params_raw: None,
            headers: kv_table::Rows::default(),
            show_default_headers: false,
            body_kind: BodyKind::None,
            raw_mime: MIME_TYPES[0].to_owned(),
            raw_text,
            form: kv_table::Rows::default(),
            form_raw: None,
            parts: parts_table::PartRows::default(),
            binary_path,
            auth_kind: AuthKind::None,
            auth_a,
            auth_b,
            api_key_in_query: false,
            oauth_grant: OAuthGrant::default(),
            oauth_pkce: true,
            oauth_client_id,
            oauth_client_secret,
            oauth_auth_url,
            oauth_token_url,
            oauth_scopes,
            oauth_redirect_uri,
            oauth_access_token: None,
            oauth_status: None,
            timeout_ms,
            follow_redirects: true,
            insecure: false,
            _keystroke_guard,
            pending_url: None,
            pending_request: None,
        }
    }

    /// Rebuild the cached palette after a theme or zoom change. The editor draws
    /// from its own copy, so the fan-out from the request pane has to reach it.
    pub(crate) fn refresh_theme(&mut self, cx: &App) {
        self.palette = Palette::new(cx);
        self.metrics = Metrics::current(cx);
    }

    pub(crate) fn method(&self) -> Method {
        self.method
    }

    pub(crate) fn set_method(&mut self, method: Method) {
        self.method = method;
    }

    /// The URL as currently shown in the input.
    pub(crate) fn url_value(&self, cx: &App) -> String {
        self.url.read(cx).value().to_string()
    }

    /// Whether a stored request or URL is still waiting for the next render.
    /// While one is queued the widgets are the *previous* request, so folding
    /// them into the document would undo the adoption for a frame — and the
    /// tab label and history would follow that stale frame.
    pub(crate) fn has_pending(&self) -> bool {
        self.pending_request.is_some() || self.pending_url.is_some()
    }

    /// Replace method and URL without touching focus or the other tabs, and
    /// without notifying when nothing changed. Used when the document changes
    /// under the editor — a history pick.
    pub(crate) fn adopt(&mut self, method: Method, url: &str, cx: &mut Context<Self>) {
        let method_changed = self.method != method;
        let url_changed = self.url_value(cx) != url;
        if !method_changed && !url_changed {
            return;
        }
        self.method = method;
        if url_changed {
            self.pending_url = Some(url.to_owned());
        }
        cx.notify();
    }

    /// Write a URL adopted from the document into the input. Called from the
    /// request pane's render, the first place a window is available.
    pub(crate) fn flush_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(request) = self.pending_request.take() {
            self.apply_request(request, window, cx);
        }

        let Some(url) = self.pending_url.take() else {
            return;
        };
        if self.url_value(cx) != url {
            self.url
                .update(cx, |state, cx| state.set_value(url, window, cx));
        }
    }

    /// Queue a stored request — a history pick — for the next render. Unlike
    /// [`Self::adopt`] this restores every tab, so the request can be re-sent
    /// exactly as it was recorded.
    pub(crate) fn adopt_request(&mut self, request: RequestDef) {
        self.pending_request = Some(request);
        // A pending URL would be a stale half of the same change.
        self.pending_url = None;
    }

    /// Write a stored request into the widgets. The URL goes last: writing it
    /// notifies the input, whose observer folds the editor back into the
    /// document, and by then every other field is in place.
    fn apply_request(&mut self, request: RequestDef, window: &mut Window, cx: &mut Context<Self>) {
        let RequestDef {
            method,
            url,
            params,
            headers,
            body,
            auth,
            timeout_ms,
            follow_redirects,
            insecure,
            ..
        } = request;

        self.method = method;
        let variables = self.variables.clone();
        self.params.replace(&params, &variables, window, cx);
        if let Some(raw) = &self.params_raw {
            raw.write(&self.params.to_kv(cx), window, cx);
        }
        self.headers.replace(&headers, &variables, window, cx);

        match body {
            Body::None => {
                self.body_kind = BodyKind::None;
                self.form.replace(&[], &variables, window, cx);
            }
            Body::Raw { mime, text } => {
                self.body_kind = BodyKind::Raw;
                self.raw_mime = if mime.trim().is_empty() {
                    MIME_TYPES[0].to_owned()
                } else {
                    mime
                };
                // The body and its language change together: a stored request
                // editing a different raw type must not keep the old grammar.
                let language = language_for_mime(&self.raw_mime);
                self.raw_text.update(cx, |state, cx| {
                    state.set_value(text, window, cx);
                    state.set_highlighter(language, cx);
                });
            }
            Body::Form(rows) => {
                self.body_kind = BodyKind::Form;
                self.form.replace(&rows, &variables, window, cx);
            }
            Body::Multipart(parts) => {
                self.body_kind = BodyKind::Multipart;
                self.parts.replace(&parts, window, cx);
            }
            Body::Binary { path } => {
                self.body_kind = BodyKind::Binary;
                self.binary_path
                    .update(cx, |state, cx| state.set_value(path, window, cx));
            }
        }

        let (kind, first, second, in_query) = auth::auth_fields(&auth);
        self.auth_kind = kind;
        self.api_key_in_query = in_query;
        self.auth_a
            .update(cx, |state, cx| state.set_value(first, window, cx));
        self.auth_b
            .update(cx, |state, cx| state.set_value(second, window, cx));

        // The OAuth settings are written even when another scheme is selected,
        // so switching to OAuth shows the stored flow rather than empty fields.
        if let Auth::OAuth2 {
            config,
            access_token,
        } = &auth
        {
            self.oauth_grant = config.grant;
            self.oauth_pkce = config.pkce;
            self.oauth_access_token = access_token.clone();
            self.oauth_status = None;
            let fields = [
                (&self.oauth_client_id, config.client_id.clone()),
                (&self.oauth_client_secret, config.client_secret.clone()),
                (&self.oauth_auth_url, config.auth_url.clone()),
                (&self.oauth_token_url, config.token_url.clone()),
                (&self.oauth_scopes, config.scopes.join(" ")),
                (&self.oauth_redirect_uri, config.redirect_uri.clone()),
            ];
            for (input, value) in fields {
                input.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }

        // A stored `None` is the default, which the field shows as empty rather
        // than as a number the user never typed.
        let timeout = timeout_ms.map(|ms| ms.to_string()).unwrap_or_default();
        self.timeout_ms
            .update(cx, |state, cx| state.set_value(timeout, window, cx));
        self.follow_redirects = follow_redirects.unwrap_or(true);
        self.insecure = insecure;

        if let Some(raw) = &self.form_raw {
            raw.write(&self.form.to_kv(cx), window, cx);
        }

        self.url
            .update(cx, |state, cx| state.set_value(url, window, cx));
        cx.notify();
    }

    /// Switch the Params tab between the table and wire-form text. The text is
    /// generated from the rows on the way in and parsed back into them on the
    /// way out, so there is never a second copy of the request.
    pub(crate) fn toggle_params_raw(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.params_raw.take() {
            Some(raw) => {
                let rows = raw.rows(cx);
                let variables = self.variables.clone();
                self.params.replace(&rows, &variables, window, cx);
            }
            None => {
                let raw = kv_raw::RawRows::new(window, cx);
                let rows = self.params.to_kv(cx);
                raw.write(&rows, window, cx);
                self.params_raw = Some(raw);
            }
        }
        cx.notify();
    }

    /// The same switch for a `Form` body.
    pub(crate) fn toggle_form_raw(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.form_raw.take() {
            Some(raw) => {
                let rows = raw.rows(cx);
                let variables = self.variables.clone();
                self.form.replace(&rows, &variables, window, cx);
            }
            None => {
                let raw = kv_raw::RawRows::new(window, cx);
                let rows = self.form.to_kv(cx);
                raw.write(&rows, window, cx);
                self.form_raw = Some(raw);
            }
        }
        cx.notify();
    }

    /// Fold an open Form text view back into its rows before the body kind
    /// changes, so switching to Multipart does not silently discard edits.
    pub(crate) fn fold_form_raw(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form_raw.is_some() {
            self.toggle_form_raw(window, cx);
        }
    }

    pub(crate) fn methods() -> [Method; 7] {
        METHODS
    }

    /// How many entries a tab holds, for its label. Zero means the tab shows
    /// no count, the way a rest client hides empty sections rather than
    /// printing `0` next to every heading.
    pub(crate) fn count(&self, tab: EditorTab) -> usize {
        match tab {
            EditorTab::Params => self.params.len(),
            EditorTab::Headers => self.headers.len(),
            EditorTab::Body => match self.body_kind {
                BodyKind::None => 0,
                BodyKind::Raw => 1,
                BodyKind::Form => self.form.len(),
                BodyKind::Multipart => self.parts.len(),
                BodyKind::Binary => 1,
            },
            EditorTab::Auth => match self.auth_kind {
                AuthKind::None => 0,
                _ => 1,
            },
            // The tab counts only what has been flipped away from the default,
            // so an untouched request shows a plain "Settings" rather than a
            // number that means nothing yet. The timeout field is not read here:
            // `count` has no `App`, and a blank field is the default anyway.
            EditorTab::Settings => usize::from(!self.follow_redirects) + usize::from(self.insecure),
        }
    }

    /// The label a tab shows: `Params` when empty, `Params 3` when not.
    pub(crate) fn tab_label(&self, tab: EditorTab) -> String {
        match self.count(tab) {
            0 => tab.label().to_owned(),
            count => format!("{} {count}", tab.label()),
        }
    }

    /// The one place the editor's widgets become a request. Called right
    /// before `prepare()`, so `prepare()` validates what the user sees.
    pub(crate) fn request(&self, cx: &App) -> RequestDef {
        let url = self.url.read(cx).value().to_string();
        RequestDef {
            id: String::new(),
            name: format!("{} {}", self.method, url),
            method: self.method,
            url,
            params: match &self.params_raw {
                Some(raw) => raw.rows(cx),
                None => self.params.to_kv(cx),
            },
            headers: self.headers.to_kv(cx),
            body: self.body(cx),
            auth: self.auth(cx),
            timeout_ms: parse_timeout_ms(&self.timeout_ms.read(cx).value()),
            // `Some(true)` is the default, so it is stored as `None` to keep a
            // saved request's JSON free of a field nobody changed.
            follow_redirects: (!self.follow_redirects).then_some(false),
            insecure: self.insecure,
        }
    }

    /// The row whose value field is `value`, across every KV list the editor
    /// owns. A completion event names its input, not its index, because a row
    /// that was removed can no longer be reached by position.
    fn kv_row_mut(&mut self, value: &Entity<InputState>) -> Option<&mut kv_table::Row> {
        self.params
            .iter_mut()
            .chain(self.headers.iter_mut())
            .chain(self.form.iter_mut())
            .find(|row| row.value_entity() == value)
    }

    /// Accept a row from a value field's completion list. `index` names the
    /// clicked row; `None` takes the highlighted one, which is what Enter does.
    ///
    /// The write is deferred because replacing the text emits `InputEvent::Change`,
    /// which comes straight back to the subscription that opened the list, and an
    /// entity cannot be updated while it is being updated.
    fn accept_value_completion(
        &mut self,
        value: Entity<InputState>,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.kv_row_mut(&value) else {
            return;
        };
        if let Some(index) = index {
            row.completion_mut().select(index);
        }
        let Some((span, insert)) = row.completion_mut().accept() else {
            return;
        };
        cx.notify();
        window.defer(cx, move |window, cx| {
            value.update(cx, |input, cx| {
                input.set_selected_range(span, cx);
                input.replace(insert, window, cx);
            });
        });
    }

    /// Route a key to a value field's completion list while it is open.
    ///
    /// A single-line `InputState` has no Up/Down handler and lets Escape propagate,
    /// so those keys arrive as plain key-down events (Ctrl+N / Ctrl+P as in the URL
    /// list). Tab stops the event so `InputState`'s `tab` → `IndentInline` binding
    /// cannot move the caret; Enter accepts when `PressEnter` did not.
    fn value_popup_key(
        &mut self,
        value: Entity<InputState>,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Only while this value field owns the caret: the same keys keep doing
        // what they always did elsewhere in the pane.
        if !value.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }

        let modifiers = event.keystroke.modifiers;
        if modifiers.platform || modifiers.alt {
            return;
        }

        let open = self
            .kv_row_mut(&value)
            .is_some_and(|row| row.completion().is_open());
        if !open {
            return;
        }

        let key = event.keystroke.key.as_str();
        if matches!(key, "enter" | "tab") && !modifiers.control && !modifiers.shift {
            self.accept_value_completion(value, None, window, cx);
            if key == "tab" {
                // `InputState` binds `tab` to `IndentInline`; without this the
                // accept would be followed by the input indenting its text.
                cx.stop_propagation();
            }
            return;
        }

        let Some(row) = self.kv_row_mut(&value) else {
            return;
        };
        let next = matches!((key, modifiers.control), ("down", false) | ("n", true));
        let previous = matches!((key, modifiers.control), ("up", false) | ("p", true));

        if next {
            row.completion_mut().move_selection(1);
            cx.notify();
        } else if previous {
            row.completion_mut().move_selection(-1);
            cx.notify();
        } else if key == "escape" {
            row.completion_mut().close();
            cx.notify();
        }
    }
}

/// A bare Enter: no Ctrl (it sends from this pane), no Shift (a deliberate
/// newline in the body), and no platform or Alt chord.
fn plain_enter(keystroke: &Keystroke) -> bool {
    let modifiers = keystroke.modifiers;
    keystroke.key == "enter"
        && !modifiers.control
        && !modifiers.shift
        && !modifiers.alt
        && !modifiers.platform
}

/// The tree-sitter language for a raw-body mime type. Only JSON has a grammar
/// in this build; the other raw types stay editable, just unhighlighted.
pub(super) fn language_for_mime(mime: &str) -> &'static str {
    if mime.to_ascii_lowercase().contains("json") {
        "json"
    } else {
        "text"
    }
}

/// The timeout field as an `Option<u64>`. Blank is `None` (the transport
/// default); anything that does not parse as a positive number is also `None`,
/// because a mistyped timeout should not silently become zero — a zero timeout
/// fails every request instantly.
pub(crate) fn parse_timeout_ms(text: &str) -> Option<u64> {
    text.trim().parse::<u64>().ok().filter(|ms| *ms > 0)
}

#[cfg(test)]
mod raw_tests {
    use std::sync::Arc;

    use gpui_kit::{
        div, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
        Styled as _, TestAppContext, VisualTestContext, Window,
    };
    use restbook_core::{Body, Kv};
    use restbook_store::MemoryStore;

    use super::{BodyKind, RequestEditor};
    use crate::variables::Variables;

    struct Host {
        editor: Entity<RequestEditor>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                self.editor
                    .update(cx, |editor, cx| editor.render_params_tab(cx)),
            )
        }
    }

    fn editor(cx: &mut TestAppContext) -> (Entity<RequestEditor>, &mut VisualTestContext) {
        let variables =
            cx.update(|cx| cx.new(|cx| Variables::new(Arc::new(MemoryStore::new()), cx)));
        let mut built = None;
        let (_view, cx) = cx.add_window_view(|window, cx| {
            let editor = cx.new(|cx| RequestEditor::new(variables.clone(), window, cx));
            built = Some(editor.clone());
            Host { editor }
        });
        (built.expect("the view built the editor"), cx)
    }

    /// The text is the editable surface while Raw is open, and the table is fed
    /// from it on the way back — repeated keys and a disabled row included.
    #[test]
    fn the_params_text_round_trips_through_the_table() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let (editor, vcx) = editor(&mut cx);

        vcx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let variables = editor.variables.clone();
                editor.params.replace(
                    &[
                        Kv::new("keep", "1"),
                        Kv {
                            enabled: false,
                            ..Kv::new("off", "2")
                        },
                    ],
                    &variables,
                    window,
                    cx,
                );
                editor.toggle_params_raw(window, cx);

                let raw = editor.params_raw.as_ref().expect("the raw view is open");
                raw.set_text("a=1&a=2\n# off=2", window, cx);
                assert_eq!(editor.request(cx).params.len(), 3);

                editor.toggle_params_raw(window, cx);
                let rows = editor.params.to_kv(cx);
                assert_eq!(
                    rows.iter().map(|row| row.key.as_str()).collect::<Vec<_>>(),
                    ["a", "a", "off"]
                );
                assert!(!rows[2].enabled);
            });
        });
    }

    #[test]
    fn the_form_text_round_trips_through_the_table() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let (editor, vcx) = editor(&mut cx);

        vcx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.body_kind = BodyKind::Form;
                editor.toggle_form_raw(window, cx);

                let raw = editor.form_raw.as_ref().expect("the raw view is open");
                raw.set_text("x=1\ny=2", window, cx);
                match editor.body(cx) {
                    Body::Form(rows) => assert_eq!(rows.len(), 2),
                    other => panic!("expected a form body, got {other:?}"),
                }

                editor.toggle_form_raw(window, cx);
                assert_eq!(editor.form.len(), 2);
            });
        });
    }
}

#[cfg(test)]
mod tests {
    // Named import, not `super::*`: this module glob-imports gpui-kit, whose
    // `test` macro would shadow the builtin `#[test]`.
    use super::parse_timeout_ms;

    #[test]
    fn a_blank_timeout_means_the_default() {
        assert_eq!(parse_timeout_ms(""), None);
        assert_eq!(parse_timeout_ms("   "), None);
    }

    #[test]
    fn a_valid_timeout_is_parsed() {
        assert_eq!(parse_timeout_ms("5000"), Some(5000));
        assert_eq!(parse_timeout_ms(" 250 "), Some(250));
    }

    #[test]
    fn a_zero_timeout_is_treated_as_unset() {
        // Zero would fail every request instantly, so a mistyped `0` must not
        // become a real deadline.
        assert_eq!(parse_timeout_ms("0"), None);
    }

    #[test]
    fn nonsense_is_treated_as_unset_rather_than_failing_the_send() {
        assert_eq!(parse_timeout_ms("soon"), None);
        assert_eq!(parse_timeout_ms("-1"), None);
        assert_eq!(parse_timeout_ms("1e3"), None);
    }
}

#[cfg(test)]
mod enter_tests {
    // Named imports only: `use super::*` would drag in GPUI's `test` macro and
    // shadow the built-in `#[test]`.
    use std::sync::Arc;

    use gpui_kit::{
        div, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
        Styled as _, TestAppContext, VisualTestContext, Window,
    };
    use restbook_store::MemoryStore;

    use super::{BodyKind, RequestEditor};
    use crate::variables::Variables;

    /// Press Enter the way the platform delivers it: a `KeyDownEvent` with no
    /// text. The simulated IME supplies `"\n"`, which would insert a newline
    /// before the input sees the binding.
    fn press_enter(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            window.dispatch_event(
                gpui_kit::PlatformInput::KeyDown(gpui_kit::KeyDownEvent {
                    keystroke: gpui_kit::Keystroke {
                        modifiers: gpui_kit::Modifiers::default(),
                        key: "enter".to_owned(),
                        key_char: None,
                    },
                    is_held: false,
                    prefer_character_input: false,
                }),
                cx,
            );
        });
        cx.run_until_parked();
    }

    struct Params {
        editor: Entity<RequestEditor>,
    }

    impl Render for Params {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                self.editor
                    .update(cx, |editor, cx| editor.render_params_tab(cx)),
            )
        }
    }

    struct BodyTab {
        editor: Entity<RequestEditor>,
    }

    impl Render for BodyTab {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                self.editor
                    .update(cx, |editor, cx| editor.render_body_tab(cx)),
            )
        }
    }

    /// A raw body, focused, with the editor's Enter interceptor registered.
    fn raw_body(cx: &mut TestAppContext) -> (Entity<RequestEditor>, &mut VisualTestContext) {
        let variables =
            cx.update(|cx| cx.new(|cx| Variables::new(Arc::new(MemoryStore::new()), cx)));
        let mut built = None;
        let (_view, cx) = cx.add_window_view(|window, cx| {
            let editor = cx.new(|cx| RequestEditor::new(variables.clone(), window, cx));
            built = Some(editor.clone());
            BodyTab { editor }
        });
        let editor = built.expect("the view built the editor");
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.body_kind = BodyKind::Raw;
                let raw = editor.raw_text.clone();
                raw.update(cx, |state, cx| state.focus(window, cx));
                cx.notify();
            });
        });
        (editor, cx)
    }

    /// The interceptor must not take a newline away: with no reference under
    /// the caret the popover is closed, so plain Enter stays the editor's own
    /// key and the body gets a line break.
    #[gpui_kit::test]
    fn a_plain_enter_still_breaks_a_line_while_no_completion_is_open(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (editor, cx) = raw_body(cx);

        cx.simulate_input("hello");
        press_enter(cx);

        let text = cx.update(|_, cx| editor.read(cx).raw_text.read(cx).value().to_string());
        assert_eq!(text, "hello\n");
    }

    /// The interceptor keeps the multi-line binding from turning an accept into a
    /// newline.
    ///
    /// The kit's overlay host is installed by a render pass, so a key can arrive
    /// before routing has a host and fall through to the `enter` binding. The
    /// interceptor accepts directly and stops propagation instead.
    #[gpui_kit::test]
    fn the_interceptor_accepts_before_the_newline_binding(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (editor, cx) = raw_body(cx);

        cx.simulate_input("{{");
        cx.run_until_parked();

        cx.update(|window, cx| {
            // Draw once so the kit installs its handler, then replace it with
            // one that refuses, as the registry lookup does when it does not
            // hold this editor. No notify: the next dispatch must not redraw
            // and put the real handler back.
            window.refresh();
            window.draw(cx).clear(cx);
            editor.update(cx, |editor, cx| {
                editor.raw_text.update(cx, |state, _| {
                    state.set_overlay_action_handler(|_, _, _, _| false);
                });
            });
            window.dispatch_event(
                gpui_kit::PlatformInput::KeyDown(gpui_kit::KeyDownEvent {
                    keystroke: gpui_kit::Keystroke {
                        modifiers: gpui_kit::Modifiers::default(),
                        key: "enter".to_owned(),
                        key_char: None,
                    },
                    is_held: false,
                    prefer_character_input: false,
                }),
                cx,
            );
        });
        cx.run_until_parked();

        let text = cx.update(|_, cx| editor.read(cx).raw_text.read(cx).value().to_string());
        assert_eq!(text, "{{$uuid}}");
    }

    /// Enter in a key/value row accepts the highlighted row, exactly like the
    /// click path. The value field is a plain `InputState`, so this is the row
    /// key-down plus the `InputEvent::PressEnter` subscription working together.
    #[gpui_kit::test]
    fn a_real_enter_key_accepts_a_completion_in_a_value_field(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let variables =
            cx.update(|cx| cx.new(|cx| Variables::new(Arc::new(MemoryStore::new()), cx)));

        let (params, cx) = cx.add_window_view(|window, cx| {
            let editor = cx.new(|cx| RequestEditor::new(variables.clone(), window, cx));
            Params { editor }
        });
        let editor = cx.update(|_, cx| params.read(cx).editor.clone());

        let value = cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let variables = editor.variables.clone();
                editor.params.push(&variables, window, cx);
                let value = editor
                    .params
                    .iter_mut()
                    .next()
                    .expect("the row was just added")
                    .value_entity()
                    .clone();
                value.update(cx, |state, cx| state.focus(window, cx));
                value
            })
        });

        cx.simulate_input("{{");
        press_enter(cx);

        let text = cx.update(|_, cx| value.read(cx).value().to_string());
        assert_eq!(text, "{{$uuid}}");
    }
}
