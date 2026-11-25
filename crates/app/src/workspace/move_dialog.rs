//! The move dialog: a saved request and a destination.
//!
//! It reuses the save dialog's picker, since a request's home is one `Destination`
//! and the two lists must not disagree. The node keeps its id, so a tab bound to
//! it follows the move unknowingly. Only a saved request moves; a folder would
//! take its children, which is a different feature.

use std::rc::Rc;

use gpui_kit::component::button::*;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::{v_flex, Disableable as _, WindowExt as _};
use gpui_kit::*;

use crate::panels::collections::Destination;
use crate::theme::{Metrics, Palette};

use super::destination_picker::{
    destination_picker, open_destination_select, selected_choice, DestinationChoice,
};
use super::Workspace;

impl Workspace {
    /// Ask where a saved request should live, then move it. Called from render
    /// once `pending_move` is set, because the row menu has no window.
    pub(crate) fn open_move_dialog(
        &mut self,
        node: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Where it is now doubles as the starting point and as the one entry that
        // would be a no-op, so it is read once here rather than guessed.
        let original = self.panes.collections.read(cx).destination_of(&node);
        self.move_node = Some(node);
        let rows = self.panes.collections.read(cx).destinations();
        // A move needs a place that already exists, so the picker has no "create"
        // row; with no saved home the selection stays empty and Move stays off.
        let initial = original
            .clone()
            .map_or(DestinationChoice::New, DestinationChoice::Existing);
        let select = open_destination_select(&rows, None, initial, window, cx);

        let workspace = cx.entity();
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);

        type Confirm = Rc<dyn Fn(&mut Window, &mut App) -> bool>;
        let confirm_workspace = workspace.clone();
        let confirm_select = select.clone();
        let confirm: Confirm = Rc::new(move |_, cx| {
            let target =
                selected_choice(&confirm_select, cx).and_then(|choice| choice.existing().cloned());
            confirm_workspace.update(cx, |workspace, cx| workspace.commit_move(target, cx))
        });
        let for_button = confirm.clone();
        let for_ok = confirm.clone();

        window.open_dialog(cx, move |dialog, _window, cx| {
            // Reading the picker observes its entity, so a pick re-renders this
            // dialog and Move enables the moment the choice would change the place.
            let unchanged = selected_choice(&select, cx)
                .as_ref()
                .and_then(|choice| choice.existing())
                == original.as_ref();

            let into = v_flex()
                .gap(px(metrics.space_xs))
                .child(div().text_xs().text_color(palette.muted).child("To"))
                .child(destination_picker(&select, "Move to"));

            let cancel = Button::new("move-request-cancel")
                .label("Cancel")
                .ghost()
                .on_click(|_, window, cx| window.close_dialog(cx));
            let save = Button::new("move-request-confirm")
                .label("Move")
                .primary()
                .disabled(unchanged)
                .on_click({
                    let confirm = for_button.clone();
                    move |_, window, cx| {
                        if confirm(window, cx) {
                            window.close_dialog(cx);
                        }
                    }
                });

            dialog
                .title("Move request")
                .w_112()
                .child(
                    v_flex()
                        .gap(px(metrics.space_md))
                        .py(px(metrics.space_sm))
                        .child(into),
                )
                .footer(DialogFooter::new().child(cancel).child(save))
                .on_ok({
                    let confirm = for_ok.clone();
                    move |_, window, cx| confirm(window, cx)
                })
        });
    }

    /// Move the request the dialog is holding to the chosen destination.
    /// `false` keeps the dialog open: either the choice is a no-op or the write
    /// failed, and the pane has already said which.
    pub(super) fn commit_move(
        &mut self,
        target: Option<Destination>,
        cx: &mut Context<Self>,
    ) -> bool {
        let (Some(node), Some(target)) = (self.move_node.clone(), target) else {
            return false;
        };
        self.panes.collections.update(cx, |panel, cx| {
            panel.move_request(&node, &target, cx).is_some()
        })
    }
}
