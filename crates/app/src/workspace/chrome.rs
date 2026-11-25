//! Window chrome: the custom titlebar and the status bar.
//!
//! Both are *chrome*, not panes: they belong to the window, hold no engine
//! state, and are plain functions so a status bar cannot be dragged into a tab
//! strip. The titlebar is app-drawn so the sidebar reaches the top edge; dragging
//! and window controls are `gpui-component`'s [`TitleBar`]; `main.rs` hides the native one.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, TitleBar};
use gpui_kit::*;

use crate::icons::{self, IconName};
use crate::notice::Notice;
use crate::theme::{Metrics, Palette};

use super::storage::{EphemeralReason, HistoryMode, PlaintextReason};

/// The window titlebar: the workspace chip, `trailing`, then the [`TitleBar`]'s
/// own window controls.
///
/// The open request's label and URL live in the request tab strip and the
/// status bar, so the bar carries no breadcrumb of its own. The dock toggles
/// live in the status bar instead. See [`status_bar`].
pub(crate) fn titlebar(
    palette: &Palette,
    metrics: &Metrics,
    workspace: &str,
    trailing: AnyElement,
) -> AnyElement {
    TitleBar::new()
        .h_full()
        // The library paints a gradient of its own theme's title bar and
        // background; this app has its own palette, so the surface is set
        // here. `refine_style` runs after the built-in background, so this
        // wins without a wrapper element.
        .bg(palette.title_bar)
        .gap(px(metrics.space_sm))
        .text_color(palette.foreground)
        .child(
            // The workspace chip: an initial in a rounded square, then the
            // name. Cheap to draw, and it gives the eye a left anchor the way
            // a workspace icon does in every client in this family.
            h_flex()
                .flex_none()
                .items_center()
                .gap(px(metrics.space_sm))
                .child(
                    div()
                        .size(px(18.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .bg(palette.sidebar_accent)
                        .text_xs()
                        .font_weight(FontWeight::BOLD)
                        .child(initial(workspace)),
                )
                .child(
                    div()
                        .max_w(px(160.))
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(workspace.to_owned())),
                ),
        )
        // Slack goes to the middle so the environment chip and menu stay at
        // the right edge, beside the window controls.
        .child(div().flex_1())
        .child(press_through(trailing))
        .into_any_element()
}

/// Swallow the press so a control in the titlebar still receives clicks.
///
/// `TitleBar` arms a window move on any left press, and the platform's move loop
/// consumes the release, so a `Button` only ever sees the mouse-down. Wrapping the
/// child keeps the bar draggable everywhere else; without this the control looks
/// dead. Remove once the kit checks `default_prevented()` before moving.
fn press_through(child: AnyElement) -> AnyElement {
    div()
        .flex_none()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(child)
        .into_any_element()
}

/// The active-environment chip, for the titlebar.
///
/// The choice is window-wide — the next send resolves `{{vars}}` against it
/// whatever tab is open — so it sits in the bar, not a pane. `cap` bounds the
/// width so a long name cannot push the window controls out of reach, and must
/// clear the placeholder or `No environment` ellipsises before one is picked.
pub(crate) fn environment_chip(
    active: Option<&str>,
    environments: Vec<String>,
    on_select: impl Fn(Option<String>, &mut Window, &mut App) + Clone + 'static,
    cap: f32,
) -> AnyElement {
    let label: SharedString = active.unwrap_or("No environment").to_owned().into();
    let selected = active.map(str::to_owned);

    Button::new("titlebar-environment")
        .icon(IconName::Globe)
        .label(label)
        .compact()
        .flex_none()
        .max_w(px(cap))
        .dropdown_caret(true)
        .debug_selector(|| "titlebar-environment-chip".to_owned())
        .tooltip("Active environment")
        .dropdown_menu(move |mut menu, _, _| {
            menu = menu.item(
                PopupMenuItem::new("No environment")
                    .icon(IconName::Globe)
                    .checked(selected.is_none())
                    .on_click({
                        let on_select = on_select.clone();
                        move |_, window, cx| on_select(None, window, cx)
                    }),
            );
            if environments.is_empty() {
                menu = menu.item(PopupMenuItem::new("No environments yet.").disabled(true));
            }
            for name in &environments {
                let chosen = name.clone();
                let checked = selected.as_deref() == Some(name.as_str());
                menu = menu.item(
                    PopupMenuItem::new(name.clone())
                        .icon(IconName::Globe)
                        .checked(checked)
                        .on_click({
                            let on_select = on_select.clone();
                            move |_, window, cx| on_select(Some(chosen.clone()), window, cx)
                        }),
                );
            }
            menu
        })
        .into_any_element()
}

fn initial(name: &str) -> String {
    name.chars()
        .next()
        .map(|first| first.to_uppercase().collect())
        .unwrap_or_else(|| {
            // The brand's own initial, for a workspace name with no characters to
            // take one from.
            crate::branding::NAME
                .chars()
                .next()
                .map(|first| first.to_uppercase().collect())
                .unwrap_or_default()
        })
}

/// What the status bar says. Every field is optional so a fresh window with
/// nothing sent yet shows a quiet line rather than `0 ms` next to nothing.
///
/// It carries no status code, duration, or size — those belong to the response
/// pane's header, and duplicating them made the footer read as a second copy.
/// What is left is what that header cannot say: the last URL, or why nothing went.
#[derive(Debug, Clone, Default)]
pub(crate) struct Status {
    /// The resolved URL of the last send.
    pub url: Option<SharedString>,
    /// A message from the engine: a send error.
    pub message: Option<SharedString>,
}

/// How loud a confirmation is. Errors stay until the next send; successes fade,
/// because a confirmation that outlives the action it confirms is just noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlashKind {
    Success,
    Error,
}

/// A short-lived message from an action that had no other way to say it worked:
/// saving a request, creating a collection, deleting a node. The panes emit
/// intents, and the workspace performs them without a `Window`, so the usual
/// `push_notification` is unavailable on that path; this is what replaces it.
#[derive(Debug, Clone)]
pub(crate) struct Flash {
    pub kind: FlashKind,
    pub message: SharedString,
    /// A one-shot command offered beside the message, e.g. Undo. It lives here
    /// rather than in a separate field so replacing or expiring the flash also
    /// retires the command, with no second piece of state to clear.
    pub action: Option<FlashAction>,
}

impl Flash {
    /// Build the toast for a pane's [`crate::notice::Notice`]. The panes and the
    /// workspace share the notice type so the mapping from an outcome to a tone
    /// lives in exactly one place.
    pub(crate) fn from_notice(notice: &Notice) -> Self {
        match notice {
            Notice::Done(message) => Self {
                kind: FlashKind::Success,
                message: message.clone(),
                action: None,
            },
            Notice::Failed(message) => Self {
                kind: FlashKind::Error,
                message: message.clone(),
                action: None,
            },
        }
    }
}

/// A toast action's callback: shared rather than copied so a flash stays cheap
/// to clone into render code.
type FlashHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// A command the toast offers, such as Undo. The callback is shared (`Rc`)
/// rather than copied so the flash stays cheap to clone into render code.
#[derive(Clone)]
pub(crate) struct FlashAction {
    pub label: SharedString,
    pub(crate) on_click: FlashHandler,
}

impl FlashAction {
    pub(crate) fn new(
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            on_click: Rc::new(on_click),
        }
    }
}

impl std::fmt::Debug for FlashAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlashAction")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// The window-wide status line.
///
/// Left to right: `left_toggle` and the storage chip, the last exchange's URL or
/// error, then `right_toggle`. The dock toggles sit at the bottom corners under
/// the docks they open, keeping the titlebar free for dragging. The active
/// environment is not repeated here; it has its own titlebar chip.
pub(crate) fn status_bar(
    palette: &Palette,
    metrics: &Metrics,
    workspace: &WorkspaceChip,
    status: &Status,
    compact: bool,
    left_toggle: AnyElement,
    right_toggle: AnyElement,
) -> AnyElement {
    // The URL is the only item that may give way when the bar is narrow: it has
    // a ceiling but no floor, so it ellipsises instead of shoving the fixed chips
    // or the corner toggles off the edge.
    let mut right = h_flex().items_center().gap(px(metrics.space_md)).min_w_0();

    if let Some(url) = &status.url {
        right = right.child(
            div()
                .id("status-url")
                .min_w_0()
                .max_w(px(520.))
                .truncate()
                .text_xs()
                .font_family(palette.mono_font.clone())
                .text_color(palette.subtle)
                .child(url.clone()),
        );
    }

    if let Some(message) = &status.message {
        right = right.child(
            div()
                .id("status-message")
                .min_w_0()
                .max_w(px(640.))
                .truncate()
                .text_xs()
                .text_color(palette.danger)
                .child(message.clone()),
        );
    }

    // Where the data actually lives. A workspace means collections and
    // environments are files in that directory and can be committed; otherwise
    // everything is in the per-user encrypted database and is private to this
    // machine. That difference is worth one chip, because it is otherwise
    // invisible and it decides whether "share this collection" is even possible.
    let storage_chip = {
        // The glyph follows the mode: a directory the user can open, or the
        // local database that is private to this machine. Both say what the
        // chip is, so the word beside it does not have to.
        let (icon, label, color) = match &workspace.root {
            Some(root) => (
                IconName::Folder,
                root.display().to_string(),
                palette.foreground,
            ),
            None => (
                IconName::HardDrive,
                "local database".to_owned(),
                palette.subtle,
            ),
        };
        let label = SharedString::from(label);
        let tooltip = label.clone();
        let mut chip = h_flex()
            .id("status-workspace")
            .flex_none()
            .min_w_0()
            .items_center()
            .gap(px(metrics.space_1p5))
            .child(icons::quiet(icon, icons::SMALL, palette));
        // A narrow bar keeps the glyph and moves the word into the tooltip.
        if !compact {
            chip = chip.child(
                div()
                    .max_w(px(360.))
                    .truncate()
                    .text_xs()
                    .text_color(color)
                    .child(label),
            );
        }
        chip.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
    };

    // Where the data lives, and how the history it holds is protected. Two
    // chips rather than one sentence: the location is a path and the protection
    // is a state, and a user looks for them in different situations.
    let history_chip = history_mode_chip(workspace, palette, metrics, compact);

    div()
        .flex()
        .h(px(metrics.status_height))
        .w_full()
        .flex_none()
        .bg(palette.status_bar)
        .border_t_1()
        .border_color(palette.chrome_border)
        .child(
            h_flex()
                .w_full()
                .h_full()
                .items_center()
                .gap(px(metrics.space_md))
                .px(px(metrics.space_1p5))
                // The corner controls come before anything else, so each one
                // hugs the edge it names even when the status text is absent.
                .child(left_toggle)
                .child(storage_chip)
                .child(history_chip)
                .child(div().flex_1())
                .child(right)
                .child(right_toggle),
        )
        .into_any_element()
}

/// The storage-mode half of the status bar.
pub(crate) struct WorkspaceChip {
    /// The workspace root, or `None` when the per-user database is in use.
    pub(crate) root: Option<PathBuf>,
    /// How history is protected in this session.
    pub(crate) history: HistoryMode,
}

/// How history at rest is protected, beside the storage chip.
///
/// The words carry the state and the tooltip carries the reason: a warning
/// glyph alone would depend on color to be read, and the bar is one line, so
/// the consequence of a plaintext history needs somewhere else to live.
fn history_mode_chip(
    workspace: &WorkspaceChip,
    palette: &Palette,
    metrics: &Metrics,
    compact: bool,
) -> AnyElement {
    let (state, icon, label, color, tooltip) = match workspace.history {
        HistoryMode::Encrypted => (
            "encrypted",
            IconName::Lock,
            "Encrypted",
            palette.subtle,
            "History is encrypted on this machine",
        ),
        HistoryMode::Plaintext(PlaintextReason::NoEncryptionSupport) => (
            "plaintext",
            IconName::TriangleAlert,
            "Not encrypted",
            palette.warning,
            "This build cannot encrypt history. Requests and responses are stored as plain text on disk",
        ),
        HistoryMode::Plaintext(PlaintextReason::EncryptionUnavailable) => (
            "plaintext",
            IconName::TriangleAlert,
            "Not encrypted",
            palette.warning,
            "The system keyring was unavailable, so history is stored as plain text",
        ),
        HistoryMode::Ephemeral(EphemeralReason::OpenFailed) => (
            "ephemeral",
            IconName::TriangleAlert,
            "Not saved",
            palette.danger,
            "History could not be opened, so this session is kept in memory and is lost when the window closes",
        ),
        HistoryMode::Ephemeral(EphemeralReason::WorkspaceRefusedPlaintext) => (
            "ephemeral",
            IconName::TriangleAlert,
            "Not saved",
            palette.danger,
            "A folder workspace never keeps a plain text history, and encryption is unavailable, so this session is kept in memory and is lost when the window closes",
        ),
    };

    let mut chip = h_flex()
        .id("status-history")
        // Recorded so the render test can tell which state was painted.
        .debug_selector(move || format!("status-history-{state}"))
        .flex_none()
        .items_center()
        .gap(px(metrics.space_1p5))
        .child(icons::sized(icon, icons::SMALL).text_color(color));
    // A narrow bar keeps the glyph and moves the word into the tooltip.
    if !compact {
        chip = chip.child(
            div()
                .truncate()
                .text_xs()
                .text_color(color)
                .child(SharedString::from(label)),
        );
    }
    chip.tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the module-level `gpui_kit::*` glob brings in a
    // macro named `test`, which shadows the built-in `#[test]` attribute here
    // and recurses until the compiler gives up.
    use super::initial;
    use crate::notice::Notice;

    #[test]
    fn a_workspace_initial_is_the_first_letter_uppercased() {
        assert_eq!(initial("acme"), "A");
        assert_eq!(initial("Acme"), "A");
    }

    #[test]
    fn an_empty_workspace_name_falls_back_to_the_app_initial() {
        assert_eq!(initial(""), "R");
    }

    #[test]
    fn a_done_notice_flashes_success() {
        use super::FlashKind;

        let flash = super::Flash::from_notice(&Notice::Done("Request saved.".into()));
        assert!(matches!(flash.kind, FlashKind::Success));
        assert_eq!(flash.message.as_ref(), "Request saved.");
    }

    #[test]
    fn a_failed_notice_flashes_an_error_with_the_store_message() {
        use super::FlashKind;

        let flash = super::Flash::from_notice(&Notice::Failed("disk on fire".into()));
        assert!(matches!(flash.kind, FlashKind::Error));
        assert_eq!(flash.message.as_ref(), "disk on fire");
    }

    /// The confirmation is the kit's notification, not ours.
    ///
    /// A notification is a window-level layer drawn above dialogs, so chrome must
    /// not paint its own toast: a second renderer at that offset cannot rise
    /// above a dialog and only competes with the layer.
    #[test]
    fn chrome_does_not_paint_its_own_toast() {
        let source = include_str!("chrome.rs");
        // The marker is the old toast's offset, spelled so this assertion does
        // not match itself.
        let toast_offset = ["status_height", " + 10."].concat();
        assert!(
            !source.contains(&toast_offset),
            "the toast moved to the kit's notification layer; do not pin one to the dock",
        );
    }
}

/// The environment chip as laid out by the real renderer.
///
/// The chip is content-sized, and a cap narrower than its own placeholder
/// ellipsises `No environment` before any environment is picked.
#[cfg(test)]
mod render_tests {
    use gpui_kit::{IntoElement as _, ParentElement as _, Styled as _, TestAppContext};

    use super::environment_chip;
    use crate::preview::tree;

    const CHIP: &str = "titlebar-environment-chip";

    /// A window root that draws nothing but the chip, at a fixed cap.
    struct ChipOnly(f32);

    impl gpui_kit::Render for ChipOnly {
        fn render(
            &mut self,
            _: &mut gpui_kit::Window,
            _: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            gpui_kit::component::h_flex().child(environment_chip(
                None,
                vec!["prod".to_owned()],
                |_, _, _| {},
                self.0,
            ))
        }
    }

    /// Draw the chip alone, capped at `cap`, and read back its painted width.
    fn width_for(cap: f32) -> f32 {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let (_view, vcx) = cx.add_window_view(|_window, _cx| ChipOnly(cap));
        vcx.update(|window, cx| window.draw(cx).clear(cx));

        let text = tree::bounds_tree(vcx, "environment-chip", &[CHIP]);
        let line = text
            .lines()
            // The first line is the `# title`, which carries the selector too.
            .filter(|line| line.contains(CHIP))
            .find(|line| line.contains('@'))
            .unwrap_or_else(|| panic!("the chip should be in the tree:\n{text}"));
        let size = line
            .split('@')
            .nth(1)
            .and_then(|rest| rest.split_whitespace().nth(1))
            .unwrap_or_else(|| panic!("a recorded box in {line:?}"));
        let width: f32 = size
            .split('x')
            .next()
            .expect("a recorded width")
            .parse()
            .expect("a numeric width");
        cx.quit();
        width
    }

    /// The chip the titlebar asks for must show the placeholder whole.
    ///
    /// The width measured with no cap is the placeholder's own width; the
    /// capped one is what the window actually paints. If the cap bites below
    /// the natural width, the label ellipsises, which is the regression.
    #[test]
    fn the_titlebar_chip_fits_its_placeholder() {
        let metrics = crate::theme::Metrics::default();
        let natural = width_for(f32::INFINITY);
        let capped = width_for(metrics.gutter * 16.0);

        assert!(
            capped >= natural - 0.5,
            "the chip truncates its placeholder: {capped}px at the cap but \
             `No environment` needs {natural}px, which is the `No enviro…` \
             defect"
        );
    }

    /// A window that draws nothing but the status bar, over one history mode.
    struct BarOnly(super::super::storage::HistoryMode);

    impl gpui_kit::Render for BarOnly {
        fn render(
            &mut self,
            _: &mut gpui_kit::Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            let palette = crate::theme::Palette::new(cx);
            let metrics = crate::theme::Metrics::current(cx);
            gpui_kit::component::v_flex()
                .size_full()
                .child(super::status_bar(
                    &palette,
                    &metrics,
                    &super::WorkspaceChip {
                        root: None,
                        history: self.0,
                    },
                    &super::Status::default(),
                    false,
                    gpui_kit::Empty.into_any_element(),
                    gpui_kit::Empty.into_any_element(),
                ))
        }
    }

    /// Draw the status bar for `mode` and report whether `selector` was painted.
    fn painted(mode: super::super::storage::HistoryMode, selector: &'static str) -> bool {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let (_view, vcx) = cx.add_window_view(|_window, _cx| BarOnly(mode));
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        let found = vcx.debug_bounds(selector).is_some();
        cx.quit();
        found
    }

    /// The window names how its history is protected. A plaintext fallback is
    /// otherwise only announced on stderr, which a GUI user never reads; the
    /// encrypted state is the one that shows no warning in its place.
    #[test]
    fn the_status_bar_names_how_history_is_protected() {
        use super::super::storage::{EphemeralReason, HistoryMode, PlaintextReason};

        assert!(
            painted(
                HistoryMode::Plaintext(PlaintextReason::EncryptionUnavailable),
                "status-history-plaintext"
            ),
            "a plaintext fallback must be visible in the window"
        );
        assert!(
            !painted(
                HistoryMode::Plaintext(PlaintextReason::EncryptionUnavailable),
                "status-history-encrypted"
            ),
            "a plaintext fallback must not claim to be encrypted"
        );
        assert!(
            painted(HistoryMode::Encrypted, "status-history-encrypted"),
            "an encrypted history still says so"
        );
        // A session kept in memory is one state to the user, however it got
        // there: an unreadable store and a workspace that refused plain text
        // both mean "this history will not survive the window".
        for reason in [
            EphemeralReason::OpenFailed,
            EphemeralReason::WorkspaceRefusedPlaintext,
        ] {
            assert!(
                painted(HistoryMode::Ephemeral(reason), "status-history-ephemeral"),
                "history that will not be saved says so ({reason:?})"
            );
        }
    }
}
