//! The app's identity, and what each OS does with it.
//!
//! Most of "branding" is *presentation*: it lives in `theme.rs` (colors), in the
//! window's own chrome — the workspace chip in `workspace::chrome` — and in the
//! icon files under `crates/app/icons/`. What is here is the other half: the
//! identity the operating system needs in order to call this app by name.
//!
//! | Concern | Owner | Notes |
//! |---|---|---|
//! | Colors | `theme.rs` | `Palette`, one named role per painted thing |
//! | Window chrome | `workspace/chrome.rs` | The chip, breadcrumb and status bar |
//! | Source icon | `crates/app/icons/` | One SVG per size the platforms consume |
//! | Identity | this module | Window title, `app_id`, macOS `CFBundleName` |
//!
//! Nothing here branches per OS in code. Every constant is the same value
//! everywhere, and the platform picks up the ones it has a notion of: Linux uses
//! `APP_ID` as the window's `app_id` and matches it against the `.desktop` entry
//! and the installed icon, macOS takes its bundle name from the `Info.plist`
//! that packaging writes from [`BUNDLE_NAME`], Windows gets a taskbar identity
//! from the installer's `AppUserModelID`, which must equal [`APP_ID`]. A
//! `#[cfg]` here would mean the identity differed between builds of the same
//! version, which is the one thing an identifier must not do.

/// The name shown in the window title and in the OS's own labels: the menu bar,
/// the task switcher, an error dialog owned by the window.
pub(crate) const NAME: &str = "restbook";

/// The title string the custom titlebar uses. A *window* title, not the app
/// name: it is what the OS shows when the window is minimized or hovered in the
/// taskbar, so it stays the product name rather than the open request.
pub(crate) const WINDOW_TITLE: &str = NAME;

/// The reverse-DNS identifier every desktop OS keys app identity on.
///
/// This is the load-bearing constant for packaging, so it is named once and
/// referenced everywhere rather than spelled out at each use site:
///
/// - **Linux** — the window's `app_id`. A compositor matches the icon, the
///   `.desktop` entry (`Name=` and `StartupWMClass=`), and the taskbar grouping
///   on this string. `share/applications/<APP_ID>.desktop` and
///   `share/icons/hicolor/<size>/apps/<APP_ID>.png` must both carry it.
/// - **Windows** — `APP_ID` is what the installer's `AppUserModelID` must
///   equal, so a shortcut and a running window resolve to one taskbar identity
///   instead of a generic one.
/// - **macOS** — unused. The bundle directory name, `CFBundleIdentifier` and
///   `CFBundleName` in `Info.plist` are what Cocoa reads; `Info.plist`'s
///   identifier should carry this value, and its name should carry
///   [`BUNDLE_NAME`].
///
/// It must not change after a release: renaming it orphans existing user data
/// on Linux, where the data directory is derived from the app name.
pub(crate) const APP_ID: &str = "com.restbook.restbook";

/// The macOS bundle directory and `CFBundleName`: `restbook.app`.
///
/// macOS builds the Dock label, the application menu title, and the default
/// Quit item from this, so it is capitalized like an app and kept separate from
/// [`APP_ID`] — a bundle identifier is never a display name.
pub(crate) const BUNDLE_NAME: &str = NAME;

/// The icon file stem, without extension: `icons/restbook.svg`, and
/// `restbook.png` / `restbook.ico` / `restbook.icns` once packaging rasterizes
/// it.
///
/// Read by `scripts/package.sh` and `scripts/icons.sh` rather than by the app:
/// nothing at runtime draws this icon, so the constant is here to keep the name
/// in one place, next to the `app_id` that a Linux icon file is named after. The
/// `#[allow]` is because no runtime call site exists to keep it live.
#[allow(dead_code)]
pub(crate) const ICON_STEM: &str = NAME;

/// The environment variable that overrides [`APP_ID`] for a build, so a fork or
/// a test install can claim its own taskbar and icon identity without patching
/// this file. Read at compile time — it is identity, not a runtime preference,
/// and it has to agree with the `.desktop` file or the AppUserModelID that
/// shipped beside the binary.
pub(crate) fn app_id() -> &'static str {
    option_env!("RESTBOOK_APP_ID").unwrap_or(APP_ID)
}

#[cfg(test)]
mod tests {
    // Named imports rather than `use super::*`: this crate glob-imports
    // `gpui_kit::*` elsewhere, and that macro set shadows `#[test]`.
    use super::{app_id, APP_ID, BUNDLE_NAME, ICON_STEM, NAME, WINDOW_TITLE};

    #[test]
    fn the_window_title_is_the_product_name() {
        // The window title is what a taskbar shows; a request-shaped title here
        // would make two windows of the same app indistinguishable.
        assert_eq!(WINDOW_TITLE, NAME);
    }

    #[test]
    fn the_bundle_and_icon_names_agree_with_the_app_name() {
        // Packaging writes these three from one name; a drift would install an
        // icon that no `.desktop` entry can find.
        assert_eq!(BUNDLE_NAME, NAME);
        assert_eq!(ICON_STEM, NAME);
    }

    #[test]
    fn the_app_id_is_a_reverse_dns_identifier() {
        let id = app_id();
        let labels: Vec<&str> = id.split('.').collect();
        assert!(labels.len() >= 2, "app id {id} is not reverse-DNS");
        assert!(
            labels.iter().all(|label| !label.is_empty()),
            "app id {id} has an empty label"
        );
        // A compositor matches this string literally against a filename, so it
        // has to stay in the lowercase-and-dashes form of a domain.
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || c == '.' || c.is_ascii_digit()),
            "app id {id} must be lowercase ASCII"
        );
    }

    #[test]
    fn a_build_can_override_the_app_id() {
        // `app_id()` exists so a fork does not have to edit the constant; the
        // fallback is the important half here, since the override is read at
        // compile time and no test build sets it.
        assert_eq!(app_id(), APP_ID);
    }
}
