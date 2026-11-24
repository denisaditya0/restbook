//! The move dialog: a saved request and a destination.
//!
//! The same picker the save dialog uses, because a request's home is one
//! `Destination` and the two lists must not disagree. The node keeps its id, so
//! the tab bound to it follows the move without knowing one happened: what
//! changes is which collection file holds it.
//!
//! Only a saved request moves. A folder would take its children with it, which
//! is a different feature and not one this lane promises.

use std::rc::Rc;

use gpui_kit::component::button::*;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::menu::{DropdownMenu, PopupMenu};
use gpui_kit::component::{h_flex, v_flex, Disableable as _, WindowExt as _};
use gpui_kit::*;

use crate::theme::{Metrics, Palette};

use super::save_dialog::{with_destinations, DestinationPick};
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
        self.move_target = self.panes.collections.read(cx).destination_of(&node);
        self.move_node = Some(node);

        let workspace = cx.entity();
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);

        type Confirm = Rc<dyn Fn(&mut Window, &mut App) -> bool>;
        let confirm_workspace = workspace.clone();
        let confirm: Confirm = Rc::new(move |_, cx| {
            confirm_workspace.update(cx, |workspace, cx| workspace.commit_move(cx))
        });
        let for_button = confirm.clone();
        let for_ok = confirm.clone();

        window.open_dialog(cx, move |dialog, _window, cx| {
            let rows = workspace.read(cx).panes.collections.read(cx).destinations();
            let node = workspace.read(cx).move_node.clone();
            let current = workspace.read(cx).move_target.clone();
            // The move button stays disabled until the choice would actually
            // change something, rather than reporting a no-op as a move.
            let unchanged = match (&node, &current) {
                (Some(node), Some(current)) => {
                    workspace
                        .read(cx)
                        .panes
                        .collections
                        .read(cx)
                        .destination_of(node)
                        == Some(current.clone())
                }
                _ => true,
            };
            let current_label = current
                .as_ref()
                .and_then(|target| rows.iter().find(|row| &row.target == target))
                .map(|row| row.label.clone())
                .unwrap_or_else(|| "Choose a collection".to_owned());

            let menu_workspace = workspace.clone();
            let menu_rows = rows.clone();
            let menu_current = current.clone();
            let menu_pick: DestinationPick = Rc::new(|workspace, target| {
                workspace.move_target = Some(target.clone());
            });
            let menu = move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                with_destinations(
                    menu,
                    &menu_rows,
                    menu_current.as_ref(),
                    &menu_workspace,
                    &menu_pick,
                )
                .scrollable(true)
            };

            let into = h_flex()
                .gap(px(metrics.space_sm))
                .items_center()
                .text_sm()
                .child(div().text_color(palette.muted).child("To"))
                .child(
                    Button::new("move-request-destination")
                        .label(current_label)
                        .ghost()
                        .compact()
                        .flex_1()
                        .min_w_0()
                        .dropdown_caret(true)
                        .tooltip("Where the request moves to")
                        .dropdown_menu(menu),
                );

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
    pub(super) fn commit_move(&mut self, cx: &mut Context<Self>) -> bool {
        let (Some(node), Some(target)) = (self.move_node.clone(), self.move_target.clone()) else {
            return false;
        };
        self.panes.collections.update(cx, |panel, cx| {
            panel.move_request(&node, &target, cx).is_some()
        })
    }
}
