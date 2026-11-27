//! One self-contained module per pane.
//!
//! A pane is a GPUI entity that renders a region of the window. It receives what
//! it needs in its constructor — a shared `Document`, its own store handle — and
//! reports what it cannot handle itself through an `EventEmitter`. None of the
//! panes name another; that is the only coupling between them.

pub(crate) mod collections;
pub(crate) mod command_palette;
mod editor;
pub(crate) mod empty;
pub(crate) mod environment;
pub(crate) mod history;
pub(crate) mod history_row;
pub(crate) mod request;
pub(crate) mod response;

pub(crate) use collections::CollectionsPanel;
pub(crate) use editor::InheritedDefaults;
pub(crate) use environment::EnvironmentPanel;
pub(crate) use history::HistoryPanel;
pub(crate) use request::RequestPanel;
pub(crate) use response::ResponsePanel;
