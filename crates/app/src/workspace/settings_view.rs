//! The Settings dialog: app-level preferences and the storage facts beside them.
//!
//! A dialog, not a second window or a dock pane: it is drawn by the workspace,
//! which owns the store, root, and settings entity, so it reads live values and
//! Escape dismisses it. Groups: Preferences, History, Theme, Updates, and
//! read-only Storage. Request defaults (timeout, redirects, TLS) live with the request.

use std::path::PathBuf;

use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::*;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{h_flex, v_flex, WindowExt as _};
use gpui_kit::*;

use super::settings::{AppSettings, NumberField, Settings};
use super::storage;
use crate::branding;
use crate::icons::IconName;
use crate::logging;
use crate::theme::{Appearance, Metrics, Palette};
use crate::updater;

/// Open the Settings dialog. It reads the same `Settings` entity the workspace
/// holds, so a toggle here is visible to every pane at once — the response pane
/// watching for a pretty-print change is the first such pane.
pub(crate) fn open(
    settings: Entity<Settings>,
    workspace_root: Option<PathBuf>,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_dialog(cx, move |dialog, _window, cx| {
        let current = settings.read(cx).current().clone();
        let metrics = Metrics::current(cx);

        let pretty = Checkbox::new("settings-pretty")
            .label("Pretty-print JSON responses")
            .checked(current.pretty_responses)
            .on_change({
                let settings = settings.clone();
                // Read the *current* settings inside the handler rather than
                // capturing a snapshot: the closure runs many times, and each
                // run must build on whatever the last one wrote.
                move |checked, _, cx| {
                    settings.update(cx, |settings, cx| {
                        let mut next = settings.current().clone();
                        next.pretty_responses = *checked;
                        settings.update(next, cx);
                    });
                }
            });

        let preferences = v_flex()
            .gap(px(metrics.space_md))
            .child(section_label("Preferences", cx))
            .child(pretty);

        // Every numeric preference comes from `NumberField::ALL`, so this layout
        // does not grow when one is added.
        let history_section = v_flex()
            .gap(px(metrics.space_md))
            .child(section_label("History", cx))
            .children(
                NumberField::ALL
                    .into_iter()
                    .map(|field| number_row(&settings, field, cx)),
            );

        let theme_section = v_flex()
            .gap(px(metrics.space_md))
            .child(section_label("Theme", cx))
            .child(appearance_row(&settings, current.appearance, cx))
            .child(theme_row(&settings, current.theme.clone(), cx))
            .child(zoom_row(&settings, current.zoom, cx));

        let updates = updates_section(&settings, &current, cx);
        let identity = identity_section(cx);

        dialog
            .title(format!("Settings · {}", crate::theme::theme_name(cx)))
            .w(px(560.))
            .child(
                v_flex()
                    .gap(px(metrics.space_xl))
                    .py(px(metrics.space_sm))
                    .child(preferences)
                    .child(history_section)
                    .child(theme_section)
                    .child(updates)
                    .child(
                        v_flex()
                            .gap(px(metrics.space_md))
                            .child(section_label("Storage", cx))
                            .children(storage_facts(&workspace_root, cx)),
                    )
                    .child(identity),
            )
    });
}

/// A small heading above a group of settings.
fn section_label(label: &str, cx: &App) -> AnyElement {
    // The labels are stored in sentence case; the caps are a presentation
    // choice applied at the boundary, so a copy edit or a translation never
    // has to know the heading is drawn in caps.
    div()
        .text_xs()
        .text_color(Palette::new(cx).muted)
        .child(label.to_uppercase())
        .into_any_element()
}

/// One numeric setting: label, the input owned by the settings entity, and a
/// hint. Shared by every [`NumberField`], so a new setting is a variant, not a
/// row of layout.
fn number_row(settings: &Entity<Settings>, field: NumberField, cx: &App) -> AnyElement {
    let spec = field.spec();
    let palette = Palette::new(cx);
    h_flex()
        .gap(px(Metrics::current(cx).space_md))
        .items_center()
        .child(
            div()
                .w(px(180.))
                .text_sm()
                .text_color(palette.muted)
                .child(spec.label),
        )
        .child(
            // The field is owned by the settings entity, not built here: the
            // dialog is rebuilt on every notify, so an input created inside it
            // would lose the caret mid-typing.
            div()
                .w(px(96.))
                .child(Input::new(settings.read(cx).number_input(field)).id(spec.key)),
        )
        .child(
            div()
                .text_xs()
                .text_color(palette.subtle)
                .child(spec.hint),
        )
        .into_any_element()
}

/// The appearance axis: System / Light / Dark. Picking one writes the setting;
/// the workspace's observer switches the window chrome and the theme to match.
fn appearance_row(settings: &Entity<Settings>, current: Appearance, cx: &App) -> AnyElement {
    let modes = [
        (Appearance::System, "System"),
        (Appearance::Light, "Light"),
        (Appearance::Dark, "Dark"),
    ];
    let buttons = modes.into_iter().map(|(mode, label)| {
        // The id is the enum variant, not the visible label, so a relabel or a
        // translation cannot re-key the control. `selected` paints the current
        // mode and `toggled` is its accessible state; `primary` would read as
        // the row's action rather than as the one already chosen.
        let button = Button::new(format!("appearance-{}", appearance_key(mode)))
            .label(label)
            .ghost()
            .compact()
            .selected(mode == current)
            .toggled(mode == current);
        let settings = settings.clone();
        button
            .on_click(move |_, _, cx| {
                settings.update(cx, |settings, cx| {
                    let mut next = settings.current().clone();
                    next.appearance = mode;
                    settings.update(next, cx);
                });
            })
            .into_any_element()
    });

    labeled_row(
        "Appearance",
        cx,
        h_flex()
            .gap(px(Metrics::current(cx).space_xs))
            .children(buttons),
    )
}

/// The stable name of each appearance mode, used as its element id. Kept apart
/// from the display label so the two can change independently.
fn appearance_key(mode: Appearance) -> &'static str {
    match mode {
        Appearance::System => "System",
        Appearance::Light => "Light",
        Appearance::Dark => "Dark",
    }
}

/// The theme file picker: the built-in theme plus every `<name>.jsonc` a user has
/// dropped into the data directory's `themes/` folder. A pick writes the setting
/// and the workspace re-installs the theme from the file.
fn theme_row(settings: &Entity<Settings>, current: Option<String>, cx: &App) -> AnyElement {
    let selected = current
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Built-in".to_owned());
    let available = storage::list_themes();
    let settings_for_menu = settings.clone();

    let menu = move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
        let settings = settings_for_menu.clone();
        menu = menu.item(
            PopupMenuItem::new("Built-in")
                .checked(current.is_none())
                .on_click(move |_, _, cx| {
                    set_theme(&settings, None, cx);
                }),
        );
        if available.is_empty() {
            menu = menu.item(
                PopupMenuItem::new("No theme files found. Add one under themes/.").disabled(true),
            );
        }
        for name in &available {
            let name = name.clone();
            let settings = settings_for_menu.clone();
            let checked = current.as_deref() == Some(name.as_str());
            menu = menu.item(
                PopupMenuItem::new(name.clone())
                    .icon(IconName::Palette)
                    .checked(checked)
                    .on_click(move |_, _, cx| {
                        set_theme(&settings, Some(name.clone()), cx);
                    }),
            );
        }
        menu
    };

    let picker = Button::new("settings-theme")
        .label(selected)
        .ghost()
        .compact()
        .dropdown_caret(true)
        .tooltip("Pick a theme file")
        .dropdown_menu(menu);

    labeled_row("Theme", cx, picker)
}

/// A reset for the zoom multiplier. Zoom is adjusted with the keyboard, so this
/// is the one place a user who has zoomed too far can get back to 100%.
fn zoom_row(settings: &Entity<Settings>, current: f32, cx: &App) -> AnyElement {
    let percent = format!("{}%", (current * 100.0).round() as i32);
    let settings = settings.clone();
    let reset = Button::new("settings-zoom-reset")
        .label("Reset")
        .ghost()
        .compact()
        .tooltip("Reset zoom to 100%")
        .on_click(move |_, _, cx| {
            settings.update(cx, |settings, cx| {
                let mut next = settings.current().clone();
                next.zoom = 1.0;
                settings.update(next, cx);
            });
        });

    let controls = h_flex()
        .gap(px(Metrics::current(cx).space_sm))
        .items_center()
        .child(div().text_sm().child(percent))
        .child(reset);

    labeled_row("Zoom", cx, controls)
}

/// Write a theme name choice through the settings entity, keeping every other
/// field. `None` selects the built-in theme.
fn set_theme(settings: &Entity<Settings>, name: Option<String>, cx: &mut App) {
    settings.update(cx, |settings, cx| {
        let mut next = settings.current().clone();
        next.theme = name;
        settings.update(next, cx);
    });
}

/// The updates group: the running version, where checks read from, the
/// launch-time switch, and an explicit check.
///
/// The check itself is phase 1 of the updater: it fetches the manifest and
/// reports the comparison in a notification. It never downloads or replaces
/// anything, so the button is safe to press at any time.
fn updates_section(settings: &Entity<Settings>, current: &AppSettings, cx: &App) -> AnyElement {
    let palette = Palette::new(cx);
    let check = Button::new("settings-check-updates")
        .label("Check now")
        .ghost()
        .compact()
        .tooltip("Fetch the release manifest and compare it with this build")
        .on_click(|_, window, cx| check_now(window, cx));

    let version = labeled_row(
        "Version",
        cx,
        h_flex()
            .gap(px(Metrics::current(cx).space_sm))
            .items_center()
            .child(
                div()
                    .text_sm()
                    .font_family(palette.mono_font.clone())
                    .child(updater::CURRENT_VERSION),
            )
            .child(check),
    );

    // Saying where the check reads from is the difference between "there is no
    // update" and "this build has no channel".
    let channel = match updater::manifest_url() {
        Some(url) => url,
        None => "not configured in this build".to_owned(),
    };
    let channel = labeled_row(
        "Channel",
        cx,
        div()
            .text_xs()
            .text_color(palette.muted)
            .child(channel),
    );

    let on_launch = Checkbox::new("settings-check-on-launch")
        .label("Check for updates on launch")
        .checked(current.check_updates)
        .on_change({
            let settings = settings.clone();
            move |checked, _, cx| {
                settings.update(cx, |settings, cx| {
                    let mut next = settings.current().clone();
                    next.check_updates = *checked;
                    settings.update(next, cx);
                });
            }
        });

    v_flex()
        .gap(px(Metrics::current(cx).space_md))
        .child(section_label("Updates", cx))
        .child(version)
        .child(channel)
        .child(on_launch)
        .into_any_element()
}

/// Run a check off the UI thread and report the result as a notification.
///
/// The dialog's builder is a plain `Fn` that may run again, so nothing here is
/// stored in it: the notification is the state, and it is pushed through the
/// window handle captured before the await.
fn check_now(window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    let url = updater::manifest_url();

    cx.spawn(async move |cx| {
        let outcome = cx
            .background_spawn(async move {
                match url {
                    Some(url) => updater::check(&url),
                    None => Ok(updater::Outcome::NotConfigured),
                }
            })
            .await;

        cx.update_window(handle, |_, window, cx| {
            window.push_notification(update_notification(outcome), cx);
        })
        .ok();
    })
    .detach();
}

/// Turn a check's result into the one sentence a notification has room for.
fn update_notification(result: anyhow::Result<updater::Outcome>) -> Notification {
    match result {
        // Unreachable for a packaged build: the channel URL is compiled in.
        // It stays as the answer for a build whose version is not semver.
        Ok(updater::Outcome::NotConfigured) => Notification::warning(
            "This build has no update channel, so it cannot check for releases.",
        ),
        Ok(updater::Outcome::UpToDate { current }) => {
            Notification::success(format!("restbook {current} is up to date."))
        }
        Ok(updater::Outcome::Available {
            latest,
            notes,
            asset,
        }) => {
            let mut message = match asset {
                // The direct link is what a phase-1 user acts on: this build
                // does not install anything yet.
                Some(asset) => format!("restbook {latest} is available: {}", asset.url),
                // The release exists, but this machine has nothing to install;
                // saying so is more useful than implying a broken download.
                None => format!(
                    "restbook {latest} is available, but no build is published for {}.",
                    updater::platform_key()
                ),
            };
            if let Some(notes) = notes.filter(|notes| !notes.trim().is_empty()) {
                message.push(' ');
                message.push_str(notes.trim());
            }
            Notification::info(message)
        }
        Err(error) => Notification::error(format!("Couldn’t check for updates. {error}")),
    }
}

/// A settings row with the shared label column, so Theme, Appearance, and Zoom
/// line up with the Preferences rows above them.
fn labeled_row(label: &str, cx: &App, control: impl IntoElement) -> AnyElement {
    h_flex()
        .gap(px(Metrics::current(cx).space_md))
        .items_center()
        .child(
            div()
                .w(px(180.))
                .text_sm()
                .text_color(Palette::new(cx).muted)
                .child(label.to_owned()),
        )
        .child(control)
        .into_any_element()
}

/// Who this build says it is, and which OS surface picks each part up.
///
/// Everything here comes from [`crate::branding`] — the same constants the
/// window's `app_id` and the packaging templates read — so this group is a
/// readout of the identity rather than a second copy of it. It exists because
/// the three names are otherwise invisible: a missing icon in a dock or a
/// generic taskbar entry is a bug report about a string nobody can see.
fn identity_section(cx: &App) -> AnyElement {
    let palette = Palette::new(cx);
    let metrics = Metrics::current(cx);
    v_flex()
        .gap(px(metrics.space_md))
        .child(section_label("Identity", cx))
        .children([
            info_row("Name", branding::NAME.to_owned(), &palette, &metrics),
            info_row("App id", branding::app_id().to_owned(), &palette, &metrics),
            // The bundle and icon names are the same string as `NAME` today, but
            // they are what packaging substitutes into `Info.plist` and the
            // hicolor install path, so naming them separately is the point.
            info_row(
                "Bundle / icon",
                branding::BUNDLE_NAME.to_owned(),
                &palette,
                &metrics,
            ),
        ])
        .into_any_element()
}

/// The read-only facts about where this session reads and writes. These are
/// already known to the workspace, so nothing here is guesswork.
fn storage_facts(workspace_root: &Option<PathBuf>, cx: &App) -> Vec<AnyElement> {
    let data_dir = storage::data_dir();
    let palette = Palette::new(cx);
    let metrics = Metrics::current(cx);
    // The log can be moved with `RESTBOOK_LOG_FILE`, so name the file that is
    // actually open rather than the default path.
    let log_path = logging::file_path()
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join(logging::LOG_FILE));

    let mut rows = match workspace_root {
        Some(root) => vec![
            info_row("Mode", "Workspace".to_owned(), &palette, &metrics),
            info_row("Root", root.display().to_string(), &palette, &metrics),
            info_row(
                "Collections",
                root.join("collections").display().to_string(),
                &palette,
                &metrics,
            ),
            info_row(
                "Environments",
                root.join("environments").display().to_string(),
                &palette,
                &metrics,
            ),
            info_row(
                "Preferences",
                data_dir.join("settings.json").display().to_string(),
                &palette,
                &metrics,
            ),
        ],
        None => vec![
            info_row("Mode", "Local database".to_owned(), &palette, &metrics),
            info_row(
                "Data directory",
                data_dir.display().to_string(),
                &palette,
                &metrics,
            ),
            info_row(
                "Enable a workspace",
                "add a restbook.json marker to a folder".to_owned(),
                &palette,
                &metrics,
            ),
        ],
    };

    // The file to attach when something fails. Named last because it is the
    // path a bug report wants, and the one users otherwise have to guess.
    rows.push(info_row(
        "Log file",
        log_path.display().to_string(),
        &palette,
        &metrics,
    ));
    rows
}

/// One read-only `label: value` row.
fn info_row(
    label: &str,
    value: String,
    palette: &Palette,
    metrics: &Metrics,
) -> AnyElement {
    h_flex()
        .gap(px(metrics.space_md))
        .items_baseline()
        .child(
            div()
                .w(px(180.))
                .flex_none()
                .text_sm()
                .text_color(palette.muted)
                .child(label.to_owned()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .font_family(palette.mono_font.clone())
                .child(value),
        )
        .into_any_element()
}
