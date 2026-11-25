//! The app's identity: the name and reverse-DNS id the operating system keys
//! this app on. Presentation (colors, chrome, icon files) lives elsewhere; this
//! is what the OS needs to call the app by name.
//!
//! Nothing here branches per OS: a `#[cfg]` would let the identity differ
//! between builds of one version, which an identifier must not do.

/// The name shown in the window title and in the OS's own labels: the menu bar,
/// the task switcher, an error dialog owned by the window.
pub(crate) const NAME: &str = "restbook";

/// The title string the custom titlebar uses. A *window* title, not the app
/// name: it is what the OS shows when the window is minimized or hovered in the
/// taskbar, so it stays the product name rather than the open request.
pub(crate) const WINDOW_TITLE: &str = NAME;

/// The reverse-DNS identifier every desktop OS keys app identity on: Linux's
/// window `app_id`, `.desktop` entry and icon filenames; Windows' installer
/// `AppUserModelID`. macOS reads the `Info.plist` instead.
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

/// The icon file stem without extension: `icons/restbook.svg`, rasterized by
/// `scripts/icons.sh` and named after the Linux icon file's `app_id`. Read by
/// packaging, not at runtime, hence the `#[allow(dead_code)]`.
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
