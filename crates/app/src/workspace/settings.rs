//! Application preferences: the handful of choices that are not about one
//! request and not about the dock layout.
//!
//! Stored plaintext in the data directory beside `layout.json` and safe to
//! hand-edit: unknown or out-of-range values fall back to the default. It is an
//! observable entity, not a global, so a pane reading a preference redraws with it.

use std::path::{Path, PathBuf};

use gpui_kit::Context;

/// The preferences file, beside `layout.json` in the data directory.
pub(crate) const SETTINGS_FILE: &str = "settings.json";

/// The preferences this app has. Fields are the *only* app-level settings that
/// exist; anything request-shaped (timeout, redirects, TLS) belongs to the
/// request Settings editor tab and is deliberately not duplicated here.
///
/// Every field has a `Default` that matches the behavior the app had before
/// this file existed, so a missing or unreadable file changes nothing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct AppSettings {
    /// Pretty-print JSON response bodies when the body tab first shows them.
    /// The response pane's own format dropdown still overrides this per
    /// response; this is only the starting state.
    pub(crate) pretty_responses: bool,
    /// How many history entries the sidebar searches for. The original limit
    /// was a constant; making it a setting costs nothing and is the kind of
    /// knob a heavy user reaches for.
    pub(crate) history_limit: u32,
    /// The most history entries the store keeps. This is retention, not display:
    /// older sends are pruned after a successful `append_history`, so the
    /// database cannot grow without bound. See [`NumberField::HistoryMaxEntries`].
    pub(crate) history_max_entries: u32,
    /// The metrics multiplier, `1.0` at rest. Persisted here rather than in the
    /// theme file so a theme stays a pure look.
    pub(crate) zoom: f32,
    /// System / light / dark. `System` follows the OS; the other two pin it.
    pub(crate) appearance: crate::theme::Appearance,
    /// The name of a user theme under `<data dir>/themes/<name>.jsonc`. `None`
    /// (or a name with no file) uses the built-in theme.
    pub(crate) theme: Option<String>,
    /// Ask the release channel for a newer version after launch. This only
    /// gates the automatic check: the Settings dialog's "Check now" always
    /// works, because that is an explicit request.
    #[serde(default = "default_true")]
    pub(crate) check_updates: bool,
    /// The version the user chose *Skip this version* for, so a launch check does
    /// not offer it again. `None` means nothing was skipped; a skipped version
    /// that is no longer newer is simply never matched, so this never needs
    /// clearing by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) skipped_update: Option<String>,
    /// Workspace roots this install has opened, newest first. Local to this
    /// machine: a path is not useful to a teammate cloning the workspace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) recent_workspaces: Vec<String>,
    /// What closing the window does with a collection request that was edited
    /// but not saved. `Ask` prompts and offers to remember the answer; the other
    /// two are that remembered answer.
    pub(crate) save_on_quit: SaveOnQuit,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            pretty_responses: true,
            history_limit: 50,
            history_max_entries: 1_000,
            zoom: 1.0,
            appearance: crate::theme::Appearance::System,
            theme: None,
            check_updates: true,
            skipped_update: None,
            recent_workspaces: Vec::new(),
            save_on_quit: SaveOnQuit::Ask,
        }
    }
}

/// What a window close does with edits that were never saved to a collection.
///
/// A collection-bound tab is the only tab that can lose work here: an unbound
/// draft is written by the debounce. `Ask` prompts and lets the user remember
/// the answer; the other two are that remembered answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SaveOnQuit {
    #[default]
    Ask,
    Save,
    Discard,
}

impl SaveOnQuit {
    pub(crate) const ALL: [SaveOnQuit; 3] =
        [SaveOnQuit::Ask, SaveOnQuit::Save, SaveOnQuit::Discard];

    /// The Settings label for this mode.
    pub(crate) fn label(self) -> &'static str {
        match self {
            SaveOnQuit::Ask => "Ask every time",
            SaveOnQuit::Save => "Always save",
            SaveOnQuit::Discard => "Don't save",
        }
    }

    /// The mode a Settings label names. An unknown label keeps the default, so a
    /// stale dropdown value cannot change the quit behavior by accident.
    pub(crate) fn from_label(label: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|mode| mode.label() == label)
            .unwrap_or_default()
    }
}

/// `true` is the useful default for a switch that enables a convenience: a
/// missing field in an older `settings.json` should not quietly turn checks off.
fn default_true() -> bool {
    true
}

/// The bounds a numeric setting is clamped to when loaded or edited.
///
/// `history_limit` bounds what the sidebar asks the store for; a zero limit
/// would show an empty sidebar, and a very large one would decode more than it
/// can usefully show. `history_max_entries` bounds what is kept at all.
pub(crate) const HISTORY_LIMIT_MIN: u32 = 10;
pub(crate) const HISTORY_LIMIT_MAX: u32 = 1_000;

/// Retention bounds. The floor keeps a trimmed history useful; the ceiling keeps
/// a hand-edited or mistyped value from pinning a whole response-body archive.
pub(crate) const HISTORY_MAX_ENTRIES_MIN: u32 = 100;
pub(crate) const HISTORY_MAX_ENTRIES_MAX: u32 = 100_000;

/// How many workspace roots the Storage page remembers. A short list stays
/// scannable; an older entry is dropped rather than growing the file forever.
pub(crate) const RECENT_WORKSPACES_MAX: usize = 8;

/// Put `root` at the front of `recent`, without duplicates, capped to the
/// recent list's length. Kept separate from [`Settings`] so the rule is testable
/// without an `App`.
pub(crate) fn remember_workspace(recent: &mut Vec<String>, root: &Path) {
    let path = root.to_string_lossy().into_owned();
    if path.trim().is_empty() {
        return;
    }
    recent.retain(|each| each != &path);
    recent.insert(0, path);
    recent.truncate(RECENT_WORKSPACES_MAX);
}

/// Metadata for one numeric preference: the text the dialog shows and the
/// bounds its value is held to.
pub(crate) struct NumberSpec {
    pub(crate) label: &'static str,
    pub(crate) hint: &'static str,
    pub(crate) min: u32,
    pub(crate) max: u32,
}

/// A numeric preference the Settings dialog edits. Every such setting is one
/// variant here: the input entity, the write-through subscription, the clamping,
/// and the persistence are all shared, so adding one is a variant plus one arm
/// in each accessor. Action-style settings (clear history, reset zoom) use the
/// same [`Settings::update`] seam and need no input plumbing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NumberField {
    HistoryLimit,
    HistoryMaxEntries,
}

impl NumberField {
    /// Every numeric preference, in the order the dialog shows them.
    pub(crate) const ALL: [NumberField; 2] =
        [NumberField::HistoryLimit, NumberField::HistoryMaxEntries];

    pub(crate) fn spec(self) -> NumberSpec {
        match self {
            NumberField::HistoryLimit => NumberSpec {
                label: "History search limit",
                hint: "How many entries the sidebar searches.",
                min: HISTORY_LIMIT_MIN,
                max: HISTORY_LIMIT_MAX,
            },
            NumberField::HistoryMaxEntries => NumberSpec {
                label: "History retention",
                hint: "The most entries kept; older sends are pruned.",
                min: HISTORY_MAX_ENTRIES_MIN,
                max: HISTORY_MAX_ENTRIES_MAX,
            },
        }
    }

    /// The current value in `settings`.
    pub(crate) fn read(self, settings: &AppSettings) -> u32 {
        match self {
            NumberField::HistoryLimit => settings.history_limit,
            NumberField::HistoryMaxEntries => settings.history_max_entries,
        }
    }

    /// Write a clamped value into `settings`.
    pub(crate) fn write(self, settings: &mut AppSettings, value: u32) {
        let value = self.clamp(value);
        match self {
            NumberField::HistoryLimit => settings.history_limit = value,
            NumberField::HistoryMaxEntries => settings.history_max_entries = value,
        }
    }

    /// Hold a value to this field's bounds. Kept as a helper so the hand-edited
    /// file and the typed field go through the same range.
    pub(crate) fn clamp(self, value: u32) -> u32 {
        let spec = self.spec();
        value.clamp(spec.min, spec.max)
    }

    /// Parse typed text and clamp it. `None` for a blank or unparseable field,
    /// which means "ignore this input" rather than "use zero". The Settings
    /// dialog's number inputs clamp on their own; this stays for a hand-edited
    /// value and for tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn parse(self, text: &str) -> Option<u32> {
        text.trim()
            .parse::<u32>()
            .ok()
            .map(|value| self.clamp(value))
    }
}

impl AppSettings {
    /// Load from `path`, or the defaults when the file is missing or unreadable.
    ///
    /// A malformed file is not an error the user should see: the defaults are
    /// always valid, and the file is rewritten on the next change. Unknown keys
    /// are ignored by `serde(default)`, so a file from a newer build still
    /// opens.
    pub(crate) fn load(path: &Path) -> Self {
        let mut settings: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        // A hand-edited file can carry a value outside the bounds the UI offers.
        for field in NumberField::ALL {
            let value = field.clamp(field.read(&settings));
            field.write(&mut settings, value);
        }
        settings
    }

    /// Write to `path`, creating the parent directory if needed.
    ///
    /// Failing to persist is not worth interrupting the user for, so the error
    /// is swallowed: the choice still applies for this session.
    pub(crate) fn save(&self, path: &Path) {
        let Ok(json) = serde_json::to_string_pretty(self) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = restbook_store::secure::create_dir(parent);
        }
        let _ = restbook_store::secure::write(path, json);
    }
}

/// The settings entity the workspace and the panes share.
pub(crate) struct Settings {
    current: AppSettings,
    /// Where `current` is written. Optional so the entity can be built in a test
    /// without a filesystem, and so a future embedding could run memory-only.
    path: Option<PathBuf>,
}

impl Settings {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            current: AppSettings::load(&path),
            path: Some(path),
        }
    }

    pub(crate) fn current(&self) -> &AppSettings {
        &self.current
    }

    /// Replace the settings, persist, and redraw dependents. The observer the
    /// response pane holds is what makes a toggle here visible there. Also the
    /// seam for action-style settings (clear history, reset zoom) that do not
    /// own an input.
    pub(crate) fn update(&mut self, next: AppSettings, cx: &mut Context<Self>) {
        self.current = next;
        self.persist();
        cx.notify();
    }

    /// Write `current` to disk. Failing to persist is not worth interrupting the
    /// user for, so the error is swallowed: the choice still applies for this
    /// session.
    fn persist(&self) {
        if let Some(path) = &self.path {
            self.current.save(path);
        }
    }

    /// Record the workspace this session opened, so Storage can offer it again.
    /// A no-op when the list would not change, so a normal launch does not
    /// rewrite `settings.json` just to say what it already said.
    pub(crate) fn record_workspace(&mut self, root: &Path, cx: &mut Context<Self>) {
        let mut next = self.current.clone();
        remember_workspace(&mut next.recent_workspaces, root);
        if next.recent_workspaces != self.current.recent_workspaces {
            self.update(next, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    // Named imports: this crate glob-imports `gpui_kit::*` elsewhere, and that
    // macro set shadows `#[test]`.
    use super::{
        AppSettings, NumberField, SaveOnQuit, HISTORY_LIMIT_MAX, HISTORY_LIMIT_MIN,
        HISTORY_MAX_ENTRIES_MAX, HISTORY_MAX_ENTRIES_MIN, RECENT_WORKSPACES_MAX,
    };

    #[test]
    fn a_workspace_is_remembered_once_and_the_list_is_bounded() {
        use super::remember_workspace;
        use std::path::Path;

        let mut recent = Vec::new();
        remember_workspace(&mut recent, Path::new("C:/work/one"));
        remember_workspace(&mut recent, Path::new("C:/work/two"));
        assert_eq!(recent, ["C:/work/two", "C:/work/one"]);

        // Re-opening an old workspace moves it to the front instead of adding
        // a second copy.
        remember_workspace(&mut recent, Path::new("C:/work/one"));
        assert_eq!(recent, ["C:/work/one", "C:/work/two"]);

        for index in 0..(RECENT_WORKSPACES_MAX + 3) {
            remember_workspace(&mut recent, Path::new(&format!("C:/work/{index}")));
        }
        assert_eq!(recent.len(), RECENT_WORKSPACES_MAX);
        assert_eq!(recent[0], format!("C:/work/{}", RECENT_WORKSPACES_MAX + 2));
    }

    #[test]
    fn the_defaults_are_the_behavior_the_app_had_before_settings_existed() {
        let settings = AppSettings::default();
        assert!(settings.pretty_responses);
        assert_eq!(settings.history_limit, 50);
        // Retention is on by default, so an untouched install cannot grow
        // forever.
        assert_eq!(settings.history_max_entries, 1_000);
        // A file written before the field existed still asks for checks.
        assert!(settings.check_updates);
        // An older file has no quit preference, so it must ask rather than
        // silently discard or silently overwrite a collection request.
        assert_eq!(settings.save_on_quit, SaveOnQuit::Ask);
    }

    #[test]
    fn a_typed_value_is_clamped_and_nonsense_is_ignored() {
        let field = NumberField::HistoryLimit;
        assert_eq!(field.parse("250"), Some(250));
        // A half-typed or out-of-range number is pulled into bounds, not stored
        // as typed.
        assert_eq!(field.parse("1"), Some(HISTORY_LIMIT_MIN));
        assert_eq!(field.parse("999999"), Some(HISTORY_LIMIT_MAX));
        // A blank or unparseable field leaves the last good value alone.
        assert_eq!(field.parse(""), None);
        assert_eq!(field.parse("soon"), None);
        assert_eq!(field.parse("-1"), None);
    }

    #[test]
    fn retention_has_its_own_bounds() {
        let field = NumberField::HistoryMaxEntries;
        assert_eq!(field.parse("1"), Some(HISTORY_MAX_ENTRIES_MIN));
        assert_eq!(field.parse("999999"), Some(HISTORY_MAX_ENTRIES_MAX));
        assert_eq!(field.parse("2000"), Some(2000));
    }

    #[test]
    fn an_out_of_range_limit_is_clamped_on_load() {
        let dir = std::env::temp_dir().join("restbook-settings-clamp-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        std::fs::write(&path, br#"{"history_limit": 0}"#).unwrap();
        assert_eq!(AppSettings::load(&path).history_limit, HISTORY_LIMIT_MIN);

        std::fs::write(&path, br#"{"history_limit": 999999}"#).unwrap();
        assert_eq!(AppSettings::load(&path).history_limit, HISTORY_LIMIT_MAX);

        // Retention is clamped the same way, with its own bounds.
        std::fs::write(&path, br#"{"history_max_entries": 0}"#).unwrap();
        assert_eq!(
            AppSettings::load(&path).history_max_entries,
            HISTORY_MAX_ENTRIES_MIN
        );
        std::fs::write(&path, br#"{"history_max_entries": 999999999}"#).unwrap();
        assert_eq!(
            AppSettings::load(&path).history_max_entries,
            HISTORY_MAX_ENTRIES_MAX
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_quit_choice_round_trips_through_its_label() {
        for mode in SaveOnQuit::ALL {
            assert_eq!(SaveOnQuit::from_label(mode.label()), mode);
        }
        // A value a build has never heard of keeps the prompt.
        assert_eq!(SaveOnQuit::from_label("whenever"), SaveOnQuit::Ask);
    }

    #[test]
    fn a_malformed_file_falls_back_to_the_defaults() {
        let dir = std::env::temp_dir().join("restbook-settings-bad-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        std::fs::write(&path, b"{ this is not json").unwrap();
        assert!(AppSettings::load(&path).pretty_responses);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_file_round_trips() {
        let dir = std::env::temp_dir().join("restbook-settings-roundtrip-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        let settings = AppSettings {
            pretty_responses: false,
            history_limit: 200,
            history_max_entries: 2_000,
            ..AppSettings::default()
        };
        settings.save(&path);
        let loaded = AppSettings::load(&path);
        assert!(!loaded.pretty_responses);
        assert_eq!(loaded.history_limit, 200);
        assert_eq!(loaded.history_max_entries, 2_000);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `settings.json` is app-owned; it is `0600` where the platform has a mode.
    #[cfg(unix)]
    #[test]
    fn a_saved_settings_file_is_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join("restbook-settings-private-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("settings.json");

        AppSettings::default().save(&path);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "settings.json should be private");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
