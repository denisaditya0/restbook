//! The first-save dialog: a name and a destination.
//!
//! A draft has neither, so the save asks for both. The destination is a picker
//! over every collection and its folders rather than the sidebar's highlight: a
//! save should not fail because nothing was clicked last, and the highlight is
//! navigation again.
//!
//! The typed name lives on the workspace (`save_name`, `save_new_collection_name`)
//! because the dialog's content closure is rebuilt on every notify, so an input
//! created inside it would lose the caret. The destination choice lives in the
//! picker's own entity, which this dialog creates once and captures.
//!
//! *New collection…* is a row of the picker rather than a separate flow: a first
//! run has no collection to save into, and making one elsewhere would cost the
//! typed request name and the dialog.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::button::*;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::Input;
use gpui_kit::component::{v_flex, WindowExt as _};
use gpui_kit::*;

use crate::theme::{Metrics, Palette};

use super::destination_picker::{
    destination_picker, open_destination_select, selected_choice, DestinationChoice,
};
use super::Workspace;

impl Workspace {
    /// Ask for a name and a destination, then save. Called from render once
    /// `pending_save` is set, because only render has the window a dialog needs.
    pub(crate) fn open_save_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Prefill with the name a save would have picked on its own, so Enter
        // keeps the old behavior and typing a name is the only new step.
        let request = self.panes.request.read(cx).current_request(cx);
        let default_name = format!("{} {}", request.method, request.url);
        self.save_name
            .update(cx, |state, cx| state.set_value(default_name, window, cx));
        self.save_new_collection_name
            .update(cx, |state, cx| state.set_value("", window, cx));

        // The selection is only the starting point: the picker can change it, and
        // an unselected sidebar still opens on the first collection. With no
        // collection at all, the dialog opens ready to make one.
        let rows = self.panes.collections.read(cx).destinations();
        let initial = self
            .panes
            .collections
            .read(cx)
            .default_destination()
            .map_or(DestinationChoice::New, DestinationChoice::Existing);
        let select = open_destination_select(&rows, Some("New collection…"), initial, window, cx);

        let workspace = cx.entity();
        let name_input = self.save_name.clone();
        let field = self.save_name.clone();
        let focus_input = self.save_name.clone();
        let new_name_input = self.save_new_collection_name.clone();
        let new_name_field = self.save_new_collection_name.clone();
        let first_build = Cell::new(true);
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);

        // One confirm for the footer button and for Enter: it reads the fields and
        // the picker at the moment of the click, not when the dialog was built.
        type Confirm = Rc<dyn Fn(&mut Window, &mut App) -> bool>;
        let confirm_workspace = workspace.clone();
        let confirm_select = select.clone();
        let confirm: Confirm = Rc::new(move |_, cx| {
            let name = name_input.read(cx).value().trim().to_owned();
            if name.is_empty() {
                // An unnamed request would be unfindable in the tree; keep the
                // dialog open rather than saving something the user has to fix.
                return false;
            }
            let Some(choice) = selected_choice(&confirm_select, cx) else {
                return false;
            };
            let collection_name = new_name_input.read(cx).value().trim().to_owned();
            if choice.is_new() && collection_name.is_empty() {
                return false;
            }
            confirm_workspace.update(cx, |workspace, cx| {
                let target = choice.existing().cloned();
                let create = choice.is_new().then_some(collection_name);
                workspace.commit_save(None, Some(&name), target, create.as_deref(), cx)
            })
        });
        let for_button = confirm.clone();
        let for_ok = confirm.clone();

        window.open_dialog(cx, move |dialog, window, cx| {
            // The dialog shell takes focus when it opens, so the name field has
            // to claim it back on the first build only — later builds are the
            // same dialog and must not steal the caret from the user.
            if first_build.replace(false) {
                focus_input.update(cx, |state, cx| {
                    state.focus_handle(cx).focus(window, cx);
                });
            }

            // Reading the picker observes its entity, so picking a row re-renders
            // this dialog and the collection field appears or disappears with it.
            let creating = selected_choice(&select, cx).is_some_and(|choice| choice.is_new());

            let into = v_flex()
                .gap(px(metrics.space_xs))
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.muted)
                        .child("Destination"),
                )
                .child(destination_picker(&select, "Destination"));

            let name_row = v_flex()
                .gap(px(metrics.space_xs))
                .child(div().text_xs().text_color(palette.muted).child("Name"))
                .child(Input::new(&field).id("save-request-name"));

            // Only while *New collection…* is the destination: the field is the
            // one thing that step adds, so it cannot be mistaken for a rename.
            let collection_row = creating.then(|| {
                v_flex()
                    .gap(px(metrics.space_xs))
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.muted)
                            .child("Collection"),
                    )
                    .child(Input::new(&new_name_field).id("save-request-collection"))
            });

            let cancel = Button::new("save-request-cancel")
                .label("Cancel")
                .ghost()
                .on_click(|_, window, cx| window.close_dialog(cx));
            let save = Button::new("save-request-confirm")
                .label("Save")
                .primary()
                .on_click({
                    let confirm = for_button.clone();
                    move |_, window, cx| {
                        if confirm(window, cx) {
                            window.close_dialog(cx);
                        }
                    }
                });

            dialog
                .title("Save request")
                // w_112 is 28rem (448px), the nearest named width to the old
                // 460px; the app's rem scale has no 28.75rem step.
                .w_112()
                .child(
                    v_flex()
                        .gap(px(metrics.space_md))
                        .py(px(metrics.space_sm))
                        .child(name_row)
                        .child(into)
                        .children(collection_row),
                )
                .footer(DialogFooter::new().child(cancel).child(save))
                .on_ok({
                    let confirm = for_ok.clone();
                    move |_, window, cx| confirm(window, cx)
                })
        });
    }
}
