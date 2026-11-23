//! The Table/Raw switch for a `Kv` list.
//!
//! The table's rows stay authoritative; the raw side is one text field in wire
//! form (`kv_text`). Switching parses the text back into rows, and a send reads
//! whichever side is showing, so the two can never disagree about the request.

use gpui_kit::component::input::EditorState;
use gpui_kit::*;

use restbook_core::{kv_text, Kv};

use super::RequestEditor;

pub(crate) struct RawRows {
    editor: Entity<EditorState>,
}

impl RawRows {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<RequestEditor>) -> Self {
        let editor =
            cx.new(|cx| EditorState::new(window, cx).placeholder("key=value\n# disabled=row"));
        Self { editor }
    }

    pub(crate) fn state(&self) -> &Entity<EditorState> {
        &self.editor
    }

    /// Write rows into the text, which is what opening the view does.
    pub(crate) fn write(&self, rows: &[Kv], window: &mut Window, cx: &mut App) {
        self.set_text(kv_text::render(rows), window, cx);
    }

    pub(crate) fn set_text(&self, text: impl Into<String>, window: &mut Window, cx: &mut App) {
        self.editor
            .update(cx, |state, cx| state.set_value(text.into(), window, cx));
    }

    pub(crate) fn rows(&self, cx: &App) -> Vec<Kv> {
        let text = self.editor.read(cx).value().to_string();
        kv_text::parse(&text)
    }
}
