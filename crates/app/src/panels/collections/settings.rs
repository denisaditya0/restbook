//! The settings disclosure: the selected collection's variables, auth, and
//! default headers.
//!
//! It is a disclosure, not a pane. It appears under the tree when the collection
//! row itself is selected, and every edit is written back through the pane's
//! normal `persist` path, so the next send inherits what the editor shows. A
//! folder or request selection shows nothing: only a collection carries these
//! defaults.

use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::component::button::*;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex, Selectable as _};
use gpui_kit::*;

use restbook_core::{Auth, Collection, Kv};

use crate::icons::IconName;
use crate::panels::editor::{auth_fields, auth_from, AuthKind};

use super::CollectionsPanel;

/// The schemes a collection can default to. OAuth 2.0 is absent: its token is
/// fetched per request, so a collection-level config would fail every send.
/// `Inherit` is a request state, not a collection one.
const SCHEMES: [(AuthKind, &str); 4] = [
    (AuthKind::None, "None"),
    (AuthKind::Basic, "Basic"),
    (AuthKind::Bearer, "Bearer"),
    (AuthKind::ApiKey, "API key"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Variables,
    Headers,
    Auth,
}

impl SettingsTab {
    fn label(self) -> &'static str {
        match self {
            SettingsTab::Variables => "Variables",
            SettingsTab::Headers => "Headers",
            SettingsTab::Auth => "Auth",
        }
    }

    fn key_placeholder(self) -> &'static str {
        match self {
            SettingsTab::Headers => "Header",
            SettingsTab::Variables | SettingsTab::Auth => "name",
        }
    }

    fn value_placeholder(self) -> &'static str {
        match self {
            SettingsTab::Headers => "Value",
            SettingsTab::Variables | SettingsTab::Auth => "value",
        }
    }

    fn empty(self) -> &'static str {
        match self {
            SettingsTab::Variables => {
                "No variables. Add one, and a saved request here can use it as {{name}}."
            }
            SettingsTab::Headers => {
                "No default headers. A saved request here sends these unless it sets the same name."
            }
            SettingsTab::Auth => "",
        }
    }
}

/// One editable variable or header row. `revealed` is view state only: a secret
/// row still holds its value in the input, it is just not painted while folded.
struct Row {
    /// Stable for the row's lifetime, so removing a neighbour cannot move the
    /// enabled flag, the reveal state, or an input's keyed identity.
    id: u64,
    key: Entity<InputState>,
    value: Entity<InputState>,
    enabled: bool,
    secret: bool,
    revealed: bool,
}

impl Row {
    fn blank(window: &mut Window, cx: &mut App, tab: SettingsTab) -> Self {
        Self {
            id: new_id(),
            key: new_input(window, cx, tab.key_placeholder()),
            value: new_input(window, cx, tab.value_placeholder()),
            enabled: true,
            secret: false,
            revealed: false,
        }
    }

    fn from_kv(kv: &Kv, window: &mut Window, cx: &mut App, tab: SettingsTab) -> Self {
        let mut row = Self::blank(window, cx, tab);
        row.enabled = kv.enabled;
        row.secret = kv.secret;
        row.key
            .update(cx, |state, cx| state.set_value(kv.key.clone(), window, cx));
        row.value.update(cx, |state, cx| {
            state.set_value(kv.value.clone(), window, cx)
        });
        row
    }

    fn to_kv(&self, cx: &App) -> Kv {
        Kv {
            key: self.key.read(cx).value().to_string(),
            value: self.value.read(cx).value().to_string(),
            enabled: self.enabled,
            secret: self.secret,
        }
    }
}

fn new_input(window: &mut Window, cx: &mut App, placeholder: &str) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_owned()))
}

/// Allocation only grows, so an id is never handed to a second row even after
/// the first is removed.
fn new_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// The rows the disclosure is editing, loaded from one collection. It keeps its
/// own working copy so a keystroke never touches the store until Save.
pub(super) struct CollectionSettings {
    /// The collection these rows belong to; a different target reloads.
    collection: String,
    tab: SettingsTab,
    variables: Vec<Row>,
    headers: Vec<Row>,
    auth_kind: AuthKind,
    auth_a: Entity<InputState>,
    auth_b: Entity<InputState>,
    api_key_in_query: bool,
    /// A stored auth this editor cannot render (OAuth 2.0 or `Inherit`). Kept so
    /// saving variables or headers does not silently rewrite it.
    preserved_auth: Option<Auth>,
}

impl CollectionSettings {
    fn load(collection: &Collection, window: &mut Window, cx: &mut App) -> Self {
        let variables = collection
            .variables
            .iter()
            .map(|kv| Row::from_kv(kv, window, cx, SettingsTab::Variables))
            .collect();
        let headers = collection
            .headers
            .iter()
            .map(|kv| Row::from_kv(kv, window, cx, SettingsTab::Headers))
            .collect();

        let (kind, first, second, in_query) = auth_fields(&collection.auth);
        let (auth_kind, preserved_auth) = if SCHEMES.iter().any(|(each, _)| *each == kind) {
            (kind, None)
        } else {
            (AuthKind::None, Some(collection.auth.clone()))
        };

        let auth_a = new_input(window, cx, "value");
        let auth_b = new_input(window, cx, "value");
        auth_a.update(cx, |state, cx| state.set_value(first, window, cx));
        auth_b.update(cx, |state, cx| state.set_value(second, window, cx));

        Self {
            collection: collection.id.clone(),
            tab: SettingsTab::Variables,
            variables,
            headers,
            auth_kind,
            auth_a,
            auth_b,
            api_key_in_query: in_query,
            preserved_auth,
        }
    }

    fn rows(&self, tab: SettingsTab) -> &[Row] {
        match tab {
            SettingsTab::Variables => &self.variables,
            SettingsTab::Headers => &self.headers,
            SettingsTab::Auth => &[],
        }
    }

    fn rows_mut(&mut self, tab: SettingsTab) -> Option<&mut Vec<Row>> {
        match tab {
            SettingsTab::Variables => Some(&mut self.variables),
            SettingsTab::Headers => Some(&mut self.headers),
            SettingsTab::Auth => None,
        }
    }

    fn variables_kvs(&self, cx: &App) -> Vec<Kv> {
        self.variables.iter().map(|row| row.to_kv(cx)).collect()
    }

    fn headers_kvs(&self, cx: &App) -> Vec<Kv> {
        self.headers.iter().map(|row| row.to_kv(cx)).collect()
    }

    fn auth(&self, cx: &App) -> Auth {
        if let Some(preserved) = &self.preserved_auth {
            return preserved.clone();
        }
        let a = self.auth_a.read(cx).value().to_string();
        let b = self.auth_b.read(cx).value().to_string();
        auth_from(self.auth_kind, self.api_key_in_query, &a, &b)
    }

    /// Whether the working copy differs from `collection`. Compared against the
    /// store rather than a snapshot, so a save clears the marker on the next
    /// repaint without a second state write.
    fn is_unsaved(&self, collection: &Collection, cx: &App) -> bool {
        self.variables_kvs(cx) != collection.variables
            || self.headers_kvs(cx) != collection.headers
            || self.auth(cx) != collection.auth
    }
}

fn scheme_name(auth: &Auth) -> &'static str {
    match auth {
        Auth::None => "no auth",
        Auth::Inherit => "inherited auth",
        Auth::Basic { .. } => "Basic auth",
        Auth::Bearer { .. } => "a bearer token",
        Auth::ApiKey { .. } => "an API key",
        Auth::OAuth2 { .. } => "OAuth 2.0",
    }
}

impl CollectionsPanel {
    /// Queue the disclosure's rows for the selected collection.
    ///
    /// Inputs can only be built and filled with a window, and `set_value`
    /// notifies its own entity, so the load happens after the frame rather than
    /// inside `render`. A selection change drops the old rows first, so a stale
    /// collection's edits never paint under a new name.
    pub(super) fn sync_settings(&mut self, window: &Window, cx: &mut Context<Self>) {
        let target = self.settings_target().map(str::to_owned);
        let loaded = self.settings.as_ref().map(|each| each.collection.as_str());
        if target.as_deref() == loaded {
            return;
        }
        self.settings = None;
        // One queued load per target: a repaint before the defer runs must not
        // queue it again.
        if self.settings_pending == target {
            return;
        }
        self.settings_pending = target.clone();
        let Some(id) = target else {
            return;
        };
        let for_load = id.clone();
        cx.defer_in(window, move |this, window, cx| {
            if this.settings_pending.as_deref() != Some(for_load.as_str()) {
                return;
            }
            this.settings_pending = None;
            if this.settings_target() != Some(for_load.as_str()) {
                return;
            }
            if let Some(collection) = this.collections.iter().find(|each| each.id == for_load) {
                this.settings = Some(CollectionSettings::load(collection, window, cx));
            }
            cx.notify();
        });
    }

    /// The collection whose settings are editable: only when the collection root
    /// itself is the selected row. A folder or request has no collection-level
    /// defaults, and showing the parent's would misname what the editor changes.
    fn settings_target(&self) -> Option<&str> {
        if self.selection.request.is_some() || !self.selection.folders.is_empty() {
            return None;
        }
        let id = self.selection.collection.as_deref()?;
        self.collections
            .iter()
            .find(|each| each.id == id)
            .map(|each| each.id.as_str())
    }

    pub(super) fn render_settings(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let settings = self.settings.as_ref()?;
        let collection = self
            .collections
            .iter()
            .find(|each| each.id == settings.collection)?;
        let palette = &self.palette;
        let metrics = self.metrics;
        let unsaved = settings.is_unsaved(collection, cx);

        let toggle = Button::new("collection-settings-toggle")
            .icon(if self.settings_open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .label("Collection settings")
            .ghost()
            .compact()
            .tooltip("This collection's variables, auth, and headers")
            .on_click(cx.listener(|this, _, _, cx| {
                this.settings_open = !this.settings_open;
                cx.notify();
            }));

        let mut region = v_flex()
            .id("collection-settings")
            .flex_none()
            .border_t_1()
            .border_color(palette.border)
            .child(
                h_flex()
                    .flex_none()
                    .h(px(metrics.row_height))
                    .px(px(metrics.space_sm))
                    .gap(px(metrics.space_xs))
                    .items_center()
                    .child(toggle)
                    .child(div().flex_1())
                    .children(
                        unsaved
                            .then(|| div().text_xs().text_color(palette.warning).child("unsaved")),
                    ),
            );

        if self.settings_open {
            region = region
                .child(self.render_tabs(cx))
                .child(self.render_body(settings, cx))
                .child(self.render_footer(settings, cx));
        }

        Some(region.into_any_element())
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(settings) = self.settings.as_ref() else {
            return div().into_any_element();
        };
        let metrics = self.metrics;
        let current = settings.tab;

        h_flex()
            .px(px(metrics.space_sm))
            .pb(px(metrics.space_xs))
            .gap(px(metrics.space_xs))
            .children(
                [
                    SettingsTab::Variables,
                    SettingsTab::Headers,
                    SettingsTab::Auth,
                ]
                .into_iter()
                .map(|tab| {
                    let selected = tab == current;
                    Button::new(format!("collection-settings-tab-{tab:?}"))
                        .label(tab.label())
                        .compact()
                        .ghost()
                        .selected(selected)
                        .toggled(selected)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(settings) = this.settings.as_mut() {
                                settings.tab = tab;
                            }
                            cx.notify();
                        }))
                        .into_any_element()
                }),
            )
            .into_any_element()
    }

    fn render_body(&self, settings: &CollectionSettings, cx: &mut Context<Self>) -> AnyElement {
        let metrics = self.metrics;
        let palette = &self.palette;

        let content = match settings.tab {
            SettingsTab::Variables | SettingsTab::Headers => {
                let tab = settings.tab;
                let rows = settings.rows(tab);
                let mut list = v_flex()
                    .gap(px(metrics.space_xs))
                    .children(self.render_rows(tab, rows, cx));
                if rows.is_empty() {
                    list = list.child(
                        div()
                            .py(px(metrics.space_sm))
                            .text_xs()
                            .text_color(palette.muted)
                            .child(tab.empty()),
                    );
                }
                list.into_any_element()
            }
            SettingsTab::Auth => self.render_auth(settings, cx),
        };

        div()
            .id("collection-settings-body")
            // Eight rows of the tree's own height, so the band tracks zoom and
            // the tree keeps the rest of the pane.
            .max_h(px(metrics.row_height * 8.))
            .overflow_y_scrollbar()
            .px(px(metrics.space_sm))
            .child(content)
            .into_any_element()
    }

    fn render_rows(
        &self,
        tab: SettingsTab,
        rows: &[Row],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let palette = &self.palette;
        let metrics = self.metrics;

        rows.iter()
            .map(|row| {
                let id = row.id;
                let enabled = Checkbox::new(("collection-settings-enabled", id))
                    .checked(row.enabled)
                    .on_change(cx.listener(move |this, checked, _, cx| {
                        if let Some(row) = this.settings_row_mut(tab, id) {
                            row.enabled = *checked;
                        }
                        cx.notify();
                    }));

                let key = Input::new(&row.key)
                    .id(("collection-settings-key", id))
                    .flex_1();

                // A folded secret is a summary, not an input: the value stays in
                // the widget but is never painted.
                let value = if row.secret && !row.revealed {
                    div()
                        .flex_1()
                        .px(px(metrics.space_sm))
                        .py(px(metrics.space_xs))
                        .font_family(palette.mono_font.clone())
                        .text_sm()
                        .text_color(palette.muted)
                        .child("••••••")
                        .into_any_element()
                } else {
                    Input::new(&row.value)
                        .id(("collection-settings-value", id))
                        .flex_1()
                        .into_any_element()
                };

                let secret = Checkbox::new(("collection-settings-secret", id))
                    .label("secret")
                    .checked(row.secret)
                    .on_change(cx.listener(move |this, checked, _, cx| {
                        if let Some(row) = this.settings_row_mut(tab, id) {
                            row.secret = *checked;
                            row.revealed = false;
                        }
                        cx.notify();
                    }));

                let reveal = row.secret.then(|| {
                    Button::new(("collection-settings-reveal", id))
                        .icon(if row.revealed {
                            IconName::EyeOff
                        } else {
                            IconName::Eye
                        })
                        .ghost()
                        .compact()
                        .tooltip(if row.revealed {
                            "Hide this value"
                        } else {
                            "Reveal this value"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(row) = this.settings_row_mut(tab, id) {
                                row.revealed = !row.revealed;
                            }
                            cx.notify();
                        }))
                        .into_any_element()
                });

                let remove = Button::new(("collection-settings-remove", id))
                    .icon(IconName::Close)
                    .ghost()
                    .compact()
                    .tooltip("Remove this row")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(settings) = this.settings.as_mut() {
                            if let Some(rows) = settings.rows_mut(tab) {
                                rows.retain(|row| row.id != id);
                            }
                        }
                        cx.notify();
                    }));

                h_flex()
                    .id(("collection-settings-row", id))
                    .gap(px(metrics.space_sm))
                    .items_center()
                    .child(enabled)
                    .child(key)
                    .child(value)
                    .child(secret)
                    .children(reveal)
                    .child(remove)
                    .into_any_element()
            })
            .collect()
    }

    fn render_auth(&self, settings: &CollectionSettings, cx: &mut Context<Self>) -> AnyElement {
        let metrics = self.metrics;
        let muted = self.palette.muted;

        let strip = h_flex()
            .gap(px(metrics.space_xs))
            .pb(px(metrics.space_sm))
            .children(SCHEMES.into_iter().map(|(kind, label)| {
                let selected = settings.preserved_auth.is_none() && settings.auth_kind == kind;
                Button::new(format!("collection-auth-{kind:?}"))
                    .label(label)
                    .compact()
                    .ghost()
                    .selected(selected)
                    .toggled(selected)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(settings) = this.settings.as_mut() {
                            settings.preserved_auth = None;
                            settings.auth_kind = kind;
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }));

        let preserved = settings.preserved_auth.as_ref().map(|auth| {
            div()
                .pb(px(metrics.space_sm))
                .text_xs()
                .text_color(muted)
                .child(format!(
                    "This collection uses {}. Choosing a scheme replaces it.",
                    scheme_name(auth)
                ))
        });

        let field = |label: &'static str, input: &Entity<InputState>| {
            v_flex()
                .gap(px(metrics.space_xs))
                .flex_1()
                .child(div().text_xs().text_color(muted).child(label))
                .child(Input::new(input))
        };

        let fields: Vec<AnyElement> = match settings.auth_kind {
            AuthKind::Basic => vec![
                field("Username", &settings.auth_a).into_any_element(),
                field("Password", &settings.auth_b).into_any_element(),
            ],
            AuthKind::Bearer => vec![field("Token", &settings.auth_a).into_any_element()],
            AuthKind::ApiKey => vec![
                field("Header name", &settings.auth_a).into_any_element(),
                field("Key value", &settings.auth_b).into_any_element(),
            ],
            AuthKind::None | AuthKind::Inherit | AuthKind::OAuth2 => Vec::new(),
        };

        let key_location = (settings.preserved_auth.is_none()
            && settings.auth_kind == AuthKind::ApiKey)
            .then(|| {
                h_flex()
                    .pt(px(metrics.space_sm))
                    .gap(px(metrics.space_sm))
                    .items_center()
                    .child(div().text_sm().text_color(muted).child("Add to"))
                    .child(
                        Checkbox::new("collection-auth-in-query")
                            .label("query string")
                            .checked(settings.api_key_in_query)
                            .on_change(cx.listener(|this, checked, _, cx| {
                                if let Some(settings) = this.settings.as_mut() {
                                    settings.api_key_in_query = *checked;
                                }
                                cx.notify();
                            })),
                    )
            });

        let hint = match settings.auth_kind {
            AuthKind::Basic => {
                Some("Username and password, sent as an Authorization: Basic header.")
            }
            AuthKind::Bearer => Some("Token, sent as an Authorization: Bearer header."),
            AuthKind::ApiKey => Some(
                "Header name and key value, sent as a header unless \"query string\" is checked.",
            ),
            AuthKind::None | AuthKind::Inherit | AuthKind::OAuth2 => None,
        };

        v_flex()
            .gap(px(metrics.space_sm))
            .child(strip)
            .children(preserved)
            .child(h_flex().gap(px(metrics.space_sm)).children(fields))
            .children(key_location)
            .children(hint.map(|hint| div().text_xs().text_color(muted).child(hint)))
            .into_any_element()
    }

    fn render_footer(&self, settings: &CollectionSettings, cx: &mut Context<Self>) -> AnyElement {
        let metrics = self.metrics;
        let tab = settings.tab;

        let add = (tab != SettingsTab::Auth).then(|| {
            Button::new("collection-settings-add-row")
                .label("+ Add row")
                .ghost()
                .compact()
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(settings) = this.settings.as_mut() {
                        if let Some(rows) = settings.rows_mut(tab) {
                            rows.push(Row::blank(window, cx, tab));
                        }
                    }
                    cx.notify();
                }))
        });

        let save = Button::new("collection-settings-save")
            .label("Save")
            // The commit of a form, so primary is the one strong action here.
            .primary()
            .compact()
            .on_click(cx.listener(|this, _, window, cx| {
                this.save_settings(window, cx);
            }));

        h_flex()
            .px(px(metrics.space_sm))
            .pt(px(metrics.space_sm))
            .gap(px(metrics.space_sm))
            .items_center()
            .children(add)
            .child(div().flex_1())
            .child(save)
            .into_any_element()
    }

    fn settings_row_mut(&mut self, tab: SettingsTab, id: u64) -> Option<&mut Row> {
        self.settings
            .as_mut()
            .and_then(|settings| settings.rows_mut(tab))
            .and_then(|rows| rows.iter_mut().find(|row| row.id == id))
    }

    /// Fold the working copy into its collection and persist it. Emits
    /// `CollectionChanged` so the workspace re-reads the active tab's collection
    /// scope: an edited variable has to reach the next send.
    fn save_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(settings) = self.settings.as_ref() else {
            return false;
        };
        let id = settings.collection.clone();
        let variables = settings.variables_kvs(cx);
        let headers = settings.headers_kvs(cx);
        let auth = settings.auth(cx);

        let Some(collection) = self.collections.iter_mut().find(|each| each.id == id) else {
            return false;
        };
        collection.variables = variables;
        collection.headers = headers;
        collection.auth = auth;
        self.revision += 1;

        let saved = self.persist(&id, Some(window), cx);
        if saved {
            cx.emit(super::Event::CollectionChanged);
        }
        saved
    }

    /// Select the first collection and open the disclosure, so a `--preview
    /// collections` capture shows the settings without a click.
    pub(crate) fn preview_select_collection(&mut self, cx: &mut Context<Self>) {
        if let Some(collection) = self.collections.first() {
            self.selection = super::Selection::collection(&collection.id);
            self.settings_open = true;
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    // Named imports, not `super::*`: the supermodule glob-imports gpui-kit,
    // whose `#[test]` macro would shadow the builtin.
    use std::sync::Arc;

    use gpui_kit::{AppContext as _, TestAppContext};
    use restbook_core::{Collection, Kv, Store};
    use restbook_store::MemoryStore;

    use super::super::{CollectionsPanel, Selection};
    use crate::document::Document;

    /// A save folds the working copy into the collection on disk, so a request
    /// in it inherits the value the user typed rather than the one it replaced.
    #[gpui_kit::test]
    fn a_settings_save_writes_the_collection(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        crate::test_support::init_ui(cx);

        let store = Arc::new(MemoryStore::new());
        store
            .save_collection(&Collection {
                variables: vec![Kv::new("base_url", "https://a")],
                ..Collection::new("col-a", "A")
            })
            .unwrap();

        let for_panel: Arc<dyn Store> = store.clone();
        let document = cx.update(|cx| cx.new(|_| Document::new()));
        let (panel, cx) =
            cx.add_window_view(|window, cx| CollectionsPanel::new(for_panel, document, window, cx));

        // Selecting the collection queues the disclosure's load; a paint runs
        // the defer, and the next paint draws the loaded rows.
        cx.update(|window, cx| {
            panel.update(cx, |panel, cx| {
                panel.selection = Selection::collection("col-a");
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });

        cx.update(|window, cx| {
            panel.update(cx, |panel, cx| {
                let row = &panel
                    .settings
                    .as_ref()
                    .expect("the disclosure loaded for the selected collection")
                    .variables[0];
                row.value
                    .update(cx, |state, cx| state.set_value("https://b", window, cx));
                panel.save_settings(window, cx);
            });
        });

        let saved = store.collections().unwrap();
        assert_eq!(saved[0].variables[0].value, "https://b");
    }
}
