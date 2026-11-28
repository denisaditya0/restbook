//! The tree's row renderers: one visible row per collection, folder, or saved
//! request, drawn from a `FlatRow`.
//!
//! These builders are free of the pane's `&self` so the virtual list's closure
//! can call them with only the small data it captured; every action goes back
//! through the panel's `Entity`.

use gpui_kit::component::button::*;
use gpui_kit::component::h_flex;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu, PopupMenu};
use gpui_kit::*;

use crate::icons::{self, IconName};
use crate::theme::{Metrics, Palette};

use super::tree::initial;
use super::{row_items, CollectionsPanel, Event, FlatRow, RowKind, RowTarget, RowVerbs, Selection};

/// Build one tree row from its flattened form.
pub(super) fn row_element(
    row: &FlatRow,
    selection: &Selection,
    renaming: Option<&str>,
    palette: &Palette,
    metrics: &Metrics,
    panel: &Entity<CollectionsPanel>,
    rename_input: &Entity<InputState>,
) -> AnyElement {
    let selected = row.is_selected(selection);
    let renaming = renaming == Some(row.id.as_ref());
    match row.kind {
        RowKind::Collection => collection_row(
            row,
            selected,
            renaming,
            palette,
            metrics,
            panel,
            rename_input,
        ),
        RowKind::Folder => folder_row(
            row,
            selected,
            renaming,
            palette,
            metrics,
            panel,
            rename_input,
        ),
        RowKind::Request(_) => request_row(
            row,
            selected,
            renaming,
            palette,
            metrics,
            panel,
            rename_input,
        ),
    }
}

/// The shared inline rename editor: an input and a confirm button, identical
/// whether the row is a collection, a folder, or a request.
fn rename_area(
    prefix: &str,
    id: &str,
    metrics: &Metrics,
    panel: &Entity<CollectionsPanel>,
    rename_input: &Entity<InputState>,
) -> AnyElement {
    h_flex()
        .id(format!("{prefix}-rename-{id}"))
        .flex_1()
        .min_w_0()
        .gap(px(metrics.space_xs))
        .items_center()
        // Escape bubbles up from the input to cancel the whole rename; Enter is
        // caught through the input's own `PressEnter` event.
        .on_key_down({
            let panel = panel.clone();
            move |event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    panel.update(cx, |this, cx| this.cancel_rename(window, cx));
                }
            }
        })
        .child(
            Input::new(rename_input)
                .id(format!("{prefix}-rename-input"))
                .flex_1(),
        )
        .child(
            Button::new(format!("{prefix}-rename-confirm"))
                .icon(IconName::Check)
                .ghost()
                .compact()
                .on_click({
                    let panel = panel.clone();
                    move |_, window, app| {
                        panel.update(app, |this, cx| this.commit_rename(window, cx));
                    }
                }),
        )
        .into_any_element()
}

fn collection_row(
    row: &FlatRow,
    selected: bool,
    renaming: bool,
    palette: &Palette,
    metrics: &Metrics,
    panel: &Entity<CollectionsPanel>,
    rename_input: &Entity<InputState>,
) -> AnyElement {
    let toggle = Button::new(format!("collection-toggle-{}", row.id))
        .icon(if row.collapsed {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        })
        .ghost()
        .compact()
        .on_click({
            let panel = panel.clone();
            let id = row.id.to_string();
            move |_, _, app| {
                panel.update(app, |this, cx| this.toggle_collapse(&id, cx));
            }
        });

    let name_area = if renaming {
        rename_area("collection", &row.id, metrics, panel, rename_input)
    } else {
        h_flex()
            .id(format!("collection-name-{}", row.id))
            .flex_1()
            .min_w_0()
            .gap(px(metrics.space_1p5))
            .items_center()
            .cursor_pointer()
            .on_click({
                let panel = panel.clone();
                let id = row.id.to_string();
                move |_, window, app| {
                    panel.update(app, |this, cx| {
                        this.selection = Selection::collection(&id);
                        // Give the pane keyboard focus, so `f2` (scoped to the
                        // `Collections` context) reaches the tree after a click.
                        window.focus(&this.focus_handle, cx);
                        cx.notify();
                    });
                }
            })
            // The monogram: one letter in a tinted square, the same shape as
            // the workspace chip in the titlebar.
            .child(
                div()
                    .flex_none()
                    .size(px(metrics.icon_sm))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .bg(palette.list_hover)
                    .text_color(palette.muted)
                    .text_size(px(metrics.font_badge))
                    .font_weight(FontWeight::BOLD)
                    .child(initial(row.name.as_ref())),
            )
            .child(
                div()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(palette.foreground)
                    .child(row.name.clone()),
            )
            .into_any_element()
    };

    let mut element = h_flex()
        .id(format!("collection-{}", row.id))
        .w_full()
        .h(px(metrics.row_height))
        .px(px(metrics.space_sm))
        .gap(px(metrics.space_xs))
        .items_center()
        .rounded_sm()
        .hover(|style| style.bg(palette.list_hover));
    if selected {
        element = element.bg(palette.list_active);
    }

    let target = row.target();
    let tools = row_tools(&target, row.verbs, metrics, panel);
    let element = element.child(toggle).child(name_area).child(tools);
    if renaming {
        element.into_any_element()
    } else {
        let menu = row_menu(panel, target, row.verbs);
        element.context_menu(menu).into_any_element()
    }
}

fn folder_row(
    row: &FlatRow,
    selected: bool,
    renaming: bool,
    palette: &Palette,
    metrics: &Metrics,
    panel: &Entity<CollectionsPanel>,
    rename_input: &Entity<InputState>,
) -> AnyElement {
    let toggle = Button::new(format!("folder-toggle-{}", row.id))
        .icon(if row.collapsed {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        })
        .ghost()
        .compact()
        .on_click({
            let panel = panel.clone();
            let id = row.id.to_string();
            move |_, _, app| {
                panel.update(app, |this, cx| this.toggle_collapse(&id, cx));
            }
        });

    let name_area = if renaming {
        rename_area("folder", &row.id, metrics, panel, rename_input)
    } else {
        h_flex()
            .id(format!("folder-name-{}", row.id))
            .flex_1()
            .min_w_0()
            .gap(px(metrics.space_xs))
            .cursor_pointer()
            .on_click({
                let panel = panel.clone();
                let collection = row.collection.to_string();
                let path: Vec<String> = row.path.iter().map(|id| id.to_string()).collect();
                move |_, window, app| {
                    panel.update(app, |this, cx| {
                        this.selection = Selection {
                            collection: Some(collection.clone()),
                            folders: path.clone(),
                            request: None,
                        };
                        window.focus(&this.focus_handle, cx);
                        cx.notify();
                    });
                }
            })
            .child(icons::quiet(
                if row.collapsed {
                    IconName::Folder
                } else {
                    IconName::FolderOpen
                },
                icons::SMALL,
                palette,
            ))
            .child(
                div()
                    .truncate()
                    .text_sm()
                    .text_color(palette.foreground)
                    .child(row.name.clone()),
            )
            .into_any_element()
    };

    let mut element = h_flex()
        .id(format!("folder-{}", row.id))
        .w_full()
        .h(px(metrics.row_height))
        .px(px(metrics.space_sm))
        .gap(px(metrics.space_xs))
        .items_center()
        .rounded_sm()
        .pl(px(metrics.gutter * 0.5 + row.depth as f32 * metrics.indent))
        .hover(|style| style.bg(palette.list_hover));
    if selected {
        element = element.bg(palette.list_active);
    }

    let target = row.target();
    let tools = row_tools(&target, row.verbs, metrics, panel);
    let element = element.child(toggle).child(name_area).child(tools);
    if renaming {
        element.into_any_element()
    } else {
        let menu = row_menu(panel, target, row.verbs);
        element.context_menu(menu).into_any_element()
    }
}

fn request_row(
    row: &FlatRow,
    selected: bool,
    renaming: bool,
    palette: &Palette,
    metrics: &Metrics,
    panel: &Entity<CollectionsPanel>,
    rename_input: &Entity<InputState>,
) -> AnyElement {
    let RowKind::Request(method) = row.kind else {
        return div().into_any_element();
    };
    let method_badge = div()
        .flex_none()
        .text_size(px(metrics.font_badge))
        .font_weight(FontWeight::BOLD)
        .text_color(palette.method(method))
        .child(method.as_str());

    let name_area = if renaming {
        rename_area("request", &row.id, metrics, panel, rename_input)
    } else {
        h_flex()
            .id(format!("request-name-{}", row.id))
            .flex_1()
            .min_w_0()
            .gap(px(metrics.space_sm))
            .items_center()
            .cursor_pointer()
            .on_click({
                let panel = panel.clone();
                let collection = row.collection.to_string();
                let path: Vec<String> = row.path.iter().map(|id| id.to_string()).collect();
                let id = row.id.to_string();
                move |_, window, app| {
                    panel.update(app, |this, cx| {
                        this.selection = Selection {
                            collection: Some(collection.clone()),
                            folders: path.clone(),
                            request: Some(id.clone()),
                        };
                        window.focus(&this.focus_handle, cx);
                        // Read the request at click time rather than caching a
                        // copy per row; the flattened row carries only what a
                        // row draws.
                        if let Some(request) = this.saved_request(&id) {
                            cx.emit(Event::Open {
                                node: id.clone(),
                                request,
                            });
                        }
                        cx.notify();
                    });
                }
            })
            .child(method_badge)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(if selected {
                        palette.foreground
                    } else {
                        palette.muted
                    })
                    .child(row.name.clone()),
            )
            .into_any_element()
    };

    let mut element = h_flex()
        .id(format!("request-{}", row.id))
        .w_full()
        .h(px(metrics.row_height))
        .px(px(metrics.space_sm))
        // The 6 px step, not `sm`: a request row has a badge before its name,
        // so it gets a half-step more than the rest of the tree.
        .gap(px(metrics.space_1p5))
        .items_center()
        .rounded_sm()
        .pl(px(metrics.gutter * 0.5 + row.depth as f32 * metrics.indent))
        .hover(|style| style.bg(palette.list_hover));
    if selected {
        element = element.bg(palette.list_active);
    }

    let target = row.target();
    let tools = row_tools(&target, row.verbs, metrics, panel);
    let element = element.child(name_area).child(tools);
    if renaming {
        element.into_any_element()
    } else {
        let menu = row_menu(panel, target, row.verbs);
        element.context_menu(menu).into_any_element()
    }
}

/// The row's overflow menu: one ghost ellipsis that opens the same verbs a
/// right-click does. `justify_end` pins it so every name truncates at the same
/// column, and the fixed lane keeps that column still across row kinds.
fn row_tools(
    target: &RowTarget,
    verbs: RowVerbs,
    metrics: &Metrics,
    panel: &Entity<CollectionsPanel>,
) -> AnyElement {
    let menu = row_menu(panel, target.clone(), verbs);
    h_flex()
        .flex_none()
        .justify_end()
        .w(px(metrics.tool_lane))
        .child(
            Button::new(format!("row-menu-{}", target.id()))
                .icon(IconName::Ellipsis)
                .ghost()
                .compact()
                .tooltip("More actions")
                .dropdown_menu(menu),
        )
        .into_any_element()
}

/// The verbs a row offers, shared by the ellipsis dropdown and a right-click
/// anywhere on the row. The panel handle is what the menu callbacks use instead
/// of a `Context`, since they arrive with an `App`.
fn row_menu(
    panel: &Entity<CollectionsPanel>,
    target: RowTarget,
    verbs: RowVerbs,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    let id = target.id().to_owned();
    let panel = panel.clone();
    move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
        row_items(menu, id.clone(), target.clone(), verbs, panel.clone())
    }
}
