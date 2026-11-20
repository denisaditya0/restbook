//! Window chrome: the custom titlebar and the status bar.
//!
//! Both are *chrome*, not panes: they belong to the window, hold no engine
//! state, and are plain functions so a status bar cannot be dragged into a tab
//! strip. The titlebar is app-drawn so the sidebar reaches the top edge; dragging
//! and window controls are `gpui-component`'s [`TitleBar`]; `main.rs` hides the native one.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, TitleBar};
use gpui_kit::*;

use crate::icons::{self, IconName};
use crate::notice::Notice;
use crate::theme::{Metrics, Palette};

/// One thing the titlebar can show, and its tooltip.
pub(crate) struct TitlePart {
    pub label: SharedString,
    pub tooltip: Option<SharedString>,
}

impl TitlePart {
    pub(crate) fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            tooltip: None,
        }
    }

    pub(crate) fn with_tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
}

/// The window titlebar.
///
/// Left to right: the workspace chip (this app, like a workspace switcher),
/// then the breadcrumb — one part per level of where the open request lives,
/// then whatever the caller puts in `trailing`. The window controls are added
/// by [`TitleBar`] itself and are already budgeted into its right padding.
///
/// The dock toggles are not here: they sit in the status bar's two bottom
/// corners, under the docks they open. See [`status_bar`].
pub(crate) fn titlebar(
    palette: &Palette,
    metrics: &Metrics,
    workspace: &str,
    parts: Vec<TitlePart>,
    trailing: AnyElement,
) -> AnyElement {
    let mut crumbs: Vec<AnyElement> = Vec::new();
    for (index, part) in parts.into_iter().enumerate() {
        if index > 0 {
            crumbs.push(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(palette.subtle)
                    .child("/")
                    .into_any_element(),
            );
        }
        // Keyed on the segment text, which is unique within one trail: an
        // index would re-key every crumb after a removal or a reorder.
        let label = div()
            .id(part.label.clone())
            .max_w(px(240.))
            .truncate()
            .text_sm()
            .text_color(palette.muted)
            .child(part.label);
        crumbs.push(match part.tooltip {
            Some(tooltip) => label
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .into_any_element(),
            None => label.into_any_element(),
        });
    }

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
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(palette.subtle)
                .child("›"),
        )
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(metrics.space_xs))
                .children(crumbs),
        )
        .child(press_through(trailing))
        .into_any_element()
}

/// Take the press for a control that lives in the bar.
///
/// `TitleBar` arms a window move on *any* left press inside the bar, and the
/// platform's move loop then consumes the release, so a `Button` in the bar
/// only ever sees the mouse-down and never its `on_click`. Swallowing the press
/// keeps the bar out of the move for that element alone: an empty stretch of
/// the bar still drags the window. Every interactive child in the bar has to go
/// through this; without it the control looks dead.
///
/// Remove this once the kit version in use carries the fix where the bar checks
/// `default_prevented()` before starting a move (gpui-kit PR #3918).
fn press_through(child: AnyElement) -> AnyElement {
    div()
        .flex_none()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(child)
        .into_any_element()
}

/// The active-environment chip, for the window's titlebar.
///
/// It lives in the titlebar rather than in a pane because the choice is
/// window-wide: the next send resolves `{{vars}}` against it whatever tab is
/// open. Keeping it out of the Collections pane is what lets that pane be about
/// collections only, and it matches where Postman and Bruno put the same
/// control. Not a [`TitlePart`]: that slot is read-only text, and this one
/// opens a menu.
///
/// `cap` bounds the width, because a long environment name in a titlebar
/// pushes the window controls out of reach. It must clear the placeholder's own
/// width: a narrower cap ellipsises `No environment` before any environment is
/// picked. The caller wraps the result in [`press_through`], since a button in
/// the titlebar otherwise only sees the mouse-down.
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
    on_click: FlashHandler,
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

/// The confirmation toast: bottom-right, above the status bar, so it does not
/// fight with the last-exchange summary for the same line.
pub(crate) fn flash(palette: &Palette, metrics: &Metrics, flash: &Flash) -> AnyElement {
    // A circle, not a bare glyph: a toast is transient and has no dismiss
    // button, but `Close` (`×`) next to a message still reads as one.
    let (icon, accent) = match flash.kind {
        FlashKind::Success => (IconName::CircleCheck, palette.success),
        FlashKind::Error => (IconName::CircleAlert, palette.danger),
    };

    div()
        .id("workspace-flash")
        .absolute()
        .bottom(px(metrics.status_height + 10.))
        .right(px(14.))
        // Above the dock, but it must not eat clicks meant for the pane under
        // it. The message stays inert; only the action button, when the flash
        // has one, is clickable.
        .occlude()
        .child(
            h_flex()
                .items_center()
                .gap(px(metrics.space_sm))
                .px(px(metrics.space_md))
                .py(px(metrics.space_1p5))
                .rounded_md()
                .bg(palette.tab_bar)
                .border_1()
                .border_color(accent)
                .shadow_md()
                .child(icons::sized(icon, icons::SMALL).text_color(accent))
                .child(
                    div()
                        .max_w(px(360.))
                        .truncate()
                        .text_xs()
                        .text_color(palette.foreground)
                        .child(flash.message.clone()),
                )
                .children(flash.action.as_ref().map(|action| {
                    let on_click = action.on_click.clone();
                    h_flex()
                        .flex_none()
                        .items_center()
                        .gap(px(metrics.space_xs))
                        .child(div().text_xs().text_color(palette.subtle).child("·"))
                        .child(
                            Button::new("workspace-flash-action")
                                .label(action.label.clone())
                                .ghost()
                                .compact()
                                .on_click(move |_, window, cx| on_click(window, cx)),
                        )
                })),
        )
        .into_any_element()
}

/// The window-wide status line.
///
/// Left to right: the storage chip, then `left_toggle` at the far left; the
/// last exchange's URL or error, then `right_toggle` at the far right.
///
/// The active environment is **not** here: it has a chip in the titlebar, and
/// repeating it in both bars was a second place to read the same fact. The
/// status bar says where the data lives and what the last send did, which are
/// the two things nothing else on screen reports.
///
/// The two dock toggles live here rather than in the titlebar because this bar
/// is the window's bottom edge: the sidebar control sits in the bottom-left
/// corner under the sidebar it opens, the tools control in the bottom-right
/// corner under the tools dock. That also keeps the titlebar for window chrome
/// and the breadcrumb, which is what a reader expects to drag the window by.
pub(crate) fn status_bar(
    palette: &Palette,
    metrics: &Metrics,
    workspace: &WorkspaceChip,
    status: &Status,
    left_toggle: AnyElement,
    right_toggle: AnyElement,
) -> AnyElement {
    let mut right = h_flex().items_center().gap(px(metrics.space_md));

    if let Some(url) = &status.url {
        right = right.child(
            div()
                .id("status-url")
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
        h_flex()
            .id("status-workspace")
            .min_w_0()
            .items_center()
            .gap(px(metrics.space_1p5))
            .child(icons::quiet(icon, icons::SMALL, palette))
            .child(
                div()
                    .max_w(px(360.))
                    .truncate()
                    .text_xs()
                    .text_color(color)
                    .child(SharedString::from(label)),
            )
    };

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
}

/// The environment chip as laid out by the real renderer.
///
/// It moved out of the Collections pane and into the titlebar, so what needs
/// pinning is the painted width: the chip is content-sized, and a cap narrower
/// than its own placeholder ellipsises `No environment` to `No enviro…` before
/// any environment is picked.
#[cfg(test)]
mod render_tests {
    use gpui_kit::{ParentElement as _, TestAppContext};

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
}
