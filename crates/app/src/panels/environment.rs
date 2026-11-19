//! The Environment pane: environments, globals, and the variable inspector.
//!
//! A `{{name}}` can be answered from several scopes, and only the highest one
//! that defines it is what a send uses. The editor shows the user's rows; the
//! inspector names the scope each `{{name}}` resolves to, or flags it unresolved.
//! Rows are local widgets, not `kv_table`, since a row also carries a secret flag.

use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::component::button::*;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{h_flex, v_flex, Disableable as _};
use gpui_kit::*;

use restbook_core::{Environment, Kv};

use crate::document::Document;
use crate::icons::{self, IconName};
use crate::theme::{Metrics, Palette};
use crate::variables::{references, secret_of, Variables};

/// What the editor is showing. `Globals` is the pseudo-environment the app
/// always has; `New` is an environment that does not exist in the store yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum Target {
    #[default]
    Globals,
    Environment(String),
    New,
}

/// One editable row. `revealed` is view state only: a secret row still holds
/// its real value in the input, it is just not painted while folded.
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
    fn blank(window: &mut Window, cx: &mut App) -> Self {
        Self {
            id: new_id(),
            key: new_input(window, cx, "name"),
            value: new_input(window, cx, "value"),
            enabled: true,
            secret: false,
            revealed: false,
        }
    }

    fn from_kv(kv: &Kv, window: &mut Window, cx: &mut App) -> Self {
        let mut row = Self::blank(window, cx);
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

pub(crate) struct EnvironmentPanel {
    palette: Palette,
    metrics: Metrics,
    variables: Entity<Variables>,
    /// The request being edited, observed so the inspector follows it.
    document: Entity<Document>,
    /// The name of a new environment. Existing environments show their stored
    /// name instead, so a rename can never orphan the old row.
    name_input: Entity<InputState>,
    rows: Vec<Row>,
    target: Target,
    /// A target waiting for the next render. Only `delete_environment` still
    /// queues: the workspace calls it from its confirmation dialog with no
    /// window in hand, so it cannot defer the input writes. Every handler that
    /// does have a window defers through `defer_load` instead.
    pending: Option<Target>,
    focus_handle: FocusHandle,
}

impl EventEmitter<gpui_kit::component::dock::PanelEvent> for EnvironmentPanel {}

/// What the pane asks the workspace to do for it. The pane cannot open a
/// window-level dialog itself — it is a dock panel with no window on every path
/// — so it names the environment and the workspace raises the confirmation.
pub(crate) enum Event {
    /// The user asked to delete the environment being edited.
    ConfirmDelete(String),
}

impl EventEmitter<Event> for EnvironmentPanel {}

/// How the dock sees this pane.
impl gpui_kit::component::dock::BasePanel for EnvironmentPanel {
    fn panel_name(&self) -> &'static str {
        "environment"
    }
}

/// Presentation half of the dock seam. The pane draws its own header, so the
/// dock does not put a one-panel title bar above it.
impl gpui_kit::component::dock::Panel for EnvironmentPanel {
    fn tab_name(&self, _cx: &App) -> Option<SharedString> {
        Some("Environment".into())
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // `Braces` rather than `Globe`: this pane edits the `{{name}}` values
        // themselves, which is what the glyph stands for everywhere else.
        icons::title(IconName::Braces, "Environment", &self.metrics)
    }

    fn title_bar(&self, _cx: &App) -> bool {
        false
    }

    fn inner_padding(&self, _cx: &App) -> bool {
        false
    }

    /// The pane's own items in the dock's ellipsis menu: the three header
    /// buttons, reused so the menu and the header cannot disagree. Delete is
    /// the one verb that only applies to a stored environment, so it is
    /// disabled while the editor shows the globals or a new, unsaved one.
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let panel = cx.entity();
        let delete_name = match &self.target {
            Target::Environment(name) => Some(name.clone()),
            Target::Globals | Target::New => None,
        };
        // Activating the globals is a no-op: there is no environment to make
        // active, and the picker's `(none)` is the verb for that.
        let activatable = !matches!(self.target, Target::Globals);

        let new = panel.clone();
        let mut menu = menu.item(PopupMenuItem::new("New environment").on_click(
            move |_, window, cx| {
                new.update(cx, |this, cx| this.follow(Target::New, window, cx));
            },
        ));

        let save = panel.clone();
        menu = menu.item(PopupMenuItem::new("Save").on_click(move |_, window, cx| {
            save.update(cx, |this, cx| {
                this.save(window, cx);
            });
        }));

        let activate = panel.clone();
        menu = menu.item(
            PopupMenuItem::new("Activate")
                .disabled(!activatable)
                .on_click(move |_, window, cx| {
                    activate.update(cx, |this, cx| this.activate(window, cx));
                }),
        );

        menu = menu.separator();
        let delete = panel;
        menu.item(
            PopupMenuItem::new("Delete environment")
                .icon(IconName::Trash)
                .disabled(delete_name.is_none())
                .on_click(move |_, _, cx| {
                    if let Some(name) = delete_name.clone() {
                        delete.update(cx, |_, cx| cx.emit(Event::ConfirmDelete(name)));
                    }
                }),
        )
    }
}

impl Focusable for EnvironmentPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EnvironmentPanel {
    pub(crate) fn new(
        variables: Entity<Variables>,
        document: Entity<Document>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Redraw when either the variables or the request change: the editor
        // reads the first and the inspector reads both.
        cx.observe(&variables, |_, _, cx| cx.notify()).detach();
        cx.observe(&document, |_, _, cx| cx.notify()).detach();

        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("name"));

        Self {
            palette: Palette::new(cx),
            metrics: Metrics::current(cx),
            variables,
            document,
            name_input,
            rows: Vec::new(),
            target: Target::Globals,
            pending: None,
            focus_handle: cx.focus_handle(),
        }
        .loaded(window, cx)
    }

    /// Load the stored globals before the first frame: without this the pane
    /// opened on an empty editor, and a save of that empty editor would
    /// overwrite the user's globals.
    fn loaded(mut self, window: &mut Window, cx: &mut Context<Self>) -> Self {
        self.load(&Target::Globals, window, cx);
        self
    }

    /// Rebuild the cached palette after a theme or zoom change.
    pub(crate) fn refresh_theme(&mut self, cx: &App) {
        self.palette = Palette::new(cx);
        self.metrics = Metrics::current(cx);
    }

    /// Point the editor at `target` after this frame, where the input writes
    /// have a window in hand.
    fn follow(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.defer_load(target, window, cx);
    }

    /// Write `target`'s rows after the current frame. Loading builds entities,
    /// sets input values, and focuses, all of which need a window; doing it
    /// here keeps those writes out of `render`.
    fn defer_load(&mut self, target: Target, window: &Window, cx: &mut Context<Self>) {
        cx.defer_in(window, move |this, window, cx| {
            this.load(&target, window, cx);
            // A new environment is named first, so put the caret in the name
            // field instead of making the user click it.
            if matches!(&target, Target::New) {
                this.name_input
                    .update(cx, |state, cx| state.focus(window, cx));
            }
            this.target = target;
            cx.notify();
        });
    }

    /// Write `target`'s stored rows into the widgets.
    fn load(&mut self, target: &Target, window: &mut Window, cx: &mut Context<Self>) {
        let (name, vars) = match target {
            Target::Globals => (String::new(), self.variables.read(cx).globals().to_vec()),
            Target::Environment(name) => self
                .variables
                .read(cx)
                .environments()
                .iter()
                .find(|each| &each.name == name)
                .map(|each| (each.name.clone(), each.vars.clone()))
                .unwrap_or_else(|| (name.clone(), Vec::new())),
            Target::New => (String::new(), Vec::new()),
        };

        self.name_input
            .update(cx, |state, cx| state.set_value(name, window, cx));
        self.rows = vars.iter().map(|kv| Row::from_kv(kv, window, cx)).collect();
    }

    fn edited_vars(&self, cx: &App) -> Vec<Kv> {
        self.rows.iter().map(|row| row.to_kv(cx)).collect()
    }

    /// Whether what the editor holds differs from the store. A new environment
    /// has nothing to differ from, so it is never marked.
    fn is_unsaved(&self, cx: &App) -> bool {
        let variables = self.variables.read(cx);
        match &self.target {
            Target::Globals => self.edited_vars(cx).as_slice() != variables.globals(),
            Target::Environment(name) => match variables
                .environments()
                .iter()
                .find(|each| &each.name == name)
            {
                Some(environment) => self.edited_vars(cx).as_slice() != environment.vars.as_slice(),
                None => true,
            },
            Target::New => false,
        }
    }

    /// Fold the rows into an environment (or the globals) and persist them
    /// through `Variables`, which owns the store write. Returns the target the
    /// editor should reload, or `None` when saving changed nothing visible
    /// (globals are already the editor's target).
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Target> {
        let vars = self.edited_vars(cx);
        let reload = match self.target.clone() {
            Target::Globals => {
                self.variables
                    .update(cx, |vars_store, cx| vars_store.save_globals(vars, cx));
                None
            }
            Target::Environment(name) => {
                let environment = Environment {
                    name: name.clone(),
                    vars,
                };
                self.variables.update(cx, |vars_store, cx| {
                    vars_store.save_environment(environment, cx)
                });
                Some(Target::Environment(name))
            }
            Target::New => {
                let typed = self.name_input.read(cx).value().trim().to_owned();
                let name = if typed.is_empty() {
                    "New environment".to_owned()
                } else {
                    typed
                };
                let environment = Environment {
                    name: name.clone(),
                    vars,
                };
                self.variables.update(cx, |vars_store, cx| {
                    vars_store.save_environment(environment, cx)
                });
                Some(Target::Environment(name))
            }
        };
        // Re-read the saved rows so the editor matches the store.
        if let Some(target) = &reload {
            self.defer_load(target.clone(), window, cx);
        }
        cx.notify();
        reload
    }

    /// Delete the named environment. Called by the workspace once the user
    /// confirms the alert dialog; the pane no longer arms a two-click delete
    /// itself, because a dialog is where a destructive act belongs.
    ///
    /// This is the one handler with no window in hand — `perform_delete`
    /// calls it inside an entity update — so it queues the reload for the next
    /// render rather than deferring. Giving it a window would change a
    /// workspace call site.
    pub(crate) fn delete_environment(&mut self, name: &str, cx: &mut Context<Self>) {
        self.variables
            .update(cx, |vars_store, cx| vars_store.delete_environment(name, cx));
        self.pending = Some(Target::Globals);
        cx.notify();
    }

    /// Make the environment being edited the one a send resolves against. A new
    /// environment has no stored name yet, so it is saved first.
    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = match self.target.clone() {
            Target::Environment(name) => Some(name),
            Target::New => match self.save(window, cx) {
                Some(Target::Environment(name)) => Some(name),
                _ => None,
            },
            Target::Globals => None,
        };

        if let Some(name) = name {
            self.variables
                .update(cx, |vars_store, cx| vars_store.set_active(Some(name), cx));
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;
        let active = self.variables.read(cx).active().map(str::to_owned);

        let panel = cx.entity();
        let variables = self.variables.clone();
        let environments: Vec<String> = variables
            .read(cx)
            .environments()
            .iter()
            .map(|each| each.name.clone())
            .collect();
        let selected_none = active.is_none();
        let checked_name = active.clone();

        let menu = move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            menu = menu.item(
                PopupMenuItem::new("(none)")
                    .checked(selected_none)
                    .on_click({
                        let panel = panel.clone();
                        let variables = variables.clone();
                        move |_, window, cx| {
                            variables.update(cx, |vars_store, cx| vars_store.set_active(None, cx));
                            panel.update(cx, |this, cx| this.follow(Target::Globals, window, cx));
                        }
                    }),
            );

            if environments.is_empty() {
                menu = menu.item(PopupMenuItem::new("No environments yet.").disabled(true));
            }

            for name in &environments {
                let select = name.clone();
                let panel = panel.clone();
                let variables = variables.clone();
                menu = menu.item(
                    PopupMenuItem::new(name.clone())
                        .checked(checked_name.as_deref() == Some(name.as_str()))
                        .on_click(move |_, window, cx| {
                            variables.update(cx, |vars_store, cx| {
                                vars_store.set_active(Some(select.clone()), cx)
                            });
                            panel.update(cx, |this, cx| {
                                this.follow(Target::Environment(select.clone()), window, cx)
                            });
                        }),
                );
            }
            menu
        };

        let picker = Button::new("environment-picker")
            .label(active.unwrap_or_else(|| "(none)".to_owned()))
            .compact()
            .flex_1()
            .min_w_0()
            .dropdown_caret(true)
            .dropdown_menu(menu);

        let new = Button::new("environment-new")
            .label("New")
            .ghost()
            .compact()
            .on_click(cx.listener(|this, _, window, cx| this.follow(Target::New, window, cx)));

        let deletable = matches!(self.target, Target::Environment(_));
        // Delete asks the workspace for a confirmation dialog; the button no
        // longer flips to "Delete?" on a first click.
        let delete = Button::new("environment-delete")
            .label("Delete")
            .ghost()
            .compact()
            .disabled(!deletable)
            .on_click(cx.listener(|this, _, _, cx| {
                if let Target::Environment(name) = this.target.clone() {
                    cx.emit(Event::ConfirmDelete(name));
                }
            }));

        let activate = Button::new("environment-activate")
            .label("Active")
            .ghost()
            .compact()
            .on_click(cx.listener(|this, _, window, cx| this.activate(window, cx)));

        // Two rows on purpose. The pane lives in the right dock, and a title,
        // a picker, and three buttons do not fit on one line without squeezing
        // the controls that name a new environment.
        v_flex()
            .px(px(self.metrics.space_md))
            .py(px(self.metrics.space_sm))
            .gap(px(self.metrics.space_sm))
            .border_b_1()
            .border_color(palette.border)
            .child(
                h_flex()
                    .gap(px(self.metrics.space_sm))
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Environment"),
                    )
                    .child(
                        h_flex()
                            .gap(px(self.metrics.space_xs))
                            .items_center()
                            .child(new)
                            .child(delete)
                            .child(activate),
                    ),
            )
            .child(h_flex().child(picker))
            .into_any_element()
    }

    fn render_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.rows
            .iter()
            .map(|row| {
                let id = row.id;
                let enabled = Checkbox::new(("environment-enabled", id))
                    .checked(row.enabled)
                    .on_change(cx.listener(move |this, checked, _, cx| {
                        if let Some(row) = this.rows.iter_mut().find(|row| row.id == id) {
                            row.enabled = *checked;
                        }
                        cx.notify();
                    }));

                let key = Input::new(&row.key).id(("environment-key", id)).flex_1();

                // A folded secret is a summary, not an input: the real value
                // stays in the widget but is never painted.
                let value = if row.secret && !row.revealed {
                    div()
                        .flex_1()
                        .px(px(self.metrics.space_sm))
                        .py(px(self.metrics.space_xs))
                        .font_family(self.palette.mono_font.clone())
                        .text_sm()
                        .text_color(self.palette.muted)
                        .child("••••••")
                        .into_any_element()
                } else {
                    Input::new(&row.value)
                        .id(("environment-value", id))
                        .flex_1()
                        .into_any_element()
                };

                let secret = Checkbox::new(("environment-secret", id))
                    .label("secret")
                    .checked(row.secret)
                    .on_change(cx.listener(move |this, checked, _, cx| {
                        if let Some(row) = this.rows.iter_mut().find(|row| row.id == id) {
                            row.secret = *checked;
                            row.revealed = false;
                        }
                        cx.notify();
                    }));

                let reveal = row.secret.then(|| {
                    // An eye, not a word: `reveal`/`hide` sitting between the
                    // "secret" checkbox and the row's `x` read as two more
                    // labels in a row that already has one. The glyph’s state
                    // says the same thing in a fixed slot, so the checkbox and
                    // the remove button do not shift when it is toggled.
                    Button::new(("environment-reveal", id))
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
                            if let Some(row) = this.rows.iter_mut().find(|row| row.id == id) {
                                row.revealed = !row.revealed;
                            }
                            cx.notify();
                        }))
                        .into_any_element()
                });

                let remove = Button::new(("environment-remove", id))
                    // A glyph, not a bare `x`: the same action is `Close` in
                    // every other table.
                    .icon(IconName::Close)
                    .ghost()
                    .compact()
                    .tooltip("Remove this row")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.rows.retain(|row| row.id != id);
                        cx.notify();
                    }));

                h_flex()
                    .id(("environment-row", id))
                    .gap(px(self.metrics.space_sm))
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

    fn render_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette.clone();

        let mut heading = match &self.target {
            Target::Globals => h_flex()
                .gap(px(self.metrics.space_sm))
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Globals"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.muted)
                        .child("workspace-wide, lowest precedence"),
                ),
            Target::Environment(name) => h_flex()
                .gap(px(self.metrics.space_sm))
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(name.clone()),
                ),
            Target::New => h_flex()
                .gap(px(self.metrics.space_sm))
                .items_center()
                .child(div().text_xs().text_color(palette.muted).child("Name"))
                .child(Input::new(&self.name_input).id("environment-name").flex_1()),
        };

        if self.is_unsaved(cx) {
            heading = heading.child(div().text_xs().text_color(palette.warning).child("unsaved"));
        }

        let add = Button::new("environment-add-row")
            .label("+ Add row")
            .ghost()
            .compact()
            .on_click(cx.listener(|this, _, window, cx| {
                this.rows.push(Row::blank(window, cx));
                cx.notify();
            }));

        let save = Button::new("environment-save")
            .label("Save")
            .primary()
            .compact()
            .on_click(cx.listener(|this, _, window, cx| {
                this.save(window, cx);
            }));

        v_flex()
            .gap(px(self.metrics.space_sm))
            .px(px(self.metrics.space_md))
            .py(px(self.metrics.space_sm))
            .border_b_1()
            .border_color(palette.border)
            .child(heading)
            .children(
                (matches!(self.target, Target::Globals)
                    && self.variables.read(cx).environments().is_empty())
                .then(|| {
                    div()
                        .text_xs()
                        .text_color(palette.muted)
                        .child("No environments yet.")
                        .into_any_element()
                }),
            )
            .child(
                v_flex()
                    .gap(px(self.metrics.space_xs))
                    .children(self.render_rows(cx)),
            )
            .child(
                h_flex()
                    .gap(px(self.metrics.space_sm))
                    .items_center()
                    .child(add)
                    .child(save),
            )
            .into_any_element()
    }

    fn render_inspector(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette.clone();
        let request = self.document.read(cx).request.clone();
        let variables = self.variables.read(cx);
        let scoped = variables.scoped(&request);

        // `references` previews every field to find the names, so a name that
        // nothing answers is listed (and flagged) rather than dropped.
        let names = references(&request);

        let mut section = v_flex()
            .gap(px(self.metrics.space_xs))
            .px(px(self.metrics.space_md))
            .py(px(self.metrics.space_sm))
            .child(
                h_flex()
                    .gap(px(self.metrics.space_sm))
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Variables"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.muted)
                            .child(names.len().to_string()),
                    ),
            );

        if names.is_empty() {
            return section
                .child(self.message("No variables referenced."))
                .into_any_element();
        }

        let rows = names
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let resolved = scoped.lookup(&name);
                let scope = resolved.as_ref().map(|each| each.scope);
                let secret = secret_of(variables, &name, scope);
                let unresolved = resolved.is_none();
                let (value, scope_label) = match resolved {
                    Some(resolved) if secret => ("•••".to_owned(), resolved.scope.label()),
                    Some(resolved) => (resolved.value, resolved.scope.label()),
                    None => (format!("{{{{{name}}}}}"), "unresolved"),
                };
                let value_color = if unresolved {
                    palette.danger
                } else {
                    palette.foreground
                };

                h_flex()
                    .id(("variable-row", index))
                    .gap(px(self.metrics.space_md))
                    .py(px(self.metrics.space_xs))
                    .items_center()
                    .border_b_1()
                    .border_color(palette.border)
                    .child(div().w_40().truncate().text_sm().child(name))
                    .child(
                        div()
                            .w(px(96.))
                            .text_xs()
                            .text_color(if unresolved {
                                palette.danger
                            } else {
                                palette.muted
                            })
                            .child(scope_label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .font_family(palette.mono_font.clone())
                            .text_sm()
                            .text_color(value_color)
                            .child(value),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        section = section.child(v_flex().children(rows));
        section.into_any_element()
    }

    fn message(&self, text: &str) -> AnyElement {
        div()
            .py(px(self.metrics.space_xs))
            .text_sm()
            .text_color(self.palette.muted)
            .child(text.to_owned())
            .into_any_element()
    }
}

impl Render for EnvironmentPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // `delete_environment` is the one handler reached without a window (the
        // workspace calls it from its confirmation dialog), so its load is
        // queued and flushed here. Every other path defers with `defer_load`.
        if let Some(target) = self.pending.take() {
            self.load(&target, window, cx);
            self.target = target;
        }

        v_flex()
            .size_full()
            .bg(self.palette.sidebar)
            .track_focus(&self.focus_handle)
            .child(self.render_header(cx))
            .child(
                v_flex()
                    .id("environment-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.render_editor(cx))
                    .child(self.render_inspector(cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    // Named import, not `super::*`: the supermodule glob-imports gpui-kit,
    // whose `#[test]` macro would shadow the builtin.
    use super::new_id;

    #[test]
    fn an_id_is_never_reused() {
        let first = new_id();
        let second = new_id();
        assert!(second > first);
    }
}
