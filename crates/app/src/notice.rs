//! The outcome a pane reports upward: one message, one tone.
//!
//! A pane emits `Event::Notice(Notice)` and the workspace owns the flash and
//! its lifetime. Kept out of `panels/` because `variables` (not a pane) and the
//! workspace both produce or consume it; kept out of `workspace/` because the
//! panes must not depend on the workspace.

use gpui_kit::SharedString;

/// A confirmation for a mutation. `Failed` carries a message rather than a bare
/// flag because the store's own error text is the most useful thing to show;
/// `Done` carries the sentence for the action that completed.
///
/// The tone is decided where it is drawn (`FlashKind`), not here, so this type
/// stays free of any palette or rendering dependency.
#[derive(Debug, Clone)]
pub(crate) enum Notice {
    Done(SharedString),
    Failed(SharedString),
}
