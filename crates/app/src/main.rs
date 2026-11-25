//! Desktop shell for restbook: the window is a [`Workspace`], a docked
//! [`DockArea`] over a [`Document`]. `DockArea` draws nothing itself, so panes
//! take their look from `DockSkin`. The engine stays behind one [`Workspace::send`]
//! call; panes report what the user did through constructor args and their own
//! `cx` — never a global, never each other. `--preview <pane>` or `--preview all`
//! replaces the workspace with one pane over fixture data; see [`preview`].

// Without this the exe keeps the console subsystem Rust defaults to, so every
// launch from Explorer, a Start-menu entry or the installer's postinstall run
// opens a terminal window beside the app. Release is the GUI build: no console,
// and the tee in `logging` is where its lines go. Debug keeps the console on
// purpose, since `cargo run` is where a developer reads them, and a test build
// keeps it too or `cargo test --release` would swallow its own output.
#![cfg_attr(
    all(windows, not(debug_assertions), not(test)),
    windows_subsystem = "windows"
)]

mod actions;
mod branding;
mod completion;
mod document;
mod icons;
mod logging;
mod notice;
mod panels;
mod preview;
mod snapshot;
#[cfg(test)]
mod test_support;
mod theme;
mod updater;
mod util;
mod variables;
mod workspace;

use gpui_kit::component::TitleBar;
use gpui_kit::*;

/// The smallest the window may be dragged to.
///
/// `DockArea` floors every pane slot at 100 px but exposes no per-split minimum,
/// so without this a window could shrink until panes render narrower than the
/// dock thinks they are. The panes are responsive below their breakpoints; the
/// window stops here rather than at nothing.
const MIN_WINDOW_WIDTH: f32 = 640.;
const MIN_WINDOW_HEIGHT: f32 = 480.;

/// The window root. The two launch paths produce different view types, and a
/// window holds one entity, so this names both. It is presentation only: the
/// workspace and the preview each still own their own state and subscriptions.
enum RootView {
    Workspace(Entity<workspace::Workspace>),
    Preview(Entity<preview::PreviewRoot>),
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        match self {
            RootView::Workspace(view) => view.clone().into_any_element(),
            RootView::Preview(view) => view.clone().into_any_element(),
        }
    }
}

fn main() {
    // Nothing else installs a `log` sink, so without this every `log::*` call
    // in the app, GPUI and wgpu is discarded. Installed first because the
    // interesting lines are written while the window is built. A GUI run
    // usually has no terminal, so `logging::init` tees to
    // `<data dir>/restbook.log` as well as stderr.
    let log_file = logging::init();
    match &log_file {
        Some(path) => log::info!(
            "restbook {} starting; log: {}",
            env!("CARGO_PKG_VERSION"),
            path.display()
        ),
        None => log::warn!("log file unavailable; only stderr is captured"),
    }

    // A developer flag on a binary that otherwise takes none. It is parsed from
    // the plain arguments so a normal launch cannot change shape by accident.
    let preview = preview::invocation(std::env::args());
    log::debug!(target: "restbook_app::ui", "{}", preview::summary(&preview));

    // The icon bundle the components draw from. Without a source registered
    // every `Icon` paints nothing, so this is a startup requirement, not a
    // per-pane choice.
    gpui_kit::application()
        // `AllAssets` embeds the complete Lucide catalog, not just the default
        // component list: the app's icon vocabulary (`icons.rs`) names glyphs
        // such as `Pencil` and `Trash` that the default bundle does not carry.
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            // Required before any GPUI Component feature is used.
            gpui_kit::init(cx);

            // One table, in `actions`, so a typo fails a test instead of the
            // launch.
            cx.bind_keys(actions::bindings(cx));

            // The app draws its own titlebar (see `workspace::chrome`), so the
            // platform one is hidden and the sidebar runs to the top edge.
            // `TitleBar::title_bar_options()` is the base for anything rendering
            // a `TitleBar`: it hides the platform bar and lets the titlebar own
            // drag and double-click. macOS still draws its traffic lights, which
            // this position keeps at the window's top-left.
            let titlebar = TitlebarOptions {
                // The preview names its pane in the title, which is what the
                // taskbar and the window's own roster show.
                title: Some(
                    preview::title(&preview)
                        .unwrap_or_else(|| branding::WINDOW_TITLE.to_owned())
                        .into(),
                ),
                traffic_light_position: Some(point(px(12.), px(14.))),
                ..TitleBar::title_bar_options()
            };

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(80.), px(80.)),
                    size: size(px(1280.), px(820.)),
                })),
                window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
                titlebar: Some(titlebar),
                // The one identity the *platform* needs rather than the app
                // painting it. On Linux this is the `app_id` a dock, taskbar and
                // `.desktop` file match on; elsewhere it is ignored, which is why
                // it can be a constant instead of a `#[cfg]`. macOS renames the
                // running bundle from `CFBundleName`, where `BRAND.name` is used.
                // See `branding` for what each OS owns.
                app_id: Some(branding::APP_ID.to_owned()),
                // Without this the window would have no frame at all on Linux,
                // which leaves nothing to resize or drag by.
                window_decorations: Some(WindowDecorations::Client),
                ..TitleBar::window_options()
            };

            gpui_kit::open_window(options, cx, move |window, cx| match preview {
                preview::Invocation::Launch => {
                    let workspace = cx.new(|cx| workspace::Workspace::new(window, cx));
                    cx.new(|_| RootView::Workspace(workspace))
                }
                // The preview root is the whole window: no dock, no chrome, no
                // updater. The panes inside it are the real ones, wired to a
                // fixture document and an in-memory store.
                invocation => {
                    let root = cx.new(|cx| preview::PreviewRoot::new(invocation, window, cx));
                    cx.new(|_| RootView::Preview(root))
                }
            })
            .expect("failed to open window");
        });
}
