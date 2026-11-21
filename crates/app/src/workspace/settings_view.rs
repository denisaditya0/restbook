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
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
    Settings as SettingsPanel,
};
use gpui_kit::component::{h_flex, v_flex, Disableable as _, WindowExt as _};
use gpui_kit::*;

use super::settings::{AppSettings, NumberField, Settings};
use super::updates::{self, Updates};
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

    // The update state lives outside the builder for the same reason: a check
    // reports back long after the frame that started it, and the builder is a
    // closure that is rebuilt on every redraw.
    let updates = cx.new(|_| Updates::new());

    window.open_dialog(cx, move |dialog, window, cx| {
        let settings = settings.clone();
        let workspace = workspace.clone();
        let workspace_root = workspace_root.clone();
        let themes = themes.clone();
        let updates = updates.clone();

        let pages = vec![
            general_page(&settings, &workspace, &themes),
            history_page(&settings),
            privacy_page(&workspace),
            updates_page(&settings, &updates),
            storage_page(&workspace_root, &workspace),
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

/// Updates: the compiled version, what the channel says, and the install.
fn updates_page(settings: &Entity<Settings>, updates: &Entity<Updates>) -> SettingPage {
    SettingPage::new("Updates")
        .icon(IconName::RotateCcw)
        .description("Check the release channel, then install what it offers.")
        .group(
            SettingGroup::new().item(version_item(updates)).item(
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

/// Storage: where this session reads and writes. The rows are facts for a bug
/// report; when the app is not in a workspace, one row is a control that makes
/// a folder into one.
fn storage_page(
    workspace_root: &Option<PathBuf>,
    workspace: &WeakEntity<Workspace>,
) -> SettingPage {
    let mut group = SettingGroup::new().item(storage_item(workspace_root));
    if workspace_root.is_none() {
        group = group.item(enable_workspace_item(workspace.clone()));
    }
    SettingPage::new("Storage")
        .icon(IconName::HardDrive)
        .description("Where this session reads and writes.")
        .group(group)
}

/// Make a folder into a workspace root.
///
/// The choice only takes effect on the next launch: the store and every pane are
/// built from the root at startup, so switching in place would rebuild the app
/// mid-dialog. Saying so in the row is cheaper than pretending the button did
/// something this frame.
fn enable_workspace_item(workspace: WeakEntity<Workspace>) -> SettingItem {
    SettingItem::new(
        "Workspace",
        SettingField::render(move |_, _, _| {
            let workspace = workspace.clone();
            Button::new("settings-enable-workspace")
                .label("Choose a folder\u{2026}")
                .outline()
                .on_click(move |_, window, cx| {
                    choose_workspace_folder(workspace.clone(), window, cx)
                })
                .into_any_element()
        }),
    )
    .description(
        "Keep collections and environments as files in a folder you choose, so \
         they can be shared or committed. Takes effect the next time restbook \
         starts.",
    )
    .keywords(["workspace", "folder", "enable", "share", "git", "files"])
}

/// Pick a folder, then write the marker into it.
///
/// The picker is the platform's, through GPUI, so no dialog dependency is
/// involved. The result is reported as a flash on the workspace rather than in
/// this dialog: the folder chooser closes over it, and a message the user
/// cannot see is the same as no message.
fn choose_workspace_folder(workspace: WeakEntity<Workspace>, _window: &mut Window, cx: &mut App) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Choose a folder for the workspace".into()),
    });

    // `spawn` rather than `spawn_in`: the oneshot carries the answer on its own,
    // so the task needs no window to run on.
    cx.spawn(async move |cx: &mut AsyncApp| {
        let Ok(Ok(Some(paths))) = paths.await else {
            return;
        };
        let Some(dir) = paths.into_iter().next() else {
            return;
        };

        let (message, failed) = match storage::enable_workspace(&dir) {
            Ok(()) => (
                format!(
                    "Workspace enabled at {}. Restart restbook to open it.",
                    dir.display()
                ),
                false,
            ),
            Err(storage::EnableWorkspaceError::AlreadyAWorkspace) => (
                format!("{} is already a workspace folder.", dir.display()),
                true,
            ),
            Err(storage::EnableWorkspaceError::NotADirectory) => {
                (format!("{} is not a folder.", dir.display()), true)
            }
            Err(storage::EnableWorkspaceError::WriteFailed) => (
                format!(
                    "Couldn\u{2019}t write the workspace marker into {}.",
                    dir.display()
                ),
                true,
            ),
        };

        let kind = if failed {
            crate::workspace::chrome::FlashKind::Error
        } else {
            crate::workspace::chrome::FlashKind::Success
        };
        workspace
            .update(cx, |workspace, cx| {
                workspace.notice_flash(kind, message, cx)
            })
            .ok();
    })
    .detach();
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

/// The version row: the compiled version, the check, and the install. Everything
/// the check learns is rendered here rather than in a floating toast, because the
/// dialog is open and the page is where the user is looking.
fn version_item(updates: &Entity<Updates>) -> SettingItem {
    // The URL names a path inside the repository and says nothing a user can
    // act on, so the row names the channel instead. `RESTBOOK_UPDATE_URL`
    // overrides it, and the log line on a failed check carries the exact
    // address for when the address is what is wrong.
    let description = match updater::manifest_url() {
        Some(_) => "Downloads from GitHub releases".to_owned(),
        None => "This build has no update channel, so it cannot check for releases.".to_owned(),
    };

    let updates = updates.clone();
    SettingItem::new(
        "Version",
        SettingField::render(move |_, _, cx| version_field(&updates, cx)),
    )
    .description(description)
    .keywords(["version", "update", "release", "check", "install"])
}

/// The version, one button whose meaning follows the state, and the sentence
/// that explains it.
fn version_field(updates: &Entity<Updates>, cx: &mut App) -> AnyElement {
    let palette = Palette::new(cx);
    let metrics = Metrics::current(cx);
    let status = updates.read(cx).status().clone();

    let row = h_flex()
        .gap_2()
        .items_center()
        .child(
            div()
                .text_sm()
                .font_family(palette.mono_font.clone())
                .child(updater::CURRENT_VERSION),
        )
        .child(update_button(updates, &status));

    let mut column = v_flex().gap_1().child(row);
    if let Some((sentence, tone)) = update_sentence(&status) {
        column = column.child(
            div()
                .text_size(px(metrics.font_xs))
                .text_color(tone.color(&palette))
                .child(sentence),
        );
    }
    column.into_any_element()
}

/// The one button the page offers. It is the next thing to do in the current
/// state, not a menu: check, or install what the check found.
fn update_button(updates: &Entity<Updates>, status: &updates::Status) -> Button {
    match status {
        updates::Status::Checking => Button::new("settings-check-updates")
            .label("Checking\u{2026}")
            .outline()
            .compact()
            .loading(true),
        updates::Status::Downloading { .. } => Button::new("settings-install-update")
            .label("Downloading\u{2026}")
            .compact()
            .loading(true),
        updates::Status::Available { asset: None, .. } => Button::new("settings-install-update")
            .label(format!("No build for {}", updater::platform_key()))
            .outline()
            .compact()
            .disabled(true)
            .tooltip("This release published nothing this machine can install"),
        updates::Status::Ready {
            closes_app: true, ..
        } => Button::new("settings-install-update")
            .label("Restarting\u{2026}")
            .outline()
            .compact()
            .loading(true),
        updates::Status::Ready { .. } => Button::new("settings-install-update")
            .label("Downloaded")
            .outline()
            .compact()
            .disabled(true)
            .tooltip("The package is open; finish the install there"),
        updates::Status::Available { asset: Some(_), .. } => {
            let updates = updates.clone();
            Button::new("settings-install-update")
                .label("Download & install")
                .compact()
                .tooltip("Download this platform's build, verify its digest, and open it")
                .on_click(move |_, _, cx| {
                    if let Some(asset) = updates.read(cx).asset() {
                        updates.update(cx, |updates, cx| updates.install(asset, cx));
                    }
                })
        }
        updates::Status::Idle | updates::Status::UpToDate { .. } | updates::Status::Failed(_) => {
            let updates = updates.clone();
            Button::new("settings-check-updates")
                .label("Check now")
                .outline()
                .compact()
                .tooltip("Fetch the release manifest and compare it with this build")
                .on_click(move |_, _, cx| updates.update(cx, |updates, cx| updates.check(cx)))
        }
    }
}

/// The tone a status line is read in. A separate value from the colour so the
/// sentence can be tested without building a palette, which needs an `App`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tone {
    Muted,
    Success,
    Info,
    Warning,
    Danger,
}

impl Tone {
    fn color(self, palette: &Palette) -> Hsla {
        match self {
            Self::Muted => palette.muted,
            Self::Success => palette.success,
            Self::Info => palette.info,
            Self::Warning => palette.warning,
            Self::Danger => palette.danger,
        }
    }
}

/// The sentence under the version, in the tone the state deserves. `None` while
/// nothing has been checked in this session: an idle page says nothing rather
/// than saying "idle".
fn update_sentence(status: &updates::Status) -> Option<(String, Tone)> {
    let line = match status {
        updates::Status::Idle => return None,
        updates::Status::Checking => (
            "Contacting the release channel\u{2026}".to_owned(),
            Tone::Muted,
        ),
        updates::Status::UpToDate { current } => {
            (format!("restbook {current} is up to date."), Tone::Success)
        }
        updates::Status::Available {
            latest,
            notes,
            asset: Some(_),
        } => {
            let mut sentence = format!("restbook {latest} is available.");
            if let Some(notes) = notes.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                sentence.push(' ');
                sentence.push_str(notes);
            }
            (sentence, Tone::Info)
        }
        updates::Status::Available { latest, .. } => (
            format!(
                "restbook {latest} is available, but no build is published for {}.",
                updater::platform_key()
            ),
            Tone::Warning,
        ),
        updates::Status::Downloading { latest } => (
            format!("Downloading restbook {latest}\u{2026}"),
            Tone::Muted,
        ),
        updates::Status::Ready {
            latest,
            closes_app: true,
            ..
        } => (
            format!(
                "The installer for restbook {latest} is opening; restbook will close so \
                 it can be replaced."
            ),
            Tone::Success,
        ),
        updates::Status::Ready { latest, path, .. } => (
            format!(
                "restbook {latest} was downloaded to {}. Open it to finish installing.",
                path.display()
            ),
            Tone::Success,
        ),
        updates::Status::Failed(message) => (message.clone(), Tone::Danger),
    };
    Some(line)
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
                .on_click(move |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| workspace.clear_cookies(window, cx))
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

#[cfg(test)]
mod tests {
    use super::{update_sentence, Tone};
    use crate::updater::{Asset, CURRENT_VERSION};
    use crate::workspace::updates::Status;
    use std::path::PathBuf;

    fn asset() -> Asset {
        Asset {
            url: "https://example.com/restbook.exe".to_owned(),
            sha256: None,
            signature: None,
        }
    }

    /// Each state says one thing, and the page never shows a stale sentence for
    /// a state it is not in.
    #[test]
    fn every_state_explains_itself() {
        assert_eq!(update_sentence(&Status::Idle), None);

        let (text, tone) = update_sentence(&Status::Checking).unwrap();
        assert!(text.starts_with("Contacting"));
        assert_eq!(tone, Tone::Muted);

        let (text, tone) = update_sentence(&Status::UpToDate {
            current: CURRENT_VERSION.to_owned(),
        })
        .unwrap();
        assert!(text.contains("up to date"));
        assert_eq!(tone, Tone::Success);

        let (text, tone) = update_sentence(&Status::Available {
            latest: "0.2.0".to_owned(),
            notes: Some("Fixes the thing.".to_owned()),
            asset: Some(asset()),
        })
        .unwrap();
        assert!(text.contains("0.2.0 is available"));
        assert!(text.ends_with("Fixes the thing."));
        assert_eq!(tone, Tone::Info);

        // A release with nothing for this machine is still news, but it is not
        // an install the page can offer.
        let (text, tone) = update_sentence(&Status::Available {
            latest: "0.2.0".to_owned(),
            notes: None,
            asset: None,
        })
        .unwrap();
        assert!(text.contains("no build is published"));
        assert_eq!(tone, Tone::Warning);

        let (text, tone) = update_sentence(&Status::Downloading {
            latest: "0.2.0".to_owned(),
        })
        .unwrap();
        assert!(text.starts_with("Downloading"));
        assert_eq!(tone, Tone::Muted);

        let (text, _) = update_sentence(&Status::Ready {
            latest: "0.2.0".to_owned(),
            path: PathBuf::from("/tmp/restbook.exe"),
            closes_app: true,
        })
        .unwrap();
        assert!(text.contains("will close"));

        let (text, _) = update_sentence(&Status::Ready {
            latest: "0.2.0".to_owned(),
            path: PathBuf::from("/tmp/restbook.deb"),
            closes_app: false,
        })
        .unwrap();
        assert!(text.contains("Open it to finish"));

        let (text, tone) = update_sentence(&Status::Failed("offline".to_owned())).unwrap();
        assert_eq!(text, "offline");
        assert_eq!(tone, Tone::Danger);
    }
}
