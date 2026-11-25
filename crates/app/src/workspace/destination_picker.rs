//! The destination picker shared by the save, move, and import dialogs.
//!
//! Every dialog that asks for a home must list the same places: every collection
//! and folder, which for a large import can be thousands of rows, so this is a
//! searchable, virtualized [`Select`] laying out only the visible rows. The
//! selection lives in a [`SelectState`] entity created once per dialog open.

use gpui_kit::component::select::{
    SearchableVec, Select, SelectDelegate as _, SelectItem, SelectState,
};
use gpui_kit::component::{h_flex, Icon};
use gpui_kit::*;

use crate::icons::IconName;
use crate::theme::Palette;

use crate::panels::collections::{Destination, DestinationRow};

use super::Workspace;

/// What a picked row means: an existing place, or "create a new collection".
#[derive(Clone, PartialEq, Eq)]
pub(super) enum DestinationChoice {
    New,
    Existing(Destination),
}

impl DestinationChoice {
    /// The destination this choice names, or `None` when it means "create one".
    pub(super) fn existing(&self) -> Option<&Destination> {
        match self {
            Self::Existing(destination) => Some(destination),
            Self::New => None,
        }
    }

    pub(super) fn is_new(&self) -> bool {
        matches!(self, Self::New)
    }
}

/// One row of the picker. The label is the full `Collection › Folder` path,
/// because the list is flat and the separators carry the hierarchy.
#[derive(Clone)]
pub(super) struct DestinationItem {
    label: SharedString,
    choice: DestinationChoice,
}

impl SelectItem for DestinationItem {
    type Value = DestinationChoice;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.choice
    }

    fn render(&self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = Palette::new(cx);
        let icon = match self.choice {
            DestinationChoice::New => IconName::Plus,
            DestinationChoice::Existing(_) => IconName::Folder,
        };
        h_flex()
            .gap_2()
            .items_center()
            .child(Icon::new(icon).text_color(palette.muted))
            .child(self.label.clone())
    }
}

/// The entity a dialog renders and reads. A type alias keeps the generic off
/// every call site.
pub(super) type DestinationSelect = SelectState<SearchableVec<DestinationItem>>;

/// Build the picker's rows: `new_label` as the first row when the dialog can
/// create a collection, then every existing destination in tree order.
fn destination_items(rows: &[DestinationRow], new_label: Option<&str>) -> Vec<DestinationItem> {
    let mut items = Vec::with_capacity(rows.len() + usize::from(new_label.is_some()));
    if let Some(label) = new_label {
        items.push(DestinationItem {
            label: label.into(),
            choice: DestinationChoice::New,
        });
    }
    items.extend(rows.iter().map(|row| DestinationItem {
        label: row.label.clone().into(),
        choice: DestinationChoice::Existing(row.target.clone()),
    }));
    items
}

/// Create the picker for one dialog. Call once per open: the returned entity is
/// the one the content closure and the confirm handler must capture.
///
/// `new_label` is the "create a collection" row, or `None` for a dialog that can
/// only move an existing request into a place that already exists.
pub(super) fn open_destination_select(
    rows: &[DestinationRow],
    new_label: Option<&str>,
    initial: DestinationChoice,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Entity<DestinationSelect> {
    let items = SearchableVec::new(destination_items(rows, new_label));
    let selected = items.position(&initial);
    cx.new(|cx| SelectState::new(items, selected, window, cx).searchable(true))
}

/// The picker control itself. The caller places it under its own label; the
/// accessible name is passed here so it does not drift from the visible one.
pub(super) fn destination_picker(
    select: &Entity<DestinationSelect>,
    accessibility_label: &'static str,
) -> impl IntoElement {
    Select::new(select)
        .placeholder("Choose a collection")
        .accessibility_label(accessibility_label)
}

/// The committed choice, cloned out of the entity so the caller does not hold a
/// borrow across an update.
pub(super) fn selected_choice(
    select: &Entity<DestinationSelect>,
    cx: &App,
) -> Option<DestinationChoice> {
    select.read(cx).selected_value().cloned()
}

#[cfg(test)]
mod tests {
    use super::{destination_items, Destination, DestinationChoice, DestinationRow};

    fn row(label: &str, collection: &str) -> DestinationRow {
        DestinationRow {
            label: label.to_owned(),
            target: Destination {
                collection: collection.to_owned(),
                folders: Vec::new(),
            },
        }
    }

    /// The "create" row leads, then every destination in tree order, so the
    /// picker's first row is the default rather than the first collection.
    #[test]
    fn the_create_row_leads_then_destinations_follow_in_order() {
        let rows = [row("A", "a"), row("A › F", "a")];
        let items = destination_items(&rows, Some("New collection(s)"));

        assert_eq!(items.len(), 3);
        assert!(items[0].choice.is_new());
        assert_eq!(items[0].label.as_ref(), "New collection(s)");
        assert_eq!(items[1].label.as_ref(), "A");
        assert_eq!(items[2].label.as_ref(), "A › F");
        assert_eq!(items[1].choice.existing().unwrap().collection, "a");
    }

    /// A move has nowhere to create, so it asks for no sentinel and lists only
    /// places that already exist.
    #[test]
    fn a_move_picker_has_no_create_row() {
        let rows = [row("A", "a")];
        let items = destination_items(&rows, None);

        assert_eq!(items.len(), 1);
        assert!(matches!(items[0].choice, DestinationChoice::Existing(_)));
    }
}
