//! The Params tab: query-string pairs, as rows or as wire-form text.

use gpui_kit::component::button::*;
use gpui_kit::component::input::Editor;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;

use super::kv_table::{self, RowAction};
use super::RequestEditor;

impl RequestEditor {
    pub(crate) fn render_params_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let on_action = cx.listener(|this, action: &RowAction, window, cx| {
            let variables = this.variables.clone();
            this.params.apply(*action, &variables, window, cx);
            cx.notify();
        });
        let raw = self.params_raw.is_some();
        let toggle = Button::new("params-view")
            .label(if raw { "Table" } else { "Raw" })
            .tooltip(if raw {
                "Edit as key/value rows"
            } else {
                "Edit as key=value text"
            })
            .ghost()
            .compact()
            .on_click(cx.listener(|this, _, window, cx| this.toggle_params_raw(window, cx)));

        let content = if let Some(raw) = &self.params_raw {
            Editor::new(raw.state())
                .h(px(self.metrics.body_row_height * 8.))
                .into_any_element()
        } else {
            kv_table::render(&self.params, &self.palette, cx, on_action)
        };

        v_flex()
            .child(
                h_flex()
                    .gap(px(self.metrics.space_md))
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .pb(px(self.metrics.space_sm))
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child("Query string parameters. Disabled rows are not sent."),
                    )
                    .child(toggle),
            )
            .child(content)
            .into_any_element()
    }
}
