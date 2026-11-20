//! The Settings dialog: a left-hand category sidebar over one page of controls,
//! the way Zed presents preferences.
//!
//! A dialog, not a second window or a dock pane: it is drawn by the workspace,
//! which owns the store, root, and settings entity, so it reads live values and
//! Escape dismisses it. Pages group the choices by the thing they change —
//! General, History, Privacy, Updates, Storage, About — and every page also gets
//! the kit's search field, so a setting is reachable by typing its name.
//!
//! Request defaults (timeout, redirects, TLS) stay with the request's Settings
//! editor tab: they travel with a shared collection and are not app-level.

use std::path::PathBuf;

use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::*;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
    Settings as SettingsPanel,
};
use gpui_kit::component::{h_flex, v_flex, WindowExt as _};
use gpui_kit::*;

use super::settings::{AppSettings, NumberField, Settings};
use super::{storage, Workspace};
use crate::branding;
use crate::icons::IconName;
use crate::logging;
use crate::theme::{self, Appearance, Metrics, Palette};
use crate::updater;

/// The value the theme picker uses for the built-in theme, which has no file
/// name of its own. It is a sentinel rather than an empty string so the menu
/// row has a label that matches its value.
const BUILT_IN_THEME: &str = "Built-in";

/// Open the Settings dialog. It reads the same `Settings` entity the workspace
/// holds, so a toggle here is visible to every pane at once — the response pane
/// watching for a pretty-print change is the first such pane.
pub(crate) fn open(
    settings: Entity<Settings>,
    workspace_root: Option<PathBuf>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    // Listing theme files is a filesystem read. Do it once when the dialog
    // opens; the builder below runs again on every redraw, and reading the
    // directory on each frame would put I/O in the render path.
    let themes = storage::list_themes();

    window.open_dialog(cx, move |dialog, window, cx| {
        let settings = settings.clone();
        let workspace = workspace.clone();
        let workspace_root = workspace_root.clone();
        let themes = themes.clone();

        let pages = vec![
            general_page(&settings, &workspace, &themes),
            history_page(&settings),
            privacy_page(&workspace),
            updates_page(&settings),
            storage_page(&workspace_root),
            about_page(),
        ];

        // Fill most of the window, but stay inside a small one. `Settings`
        // draws a resizable sidebar and a scrolling page, so it needs a
        // definite height rather than the dialog's content-driven default.
        let height = (window.viewport_size().height * 0.75).clamp(px(360.), px(640.));

        dialog
            .title(format!("Settings \u{b7} {}", crate::theme::theme_name(cx)))
            .w(px(880.))
            .child(
                div().h(height).w_full().child(
                    SettingsPanel::new("restbook-settings")
                        // A category rail, not a second column of work: wide enough
                        // for the longest page name, subordinate to the controls.
                        .sidebar_width(px(190.))
                        .sidebar_size_range(px(170.)..px(300.))
                        .pages(pages),
                ),
            )
    });
}

/// General: look, response presentation, and the window arrangement.
fn general_page(
    settings: &Entity<Settings>,
    workspace: &WeakEntity<Workspace>,
    themes: &[String],
) -> SettingPage {
    SettingPage::new("General")
        .icon(IconName::Settings)
        .description("How the window looks, how responses are shown, and how panes are arranged.")
        .group(
            SettingGroup::new()
                .title("Appearance")
                .item(
                    SettingItem::new("Appearance", appearance_field(settings))
                        .description("Follow the system, or pin light or dark."),
                )
                .item(
                    SettingItem::new("Theme", theme_field(settings, themes.to_vec())).description(
                        "The built-in theme, or a file from the data directory's themes folder.",
                    ),
                )
                .item(
                    SettingItem::new("Zoom", zoom_field(settings))
                        .description("Percent of the default size; 100% restores it."),
                ),
        )
        .group(
            SettingGroup::new().title("Responses").item(
                SettingItem::new(
                    "Pretty-print JSON",
                    bool_field(
                        settings,
                        |settings| settings.pretty_responses,
                        |settings, value| settings.pretty_responses = value,
                        true,
                    ),
                )
                .description(
                    "Format JSON bodies when they first appear. The response pane's \
                         format control still overrides this per response.",
                ),
            ),
        )
        .group(
            SettingGroup::new()
                .title("Layout")
                .item(reset_layout_item(workspace.clone())),
        )
}

/// History: the two numeric limits the sidebar and the store read.
fn history_page(settings: &Entity<Settings>) -> SettingPage {
    SettingPage::new("History")
        .icon(IconName::Clock)
        .description("How past sends are searched, and how many are kept.")
        .group(
            SettingGroup::new()
                .title("Limits")
                .items(NumberField::ALL.into_iter().map(|field| {
                    let spec = field.spec();
                    SettingItem::new(spec.label, numbered_field(settings, field))
                        .description(spec.hint)
                })),
        )
}

/// Privacy: the two destructive actions that were previously reachable only
/// from their panels, plus the fact that nothing leaves this machine.
fn privacy_page(workspace: &WeakEntity<Workspace>) -> SettingPage {
    SettingPage::new("Privacy")
        .icon(IconName::ShieldCheck)
        .description("Data this app keeps on this machine. Clearing it cannot be undone.")
        .group(
            SettingGroup::new()
                .title("Clear data")
                .item(clear_history_item(workspace.clone()))
                .item(clear_cookies_item(workspace.clone())),
        )
}

/// Updates: the compiled version, the channel it polls, and the launch switch.
fn updates_page(settings: &Entity<Settings>) -> SettingPage {
    SettingPage::new("Updates")
        .icon(IconName::RotateCcw)
        .description("This build checks a release manifest and reports; it never downloads or installs on its own.")
        .group(
            SettingGroup::new()
                .item(version_item())
                .item(
                    SettingItem::new(
                        "Check for updates on launch",
                        bool_field(
                            settings,
                            |settings| settings.check_updates,
                            |settings, value| settings.check_updates = value,
                            true,
                        ),
                    )
                    .description("The automatic check only. \u{201c}Check now\u{201d} always runs."),
                ),
        )
}

/// Storage: where this session reads and writes. Read-only, but useful in a bug
/// report and otherwise invisible.
fn storage_page(workspace_root: &Option<PathBuf>) -> SettingPage {
    SettingPage::new("Storage")
        .icon(IconName::HardDrive)
        .description("Where this session reads and writes. Read-only.")
        .group(SettingGroup::new().item(storage_item(workspace_root)))
}

/// About: the identity the operating system is shown.
fn about_page() -> SettingPage {
    SettingPage::new("About")
        .icon(IconName::Info)
        .description("Who this build says it is to the operating system.")
        .group(SettingGroup::new().item(identity_item()))
}

/// Write a change through the settings entity, keeping every other field. The
/// entity persists and notifies, which is what the workspace's theme observer
/// and the response pane listen for.
fn update(settings: &Entity<Settings>, cx: &mut App, edit: impl FnOnce(&mut AppSettings)) {
    settings.update(cx, |settings, cx| {
        let mut next = settings.current().clone();
        edit(&mut next);
        settings.update(next, cx);
    });
}

/// A switch for a boolean preference. `read`/`write` are field accessors rather
/// than a closure over the whole struct, so the call site names the field once.
fn bool_field(
    settings: &Entity<Settings>,
    read: fn(&AppSettings) -> bool,
    write: fn(&mut AppSettings, bool),
    default: bool,
) -> SettingField<bool> {
    let reader = settings.clone();
    let writer = settings.clone();
    SettingField::switch(
        move |cx| read(reader.read(cx).current()),
        move |value, cx| update(&writer, cx, |next| write(next, value)),
    )
    .default_value(default)
}

/// A numeric field for one [`NumberField`], with its bounds and default taken
/// from the same spec the loader clamps against.
fn numbered_field(settings: &Entity<Settings>, field: NumberField) -> SettingField<f64> {
    let spec = field.spec();
    let reader = settings.clone();
    let writer = settings.clone();
    SettingField::number_input(
        NumberFieldOptions {
            min: spec.min as f64,
            max: spec.max as f64,
            step: 1.0,
        },
        move |cx| field.read(reader.read(cx).current()) as f64,
        move |value, cx| {
            let value = field.clamp(value.round() as u32);
            update(&writer, cx, |next| field.write(next, value));
        },
    )
    .default_value(field.read(&AppSettings::default()) as f64)
}

/// The appearance axis as a segmented control: the current mode is a persistent
/// selected state, not merely hover, and each segment keeps a stable id.
fn appearance_field(settings: &Entity<Settings>) -> SettingField<SharedString> {
    let reader = settings.clone();
    let writer = settings.clone();
    let dirty = settings.clone();
    let resetter = settings.clone();
    SettingField::render(move |_, _, cx| {
        let current = reader.read(cx).current().appearance;
        h_flex()
            .gap_1()
            .children(
                [Appearance::System, Appearance::Light, Appearance::Dark].map(|mode| {
                    let settings = writer.clone();
                    Button::new(format!("settings-appearance-{}", appearance_key(mode)))
                        .label(appearance_label(mode))
                        .ghost()
                        .compact()
                        .selected(mode == current)
                        .toggled(mode == current)
                        .on_click(move |_, _, cx| {
                            update(&settings, cx, |next| next.appearance = mode);
                        })
                        .into_any_element()
                }),
            )
            .into_any_element()
    })
    .on_reset(
        move |cx| dirty.read(cx).current().appearance != Appearance::System,
        move |_, cx| update(&resetter, cx, |next| next.appearance = Appearance::System),
    )
}

/// The stable name of each appearance mode, used as its element id. Kept apart
/// from the display label so the two can change independently.
fn appearance_key(mode: Appearance) -> &'static str {
    match mode {
        Appearance::System => "system",
        Appearance::Light => "light",
        Appearance::Dark => "dark",
    }
}

fn appearance_label(mode: Appearance) -> &'static str {
    match mode {
        Appearance::System => "System",
        Appearance::Light => "Light",
        Appearance::Dark => "Dark",
    }
}

/// The theme file picker: the built-in theme plus every `<name>.jsonc` a user
/// has dropped into the data directory's `themes/` folder.
fn theme_field(settings: &Entity<Settings>, themes: Vec<String>) -> SettingField<SharedString> {
    let reader = settings.clone();
    let writer = settings.clone();

    let mut options = vec![(
        SharedString::from(BUILT_IN_THEME),
        SharedString::from(BUILT_IN_THEME),
    )];
    options.extend(themes.into_iter().map(|name| {
        let name = SharedString::from(name);
        (name.clone(), name)
    }));

    SettingField::dropdown(
        options,
        move |cx| {
            reader
                .read(cx)
                .current()
                .theme
                .clone()
                .unwrap_or_else(|| BUILT_IN_THEME.to_owned())
                .into()
        },
        move |value, cx| {
            let theme = (value.as_ref() != BUILT_IN_THEME).then(|| value.to_string());
            update(&writer, cx, |next| next.theme = theme);
        },
    )
    .default_value(BUILT_IN_THEME)
}

/// Zoom as a percentage of the default size. The value is clamped here as well
/// as by the number input, so a hand-typed value cannot reach the theme global
/// outside the readable range.
fn zoom_field(settings: &Entity<Settings>) -> SettingField<f64> {
    let reader = settings.clone();
    let writer = settings.clone();
    SettingField::number_input(
        NumberFieldOptions {
            min: (theme::ZOOM_MIN * 100.0).round() as f64,
            max: (theme::ZOOM_MAX * 100.0).round() as f64,
            step: 10.0,
        },
        move |cx| (reader.read(cx).current().zoom * 100.0).round() as f64,
        move |value, cx| {
            let zoom = theme::clamp_zoom((value / 100.0) as f32);
            update(&writer, cx, |next| next.zoom = zoom);
        },
    )
    .default_value(100.0)
}

/// The version row: the compiled version and the explicit check. The check is
/// phase 1 of the updater — it fetches and compares, and installs nothing.
fn version_item() -> SettingItem {
    let description = match updater::manifest_url() {
        Some(url) => format!("Checks {url}"),
        None => "This build has no update channel, so it cannot check for releases.".to_owned(),
    };

    SettingItem::new(
        "Version",
        SettingField::render(|_, _, cx| {
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .font_family(Palette::new(cx).mono_font.clone())
                        .child(updater::CURRENT_VERSION),
                )
                .child(
                    Button::new("settings-check-updates")
                        .label("Check now")
                        .outline()
                        .compact()
                        .tooltip("Fetch the release manifest and compare it with this build")
                        .on_click(|_, window, cx| check_now(window, cx)),
                )
                .into_any_element()
        }),
    )
    .description(description)
    .keywords(["version", "update", "release", "check"])
}

/// Restore the first-run arrangement. The dialog closes first so the reset (and
/// its toast) is the only thing the user sees change.
fn reset_layout_item(workspace: WeakEntity<Workspace>) -> SettingItem {
    SettingItem::new(
        "Reset layout",
        SettingField::render(move |_, _, _| {
            let workspace = workspace.clone();
            Button::new("settings-reset-layout")
                .label("Reset layout")
                .outline()
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    workspace
                        .update(cx, |workspace, cx| workspace.reset_layout(window, cx))
                        .ok();
                })
        }),
    )
    .description(
        "Restores the first-run arrangement: Collections on the left, Request over \
         Response in the centre.",
    )
    .keywords(["layout", "panels", "sidebar", "dock", "reset", "restore"])
}

/// Delete every stored send. The confirmation is a separate window-level
/// surface, so Settings closes and the alert becomes the only dialog.
fn clear_history_item(workspace: WeakEntity<Workspace>) -> SettingItem {
    SettingItem::new(
        "Clear history",
        SettingField::render(move |_, _, _| {
            let workspace = workspace.clone();
            Button::new("settings-clear-history")
                .label("Clear history\u{2026}")
                .outline()
                .on_click(move |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.request_clear_history(window, cx)
                        })
                        .ok();
                })
        }),
    )
    .description("Delete every stored send. A confirmation appears before anything is removed.")
    .keywords(["history", "delete", "clear", "privacy", "data"])
}

/// Drop the shared cookie jar. This is the only way to start a clean session
/// without restarting the app, and it previously had no UI at all.
fn clear_cookies_item(workspace: WeakEntity<Workspace>) -> SettingItem {
    SettingItem::new(
        "Cookies",
        SettingField::render(move |_, _, _| {
            let workspace = workspace.clone();
            Button::new("settings-clear-cookies")
                .label("Clear cookies")
                .outline()
                .on_click(move |_, _, cx| {
                    workspace
                        .update(cx, |workspace, cx| workspace.clear_cookies(cx))
                        .ok();
                })
        }),
    )
    .description(
        "Drop every cookie this session stored. Requests made after this start with an \
         empty jar.",
    )
    .keywords(["cookies", "session", "clear", "privacy", "network"])
}

/// The read-only facts about where this session reads and writes. These are
/// already known to the workspace, so nothing here is guesswork.
fn storage_rows(workspace_root: &Option<PathBuf>) -> Vec<(String, String)> {
    let data_dir = storage::data_dir();
    // The log can be moved with `RESTBOOK_LOG_FILE`, so name the file that is
    // actually open rather than the default path.
    let log_path = logging::file_path()
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join(logging::LOG_FILE));

    let mut rows = match workspace_root {
        Some(root) => vec![
            ("Mode".to_owned(), "Workspace".to_owned()),
            ("Root".to_owned(), root.display().to_string()),
            (
                "Collections".to_owned(),
                root.join("collections").display().to_string(),
            ),
            (
                "Environments".to_owned(),
                root.join("environments").display().to_string(),
            ),
            (
                "Preferences".to_owned(),
                data_dir.join("settings.json").display().to_string(),
            ),
        ],
        None => vec![
            ("Mode".to_owned(), "Local database".to_owned()),
            ("Data directory".to_owned(), data_dir.display().to_string()),
            (
                "Enable a workspace".to_owned(),
                "add a restbook.json marker to a folder".to_owned(),
            ),
        ],
    };

    // The file to attach when something fails. Named last because it is the
    // path a bug report wants, and the one users otherwise have to guess.
    rows.push(("Log file".to_owned(), log_path.display().to_string()));
    rows
}

fn storage_item(workspace_root: &Option<PathBuf>) -> SettingItem {
    let rows = storage_rows(workspace_root);
    SettingItem::render(move |_, _, cx| {
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);
        v_flex()
            .gap_3()
            .children(
                rows.iter()
                    .map(|(label, value)| info_row(label, value.clone(), &palette, &metrics)),
            )
            .into_any_element()
    })
    .keywords([
        "storage",
        "data",
        "directory",
        "path",
        "log",
        "workspace",
        "collections",
        "environments",
        "preferences",
    ])
}

/// Who this build says it is, and which OS surface picks each part up.
///
/// Everything here comes from [`crate::branding`] — the same constants the
/// window's `app_id` and the packaging templates read — so this group is a
/// readout of the identity rather than a second copy of it.
fn identity_item() -> SettingItem {
    SettingItem::render(|_, _, cx| {
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);
        v_flex()
            .gap_3()
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
                info_row(
                    "Version",
                    updater::CURRENT_VERSION.to_owned(),
                    &palette,
                    &metrics,
                ),
            ])
            .into_any_element()
    })
    .keywords(["about", "name", "app id", "bundle", "icon", "version"])
}

/// One read-only `label: value` row.
fn info_row(label: &str, value: String, palette: &Palette, metrics: &Metrics) -> AnyElement {
    h_flex()
        .gap(px(metrics.space_md))
        .items_baseline()
        .child(
            div()
                .w(px(150.))
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
