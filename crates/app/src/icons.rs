//! The app's icon vocabulary: which glyph each surface uses, and at which size.
//! Glyphs are `gpui_kit::assets::IconName`, the full Lucide catalog, so
//! `main.rs` installs `AllAssets`; a name outside the default bundle would
//! paint nothing. An [`Icon`] without `with_size` already matches its text,
//! which zoom scales; the constants cover the deliberately smaller cases — a
//! tab close button, a search prefix, a status-bar chip.

use gpui_kit::component::{h_flex, Icon, Sizable as _};
use gpui_kit::*;

pub(crate) use gpui_kit::assets::IconName;

use crate::theme::{Metrics, Palette};

/// A glyph smaller than its line: the tab close button, the search prefix, the
/// status-bar chips.
pub(crate) const SMALL: f32 = 12.;

/// A pane title, one step up from the header's own text.
pub(crate) const MEDIUM: f32 = 14.;

/// An icon at `size`, inheriting its color from the row it sits in.
pub(crate) fn sized(name: IconName, size: f32) -> Icon {
    Icon::new(name).with_size(px(size))
}

/// The quiet tone: a glyph that labels its row rather than competing with it.
pub(crate) fn quiet(name: IconName, size: f32, palette: &Palette) -> Icon {
    sized(name, size).text_color(palette.subtle)
}

/// A pane title: one glyph and one label, the same shape in every pane.
pub(crate) fn title(name: IconName, label: &'static str, metrics: &Metrics) -> impl IntoElement {
    h_flex()
        .gap(px(metrics.space_xs))
        .items_center()
        .child(sized(name, MEDIUM))
        .child(label)
}
