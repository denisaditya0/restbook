//! The Params tab: query-string pairs.

use gpui_kit::component::v_flex;
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

        v_flex()
            .child(
                div()
                    .pb(px(self.metrics.space_sm))
                    .text_xs()
                    .text_color(self.palette.muted)
                    .child("Query string parameters. Disabled rows are not sent."),
            )
            .child(kv_table::render(&self.params, &self.palette, cx, on_action))
            .into_any_element()
    }
}
