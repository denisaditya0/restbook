//! The developer preview: one pane over fixture data, on demand.
//!
//! `TestAppContext` render tests prove a pane lays out and paints, but nothing
//! puts it on a screen without the whole window, dock, store and updater. This
//! module fills that gap. `main` reads `--preview <pane>` from plain
//! `std::env::args`, and instead of the workspace the window root becomes one
//! pane wired exactly as the workspace wires it: a fixture [`Document`], a
//! [`MemoryStore`] holding a few seeded sends, and the real [`Settings`] entity
//! (so the preview opens on the user's theme and zoom).
//!
//! `--preview all` lays every pane out at once, which is how a change to the
//! shared theme is checked in one window. Nothing here sends a request, writes
//! the user's store, or saves a layout: the store is in memory and the panes are
//! the production ones.
//!
//! Degrading gracefully:
//!
//! - A pane name the preview does not know opens a window naming the valid ones
//!   rather than falling through to a normal launch, which would look like the
//!   flag was ignored.
//! - A pane that cannot be built is reported in that pane's cell instead of
//!   taking the window down. Today every constructor succeeds and the fixture
//!   loader's `Result`s are only logged, but [`build_pane`] returns a `Result`
//!   so a pane with genuinely fallible setup has somewhere to say so.

use std::sync::Arc;

use chrono::{Duration, Utc};
use gpui_kit::component::{h_flex, v_flex, TitleBar};
use gpui_kit::*;

use restbook_core::{
    Body, Collection, CollectionItem, Draft, Environment, Folder, HistoryEntry, Kv, Method, Origin,
    RequestDef, ResponseRecord, SavedRequest, Store,
};
use restbook_store::MemoryStore;

use crate::document::Document;
use crate::panels::collections::CollectionsPanel;
use crate::panels::environment::EnvironmentPanel;
use crate::panels::history::HistoryPanel;
use crate::panels::request::RequestPanel;
use crate::panels::response::ResponsePanel;
use crate::theme::{self, Metrics, Palette};
use crate::variables::Variables;
use crate::workspace::settings::{Settings, SETTINGS_FILE};
use crate::workspace::storage;

/// The URL the fixture request names. It is never contacted: the response in
/// the fixture document is the one on screen.
const FIXTURE_URL: &str = "https://sample/echo";

/// The saved request the fixture document pretends to be, so the response
/// pane's history control has rows of its own to list.
const FIXTURE_NODE: &str = "req-echo";

/// What the command line asked the app for.
#[derive(Clone)]
pub(crate) enum Invocation {
    /// No `--preview` on the command line: the normal workspace launch.
    Launch,
    /// `--preview <pane>` or `--preview all`.
    Show(Preview),
    /// `--preview` with a name this module does not know.
    Unknown(String),
}

/// Which panes the preview window shows.
#[derive(Clone, Copy)]
pub(crate) enum Preview {
    /// One pane filling the window.
    One(Pane),
    /// Every pane in a grid, so a shared change is visible everywhere at once.
    All,
}

/// A pane the preview can build. The names are the `--preview` arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pane {
    History,
    Request,
    Response,
    Environment,
    Collections,
}

impl Pane {
    /// Every pane, in the order `--preview all` lays them out. The list is also
    /// what an unknown name is answered with.
    const ALL: [Pane; 5] = [
        Pane::History,
        Pane::Request,
        Pane::Response,
        Pane::Environment,
        Pane::Collections,
    ];

    /// The command-line name, lowercase and stable.
    fn name(self) -> &'static str {
        match self {
            Pane::History => "history",
            Pane::Request => "request",
            Pane::Response => "response",
            Pane::Environment => "environment",
            Pane::Collections => "collections",
        }
    }

    /// The label drawn above a pane in `--preview all`.
    fn label(self) -> &'static str {
        match self {
            Pane::History => "History",
            Pane::Request => "Request",
            Pane::Response => "Response",
            Pane::Environment => "Environment",
            Pane::Collections => "Collections",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|pane| pane.name() == name)
    }
}

/// Read `--preview` out of the process arguments.
///
/// Plain `std::env::args` rather than a parser crate: this is one developer flag
/// on a binary that otherwise takes none, and the app must not grow an argument
/// vocabulary by accident.
pub(crate) fn invocation<I: IntoIterator<Item = String>>(args: I) -> Invocation {
    let mut args = args.into_iter().skip(1);
    while let Some(argument) = args.next() {
        let value = if argument == "--preview" {
            match args.next() {
                Some(value) => value,
                // The missing name is reported, not ignored: a window saying so
                // is how the developer sees the typo.
                None => return Invocation::Unknown(String::new()),
            }
        } else if let Some(value) = argument.strip_prefix("--preview=") {
            value.to_owned()
        } else {
            continue;
        };

        if value == "all" {
            return Invocation::Show(Preview::All);
        }
        return match Pane::parse(&value) {
            Some(pane) => Invocation::Show(Preview::One(pane)),
            None => Invocation::Unknown(value),
        };
    }
    Invocation::Launch
}

/// The window title for an invocation: `None` keeps the product name.
pub(crate) fn title(invocation: &Invocation) -> Option<String> {
    match invocation {
        Invocation::Launch => None,
        Invocation::Show(Preview::One(pane)) => Some(format!(
            "{} — preview: {}",
            crate::branding::NAME,
            pane.name()
        )),
        Invocation::Show(Preview::All) => {
            Some(format!("{} — preview: all panes", crate::branding::NAME))
        }
        Invocation::Unknown(_) => {
            Some(format!("{} — preview: unknown pane", crate::branding::NAME))
        }
    }
}

/// One line for the interaction log, so a launch that opens the preview is
/// distinguishable from a normal one in the file a bug report carries.
pub(crate) fn summary(invocation: &Invocation) -> String {
    match invocation {
        Invocation::Launch => "launch: workspace".to_owned(),
        Invocation::Show(Preview::One(pane)) => format!("preview: {}", pane.name()),
        Invocation::Show(Preview::All) => "preview: all panes".to_owned(),
        Invocation::Unknown(name) if name.is_empty() => {
            "preview: --preview has no pane name".to_owned()
        }
        Invocation::Unknown(name) => format!("preview: unknown pane {name}"),
    }
}

/// The window root when `--preview` was passed.
pub(crate) struct PreviewRoot {
    /// The panes this window was asked for. Empty for an unknown name, which
    /// renders `problem` instead.
    slots: Vec<PreviewSlot>,
    /// Whether the window is one pane or the grid.
    layout: Option<Preview>,
    /// Why there is no pane, already written for the screen.
    problem: Option<String>,
}

struct PreviewSlot {
    pane: Pane,
    /// `Err` is a pane that could not be built; see the module comment.
    view: Result<PaneView, String>,
}

/// A built pane. The variants exist because the panes are different entity
/// types; the shell renders whichever one it holds.
#[derive(Clone)]
enum PaneView {
    History(Entity<HistoryPanel>),
    Request(Entity<RequestPanel>),
    Response(Entity<ResponsePanel>),
    Environment(Entity<EnvironmentPanel>),
    Collections(Entity<CollectionsPanel>),
}

impl PaneView {
    fn into_element(self) -> AnyElement {
        match self {
            PaneView::History(view) => view.into_any_element(),
            PaneView::Request(view) => view.into_any_element(),
            PaneView::Response(view) => view.into_any_element(),
            PaneView::Environment(view) => view.into_any_element(),
            PaneView::Collections(view) => view.into_any_element(),
        }
    }
}

/// The shared state every pane is wired to.
struct Fixture {
    store: Arc<dyn Store>,
    document: Entity<Document>,
    variables: Entity<Variables>,
    settings: Entity<Settings>,
}

impl PreviewRoot {
    pub(crate) fn new(invocation: Invocation, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state_dir = storage::data_dir();

        let store = Arc::new(MemoryStore::new());
        seed_store(&store);
        let store: Arc<dyn Store> = store;

        let document = cx.new(|_| fixture_document());
        let variables = cx.new(|cx| Variables::new(store.clone(), cx));
        let settings = cx.new(|_| Settings::new(state_dir.join(SETTINGS_FILE)));

        // The panes snapshot their palette in their constructors, so the theme
        // has to be installed first — and the user's own, so the preview is
        // what the app would look like rather than the component defaults.
        let current = settings.read(cx).current().clone();
        theme::ThemeRegistry::install(
            cx,
            &storage::load_theme_text(current.theme.as_deref()),
            current.zoom,
            current.appearance,
        );

        let fixture = Fixture {
            store,
            document,
            variables,
            settings,
        };

        let (layout, problem) = match invocation {
            Invocation::Show(preview) => (Some(preview), None),
            Invocation::Launch => (
                None,
                Some("The preview window was opened without --preview.".to_owned()),
            ),
            Invocation::Unknown(name) => (None, Some(unknown_message(&name))),
        };

        let panes: Vec<Pane> = match layout {
            Some(Preview::One(pane)) => vec![pane],
            Some(Preview::All) => Pane::ALL.to_vec(),
            None => Vec::new(),
        };
        let slots = panes
            .into_iter()
            .map(|pane| PreviewSlot {
                pane,
                view: build_pane(pane, &fixture, window, cx),
            })
            .collect();

        Self {
            slots,
            layout,
            problem,
        }
    }

    /// The one-line label above the pane: what is showing and that it is
    /// fixture data.
    ///
    /// It is a [`TitleBar`] rather than a plain strip so the preview window
    /// still drags and closes: the platform titlebar is hidden for the real
    /// window (the workspace draws its own), and a preview without one would
    /// have no way to move or close it.
    fn render_bar(&self, cx: &App) -> AnyElement {
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);
        let (title, hint) = match self.layout {
            Some(Preview::One(pane)) => (
                format!("Preview · {}", pane.label()),
                format!("fixture {FIXTURE_URL} · nothing is sent"),
            ),
            Some(Preview::All) => (
                "Preview · all panes".to_owned(),
                format!("fixture {FIXTURE_URL} · nothing is sent"),
            ),
            None => ("Preview".to_owned(), "no pane was selected".to_owned()),
        };

        TitleBar::new()
            .bg(palette.title_bar)
            .border_color(palette.border)
            .gap(px(metrics.space_sm))
            .px(px(metrics.space_md))
            .text_color(palette.foreground)
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_xs()
                    .text_color(palette.muted)
                    .child(hint),
            )
            .into_any_element()
    }

    /// One grid cell: the pane's label and the pane, or whatever stood in for
    /// the pane.
    fn render_cell(&self, pane: Pane, cx: &App) -> AnyElement {
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);
        let body = match self.slots.iter().find(|slot| slot.pane == pane) {
            Some(slot) => view_element(&slot.view, cx),
            None => message("This pane is not part of the preview.", cx),
        };

        v_flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .border_1()
            .border_color(palette.border)
            .child(
                div()
                    .flex_none()
                    .px(px(metrics.space_sm))
                    .py(px(metrics.space_xs))
                    .bg(palette.tab_active)
                    .text_xs()
                    .text_color(palette.muted)
                    .child(pane.label()),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    fn render_body(&self, cx: &App) -> AnyElement {
        if let Some(problem) = &self.problem {
            return message(problem.as_str(), cx);
        }

        match self.layout {
            Some(Preview::One(_)) => {
                let body = match self.slots.first() {
                    Some(slot) => view_element(&slot.view, cx),
                    None => message("This pane is not part of the preview.", cx),
                };
                v_flex().flex_1().min_h_0().child(body).into_any_element()
            }
            // Three narrow panes over the two wide ones: the pair that share
            // the widest content (request and response) gets a full half each.
            Some(Preview::All) => v_flex()
                .flex_1()
                .min_h_0()
                .child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .child(self.render_cell(Pane::History, cx))
                        .child(self.render_cell(Pane::Collections, cx))
                        .child(self.render_cell(Pane::Environment, cx)),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .child(self.render_cell(Pane::Request, cx))
                        .child(self.render_cell(Pane::Response, cx)),
                )
                .into_any_element(),
            None => message("Nothing to preview.", cx),
        }
    }
}

impl Render for PreviewRoot {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::new(cx);
        v_flex()
            .size_full()
            .bg(palette.background)
            .child(self.render_bar(cx))
            .child(self.render_body(cx))
    }
}

/// The pane, or the reason it is absent. A failed pane leaves the rest of the
/// window usable (`--preview all`) instead of aborting the launch.
fn view_element(view: &Result<PaneView, String>, cx: &App) -> AnyElement {
    match view {
        Ok(view) => view.clone().into_element(),
        Err(error) => message(error.as_str(), cx),
    }
}

/// A centred sentence in the pane's place.
fn message(text: &str, cx: &App) -> AnyElement {
    let palette = Palette::new(cx);
    let metrics = Metrics::current(cx);
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p(px(metrics.space_lg))
        .child(
            div()
                .text_sm()
                .text_color(palette.muted)
                .child(text.to_owned()),
        )
        .into_any_element()
}

fn unknown_message(name: &str) -> String {
    let names = Pane::ALL
        .iter()
        .map(|pane| pane.name())
        .collect::<Vec<_>>()
        .join(", ");
    if name.is_empty() {
        format!("--preview needs a pane name. Try one of: {names}, or all.")
    } else {
        format!("'{name}' is not a preview pane. Try one of: {names}, or all.")
    }
}

/// Build one pane against the fixture, wired the way `workspace/mod.rs` wires
/// the production ones.
///
/// Every arm currently succeeds. The `Result` is the extension point: a pane
/// whose setup can fail (a fixture file that must parse, say) returns the
/// reason here and the shell shows it in that pane's place.
fn build_pane(
    pane: Pane,
    fixture: &Fixture,
    window: &mut Window,
    cx: &mut Context<PreviewRoot>,
) -> Result<PaneView, String> {
    Ok(match pane {
        Pane::History => PaneView::History(cx.new(|cx| {
            HistoryPanel::new(fixture.store.clone(), fixture.settings.clone(), window, cx)
        })),
        Pane::Request => PaneView::Request(cx.new(|cx| {
            RequestPanel::new(
                fixture.document.clone(),
                fixture.variables.clone(),
                window,
                cx,
            )
        })),
        Pane::Response => PaneView::Response(cx.new(|cx| {
            ResponsePanel::new(
                fixture.document.clone(),
                fixture.store.clone(),
                fixture.settings.clone(),
                cx,
            )
        })),
        Pane::Environment => PaneView::Environment(cx.new(|cx| {
            EnvironmentPanel::new(
                fixture.variables.clone(),
                fixture.document.clone(),
                window,
                cx,
            )
        })),
        Pane::Collections => PaneView::Collections(cx.new(|cx| {
            CollectionsPanel::new(
                fixture.store.clone(),
                fixture.document.clone(),
                fixture.variables.clone(),
                window,
                cx,
            )
        })),
    })
}

/// The request every pane reads: a small `POST` with a body, a header, and a
/// query parameter, so the editor's tabs all have something in them.
fn fixture_request() -> RequestDef {
    let mut request = RequestDef::new(Method::Post, FIXTURE_URL);
    request.name = "Echo (preview)".to_owned();
    request.params = vec![Kv::new("page", "1")];
    request.headers = vec![
        Kv::new("content-type", "application/json"),
        Kv::new("x-fixture", "restbook-preview"),
    ];
    request.body = Body::Raw {
        mime: "application/json".to_owned(),
        text: "{\n  \"message\": \"hello from the preview fixture\"\n}".to_owned(),
    };
    request
}

/// The document every pane observes: a finished send, so the response pane is
/// in its completed state on the first frame, and an owner so the history
/// control lists the seeded sends.
fn fixture_document() -> Document {
    let mut document = Document::new();
    document.request = fixture_request();
    document.last_url = Some(FIXTURE_URL.to_owned());
    document.origin = Origin::Saved {
        node: FIXTURE_NODE.to_owned(),
    };
    document.response = Some(fixture_response(200, Duration::milliseconds(142)));
    document
}

fn fixture_response(status: u16, duration: chrono::Duration) -> ResponseRecord {
    let body = format!(
        "{{\n  \"ok\": true,\n  \"url\": \"{FIXTURE_URL}\",\n  \"echo\": {{\n    \"page\": \"1\",\n    \"x-fixture\": \"restbook-preview\"\n  }},\n  \"items\": [\n    {{ \"id\": 1, \"name\": \"one\" }},\n    {{ \"id\": 2, \"name\": \"two\" }}\n  ]\n}}"
    );
    ResponseRecord {
        status,
        headers: vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("server".to_owned(), "sample/0".to_owned()),
            ("x-sample".to_owned(), "preview".to_owned()),
            (
                "set-cookie".to_owned(),
                "sid=preview; Path=/; HttpOnly".to_owned(),
            ),
            (
                "set-cookie".to_owned(),
                "theme=dark; Path=/; SameSite=Lax".to_owned(),
            ),
        ],
        body: body.into_bytes().into(),
        duration_ms: duration.num_milliseconds().max(0) as u64,
        received_at: Utc::now(),
        final_url: None,
        redirects: Vec::new(),
    }
}

/// The rows the non-response panes draw: one environment (active), a global, a
/// collection with a folder and the fixture request, one unsaved draft, and a
/// short send log the response pane's history control can list.
///
/// Fixture failures are logged, not fatal: a preview missing one row is still
/// worth opening, and a poisoned in-memory lock is the only way these fail.
fn seed_store(store: &MemoryStore) {
    let environment = Environment {
        name: "local".to_owned(),
        vars: vec![
            Kv::new("base_url", FIXTURE_URL),
            Kv {
                secret: true,
                ..Kv::new("token", "fixture-token")
            },
        ],
    };
    seeded(store.save_environment(&environment), "environment");
    seeded(
        store.set_active_environment(Some("local")),
        "active environment",
    );
    seeded(
        store.save_globals(&[Kv::new("user", "preview-user")]),
        "globals",
    );

    let collection = Collection {
        id: "col-sample".to_owned(),
        name: "Sample API".to_owned(),
        items: vec![CollectionItem::Folder(Folder {
            id: "fold-echo".to_owned(),
            name: "Echo".to_owned(),
            items: vec![CollectionItem::Request(SavedRequest {
                id: FIXTURE_NODE.to_owned(),
                name: "Echo (preview)".to_owned(),
                request: fixture_request(),
            })],
        })],
    };
    seeded(store.save_collection(&collection), "collection");

    let draft = Draft {
        id: 0,
        request: RequestDef::new(Method::Get, FIXTURE_URL),
        updated_at: Utc::now(),
    };
    seeded(store.save_draft(&draft), "draft");

    let owner = Origin::Saved {
        node: FIXTURE_NODE.to_owned(),
    };
    for (index, status) in [200_u16, 201, 500].into_iter().enumerate() {
        let entry = HistoryEntry {
            id: 0,
            request: fixture_request(),
            response: fixture_response(status, Duration::milliseconds(90 + index as i64 * 20)),
            url: FIXTURE_URL.to_owned(),
            origin: owner.clone(),
        };
        seeded(store.append_history(&entry), "history row");
    }
}

fn seeded<T>(result: restbook_core::Result<T>, what: &str) {
    if let Err(error) = result {
        log::warn!("preview fixture: could not store the {what}: {error}");
    }
}

/// Render-test support: a pane's recorded bounds as a deterministic text tree.
///
/// A real screenshot diff needs a GPU and a font stack the tests do not have.
/// What the mock window *does* record is the bounds of every element that opted
/// in with `debug_selector`, and that is enough to catch a pane whose layout
/// moved, collapsed, or lost a row: the tree is the assertion, and the written
/// file is the visual diff (plain text, so `git diff` is the comparison).
#[cfg(test)]
pub(crate) mod tree {
    use std::fmt::Write as _;
    use std::path::PathBuf;

    use gpui_kit::VisualTestContext;

    /// One recorded element, flattened out of GPUI's pixel types so the tree
    /// has no float formatting surprises.
    struct Node {
        selector: &'static str,
        left: f32,
        top: f32,
        width: f32,
        height: f32,
    }

    impl Node {
        fn right(&self) -> f32 {
            self.left + self.width
        }

        fn bottom(&self) -> f32 {
            self.top + self.height
        }

        fn area(&self) -> f32 {
            self.width * self.height
        }

        /// Whether `self` visually contains `child`, with a tolerance for the
        /// rounding a layout pass does.
        fn contains(&self, child: &Node) -> bool {
            const EPSILON: f32 = 0.5;
            self.left <= child.left + EPSILON
                && self.top <= child.top + EPSILON
                && self.right() >= child.right() - EPSILON
                && self.bottom() >= child.bottom() - EPSILON
        }
    }

    /// The pane's tree, as text: one line per recorded selector, indented by
    /// how many other recorded elements contain it.
    ///
    /// Deterministic: nodes are ordered by position and then by selector, so
    /// two draws of the same pane produce byte-identical output, which is what
    /// makes the file diffable.
    pub(crate) fn bounds_tree(
        cx: &mut VisualTestContext,
        title: &str,
        selectors: &[&'static str],
    ) -> String {
        let mut nodes: Vec<Node> = selectors
            .iter()
            .filter_map(|selector| {
                let bounds = cx.debug_bounds(*selector)?;
                Some(Node {
                    selector: *selector,
                    left: f32::from(bounds.origin.x),
                    top: f32::from(bounds.origin.y),
                    width: f32::from(bounds.size.width),
                    height: f32::from(bounds.size.height),
                })
            })
            .collect();

        nodes.sort_by(|a, b| {
            a.top
                .total_cmp(&b.top)
                .then(a.left.total_cmp(&b.left))
                .then(a.selector.cmp(b.selector))
        });

        let mut text = format!("# {title}\n");
        for node in &nodes {
            // A bigger element that contains this one is its ancestor. Counting
            // them gives a nesting depth without the render tree, which GPUI
            // does not expose to tests.
            let depth = nodes
                .iter()
                .filter(|other| {
                    other.area() > node.area()
                        && other.contains(node)
                        && !(other.left == node.left
                            && other.top == node.top
                            && other.selector == node.selector)
                })
                .count();
            let _ = writeln!(
                text,
                "{}{} @{:.1},{:.1} {:.1}x{:.1}",
                "  ".repeat(depth),
                node.selector,
                node.left,
                node.top,
                node.width,
                node.height,
            );
        }
        text
    }

    /// Write [`bounds_tree`] to `target/preview/<name>.txt` and return the path,
    /// so a test can hand a developer the file to diff.
    pub(crate) fn write_tree(
        cx: &mut VisualTestContext,
        name: &str,
        selectors: &[&'static str],
    ) -> std::io::Result<PathBuf> {
        let text = bounds_tree(cx, name, selectors);
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("preview");
        std::fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{name}.txt"));
        std::fs::write(&path, text)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    // Named imports only: the crate glob-imports `gpui_kit::*`, whose `test`
    // macro would shadow the built-in `#[test]`.
    use super::{invocation, Invocation, Pane, Preview};

    fn args(list: &[&str]) -> Invocation {
        invocation(
            std::iter::once("restbook")
                .chain(list.iter().copied())
                .map(str::to_owned),
        )
    }

    #[test]
    fn no_preview_flag_launches_the_workspace() {
        assert!(matches!(args(&[]), Invocation::Launch));
        assert!(matches!(args(&["--other"]), Invocation::Launch));
    }

    #[test]
    fn a_pane_name_selects_that_pane() {
        assert!(matches!(
            args(&["--preview", "response"]),
            Invocation::Show(Preview::One(Pane::Response))
        ));
        // The `=` form costs nothing and is what a shell completion offers.
        assert!(matches!(
            args(&["--preview=history"]),
            Invocation::Show(Preview::One(Pane::History))
        ));
    }

    #[test]
    fn all_names_every_pane() {
        assert!(matches!(
            args(&["--preview", "all"]),
            Invocation::Show(Preview::All)
        ));
    }

    #[test]
    fn an_unknown_name_is_reported_rather_than_ignored() {
        match args(&["--preview", "responses"]) {
            Invocation::Unknown(name) => assert_eq!(name, "responses"),
            _ => panic!("an unknown pane name must not fall through to a launch"),
        }
        match args(&["--preview"]) {
            Invocation::Unknown(name) => assert!(name.is_empty()),
            _ => panic!("a missing name must not fall through to a launch"),
        }
    }
}
