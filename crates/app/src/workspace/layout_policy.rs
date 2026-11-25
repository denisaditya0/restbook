//! The window's real minimum, and which docks fit at a given width.
//!
//! The design guide asks for a documented minimum window size and for a narrow
//! window to collapse a secondary region rather than squeeze a primary one.
//! Every number here is a component's usable width, not a guess: a request or
//! response pane already responds below [`CENTRE_MIN`] (see `NARROW_URL_ROW`),
//! and each dock holds its own controls at [`LEFT_MIN`] / [`RIGHT_MIN`]. The
//! window floor is the centre alone — the docks drop out before a pane is ever
//! narrowed past what its header and controls need.

use super::CenterOrientation;

/// A request or response pane keeps its header, URL row, and Send button at
/// this width. The same number is `panels::request::NARROW_URL_ROW`.
pub(crate) const CENTRE_MIN: f32 = 340.;

/// The Collections dock's usable width. `HISTORY_WIDTH` is the narrower width
/// it opens at; this is the floor the window arithmetic treats as needed.
pub(crate) const LEFT_MIN: f32 = 400.;

/// The tools dock's usable width: an environment row's key, value, and controls.
pub(crate) const RIGHT_MIN: f32 = 400.;

/// Below this window width the status bar's chips drop their labels and keep
/// only the icon (with the label in the tooltip). The bar carries the URL on
/// the right and the storage/history chips on the left; under this width the
/// two compete, and the guide's rule is to collapse a secondary label before
/// clipping anything.
pub(crate) const STATUS_COMPACT_WIDTH: f32 = 900.;

/// The centre's width for `orientation`: one pane stacked, two side by side.
pub(crate) const fn centre_width(orientation: CenterOrientation) -> f32 {
    match orientation {
        CenterOrientation::Stacked => CENTRE_MIN,
        CenterOrientation::SideBySide => CENTRE_MIN * 2.0,
    }
}

/// The narrowest window that keeps `orientation`'s centre usable with both
/// docks hidden. Because the docks collapse before the centre is squeezed,
/// this — not the three-region sum — is the window's floor.
pub(crate) const fn window_min_width(orientation: CenterOrientation) -> f32 {
    centre_width(orientation)
}

/// Which docks are wanted, or visible. The same shape names the user's intent
/// and the fit derived from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Docks {
    pub(crate) left: bool,
    pub(crate) right: bool,
}

impl Docks {
    /// First run: the sidebar alone; the tools dock opens on demand.
    pub(crate) const FIRST_RUN: Docks = Docks {
        left: true,
        right: false,
    };
}

/// The docks to show at `viewport_width`, in window pixels. Each requested
/// dock shows only while the centre keeps its floor; when both are requested
/// but only one fits, the tools dock yields first so the sidebar — the route
/// to saved requests — survives a little longer.
pub(crate) fn visible_docks(
    viewport_width: f32,
    orientation: CenterOrientation,
    zoom: f32,
    intent: Docks,
) -> Docks {
    let centre = centre_width(orientation) * zoom;
    let left = LEFT_MIN * zoom;
    let right = RIGHT_MIN * zoom;
    let left_fits = intent.left && viewport_width >= centre + left;
    let mut right_fits = intent.right && viewport_width >= centre + right;
    if left_fits && right_fits && viewport_width < centre + left + right {
        right_fits = false;
    }
    Docks {
        left: left_fits,
        right: right_fits,
    }
}

/// The window width that holds `orientation`'s centre with the given docks
/// shown, in window pixels.
pub(crate) fn needed_width(orientation: CenterOrientation, docks: Docks, zoom: f32) -> f32 {
    (centre_width(orientation)
        + if docks.left { LEFT_MIN } else { 0. }
        + if docks.right { RIGHT_MIN } else { 0. })
        * zoom
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window floor is one usable pane stacked and two side by side; the
    /// docks are not part of it because they collapse before the centre shrinks.
    #[test]
    fn the_window_minimum_is_the_centre_pane() {
        assert_eq!(window_min_width(CenterOrientation::Stacked), 340.0);
        assert_eq!(window_min_width(CenterOrientation::SideBySide), 680.0);
    }

    /// Everything the user asked for shows at a wide window.
    #[test]
    fn a_wide_window_shows_every_requested_dock() {
        let intent = Docks {
            left: true,
            right: true,
        };
        let at = |width| visible_docks(width, CenterOrientation::Stacked, 1.0, intent);
        assert_eq!(
            at(1140.0),
            Docks {
                left: true,
                right: true
            }
        );
        assert_eq!(
            at(2000.0),
            Docks {
                left: true,
                right: true
            }
        );
    }

    /// Below the three-region sum the tools dock leaves first, and below the
    /// two-region sum the sidebar goes too, so a pane is never squeezed.
    #[test]
    fn the_tools_dock_yields_before_the_sidebar() {
        let intent = Docks {
            left: true,
            right: true,
        };
        let at = |width| visible_docks(width, CenterOrientation::Stacked, 1.0, intent);
        assert_eq!(
            at(1139.0),
            Docks {
                left: true,
                right: false
            }
        );
        assert_eq!(
            at(740.0),
            Docks {
                left: true,
                right: false
            }
        );
        assert_eq!(
            at(739.0),
            Docks {
                left: false,
                right: false
            }
        );
    }

    /// A dock the user closed stays closed however wide the window is.
    #[test]
    fn a_closed_dock_stays_closed() {
        let intent = Docks {
            left: false,
            right: false,
        };
        assert_eq!(
            visible_docks(4000.0, CenterOrientation::Stacked, 1.0, intent),
            Docks {
                left: false,
                right: false
            }
        );
    }

    /// A right-only request shows the tools dock as soon as it fits, without a
    /// hiding sidebar biasing the sum.
    #[test]
    fn the_tools_dock_shows_alone_when_it_is_the_only_request() {
        let intent = Docks {
            left: false,
            right: true,
        };
        assert_eq!(
            visible_docks(740.0, CenterOrientation::Stacked, 1.0, intent),
            Docks {
                left: false,
                right: true
            }
        );
    }

    /// The width needed to honour an intent is the centre plus every shown dock.
    #[test]
    fn the_needed_width_is_the_centre_plus_shown_docks() {
        let both = Docks {
            left: true,
            right: true,
        };
        assert_eq!(needed_width(CenterOrientation::Stacked, both, 1.0), 1140.0);
        assert_eq!(
            needed_width(CenterOrientation::SideBySide, both, 1.0),
            1480.0
        );
    }
}
