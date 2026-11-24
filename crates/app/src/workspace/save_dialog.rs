//! The first-save dialog: a name and a destination.
//!
//! A draft has neither, so the save asks for both. The destination is a picker
//! over every collection and its folders rather than the sidebar's highlight: a
//! save should not fail because nothing was clicked last, and the highlight is
//! navigation again.
//!
//! The choice lives on the workspace (`save_name`, `save_target`) because the
//! dialog's content closure is rebuilt on every notify, so state created inside
//! it would be lost between builds.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::button::*;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{h_flex, v_flex, Disableable as _, WindowExt as _};
use gpui_kit::*;

use crate::icons::IconName;
use crate::theme::{Metrics, Palette};

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
        // The selection is only the starting point: the picker can change it, and
        // an unselected sidebar still opens on the first collection.
        self.save_target = self.panes.collections.read(cx).default_destination();

        let workspace = cx.entity();
        let name_input = self.save_name.clone();
        let field = self.save_name.clone();
        let focus_input = self.save_name.clone();
        let first_build = Cell::new(true);
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);

        // One confirm for the footer button and for Enter: it reads the field and
        // the picker at the moment of the click, not when the dialog was built.
        type Confirm = Rc<dyn Fn(&mut Window, &mut App) -> bool>;
        let confirm_workspace = workspace.clone();
        let confirm: Confirm = Rc::new(move |_, cx| {
            let name = name_input.read(cx).value().trim().to_owned();
            if name.is_empty() {
                // An unnamed request would be unfindable in the tree; keep the
                // dialog open rather than saving something the user has to fix.
                return false;
            }
            confirm_workspace.update(cx, |workspace, cx| {
                let target = workspace.save_target.clone();
                workspace.commit_save(None, Some(&name), target, cx)
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

            let rows = workspace.read(cx).panes.collections.read(cx).destinations();
            let current = workspace.read(cx).save_target.clone();
            let current_label = current
                .as_ref()
                .and_then(|target| rows.iter().find(|row| &row.target == target))
                .map(|row| row.label.clone())
                .unwrap_or_else(|| "Choose a collection".to_owned());

            let menu_workspace = workspace.clone();
            let menu_rows = rows.clone();
            let menu_current = current.clone();
            let menu = move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                if menu_rows.is_empty() {
                    menu = menu.item(PopupMenuItem::new("No collections yet.").disabled(true));
                }
                for row in &menu_rows {
                    let target = row.target.clone();
                    let checked = menu_current.as_ref() == Some(&target);
                    let workspace = menu_workspace.clone();
                    menu = menu.item(
                        PopupMenuItem::new(row.label.clone())
                            .icon(IconName::Folder)
                            .checked(checked)
                            .on_click(move |_, _, cx| {
                                workspace.update(cx, |workspace, cx| {
                                    workspace.save_target = Some(target.clone());
                                    cx.notify();
                                });
                            }),
                    );
                }
                // The list is every folder in every collection, so it can be
                // taller than a minimum-height window.
                menu.scrollable(true)
            };

            let into = h_flex()
                .gap(px(metrics.space_sm))
                .items_center()
                .text_sm()
                .child(div().text_color(palette.muted).child("Into"))
                .child(
                    Button::new("save-request-destination")
                        .label(current_label)
                        .ghost()
                        .compact()
                        .flex_1()
                        .min_w_0()
                        .dropdown_caret(true)
                        .tooltip("Where the request is saved")
                        .dropdown_menu(menu),
                );

            let name_row = v_flex()
                .gap(px(metrics.space_xs))
                .child(div().text_xs().text_color(palette.muted).child("Name"))
                .child(Input::new(&field).id("save-request-name"));

            let cancel = Button::new("save-request-cancel")
                .label("Cancel")
                .ghost()
                .on_click(|_, window, cx| window.close_dialog(cx));
            let save = Button::new("save-request-confirm")
                .label("Save")
                .primary()
                .disabled(current.is_none())
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
                        .child(into),
                )
                .footer(DialogFooter::new().child(cancel).child(save))
                .on_ok({
                    let confirm = for_ok.clone();
                    move |_, window, cx| confirm(window, cx)
                })
        });
    }
}
