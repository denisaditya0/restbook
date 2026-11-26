//! The design tokens: the palette and the metrics, both loaded from a theme
//! file so a new look is data rather than code. Panes render from a [`Palette`]
//! and [`Metrics`], not `cx.theme()` or inline `px(…)`, so roles are named for
//! purpose (`selected`, `row_height`) and a swap renames nothing. The embedded
//! [`DEFAULT_THEME`] is overlaid role by role by `<data dir>/themes/<name>.jsonc`;
//! unset colors fall through.

use std::rc::Rc;

use gpui_kit::component::{try_parse_color, ActiveTheme as _};
use gpui_kit::{App, Global, Hsla, SharedString};
use serde::Deserialize;

use restbook_core::Method;

/// The built-in theme, embedded so the binary is self-contained.
pub(crate) const DEFAULT_THEME: &str = include_str!("../themes/default-dark.jsonc");

/// How much one zoom step scales the metrics. A fixed ratio, so zooming in
/// then out returns to exactly where it started.
pub(crate) const ZOOM_STEP: f32 = 1.1;

/// The zoom is clamped to this range. Past it, text is either illegible or the
/// layout stops fitting the small rows this app deliberately keeps.
pub(crate) const ZOOM_MIN: f32 = 0.6;
pub(crate) const ZOOM_MAX: f32 = 3.0;

/// The light/dark axis, independent of which theme *file* is loaded. System
/// follows the OS appearance; the other two pin it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    /// The concrete light/dark mode to paint with. `ThemeMode` has no "system"
    /// variant, so `System` is resolved against the OS appearance the platform
    /// reports for the window. `None` (no window yet) follows light, which is
    /// the component default.
    pub(crate) fn mode(
        self,
        system: Option<gpui_kit::WindowAppearance>,
    ) -> gpui_kit::component::ThemeMode {
        use gpui_kit::component::ThemeMode;
        let dark = match self {
            Self::Light => false,
            Self::Dark => true,
            Self::System => matches!(
                system,
                Some(gpui_kit::WindowAppearance::Dark | gpui_kit::WindowAppearance::VibrantDark)
            ),
        };
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        }
    }

    /// The appearance to hand the platform: `None` releases the window back to
    /// the OS so it reports the system appearance, the other two pin it. Only
    /// `System` may be `None`.
    pub(crate) fn window_appearance(self) -> Option<gpui_kit::WindowAppearance> {
        match self {
            Self::System => None,
            Self::Light => Some(gpui_kit::WindowAppearance::Light),
            Self::Dark => Some(gpui_kit::WindowAppearance::Dark),
        }
    }
}

/// The theme state the whole app reads. A global rather than an entity, because
/// panes build their palette with `Palette::new(cx)` and this keeps that call
/// signature: a pane never threads a theme handle through its constructor.
///
/// Zoom lives here too. It is a multiplier over [`Metrics`], so changing it is
/// one value and a redraw, not an edit to every `px(…)` in the app.
#[derive(Clone)]
pub(crate) struct ThemeRegistry {
    file: Rc<ThemeFile>,
    base_metrics: Metrics,
    zoom: f32,
    appearance: Appearance,
}

impl Global for ThemeRegistry {}

impl ThemeRegistry {
    /// Install the theme from a file's text plus the persisted zoom and
    /// appearance. Called once at startup, and again when the user switches.
    pub(crate) fn install(cx: &mut App, text: &str, zoom: f32, appearance: Appearance) {
        let file = parse(text);
        let base_metrics = file
            .metrics
            .clone()
            .map(|tokens| tokens.over(Metrics::default()))
            .unwrap_or_default();
        cx.set_global(Self {
            file: Rc::new(file),
            base_metrics,
            zoom: clamp_zoom(zoom),
            appearance,
        });
    }

    /// The name of the theme file in use, for the settings dialog title.
    pub(crate) fn name(&self) -> &str {
        &self.file.name
    }

    fn get(cx: &App) -> Self {
        cx.try_global::<Self>()
            .cloned()
            .unwrap_or_else(Self::default)
    }
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        let file = parse(DEFAULT_THEME);
        let base_metrics = file
            .metrics
            .clone()
            .map(|tokens| tokens.over(Metrics::default()))
            .unwrap_or_default();
        Self {
            file: Rc::new(file),
            base_metrics,
            zoom: 1.0,
            appearance: Appearance::System,
        }
    }
}

/// Clamp a zoom factor into the range the controls offer, so a hand-edited
/// settings file cannot make the window unreadable.
pub(crate) fn clamp_zoom(zoom: f32) -> f32 {
    if zoom.is_finite() {
        zoom.clamp(ZOOM_MIN, ZOOM_MAX)
    } else {
        1.0
    }
}

/// The theme's name, for the settings dialog and the file it came from.
pub(crate) fn theme_name(cx: &App) -> SharedString {
    SharedString::from(ThemeRegistry::get(cx).name().to_owned())
}

/// Set the zoom multiplier and return the clamped value that was stored. The
/// caller redraws; see [`Redraw`].
pub(crate) fn set_zoom(cx: &mut App, zoom: f32) -> f32 {
    let clamped = clamp_zoom(zoom);
    let mut registry = ThemeRegistry::get(cx);
    registry.zoom = clamped;
    cx.set_global(registry);
    clamped
}

/// The zoom in effect.
pub(crate) fn zoom(cx: &App) -> f32 {
    ThemeRegistry::get(cx).zoom
}

/// The appearance in effect.
pub(crate) fn appearance(cx: &App) -> Appearance {
    ThemeRegistry::get(cx).appearance
}

/// Push the active theme file's roles onto the live component theme.
///
/// `apply_appearance` switches the component theme to the built-in light/dark
/// set first, so this must run after it: a mode switch reloads the built-in
/// colors. Every `gpui-kit` control — button, input, menu, dialog, scrollbar,
/// tab bar — then paints from the same palette the panes get from [`Palette`].
/// A role the file leaves out keeps the component theme's value.
pub(crate) fn sync_component_theme(cx: &mut App) {
    let registry = ThemeRegistry::get(cx);
    let Some(colors) = registry.file.colors_for(cx.theme().is_dark()) else {
        return;
    };

    gpui_kit::component::Theme::update(cx, |theme| {
        let c = &mut theme.colors;
        // Surfaces and chrome.
        set_role(&colors.background, &mut c.background);
        set_role(&colors.tab_bar, &mut c.tab_bar);
        set_role(&colors.tab_active, &mut c.tab_active);
        set_role(&colors.sidebar, &mut c.sidebar);
        set_role(&colors.sidebar_accent, &mut c.sidebar_accent);
        set_role(&colors.title_bar, &mut c.title_bar);
        set_role(&colors.status_bar, &mut c.status_bar);
        // One hairline feeds both bar borders.
        set_role(&colors.chrome_border, &mut c.title_bar_border);
        set_role(&colors.chrome_border, &mut c.status_bar_border);
        // Rows and text.
        set_role(&colors.list_hover, &mut c.list_hover);
        set_role(&colors.list_active, &mut c.list_active);
        set_role(&colors.segmented, &mut c.tab_bar_segmented);
        set_role(&colors.foreground, &mut c.foreground);
        set_role(&colors.muted, &mut c.muted_foreground);
        set_role(&colors.subtle, &mut c.secondary_foreground);
        set_role(&colors.subtle, &mut c.button_secondary_foreground);
        set_role(&colors.border, &mut c.border);
        set_role(&colors.input_border, &mut c.input);
        set_role(&colors.hover, &mut c.secondary_hover);
        set_role(&colors.hover, &mut c.button_secondary_hover);
        set_role(&colors.selected, &mut c.secondary_active);
        set_role(&colors.selected, &mut c.button_secondary_active);
        set_role(&colors.success, &mut c.success);
        set_role(&colors.warning, &mut c.warning);
        set_role(&colors.info, &mut c.info);
        set_role(&colors.danger, &mut c.danger);
        // Primary fill. A `.primary()` control paints the `button_primary`
        // token, which is built from its own color, so setting only `primary`
        // would leave the button on the kit's blue.
        set_role(&colors.primary, &mut c.primary);
        set_role(&colors.primary, &mut c.button_primary);
        set_role(&colors.primary_foreground, &mut c.primary_foreground);
        set_role(&colors.primary_foreground, &mut c.button_primary_foreground);
        set_role(&colors.primary_hover, &mut c.primary_hover);
        set_role(&colors.primary_hover, &mut c.button_primary_hover);
        set_role(&colors.primary_active, &mut c.primary_active);
        set_role(&colors.primary_active, &mut c.button_primary_active);
        set_role(&colors.secondary, &mut c.secondary);
        set_role(&colors.secondary, &mut c.button_secondary);
        // Menus, overlays, focus, scroll, tabs.
        set_role(&colors.accent, &mut c.accent);
        set_role(&colors.accent_foreground, &mut c.accent_foreground);
        set_role(&colors.popover, &mut c.popover);
        set_role(&colors.popover_foreground, &mut c.popover_foreground);
        set_role(&colors.caret, &mut c.caret);
        set_role(&colors.selection, &mut c.selection);
        set_role(&colors.scrollbar, &mut c.scrollbar);
        set_role(&colors.scrollbar_thumb, &mut c.scrollbar_thumb);
        set_role(&colors.scrollbar_thumb_hover, &mut c.scrollbar_thumb_hover);
        set_role(&colors.tab_foreground, &mut c.tab_foreground);
        set_role(&colors.tab_active_foreground, &mut c.tab_active_foreground);
        set_role(&colors.overlay, &mut c.overlay);
        set_role(&colors.ring, &mut c.ring);
    });
}

/// Apply one optional theme-file role to a component-theme color slot.
fn set_role(role: &Option<String>, target: &mut Hsla) {
    if let Some(color) = role
        .as_deref()
        .and_then(|value| try_parse_color(value).ok())
    {
        *target = color;
    }
}

/// The colors a pane draws from. Every field is a role, not a token name.
///
/// A file may set any subset; the rest come from the live component theme. See
/// the module docs for the load order.
#[derive(Clone)]
pub(crate) struct Palette {
    /// The window surface, and the body of every pane that is not a list.
    pub background: Hsla,
    /// The strip the request tabs sit in, and the header of every pane.
    pub tab_bar: Hsla,
    /// The background of the selected tab.
    pub tab_active: Hsla,
    /// The sidebar tree's surface, a shade away from the editor.
    pub sidebar: Hsla,
    /// The custom titlebar. Darker than the tab strip so the window edge reads
    /// as a frame, the way every desktop API client draws it.
    pub title_bar: Hsla,
    /// The hairline under the titlebar and above the status bar.
    pub chrome_border: Hsla,
    /// The window-wide status line at the bottom.
    pub status_bar: Hsla,
    /// A list row under the pointer.
    pub list_hover: Hsla,
    /// The list row that is selected.
    pub list_active: Hsla,
    /// The rail behind the sidebar's selected collection, used for the
    /// collapsed-collection accent.
    pub sidebar_accent: Hsla,
    /// The segmented control's track, one step off `tab_bar`.
    pub segmented: Hsla,
    pub foreground: Hsla,
    /// Secondary text: values, counts, timestamps.
    pub muted: Hsla,
    /// The quietest text: hints, empty states, separators.
    pub subtle: Hsla,
    /// Hairlines between rows, headers, and panes.
    pub border: Hsla,
    /// The outline of an input or a bordered control.
    pub input_border: Hsla,
    /// A row under the pointer.
    pub hover: Hsla,
    /// The row the user picked.
    pub selected: Hsla,
    /// The wash behind selected text. Separate from `selected`: a row stays
    /// highlighted while it is focused, while a text selection is a transient
    /// gesture and reads better as a translucent wash.
    pub selection: Hsla,
    /// The mark behind a find-in-body match (U56). A translucent `warning`, so
    /// a found substring reads as the same class of "look here" as every other
    /// warning without adding a token a theme file has to define.
    pub search_match: Hsla,
    /// The stronger wash behind the match the arrows are currently on, so
    /// stepping through a list moves something visible rather than only the
    /// count beside the field.
    pub search_match_current: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub info: Hsla,
    pub danger: Hsla,
    /// The wash behind a failed request's strip. A translucent `danger`, named
    /// so the error band is one role instead of an opacity literal.
    pub danger_wash: Hsla,
    /// The hairline closing that strip.
    pub danger_border: Hsla,
    pub mono_font: SharedString,
}

/// Spacing and type, in logical pixels. Panes read these instead of writing
/// `px(…)` literals, which is what lets zoom scale the whole window from one
/// place (see [`Metrics::scaled`]).
#[derive(Clone, Copy)]
pub(crate) struct Metrics {
    /// The default text size for a pane's body.
    pub font_body: f32,
    /// Monospace payload text: the response body, headers, cookies.
    pub font_small: f32,
    /// The quietest text: hints, counts, crumbs.
    pub font_xs: f32,
    /// Method badges and counted tab chips, where space is tightest.
    pub font_badge: f32,
    /// The spacing scale, in logical pixels: 2 / 4 / 6 / 8 / 12 / 16 / 24 / 32.
    /// Read in relationships rather than copied as numbers: `xs` for the seam
    /// between two related lines, `sm` for a row's breathing room, `md` for a
    /// pane's edge, and upward for a break between sections.
    pub space_2xs: f32,
    pub space_xs: f32,
    /// The half-step between `xs` and `sm`, where a 4 px gap is too tight and
    /// an 8 px one would break a column's rhythm.
    pub space_1p5: f32,
    pub space_sm: f32,
    pub space_md: f32,
    pub space_lg: f32,
    pub space_xl: f32,
    pub space_2xl: f32,
    /// One sidebar / list / collection-tree row.
    pub row_height: f32,
    /// One response-body line, uniform so the virtualized list places rows
    /// without measuring each.
    pub body_row_height: f32,
    /// The window-wide status line.
    pub status_height: f32,
    /// A pane's own header bar (`Body | Headers … · 200 OK`).
    pub header_height: f32,
    /// The room an edge of a pane or chip leaves before its content.
    pub gutter: f32,
    /// The horizontal room a chip or an icon button carries.
    pub chip_padding: f32,
    /// One level of a tree's indent.
    pub indent: f32,
    /// A small icon in a tree row or a chip.
    pub icon_sm: f32,
    /// The lane the per-row tools of the collections tree occupy.
    pub tool_lane: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            font_body: 13.0,
            font_small: 12.0,
            font_xs: 11.0,
            font_badge: 10.5,
            space_2xs: 2.0,
            space_xs: 4.0,
            space_1p5: 6.0,
            space_sm: 8.0,
            space_md: 12.0,
            space_lg: 16.0,
            space_xl: 24.0,
            space_2xl: 32.0,
            row_height: 26.0,
            body_row_height: 18.0,
            status_height: 24.0,
            header_height: 36.0,
            gutter: 12.0,
            chip_padding: 10.0,
            indent: 12.0,
            icon_sm: 15.0,
            tool_lane: 22.0,
        }
    }
}

impl Metrics {
    /// The metrics in effect: the theme's base block scaled by the current
    /// zoom. Panes read this once per construction and re-read it on a theme or
    /// zoom change through [`crate::theme::Palette::new`]'s sibling call in
    /// their `refresh_theme`.
    pub(crate) fn current(cx: &App) -> Self {
        let registry = ThemeRegistry::get(cx);
        registry.base_metrics.scaled(registry.zoom)
    }

    /// Every metric multiplied by `factor`, the zoom level. A single multiplier
    /// over the token block is what makes zoom one value instead of an edit to
    /// every `px(…)` in the app.
    pub(crate) fn scaled(&self, factor: f32) -> Self {
        Self {
            font_body: self.font_body * factor,
            font_small: self.font_small * factor,
            font_xs: self.font_xs * factor,
            font_badge: self.font_badge * factor,
            space_2xs: self.space_2xs * factor,
            space_xs: self.space_xs * factor,
            space_1p5: self.space_1p5 * factor,
            space_sm: self.space_sm * factor,
            space_md: self.space_md * factor,
            space_lg: self.space_lg * factor,
            space_xl: self.space_xl * factor,
            space_2xl: self.space_2xl * factor,
            row_height: self.row_height * factor,
            body_row_height: self.body_row_height * factor,
            status_height: self.status_height * factor,
            header_height: self.header_height * factor,
            gutter: self.gutter * factor,
            chip_padding: self.chip_padding * factor,
            indent: self.indent * factor,
            icon_sm: self.icon_sm * factor,
            tool_lane: self.tool_lane * factor,
        }
    }
}

/// A theme file: the colors, the metrics, and the two facts a switcher needs —
/// what to call it and whether it is the dark or the light half of the pair.
///
/// Every field is optional so a file can be a partial overlay. Unknown keys are
/// ignored, so a file from a newer build still loads.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ThemeFile {
    pub name: String,
    /// `"dark"` or `"light"`. Anything else (or omitted) leaves the appearance
    /// to the app-level setting.
    pub appearance: Option<String>,
    /// A font family string for payloads. Omitted keeps the component theme's
    /// monospace family.
    pub mono_font: Option<String>,
    pub colors: ColorRoles,
    pub metrics: Option<MetricTokens>,
}

/// The color half of a theme file. Strings, not `Hsla`, so the file is
/// hand-editable and a bad color drops that one role instead of the file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ColorRoles {
    pub background: Option<String>,
    pub tab_bar: Option<String>,
    pub tab_active: Option<String>,
    pub sidebar: Option<String>,
    pub title_bar: Option<String>,
    pub chrome_border: Option<String>,
    pub status_bar: Option<String>,
    pub list_hover: Option<String>,
    pub list_active: Option<String>,
    pub sidebar_accent: Option<String>,
    pub segmented: Option<String>,
    pub foreground: Option<String>,
    pub muted: Option<String>,
    pub subtle: Option<String>,
    pub border: Option<String>,
    pub input_border: Option<String>,
    pub hover: Option<String>,
    pub selected: Option<String>,
    pub success: Option<String>,
    pub warning: Option<String>,
    pub info: Option<String>,
    pub danger: Option<String>,
    pub danger_wash: Option<String>,
    pub danger_border: Option<String>,
    // Component roles: surfaces and accents the kit components paint with but
    // a pane never names. See `sync_component_theme` for where they land.
    pub primary: Option<String>,
    pub primary_foreground: Option<String>,
    pub primary_hover: Option<String>,
    pub primary_active: Option<String>,
    pub secondary: Option<String>,
    pub accent: Option<String>,
    pub accent_foreground: Option<String>,
    pub popover: Option<String>,
    pub popover_foreground: Option<String>,
    pub caret: Option<String>,
    pub selection: Option<String>,
    pub scrollbar: Option<String>,
    pub scrollbar_thumb: Option<String>,
    pub scrollbar_thumb_hover: Option<String>,
    pub tab_foreground: Option<String>,
    pub tab_active_foreground: Option<String>,
    pub overlay: Option<String>,
    pub ring: Option<String>,
}

/// The metric half of a theme file. All numbers, all optional, so a file that
/// only wants a bigger body font sets `font_body` and nothing else.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct MetricTokens {
    pub font_body: Option<f32>,
    pub font_small: Option<f32>,
    pub font_xs: Option<f32>,
    pub font_badge: Option<f32>,
    pub space_2xs: Option<f32>,
    pub space_xs: Option<f32>,
    pub space_1p5: Option<f32>,
    pub space_sm: Option<f32>,
    pub space_md: Option<f32>,
    pub space_lg: Option<f32>,
    pub space_xl: Option<f32>,
    pub space_2xl: Option<f32>,
    pub row_height: Option<f32>,
    pub body_row_height: Option<f32>,
    pub status_height: Option<f32>,
    pub header_height: Option<f32>,
    pub gutter: Option<f32>,
    pub chip_padding: Option<f32>,
    pub indent: Option<f32>,
    pub icon_sm: Option<f32>,
    pub tool_lane: Option<f32>,
}

/// The theme file's colors when they apply to the mode being painted. A file
/// that names an appearance only overlays its colors in that mode; a file with
/// no (or an unknown) appearance overlays in both, which is what makes a user
/// theme a patch rather than a full palette.
impl ThemeFile {
    fn colors_for(&self, dark: bool) -> Option<&ColorRoles> {
        match self.appearance.as_deref() {
            Some("dark") if !dark => None,
            Some("light") if dark => None,
            _ => Some(&self.colors),
        }
    }
}

impl ColorRoles {
    /// Whether a theme file named any color at all. A file with no explicit
    /// colors is a metrics-only overlay, which keeps the component theme.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_empty(&self) -> bool {
        self.background.is_none()
            && self.tab_bar.is_none()
            && self.tab_active.is_none()
            && self.sidebar.is_none()
            && self.title_bar.is_none()
            && self.chrome_border.is_none()
            && self.status_bar.is_none()
            && self.list_hover.is_none()
            && self.list_active.is_none()
            && self.sidebar_accent.is_none()
            && self.segmented.is_none()
            && self.foreground.is_none()
            && self.muted.is_none()
            && self.subtle.is_none()
            && self.border.is_none()
            && self.input_border.is_none()
            && self.hover.is_none()
            && self.selected.is_none()
            && self.success.is_none()
            && self.warning.is_none()
            && self.info.is_none()
            && self.danger.is_none()
            && self.primary.is_none()
            && self.primary_foreground.is_none()
            && self.primary_hover.is_none()
            && self.primary_active.is_none()
            && self.secondary.is_none()
            && self.accent.is_none()
            && self.accent_foreground.is_none()
            && self.popover.is_none()
            && self.popover_foreground.is_none()
            && self.caret.is_none()
            && self.selection.is_none()
            && self.scrollbar.is_none()
            && self.scrollbar_thumb.is_none()
            && self.scrollbar_thumb_hover.is_none()
            && self.tab_foreground.is_none()
            && self.tab_active_foreground.is_none()
            && self.overlay.is_none()
            && self.ring.is_none()
    }
}

/// Parse a theme file's text. JSONC — JSON with `//` and `/* */` comments — so
/// the built-in file can explain each block the way this codebase's Rust does.
///
/// A file that does not parse yields the defaults rather than an error: the
/// built-in theme is always valid, and the user only loses their overrides.
pub(crate) fn parse(text: &str) -> ThemeFile {
    serde_json::from_str(&strip_comments(text)).unwrap_or_default()
}

/// Remove `//` and `/* */` comments that are not inside a string. A theme is
/// small and hand-written, so a full JSONC parser is not worth a dependency;
/// this is enough for the files this app ships and a user writes.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                // Line comment: drop to the end of the line, keep the newline.
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

impl Palette {
    /// Build the palette from the active theme file, overlaid on the live
    /// component theme. A role the file omits keeps the theme's value, which is
    /// what the app showed before themes became a file.
    pub(crate) fn new(cx: &App) -> Self {
        let registry = ThemeRegistry::get(cx);
        let file = &registry.file;
        let theme = cx.theme();

        let no_colors = ColorRoles::default();
        let colors = file.colors_for(theme.is_dark()).unwrap_or(&no_colors);
        let pick = |role: &Option<String>, fallback: Hsla| {
            role.as_deref()
                .and_then(|value| try_parse_color(value).ok())
                .unwrap_or(fallback)
        };

        Self {
            background: pick(&colors.background, theme.background),
            tab_bar: pick(&colors.tab_bar, theme.tab_bar),
            tab_active: pick(&colors.tab_active, theme.tab_active),
            sidebar: pick(&colors.sidebar, theme.sidebar),
            title_bar: pick(&colors.title_bar, theme.title_bar),
            chrome_border: pick(&colors.chrome_border, theme.title_bar_border),
            status_bar: pick(&colors.status_bar, theme.status_bar),
            list_hover: pick(&colors.list_hover, theme.list_hover),
            list_active: pick(&colors.list_active, theme.list_active),
            sidebar_accent: pick(&colors.sidebar_accent, theme.sidebar_accent),
            segmented: pick(&colors.segmented, theme.tab_bar_segmented),
            foreground: pick(&colors.foreground, theme.foreground),
            muted: pick(&colors.muted, theme.muted_foreground),
            subtle: pick(&colors.subtle, theme.secondary_foreground),
            border: pick(&colors.border, theme.border),
            input_border: pick(&colors.input_border, theme.input),
            hover: pick(&colors.hover, theme.secondary_hover),
            selected: pick(&colors.selected, theme.secondary_active),
            selection: theme.selection,
            search_match: theme.warning.opacity(0.32),
            search_match_current: theme.warning.opacity(0.62),
            success: pick(&colors.success, theme.success),
            warning: pick(&colors.warning, theme.warning),
            info: pick(&colors.info, theme.info),
            danger: pick(&colors.danger, theme.danger),
            danger_wash: pick(&colors.danger_wash, theme.danger.opacity(0.12)),
            danger_border: pick(&colors.danger_border, theme.danger.opacity(0.4)),
            mono_font: file
                .mono_font
                .clone()
                .map(SharedString::from)
                .unwrap_or_else(|| theme.mono_font_family.clone()),
        }
    }

    /// Green for 2xx, red for 4xx and up, muted for everything else.
    pub(crate) fn status(&self, status: u16) -> Hsla {
        if (200..300).contains(&status) {
            self.success
        } else if status >= 400 {
            self.danger
        } else {
            self.muted
        }
    }

    /// The color a method is known by. Every rest client colors the verb so
    /// the one line of a request reads at a glance: green reads, red deletes.
    pub(crate) fn method(&self, method: Method) -> Hsla {
        match method {
            Method::Get => self.success,
            Method::Post => self.warning,
            Method::Put | Method::Patch => self.info,
            Method::Delete => self.danger,
            Method::Head | Method::Options => self.muted,
        }
    }
}

impl MetricTokens {
    /// Fold a file's tokens over the built-in defaults. A token the file omits
    /// keeps its default, which is what makes a file a partial overlay here
    /// just as it is for colors.
    pub(crate) fn over(self, base: Metrics) -> Metrics {
        Metrics {
            font_body: self.font_body.unwrap_or(base.font_body),
            font_small: self.font_small.unwrap_or(base.font_small),
            font_xs: self.font_xs.unwrap_or(base.font_xs),
            font_badge: self.font_badge.unwrap_or(base.font_badge),
            space_2xs: self.space_2xs.unwrap_or(base.space_2xs),
            space_xs: self.space_xs.unwrap_or(base.space_xs),
            space_1p5: self.space_1p5.unwrap_or(base.space_1p5),
            space_sm: self.space_sm.unwrap_or(base.space_sm),
            space_md: self.space_md.unwrap_or(base.space_md),
            space_lg: self.space_lg.unwrap_or(base.space_lg),
            space_xl: self.space_xl.unwrap_or(base.space_xl),
            space_2xl: self.space_2xl.unwrap_or(base.space_2xl),
            row_height: self.row_height.unwrap_or(base.row_height),
            body_row_height: self.body_row_height.unwrap_or(base.body_row_height),
            status_height: self.status_height.unwrap_or(base.status_height),
            header_height: self.header_height.unwrap_or(base.header_height),
            gutter: self.gutter.unwrap_or(base.gutter),
            chip_padding: self.chip_padding.unwrap_or(base.chip_padding),
            indent: self.indent.unwrap_or(base.indent),
            icon_sm: self.icon_sm.unwrap_or(base.icon_sm),
            tool_lane: self.tool_lane.unwrap_or(base.tool_lane),
        }
    }
}

#[cfg(test)]
mod tests {
    // Named imports: this crate glob-imports `gpui_kit::*` elsewhere, and that
    // macro set shadows `#[test]`.
    use super::{parse, strip_comments, Metrics};

    #[gpui_kit::test]
    fn the_theme_file_colors_reach_the_component_theme(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};

        cx.update(gpui_kit::init);
        cx.update(|cx| {
            super::ThemeRegistry::install(cx, super::DEFAULT_THEME, 1.0, super::Appearance::Dark);
            Theme::change(ThemeMode::Dark, None, cx);
            super::sync_component_theme(cx);

            let theme = cx.theme();
            // Panes and controls share the file's charcoal, not the kit default.
            assert_eq!(theme.background, cb("#1b1d21"));
            assert_eq!(theme.foreground, cb("#e6e8ec"));
            // The roles a control needs but a pane never names.
            assert_eq!(theme.primary, cb("#6cb6e8"));
            assert_eq!(theme.button_primary, cb("#6cb6e8"));
            assert_eq!(theme.popover, cb("#202329"));
            assert_eq!(theme.caret, cb("#6cb6e8"));
            assert_eq!(theme.tab_foreground, cb("#9aa0aa"));
            assert_eq!(theme.scrollbar_thumb, cb("#3a3f47"));
        });
    }

    fn cb(hex: &str) -> gpui_kit::Hsla {
        gpui_kit::component::try_parse_color(hex).unwrap()
    }

    #[test]
    fn the_built_in_theme_parses_and_names_the_appearance() {
        let file = parse(super::DEFAULT_THEME);
        assert_eq!(file.name, "Default Dark");
        assert_eq!(file.appearance.as_deref(), Some("dark"));
        // The built-in theme names a soft charcoal instead of inheriting the
        // component theme's near-black. Its declared appearance keeps that
        // palette out of light mode.
        assert!(!file.colors.is_empty(), "the built-in theme sets colors");
        assert_eq!(file.colors.background.as_deref(), Some("#1b1d21"));
        assert_eq!(file.metrics.unwrap().font_body, Some(13.0));
    }

    #[test]
    fn colors_only_apply_to_the_appearance_the_file_names() {
        let dark = parse(r##"{ "appearance": "dark", "colors": { "background": "#111111" } }"##);
        assert!(dark.colors_for(true).is_some());
        assert!(dark.colors_for(false).is_none());

        let light = parse(r##"{ "appearance": "light", "colors": { "background": "#eeeeee" } }"##);
        assert!(light.colors_for(false).is_some());
        assert!(light.colors_for(true).is_none());

        // No appearance (or one this build does not know) overlays in both.
        let both = parse(r##"{ "colors": { "background": "#123456" } }"##);
        assert!(both.colors_for(true).is_some());
        assert!(both.colors_for(false).is_some());
    }

    #[test]
    fn comments_are_stripped_outside_strings_but_kept_inside_them() {
        let text = "{\n // a comment\n \"url\": \"http://x\", /* inline */ \"a\": 1\n}";
        let stripped = strip_comments(text);
        assert!(!stripped.contains("a comment"));
        assert!(!stripped.contains("inline"));
        // The `//` inside the string value must survive.
        assert!(stripped.contains("http://x"));
    }

    #[test]
    fn a_file_only_overrides_the_tokens_it_sets() {
        let base = Metrics::default();
        let tokens = parse(r#"{ "metrics": { "font_body": 16.0 } }"#)
            .metrics
            .unwrap();
        let over = tokens.over(base);
        assert_eq!(over.font_body, 16.0);
        assert_eq!(over.row_height, base.row_height);
    }

    #[test]
    fn a_file_with_no_metrics_keeps_every_default() {
        let base = Metrics::default();
        let over = super::MetricTokens::default().over(base);
        assert_eq!(over.font_body, base.font_body);
        assert_eq!(over.status_height, base.status_height);
    }

    #[test]
    fn a_malformed_file_yields_the_defaults_rather_than_failing() {
        let file = parse("{ this is not json");
        assert!(file.name.is_empty());
        assert!(file.colors.is_empty());
        assert!(file.metrics.is_none());
    }

    #[test]
    fn zoom_scales_every_metric_by_the_same_factor() {
        let base = Metrics::default();
        let doubled = base.scaled(2.0);
        assert_eq!(doubled.font_body, base.font_body * 2.0);
        assert_eq!(doubled.row_height, base.row_height * 2.0);
        assert_eq!(doubled.status_height, base.status_height * 2.0);
        assert_eq!(doubled.space_md, base.space_md * 2.0);
    }

    #[test]
    fn the_spacing_scale_is_the_expected_ramp() {
        let metrics = Metrics::default();
        assert_eq!(metrics.space_2xs, 2.0);
        assert_eq!(metrics.space_xs, 4.0);
        assert_eq!(metrics.space_1p5, 6.0);
        assert_eq!(metrics.space_sm, 8.0);
        assert_eq!(metrics.space_md, 12.0);
        assert_eq!(metrics.space_lg, 16.0);
        assert_eq!(metrics.space_xl, 24.0);
        assert_eq!(metrics.space_2xl, 32.0);
    }

    #[test]
    fn a_pinned_appearance_ignores_the_os_and_system_follows_it() {
        use super::Appearance;
        use gpui_kit::component::ThemeMode;
        use gpui_kit::WindowAppearance;

        // Pinned modes never consult the OS.
        assert_eq!(
            Appearance::Light.mode(Some(WindowAppearance::Dark)),
            ThemeMode::Light
        );
        assert_eq!(
            Appearance::Dark.mode(Some(WindowAppearance::Light)),
            ThemeMode::Dark
        );
        // System reads the OS appearance the platform reports.
        assert_eq!(
            Appearance::System.mode(Some(WindowAppearance::Dark)),
            ThemeMode::Dark
        );
        assert_eq!(
            Appearance::System.mode(Some(WindowAppearance::Light)),
            ThemeMode::Light
        );
        // No window yet (startup) resolves to light rather than guessing dark.
        assert_eq!(Appearance::System.mode(None), ThemeMode::Light);
    }

    #[test]
    fn only_system_releases_the_window_back_to_the_os() {
        use super::Appearance;
        use gpui_kit::WindowAppearance;

        assert_eq!(Appearance::System.window_appearance(), None);
        assert_eq!(
            Appearance::Light.window_appearance(),
            Some(WindowAppearance::Light)
        );
        assert_eq!(
            Appearance::Dark.window_appearance(),
            Some(WindowAppearance::Dark)
        );
    }

    #[test]
    fn zoom_is_clamped_to_the_readable_range() {
        // A hand-edited `settings.json` must not make the window unreadable.
        assert_eq!(super::clamp_zoom(0.1), super::ZOOM_MIN);
        assert_eq!(super::clamp_zoom(99.0), super::ZOOM_MAX);
        assert_eq!(super::clamp_zoom(f32::NAN), 1.0);
    }

    /// The arithmetic the three zoom actions run. Kept pure rather than driving
    /// `set_zoom`, because the theme is a process global and the test harness
    /// runs cases in parallel: a window that changed it would leak into a
    /// neighbour. `set_zoom` applies exactly this clamp.
    #[test]
    fn a_zoom_step_is_reversible_and_stops_at_the_limits() {
        let up = super::clamp_zoom(1.0 * super::ZOOM_STEP);
        assert!(up > 1.0);

        // A step out undoes a step in, which is why `ZOOM_STEP` is a ratio and
        // not an increment.
        let back = super::clamp_zoom(up / super::ZOOM_STEP);
        assert!((back - 1.0).abs() < 1e-5);

        // Holding the shortcut stops at the limit instead of going unreadable.
        let mut level = 1.0f32;
        for _ in 0..100 {
            level = super::clamp_zoom(level * super::ZOOM_STEP);
        }
        assert_eq!(level, super::ZOOM_MAX);

        let mut level = 1.0f32;
        for _ in 0..100 {
            level = super::clamp_zoom(level / super::ZOOM_STEP);
        }
        assert_eq!(level, super::ZOOM_MIN);
    }
}
