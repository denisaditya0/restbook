//! What a window close does with edits that were never saved to a collection.
//!
//! A collection-bound tab can hold changes its node does not: the draft
//! debounce writes only unbound drafts, and a bound tab has no draft row. Closing
//! is the last moment to save them, so it is the one place that can prompt.
//!
//! Cancel here means "do not quit", unlike every other dialog in the app, where
//! Cancel still performs the action. The close was already cancelled to get to
//! the dialog, so its buttons are what actually closes the window.

use gpui_kit::component::button::*;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::{v_flex, WindowExt as _};
use gpui_kit::*;

use crate::panels::request::tab_label;
use crate::theme::{Metrics, Palette};

use super::settings::SaveOnQuit;
use super::{RequestStamp, TabState, Workspace};

impl Workspace {
    /// Decide what a window close does, and return whether the window may close.
    ///
    /// `Ask` with unsaved bound tabs shows the prompt and returns `false`, which
    /// cancels the close the platform already started. The prompt's buttons call
    /// [`Window::remove_window`] once the user has answered.
    pub(crate) fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // The active tab's edits live in the editor, not yet in its snapshot.
        self.snapshot_active(cx);
        let dirty = dirty_bound_tabs(&self.tabs);
        if dirty.is_empty() {
            self.flush(cx);
            return true;
        }
        match self.settings.read(cx).current().save_on_quit {
            SaveOnQuit::Ask => {
                // A tick from an earlier prompt must not carry over.
                self.quit_remember = false;
                self.open_quit_dialog(dirty, window, cx);
                false
            }
            SaveOnQuit::Save => {
                self.save_dirty_bound_tabs(cx);
                self.flush(cx);
                true
            }
            SaveOnQuit::Discard => {
                self.flush(cx);
                true
            }
        }
    }

    /// Save every bound tab whose request differs from its node, without
    /// switching to it. The active tab's live request is snapshotted first.
    fn save_dirty_bound_tabs(&mut self, cx: &mut Context<Self>) {
        self.snapshot_active(cx);
        for index in 0..self.tabs.len() {
            let Some((node, request, stamp)) = self.tabs.get(index).and_then(|tab| {
                let node = tab.node.clone()?;
                let stamp = RequestStamp::of(&tab.request);
                (tab.saved.as_deref() != Some(stamp.as_str()))
                    .then(|| (node, tab.request.clone(), stamp))
            }) else {
                continue;
            };
            let landed = self
                .panes
                .collections
                .update(cx, |panel, cx| panel.save_request_at(&node, request, cx));
            if landed {
                // The tab now matches its node, so a later close will not count it.
                self.tabs[index].saved = Some(stamp);
            }
        }
    }

    /// Raise the quit prompt. The close that started this was cancelled, so the
    /// buttons are what actually closes the window.
    fn open_quit_dialog(
        &mut self,
        labels: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, description) = quit_prompt(&labels);
        let workspace = cx.entity();
        let palette = Palette::new(cx);
        let metrics = Metrics::current(cx);

        window.open_dialog(cx, move |dialog, _window, cx| {
            let remember = workspace.read(cx).quit_remember;
            let toggle = Checkbox::new(("quit-remember", 0u64))
                .label("Remember my choice")
                .checked(remember)
                .on_click({
                    let workspace = workspace.clone();
                    move |checked, _, cx| {
                        workspace.update(cx, |this, cx| {
                            this.quit_remember = *checked;
                            cx.notify();
                        });
                    }
                });

            let cancel = Button::new("quit-cancel")
                .label("Cancel")
                .ghost()
                .on_click(|_, window, cx| window.close_dialog(cx));

            let discard = Button::new("quit-discard")
                .label("Discard and quit")
                .danger()
                .on_click(quit_button(workspace.clone(), QuitChoice::Discard));

            let save = Button::new("quit-save")
                .label("Save and quit")
                .primary()
                .on_click(quit_button(workspace.clone(), QuitChoice::Save));

            dialog
                .title(title.clone())
                .w_112()
                .child(
                    v_flex()
                        .gap(px(metrics.space_md))
                        .py(px(metrics.space_sm))
                        .child(
                            div()
                                .text_sm()
                                .text_color(palette.foreground)
                                .child(description.clone()),
                        )
                        .child(toggle),
                )
                .footer(DialogFooter::new().child(cancel).child(discard).child(save))
                .on_cancel(|_, window, cx| {
                    window.close_dialog(cx);
                    true
                })
        });
    }

    /// Carry out the answer, then let the button close the window. A remembered
    /// answer is written to Settings so the next close does not ask.
    fn finish_quit(&mut self, choice: QuitChoice, cx: &mut Context<Self>) {
        if self.quit_remember {
            let mode = choice.mode();
            let mut next = self.settings.read(cx).current().clone();
            if next.save_on_quit != mode {
                next.save_on_quit = mode;
                self.settings
                    .update(cx, |settings, cx| settings.update(next, cx));
            }
        }
        if matches!(choice, QuitChoice::Save) {
            self.save_dirty_bound_tabs(cx);
        }
        self.flush(cx);
    }
}

/// The click handler both quit buttons share: act, then close the window.
fn quit_button(
    workspace: Entity<Workspace>,
    choice: QuitChoice,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_, window, cx| {
        workspace.update(cx, |this, cx| this.finish_quit(choice, cx));
        window.remove_window();
    }
}

#[derive(Clone, Copy)]
enum QuitChoice {
    Save,
    Discard,
}

impl QuitChoice {
    /// The setting a remembered choice becomes.
    fn mode(self) -> SaveOnQuit {
        match self {
            QuitChoice::Save => SaveOnQuit::Save,
            QuitChoice::Discard => SaveOnQuit::Discard,
        }
    }
}

/// The labels of collection-bound tabs whose request differs from what their
/// node holds, in strip order. An unbound draft is never included: it has its
/// own row, written by the debounce, so closing it loses nothing.
fn dirty_bound_tabs(tabs: &TabState) -> Vec<String> {
    tabs.iter()
        .filter(|tab| tab.node.is_some())
        .filter(|tab| tab.saved.as_deref() != Some(RequestStamp::of(&tab.request).as_str()))
        .map(|tab| tab_label(&tab.request).to_string())
        .collect()
}

/// The prompt's title and body. Kept apart from the dialog so the copy is
/// testable without a window, the way the delete dialog's is.
fn quit_prompt(labels: &[String]) -> (String, String) {
    let title = match labels.len() {
        1 => "Save 1 changed request?".to_owned(),
        count => format!("Save {count} changed requests?"),
    };
    let names = labels
        .iter()
        .map(|label| format!("\u{201c}{label}\u{201d}"))
        .collect::<Vec<_>>()
        .join(", ");
    (title, format!("{names} changed since the last save."))
}

#[cfg(test)]
mod tests {
    // Named imports: the crate glob-imports `gpui_kit::*`, whose macro set
    // shadows `#[test]`.
    use super::{dirty_bound_tabs, quit_prompt};
    use crate::workspace::{RequestStamp, Tab, TabState};

    fn bound(url: &str, saved: Option<String>) -> Tab {
        let mut tab = Tab::blank();
        tab.request.url = url.to_owned();
        tab.node = Some("node-1".to_owned());
        tab.saved = saved;
        tab
    }

    #[test]
    fn a_bound_tab_counts_only_once_its_request_changed() {
        // A tab whose request still matches its node is not at risk. The baseline
        // is the stamp of the very same request, the way a save records it.
        let mut clean = bound("https://a.example", None);
        clean.saved = Some(RequestStamp::of(&clean.request));
        assert!(dirty_bound_tabs(&TabState::new(vec![clean])).is_empty());

        // An edit the node has not seen is dirty.
        let baseline = RequestStamp::of(&Tab::blank().request);
        let edited = bound("https://b.example", Some(baseline));
        assert_eq!(dirty_bound_tabs(&TabState::new(vec![edited])).len(), 1);

        // A bound tab never saved to its node has no baseline and counts.
        let unsaved = bound("https://c.example", None);
        assert_eq!(dirty_bound_tabs(&TabState::new(vec![unsaved])).len(), 1);
    }

    #[test]
    fn an_unbound_draft_is_never_a_quit_prompt() {
        let mut tab = Tab::blank();
        tab.request.url = "https://draft.example".to_owned();
        tab.draft = Some(7);
        assert!(dirty_bound_tabs(&TabState::new(vec![tab])).is_empty());
    }

    #[test]
    fn the_prompt_names_the_tabs_and_counts_them() {
        let (title, body) = quit_prompt(&["GET example.com".to_owned()]);
        assert_eq!(title, "Save 1 changed request?");
        assert!(body.contains("\u{201c}GET example.com\u{201d}"));

        let (title, body) = quit_prompt(&["GET a".to_owned(), "POST b".to_owned()]);
        assert_eq!(title, "Save 2 changed requests?");
        assert!(body.contains("\u{201c}GET a\u{201d}, \u{201c}POST b\u{201d}"));
    }
}
