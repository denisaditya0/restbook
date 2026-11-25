//! The Request pane: request tabs, the URL row, and the editor tabs.
//!
//! The pane holds [`RequestEditor`] and the shared [`Document`], kept in step
//! without either owning the other; both directions compare before writing so a
//! keystroke never moves the cursor. The tab strip lives here, not in the canvas
//! where it would read as a second title bar, and the workspace still owns it.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::base::{ElementExt as _, InteractiveElementExt as _};
use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputEvent};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{TabBar, TabVariant};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex, Disableable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use restbook_core::{Method, OAuthConfig, RequestDef};

use crate::actions::{shortcut, FocusUrl, NewTab, PasteCurl, SaveRequest, SendRequest};
use crate::completion::Autocomplete;
use crate::document::Document;
use crate::icons::{self, IconName};
use crate::notice::Notice;
use crate::panels::editor::{EditorEvent, EditorTab, RequestEditor};
use crate::theme::{Metrics, Palette};
use crate::variables::{references, Variables};
use crate::workspace::{RequestStamp, TabAction, TabEntry, TabId};

pub(crate) enum Event {
    /// Send the request as currently edited.
    Send,
    /// Cancel the request in flight.
    Cancel,
    /// Save the current request into a collection. The workspace reads the
    /// live document and decides where it lands; this pane only asks.
    Save,
    /// Save the active request as a new collection node, even when the tab is
    /// already bound. The workspace prefills the dialog with the current name.
    SaveAs,
    /// The user picked another open request tab, by its stable id.
    SelectTab(TabId),
    /// The user asked for one more tab.
    NewTab,
    /// The user closed the tab with this id.
    CloseTab(TabId),
    /// A tab verb the strip's menu reported. The workspace owns the policy for
    /// a close, a batch, a duplicate, or a reorder.
    TabAction(TabAction),
    /// An import finished and wants to say so. Routed to the workspace for the
    /// same reason the collections tree routes its confirmations there: one
    /// toast, drawn in one place, with one lifetime. The workspace also offers
    /// to name and save the imported request.
    Imported,
    /// An import failed and wants to say why.
    Notice(Notice),
    /// The empty-state button asked for a file. The pane holds no window, so the
    /// workspace opens the picker from its next render, the same way it defers
    /// a save.
    Import,
    /// The editor menu asked to copy the live request as a cURL command. The
    /// workspace owns the clipboard, so the pane only asks.
    CopyCurl,
    /// The Auth tab asked to fetch an OAuth token with these settings. The
    /// workspace runs it, because only it holds the transport.
    FetchToken(OAuthConfig),
}

/// One line for the interaction log, derived from the event itself. The match
/// is exhaustive on purpose: a new variant has to name itself here, so a click
/// cannot start going unlogged just because an arm was added.
impl Event {
    pub(crate) fn summary(&self) -> Option<String> {
        match self {
            Event::Send => Some("event: send".to_owned()),
            Event::Cancel => Some("event: cancel".to_owned()),
            Event::Save => Some("event: save".to_owned()),
            Event::SaveAs => Some("event: save as".to_owned()),
            Event::SelectTab(id) => Some(format!("event: select tab {id}")),
            Event::NewTab => Some("event: new tab".to_owned()),
            Event::CloseTab(id) => Some(format!("event: close tab {id}")),
            Event::TabAction(action) => Some(format!("event: tab {action:?}")),
            // The workspace logs the import and the save prompt it opens.
            Event::Imported => None,
            // A notice is logged by `Workspace::notice`, which also routes it
            // to the flash; logging it twice would just repeat the message.
            Event::Notice(_) => None,
            Event::Import => Some("event: import file".to_owned()),
            Event::CopyCurl => Some("event: copy as curl".to_owned()),
            Event::FetchToken(_) => Some("event: fetch oauth token".to_owned()),
        }
    }
}

/// Whether a tab shows the unsaved marker: it has a saved baseline, and the
/// request it holds no longer matches it. A tab that was never saved is not
/// marked — a fresh tab is an empty page, not unsaved work.
fn is_unsaved(saved: Option<&str>, current: &str) -> bool {
    saved.is_some_and(|baseline| baseline != current)
}

/// What a strip entry is keyed on, so retained hover or focus does not jump to
/// a neighbour when an earlier tab closes. The tab's stable id is the one value
/// that survives a close, a reorder, and a save all at once.
fn tab_key(tab: &TabEntry) -> SharedString {
    SharedString::from(format!("tab:{}", tab.id))
}

/// The tab the pane's own menu acts on: the one on screen. `None` when the
/// workspace has closed every tab, which is when Duplicate and Save As have
/// nothing to act on and should read as unavailable.
fn menu_tab(tabs: &[TabEntry], active: usize) -> Option<TabId> {
    tabs.get(active).map(|tab| tab.id)
}

/// The short name a request goes by in the tab strip: the URL with its scheme
/// removed, because every tab in one session shares it.
pub(crate) fn tab_label(request: &RequestDef) -> SharedString {
    let url = request.url.trim();
    if url.is_empty() {
        return "(new request)".into();
    }
    url.split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .into()
}

/// `verb (⌘⏎)` when the action has a binding, the bare verb otherwise. The key
/// comes from the live keymap, so the tooltip is correct on every platform.
fn with_shortcut(verb: &str, shortcut: Option<String>) -> String {
    match shortcut {
        Some(key) => format!("{verb} ({key})"),
        None => verb.to_owned(),
    }
}

/// The width below which the URL row drops the Save verb's label to its glyph:
/// enough for the method, a usable field, and Send. The dock lets a pane shrink
/// far below this, so the row has to decide what gives rather than overflow.
const NARROW_URL_ROW: f32 = 340.;

pub(crate) struct RequestPanel {
    palette: Palette,
    /// The metrics the strip and the URL row are sized with, so a zoom step
    /// reaches the bands and controls, not only the text inside them.
    metrics: Metrics,
    document: Entity<Document>,
    editor: Entity<RequestEditor>,
    /// The variable scopes a send resolves against. Observed so the unresolved
    /// warning follows what the user edits in the Environment pane.
    variables: Entity<Variables>,
    /// One entry per open tab, as the workspace sees them: the method, the
    /// name, and the request as it was when last saved (if ever). The active
    /// entry's label comes from the document, and the unsaved marker is read
    /// from the cached live stamp, so both follow a keystroke without a fold.
    tabs: Vec<TabEntry>,
    active_tab: usize,
    /// The live request, folded once when it changes. The strip and the URL row
    /// are rebuilt on every repaint, so the pane caches the stamp and the
    /// unresolved names instead of folding the request (and its body) during
    /// render, which would be O(body) per frame.
    live_stamp: String,
    live_unresolved: Vec<String>,
    /// The document's `is_pending` the URL row was last drawn with. A send
    /// starting or resolving changes no request field, so the observer below
    /// would find nothing stale and leave the button saying "Sending" until an
    /// unrelated repaint — a dock with nothing else moving has none.
    pending_painted: bool,
    /// The URL bar's `{{…}}` completion list. `InputState` has no completion
    /// hook the way `EditorState` does, so the pane keeps the state and draws
    /// the list under the field itself.
    url_popup: Autocomplete,
    /// The horizontal scroll position of the request-tab strip. Kept so a tab
    /// selected from off-screen can be scrolled into view, instead of the strip
    /// leaving the active tab hidden when there are many.
    tabs_scroll: ScrollHandle,
    /// The horizontal scroll of the editor's own tab strip (`Params | Headers …`).
    /// `TabBar` only keeps its tabs inside the band when it is given a bounded
    /// width and a handle to reveal a clicked tab into; left unbounded it lays
    /// out at its content width and the last tabs are clipped by the pane.
    editor_tabs_scroll: ScrollHandle,
    /// The URL row's last measured width, in unscaled pixels. A dock pane can be
    /// dragged down to 100 px, so the row collapses the Save verb's label to a
    /// glyph when the label would not fit. Written from `on_prepaint`, because a
    /// pane's own width is only known after layout.
    url_row_width: Rc<Cell<f32>>,
    focus_handle: FocusHandle,
}

/// One tab as the strip draws it: the workspace's [`TabEntry`], with the
/// active tab's method and URL read from the live document so a keystroke
/// reaches the strip without another push from the workspace.
struct StripTab {
    id: TabId,
    key: SharedString,
    method: Method,
    label: SharedString,
    /// The full address, for the tooltip when the label is truncated.
    url: SharedString,
    active: bool,
    unsaved: bool,
}

impl EventEmitter<Event> for RequestPanel {}
impl EventEmitter<gpui_kit::component::dock::PanelEvent> for RequestPanel {}

impl gpui_kit::component::dock::BasePanel for RequestPanel {
    fn panel_name(&self) -> &'static str {
        "request"
    }
}

/// Presentation half of the dock seam. The pane already opens with its tab
/// strip, URL row, and Send controls, so a dock title bar would only repeat it.
impl gpui_kit::component::dock::Panel for RequestPanel {
    fn tab_name(&self, _cx: &App) -> Option<SharedString> {
        Some("Request".into())
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child("Request")
    }

    fn title_bar(&self, _cx: &App) -> bool {
        false
    }

    /// The pane draws its own tab strip flush to the top edge; the group's
    /// padding would push it down and leave a seam.
    fn inner_padding(&self, _cx: &App) -> bool {
        false
    }

    /// The pane's own items in the dock's ellipsis menu.
    ///
    /// These are the tab verbs that do not need a tab to be clicked on, plus
    /// the two cURL commands. Each one reuses the event or action the strip
    /// and the toolbar already dispatch, so a menu entry cannot drift from the
    /// control it mirrors.
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let panel = cx.entity();
        let target = menu_tab(&self.tabs, self.active_tab);

        let save_as = panel.clone();
        let mut menu = menu.item(
            PopupMenuItem::new("Save as\u{2026}")
                .icon(IconName::FileText)
                .disabled(target.is_none())
                .on_click(move |_, _, cx| {
                    save_as.update(cx, |_, cx| cx.emit(Event::SaveAs));
                }),
        );

        let duplicate = panel.clone();
        menu = menu.item(
            PopupMenuItem::new("Duplicate tab")
                .icon(IconName::Copy)
                .disabled(target.is_none())
                .on_click(move |_, _, cx| {
                    if let Some(id) = target {
                        duplicate.update(cx, |_, cx| {
                            cx.emit(Event::TabAction(TabAction::Duplicate(id)))
                        });
                    }
                }),
        );

        // Direct calls rather than dispatched actions: a menu click does not
        // focus the pane, so an action would be routed from wherever focus
        // happens to sit. Copy is a workspace intent because the clipboard is
        // a window resource.
        menu = menu.separator();
        let paste = panel.clone();
        menu = menu.item(
            PopupMenuItem::new("Paste cURL").on_click(move |_, window, cx| {
                paste.update(cx, |this, cx| this.paste_curl(window, cx));
            }),
        );
        menu.item(
            PopupMenuItem::new("Copy as cURL").on_click(move |_, _, cx| {
                panel.update(cx, |_, cx| cx.emit(Event::CopyCurl));
            }),
        )
    }
}

impl Focusable for RequestPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl RequestPanel {
    pub(crate) fn new(
        document: Entity<Document>,
        variables: Entity<Variables>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| RequestEditor::new(variables.clone(), window, cx));
        // The URL is an `InputState` of its own, so typing in it notifies that
        // entity rather than the editor. Observe it too, or the document (and
        // therefore Send) would keep the URL from the last tab or row change.
        let url_input = editor.read(cx).url.clone();

        // Editor -> document: the editor's method and URL are what the pane
        // shows, so they become the document's as soon as they change. The
        // observer carries a window, so it also writes out an adoption queued by
        // a handler that had none (a history pick, a tab switch), instead of
        // `render` doing that write.
        let sync_to_document = |this: &mut Self, cx: &mut Context<Self>| {
            // The flush below clears the queue, so this only guards the URL
            // observer, whose notification can arrive while a write is queued.
            if this.editor.read(cx).has_pending() {
                return;
            }
            // Fold once, then move the request in: the body can be large, and a
            // clone here would cost O(body) on every keystroke.
            let request = this.refresh_live(cx);
            this.document.update(cx, |document, cx| {
                document.request = request;
                cx.notify();
            });
            // The strip shows the active tab's live URL, so a keystroke is a
            // reason to redraw the strip too.
            cx.notify();
        };
        cx.observe_in(&editor, window, move |this, _, window, cx| {
            if this.editor.read(cx).has_pending() {
                let editor = this.editor.clone();
                editor.update(cx, |editor, cx| editor.flush_pending(window, cx));
            }
            sync_to_document(this, cx);
        })
        .detach();
        cx.observe(&url_input, move |this, _, cx| sync_to_document(this, cx))
            .detach();

        // The Auth tab asks for a token; the workspace owns the transport, so
        // this pane only forwards the settings it read from the editor.
        cx.subscribe_in(
            &editor,
            window,
            |this, editor, event: &EditorEvent, _, cx| {
                if matches!(event, EditorEvent::FetchToken) {
                    let config = editor.read(cx).oauth_config(cx);
                    let _ = this;
                    cx.emit(Event::FetchToken(config));
                }
            },
        )
        .detach();

        // The URL bar's completion list follows the input's own events: an edit
        // re-scans for a reference under the caret, losing focus closes the
        // list, and Enter (but not Cmd/Ctrl+Enter, which is Send) accepts.
        let completion_variables = variables.clone();
        cx.subscribe_in(
            &url_input,
            window,
            move |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let (text, cursor) = {
                        let input = input.read(cx);
                        (input.text().clone(), input.cursor())
                    };
                    this.url_popup
                        .sync(&text, cursor, completion_variables.read(cx));
                    cx.notify();
                }
                InputEvent::PressEnter { secondary, shift } => {
                    if !secondary && !shift {
                        this.accept_url_completion(None, window, cx);
                    }
                }
                InputEvent::Blur => {
                    this.url_popup.close();
                    cx.notify();
                }
                InputEvent::Focus => {}
            },
        )
        .detach();

        // Document -> editor: a history pick. The comparison lives in
        // `RequestEditor::adopt`, which leaves the input alone when the URL
        // already matches.
        cx.observe(&document, |this, document, cx| {
            let (method, url, pending) = {
                let document = document.read(cx);
                (
                    document.request.method,
                    document.request.url.clone(),
                    document.is_pending(),
                )
            };
            // Only adopt when the document actually differs from the editor,
            // so the editor's own write-through does not trigger a second frame.
            let stale = {
                let editor = this.editor.read(cx);
                editor.url_value(cx) != url || editor.method() != method
            };
            // The Send button is not a request field: `begin_send` and the
            // resolution touch `pending` alone, so `stale` stays false while the
            // row still says "Sending". Ask for that frame here for the same
            // reason the response pane keys its redraw on the send state.
            let repaint = pending != this.pending_painted;
            this.pending_painted = pending;
            if repaint {
                log::debug!(
                    target: "restbook_app::perf",
                    "request pane send state change: pending={pending}",
                );
            }
            if !stale && !repaint {
                return;
            }
            if stale {
                this.editor
                    .update(cx, |editor, cx| editor.adopt(method, &url, cx));
            }
            // The adopted URL lands in the input on the next render.
            cx.notify();
        })
        .detach();

        // A new environment or active selection changes which names resolve
        // and therefore whether Send is usable.
        cx.observe(&variables, |this, _, cx| {
            this.refresh_live(cx);
            cx.notify();
        })
        .detach();

        // Seed the cached derived state once; the observers above keep it in
        // step from here on.
        let initial = editor.read(cx).request(cx);
        let live_stamp = RequestStamp::of(&initial);
        let live_unresolved: Vec<String> = {
            let scoped = variables.read(cx).scoped(&initial);
            references(&initial)
                .into_iter()
                .filter(|name| scoped.lookup(name).is_none())
                .collect()
        };
        // Seeded from the document, or a pane built while a send is running
        // would draw "Send" until the request resolved.
        let pending_painted = document.read(cx).is_pending();

        Self {
            palette: Palette::new(cx),
            metrics: Metrics::current(cx),
            document,
            editor,
            variables,
            tabs: Vec::new(),
            active_tab: 0,
            live_stamp,
            live_unresolved,
            pending_painted,
            url_popup: Autocomplete::default(),
            tabs_scroll: ScrollHandle::new(),
            editor_tabs_scroll: ScrollHandle::new(),
            url_row_width: Rc::new(Cell::new(f32::INFINITY)),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Rebuild the palette after a theme or zoom change, and pass the cue to the
    /// editor it owns, which caches its own palette.
    pub(crate) fn refresh_theme(&mut self, cx: &mut App) {
        self.palette = Palette::new(cx);
        self.metrics = Metrics::current(cx);
        self.editor
            .update(cx, |editor, cx| editor.refresh_theme(cx));
    }

    /// Replace the open-tab entries. Called by the workspace after any change
    /// to its tab list, so this pane never has to guess what is open.
    pub(crate) fn set_tabs(&mut self, tabs: Vec<TabEntry>, active: usize, cx: &mut Context<Self>) {
        // `TabEntry` compares by value, so a new field in the workspace's
        // projection cannot silently stop reaching the strip.
        if self.active_tab == active && self.tabs == tabs {
            return;
        }
        self.tabs = tabs;
        self.active_tab = active;
        // The strip may be wider than the pane; the tab the user just selected
        // (or opened) is the one they expect to see, so bring it into view.
        if self.active_tab < self.tabs.len() {
            self.tabs_scroll.scroll_to_item(self.active_tab);
        }
        cx.notify();
    }

    /// Whether an adopted request is still queued for the editor's next frame.
    ///
    /// While one is queued the widgets show the *previous* request, so a caller
    /// that folds them back into a tab (a tab switch, a draft write) would undo
    /// the adoption it is about to make.
    pub(crate) fn has_pending_adoption(&self, cx: &App) -> bool {
        self.editor.read(cx).has_pending()
    }

    /// Adopt a stored request from history. The editor queues the write, and
    /// its window-aware observer applies it outside render.
    pub(crate) fn adopt_request(&mut self, request: RequestDef, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.adopt_request(request);
            cx.notify();
        });
    }

    /// The editor's state folded into a request, live. The workspace calls
    /// this right before sending instead of trusting a snapshot in the
    /// document: some inputs (the raw body, the auth fields) notify their own
    /// state, not the editor, so a copied request can be one keystroke stale.
    pub(crate) fn current_request(&self, cx: &App) -> RequestDef {
        self.editor.read(cx).request(cx)
    }

    /// The editor entity, so the workspace can push an OAuth token into the
    /// Auth tab after a fetch. The pane still owns the widgets.
    pub(crate) fn editor(&self) -> Entity<RequestEditor> {
        self.editor.clone()
    }

    /// Focus the URL field. Used by the `secondary-l` binding and the command
    /// palette, both of which can fire while another pane owns focus.
    pub(crate) fn focus_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.editor.read(cx).url.clone();
        let handle = url.read(cx).focus_handle(cx);
        handle.focus(window, cx);
    }

    /// Accept a row from the URL completion list. `index` names the clicked
    /// row; `None` takes the highlighted one, which is what Enter and Tab do.
    ///
    /// The write is deferred: replacing the text emits `InputEvent::Change`,
    /// which comes straight back to the subscription that opened the list, and
    /// an entity cannot be updated while it is being updated.
    fn accept_url_completion(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = index {
            self.url_popup.select(index);
        }
        let Some((span, insert)) = self.url_popup.accept() else {
            return;
        };
        let url = self.editor.read(cx).url.clone();
        cx.notify();
        window.defer(cx, move |window, cx| {
            url.update(cx, |input, cx| {
                input.set_selected_range(span, cx);
                input.replace(insert, window, cx);
            });
        });
    }

    /// Route a key to the URL completion list while it is open.
    ///
    /// A single-line `InputState` has no Up/Down handler and lets Escape propagate,
    /// so those keys arrive as plain key-down events (Ctrl+N / Ctrl+P here too). Tab
    /// accepts and stops the event so `InputState`'s `tab` → `IndentInline` binding
    /// cannot move the caret; Enter accepts when `PressEnter` did not.
    fn url_popup_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.url_popup.is_open() {
            return;
        }
        // Only while the URL bar owns the caret: elsewhere the same keys keep
        // doing what they always did.
        let focused = self
            .editor
            .read(cx)
            .url
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if !focused {
            return;
        }

        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.platform || modifiers.alt {
            return;
        }

        let key = keystroke.key.as_str();
        let next = matches!((key, modifiers.control), ("down", false) | ("n", true));
        let previous = matches!((key, modifiers.control), ("up", false) | ("p", true));

        if next {
            self.url_popup.move_selection(1);
            cx.notify();
        } else if previous {
            self.url_popup.move_selection(-1);
            cx.notify();
        } else if key == "escape" {
            self.url_popup.close();
            cx.notify();
        } else if key == "enter" && !modifiers.control && !modifiers.shift {
            self.accept_url_completion(None, window, cx);
        } else if key == "tab" && !modifiers.control && !modifiers.shift {
            self.accept_url_completion(None, window, cx);
            // `InputState` binds `tab` to `IndentInline`; without this the
            // accept would be followed by the input indenting its own text.
            cx.stop_propagation();
        }
    }

    /// Fold the live request once and refresh the two values render needs: its
    /// stamp (for the unsaved marker) and its unresolved names (for Send).
    /// Cached because the strip and URL row rebuild every repaint, and folding a
    /// large raw body there would be O(body). Returned so it can move into the
    /// document without a clone.
    fn refresh_live(&mut self, cx: &App) -> RequestDef {
        let request = self.editor.read(cx).request(cx);
        let stamp = RequestStamp::of(&request);
        let unresolved: Vec<String> = {
            let scoped = self.variables.read(cx).scoped(&request);
            references(&request)
                .into_iter()
                .filter(|name| scoped.lookup(name).is_none())
                .collect()
        };
        self.live_stamp = stamp;
        self.live_unresolved = unresolved;
        request
    }

    /// The open requests, as the strip draws them: the key to retain state
    /// against, the method to color, the name to show, whether this is the tab
    /// on screen, and whether it has been edited since it was saved. The flag is
    /// read from the cached live stamp, so it follows every keystroke without a
    /// fold during render.
    fn tab_entries(&self, cx: &App) -> Vec<StripTab> {
        // Every tab was closed: the strip is just the new-tab button and the
        // editor shows an empty state, so there is nothing to label.
        if self.tabs.is_empty() {
            return Vec::new();
        }

        // The observer keeps the document's request in step with the editor
        // before the next render, so the active tab's live method and label
        // can be read from it instead of folding the request again.
        let document = self.document.read(cx);
        let live_method = document.request.method;
        let live_label = tab_label(&document.request);
        let live_url: SharedString = document.request.url.clone().into();

        self.tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let active = index == self.active_tab;
                // The active tab is marked against the cached live stamp; the
                // others against the snapshot the workspace handed over.
                let current = if active {
                    &self.live_stamp
                } else {
                    &tab.current
                };
                StripTab {
                    id: tab.id,
                    key: tab_key(tab),
                    method: if active { live_method } else { tab.method },
                    label: if active {
                        live_label.clone()
                    } else {
                        tab.label.clone()
                    },
                    url: if active {
                        live_url.clone()
                    } else {
                        tab.url.clone()
                    },
                    active,
                    unsaved: is_unsaved(tab.saved.as_deref(), current),
                }
            })
            .collect()
    }

    /// The tab context menu. One builder so a future ellipsis button and the
    /// right-click path cannot disagree about what a tab can do; every item names
    /// the tab by its stable id and leaves the policy to the workspace.
    fn tab_menu(
        panel: Entity<RequestPanel>,
        id: TabId,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        move |mut menu, _, _| {
            let action = |menu: PopupMenu, label: &'static str, action: TabAction| {
                let panel = panel.clone();
                menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    panel.update(cx, |_, cx| cx.emit(Event::TabAction(action)));
                }))
            };
            menu = action(menu, "Duplicate tab", TabAction::Duplicate(id));
            menu = action(menu, "Move left", TabAction::MoveLeft(id));
            menu = action(menu, "Move right", TabAction::MoveRight(id));
            // Saving from a background tab first brings it forward, so the
            // dialog names the request the user right-clicked, not the one that
            // happened to be on screen.
            let save_as = panel.clone();
            menu = menu.item(
                PopupMenuItem::new("Save as\u{2026}").on_click(move |_, _, cx| {
                    save_as.update(cx, |_, cx| {
                        cx.emit(Event::SelectTab(id));
                        cx.emit(Event::SaveAs);
                    });
                }),
            );
            menu = menu.separator();
            menu = action(menu, "Close tab", TabAction::Close(id));
            menu = action(menu, "Close other tabs", TabAction::CloseOthers(id));
            menu = action(menu, "Close tabs to the right", TabAction::CloseToRight(id));
            action(menu, "Close all tabs", TabAction::CloseAll)
        }
    }

    /// One tab: the verb in its method color, the name, its unsaved dot, and a
    /// close button. The cell owns the right-click menu, a middle click closes it,
    /// and its tooltip carries the full method and URL the truncated label loses.
    fn render_tab_strip(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette.clone();
        let entries = self.tab_entries(cx);
        // Every open tab is closable now, including the last one.
        let closable = !entries.is_empty();
        let badge_size = self.metrics.font_badge;

        // The overflow menu's rows, captured before the entries are consumed by the
        // strip. Selecting one reuses the same event a tab click sends.
        let overflow: Vec<(TabId, SharedString, Method, bool)> = entries
            .iter()
            .map(|tab| (tab.id, tab.label.clone(), tab.method, tab.active))
            .collect();

        let tabs = entries
            .into_iter()
            .map(|entry| {
                let StripTab {
                    id,
                    key,
                    method,
                    label,
                    url,
                    active,
                    unsaved,
                } = entry;

                let select = cx.listener(move |_, _, _, cx| {
                    // The strip's own click handler opens a tab on a double click;
                    // a click on a tab must not also count as one on empty strip.
                    cx.stop_propagation();
                    cx.emit(Event::SelectTab(id));
                });
                let close = cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(Event::CloseTab(id));
                });
                let middle = cx.listener(
                    move |_: &mut RequestPanel,
                          event: &ClickEvent,
                          _: &mut Window,
                          cx: &mut Context<RequestPanel>| {
                        if event.is_middle_click() {
                            cx.stop_propagation();
                            cx.emit(Event::CloseTab(id));
                        }
                    },
                );
                let menu = Self::tab_menu(cx.entity(), id);
                let tip = SharedString::from(format!("{method} {url}"));

                let text_color = if active {
                    palette.foreground
                } else {
                    palette.muted
                };

                // Zaku's strip is segmented, not pilled: each tab is a cell
                // with a hairline on its right, and only the selected one is
                // filled. That keeps a row of six tabs readable at 13px.
                h_flex()
                    .id(format!("request-tab:{key}"))
                    .h_full()
                    // The strip scrolls instead of squeezing tabs down to
                    // nothing: each tab keeps its readable width and the row
                    // overflows into a scrollport when there are many. The cap is
                    // a span of gutters so it tracks zoom.
                    .flex_none()
                    .max_w(px(self.metrics.gutter * 18.))
                    .items_center()
                    .gap(px(self.metrics.space_sm))
                    .pl(px(self.metrics.space_md))
                    .pr(px(self.metrics.space_sm))
                    .border_r_1()
                    .border_color(palette.border)
                    .cursor_pointer()
                    .when(active, |style| style.bg(palette.tab_active))
                    .when(!active, |style| {
                        style.hover(|style| style.bg(palette.hover))
                    })
                    // The label truncates, so the full method and address ride in
                    // the tooltip: the one thing you want when a tab is cut off.
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(select)
                    .on_aux_click(middle)
                    .child(
                        // The method wears its own color and keeps its own width,
                        // the same badge the sidebar tree draws: same size, same
                        // weight. A tab and the tree row for the same request
                        // should not disagree on how big `GET` is.
                        div()
                            .flex_none()
                            .text_size(px(badge_size))
                            .font_weight(FontWeight::BOLD)
                            .text_color(palette.method(method))
                            .child(method.as_str()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(text_color)
                            .child(label),
                    )
                    // The unsaved marker is a glyph of its own, not a replacement
                    // for the close button: the tab can still be closed, and the
                    // dot is what says the close would ask first.
                    .when(unsaved, |style| {
                        style.child(
                            div()
                                .flex_none()
                                .size(px(6.))
                                .rounded_full()
                                .bg(palette.warning),
                        )
                    })
                    .when(closable, |style| {
                        style.child(
                            // A real `Button`: the hand-built `div` had no
                            // accessible name and no keyboard path. The inner
                            // click is stopped so closing does not also select
                            // the tab it is closing.
                            Button::new(format!("request-tab-close:{key}"))
                                .icon(IconName::Close)
                                .ghost()
                                .compact()
                                .tooltip("Close tab")
                                .on_click(close),
                        )
                    })
                    // One menu for the right-click path; `ContextMenuExt` adds it
                    // as an absolutely positioned child, so it goes last.
                    .context_menu(menu)
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let overflow_button = (!overflow.is_empty()).then(|| {
            let panel = cx.entity();
            Button::new("request-tab-overflow")
                .icon(IconName::ChevronDown)
                .ghost()
                .compact()
                .mr(px(self.metrics.space_xs))
                .tooltip("Open tabs")
                .dropdown_menu(move |mut menu: PopupMenu, _, _| {
                    for (id, label, method, active) in overflow.clone() {
                        let panel = panel.clone();
                        menu = menu.item(
                            PopupMenuItem::new(format!("{method} {label}"))
                                .checked(active)
                                .on_click(move |_, _, cx| {
                                    panel.update(cx, |_, cx| cx.emit(Event::SelectTab(id)));
                                }),
                        );
                    }
                    menu
                })
        });

        h_flex()
            .h(px(self.metrics.header_height))
            .flex_none()
            .items_center()
            .bg(palette.tab_bar)
            .border_b_1()
            .border_color(palette.border)
            // The tabs live in a horizontal scrollport so a long row can be
            // reached instead of overflowing past the pane. It takes the free
            // width and leaves the + pinned outside it, so the button stays
            // clickable no matter how far the strip is scrolled.
            .child(
                h_flex()
                    .id("request-tabs-scroll")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .pl(px(self.metrics.space_xs))
                    .pr(px(self.metrics.space_xs))
                    // Stays on the hidden variant: `track_scroll` feeds the shared
                    // handle `scroll_to_item` drives, and `overflow_x_scrollbar`
                    // wraps the element in a scroll area that owns its own handle.
                    .overflow_x_scroll()
                    // The handle lives on the scrollport and its direct children
                    // are the tabs, so `scroll_to_item(index)` lines up with the
                    // tab index the workspace tracks.
                    .track_scroll(&self.tabs_scroll)
                    // Double-clicking the empty run of strip opens a tab, the way
                    // every browser's tab bar does. A tab's own click stops
                    // propagation, so this only fires on the blank area.
                    .on_double_click(cx.listener(|_, _, _, cx| cx.emit(Event::NewTab)))
                    .children(tabs),
            )
            .children(overflow_button)
            .child(
                Button::new("paste-curl-tab")
                    .icon(IconName::ClipboardPaste)
                    .ghost()
                    .compact()
                    .mr(px(self.metrics.space_xs))
                    .tooltip(with_shortcut("Paste cURL", shortcut(&PasteCurl, window)))
                    .on_click(cx.listener(|this, _, window, cx| this.paste_curl(window, cx))),
            )
            .child(
                Button::new("new-request-tab")
                    .icon(IconName::Plus)
                    .ghost()
                    .compact()
                    .mr(px(self.metrics.space_xs))
                    .tooltip(with_shortcut("New request tab", shortcut(&NewTab, window)))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::NewTab))),
            )
            .into_any_element()
    }

    /// What the pane shows when every tab was closed: a line naming the state
    /// and the two actions that leave it without opening a blank tab first.
    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(self.metrics.space_md))
            .child(icons::quiet(IconName::Plus, 28., &self.palette))
            .child(
                div()
                    .text_sm()
                    .text_color(self.palette.muted)
                    .child("No request open"),
            )
            .child(
                h_flex()
                    .gap(px(self.metrics.space_sm))
                    .child(
                        Button::new("new-request-empty")
                            .label("New request")
                            .icon(IconName::Plus)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::NewTab))),
                    )
                    .child(
                        Button::new("import-file-empty")
                            .label("Import…")
                            .icon(IconName::Upload)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Import))),
                    ),
            )
            .into_any_element()
    }

    /// The verb as one dropdown, colored by method, instead of a strip of
    /// seven buttons: the row then reads `GET ▾ | url | Send`, which is the
    /// shape every rest client uses.
    fn render_method(&self, cx: &mut Context<Self>) -> AnyElement {
        let method = self.editor.read(cx).method();
        let editor = self.editor.clone();
        let menu = move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            for option in RequestEditor::methods() {
                let editor = editor.clone();
                menu = menu.item(
                    PopupMenuItem::new(option.as_str())
                        .checked(option == method)
                        .on_click(move |_, _, cx| {
                            editor.update(cx, |editor, cx| {
                                editor.set_method(option);
                                cx.notify();
                            });
                        }),
                );
            }
            menu
        };

        Button::new("request-method")
            .label(method.as_str())
            .ghost()
            .compact()
            .min_w(px(self.metrics.gutter * 6.))
            .text_color(self.palette.method(method))
            .dropdown_caret(true)
            .dropdown_menu(menu)
            .into_any_element()
    }

    /// `[GET ⌄ | url]  Send`. The verb and the URL share one bordered group,
    /// so the row reads as one control rather than three loose ones.
    fn render_url_row(
        &self,
        window: &mut Window,
        unresolved: &[String],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = self.palette.clone();
        let document = self.document.read(cx);
        let pending = document.is_pending();
        let sendable = document.is_sendable() && unresolved.is_empty();

        // A dock pane can be dragged down to the dock's floor, well under the
        // width this row needs. The field and Send are the task; the Save verb
        // also lives in the pane menu, so it is the one that gives.
        let breakpoint = NARROW_URL_ROW * crate::theme::zoom(cx);
        let narrow = self.url_row_width.get() < breakpoint;

        let send_tooltip = if unresolved.is_empty() {
            with_shortcut("Send", shortcut(&SendRequest, window))
        } else {
            format!("Unresolved variables: {}", unresolved.join(", "))
        };

        let mut row = h_flex()
            .items_center()
            .gap(px(self.metrics.space_sm))
            .px(px(self.metrics.space_md))
            .py(px(self.metrics.space_sm))
            .border_b_1()
            .border_color(palette.border)
            .child({
                // The group is the anchor for the completion list, which is a
                // child so it can hang from the field's own bottom edge.
                let mut group = div().relative().flex_1().min_w_0().child(
                    h_flex()
                        .h(px(self.metrics.header_height))
                        .min_w_0()
                        .items_center()
                        .rounded_md()
                        .border_1()
                        .border_color(palette.input_border)
                        .child(
                            div()
                                .h_full()
                                .flex()
                                .items_center()
                                .px(px(self.metrics.space_xs))
                                .border_r_1()
                                .border_color(palette.input_border)
                                .child(self.render_method(cx)),
                        )
                        .child(
                            Input::new(&self.editor.read(cx).url)
                                .id("url-bar")
                                .flex_1()
                                // Without this the field keeps its content
                                // width and paints past the group's edge,
                                // over the verbs beside it.
                                .min_w_0()
                                .appearance(false)
                                .bordered(false)
                                .cleanable(true)
                                .font_family(palette.mono_font.clone()),
                        ),
                );
                if self.url_popup.is_open() {
                    group = group.child(self.render_url_popup(cx));
                }
                group
            });

        if pending {
            row = row.child(
                Button::new("cancel")
                    .label("Cancel")
                    .danger()
                    .tooltip("Stop waiting for the response")
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(Event::Cancel);
                    })),
            );
        }

        // Save sits beside Send as the second most-used verb. When the active
        // tab is bound to a collection node the button overwrites that node
        // ("Save"); a draft that belongs nowhere yet creates one ("Save As").
        let bound = self
            .tabs
            .get(self.active_tab)
            .and_then(|tab| tab.node.as_deref())
            .is_some();
        let (save_label, save_tip) = if bound {
            (
                "Save",
                with_shortcut("Update the saved request", shortcut(&SaveRequest, window)),
            )
        } else {
            (
                "Save As",
                with_shortcut(
                    "Name this request and save it to a collection",
                    shortcut(&SaveRequest, window),
                ),
            )
        };
        let save = if narrow {
            Button::new("save-request")
                .icon(if bound {
                    IconName::Save
                } else {
                    IconName::SavePlus
                })
                .ghost()
                .compact()
        } else {
            Button::new("save-request")
                .label(save_label)
                .ghost()
                .compact()
        };
        row = row.child(
            save.flex_none()
                .tooltip(save_tip)
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.emit(Event::Save);
                })),
        );

        let send = Button::new("send")
            .label(if pending { "Sending" } else { "Send" })
            .primary()
            .loading(pending)
            .disabled(!sendable)
            .tooltip(send_tooltip)
            .on_click(cx.listener(|_, _, _, cx| {
                cx.emit(Event::Send);
            }));
        // The wide Send keeps a fixed footprint so the row does not twitch as
        // it goes from Send to Sending; at a narrow pane that room belongs to
        // the field instead.
        let send = if narrow {
            send
        } else {
            send.min_w(px(self.metrics.gutter * 7.))
        };

        let this = cx.entity();
        let measured = self.url_row_width.clone();
        row.child(send.flex_none())
            // A pane's own width is only known after layout, so the row records
            // it here and re-renders once if it crossed the breakpoint. The row
            // is full-width either way, so the measurement cannot oscillate.
            .on_prepaint(move |bounds, _, cx| {
                let width = f32::from(bounds.size.width);
                let previous = measured.replace(width);
                if (previous < breakpoint) != (width < breakpoint) {
                    this.update(cx, |_, cx| cx.notify());
                }
            })
            .into_any_element()
    }

    /// The `{{…` list hanging under the URL field.
    ///
    /// It is `deferred` so it paints after the editor below it: its layout
    /// stays inside the URL row, but its ink is lifted above every later
    /// sibling, which is what a dropdown over a scrolling body needs.
    fn render_url_popup(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some((rows, selected)) = self.url_popup.visible() else {
            return div().into_any_element();
        };
        let palette = self.palette.clone();

        let mut list = v_flex().gap(px(self.metrics.space_xs));
        for (index, row) in rows.iter().enumerate() {
            list = list.child(
                h_flex()
                    .id(format!("url-completion:{}", row.label))
                    .items_center()
                    .gap(px(self.metrics.space_md))
                    .px(px(self.metrics.space_sm))
                    .py(px(self.metrics.space_xs))
                    .rounded_sm()
                    .text_sm()
                    .cursor_pointer()
                    .when(index == selected, |this| this.bg(palette.selection))
                    .hover(|this| this.bg(palette.hover))
                    .child(div().flex_none().child(row.label.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(palette.muted)
                            .child(row.detail.clone()),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.accept_url_completion(Some(index), window, cx);
                        }),
                    ),
            );
        }

        deferred(
            div()
                .absolute()
                .top_full()
                .left_0()
                .right_0()
                .pt(px(self.metrics.space_xs))
                .child(
                    div()
                        .id("url-completions")
                        .occlude()
                        .max_h(px(240.))
                        .overflow_y_scrollbar()
                        .rounded_md()
                        .border_1()
                        .border_color(palette.border)
                        .bg(palette.background)
                        .p(px(self.metrics.space_xs))
                        .child(list),
                ),
        )
        .into_any_element()
    }

    /// Rebuild the request from a cURL command on the clipboard. The parsed
    /// value goes through [`Self::adopt_request`], the same queued-write path a
    /// history pick uses, so every tab is written with a window in hand.
    pub(crate) fn paste_curl(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // A non-text clipboard (an image, a file list) is not a cURL command.
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };

        match restbook_core::import::curl::parse(&text) {
            Ok(request) => {
                self.adopt_request(request, cx);
                cx.emit(Event::Imported);
            }
            Err(message) => {
                cx.emit(Event::Notice(Notice::Failed(message.into())));
            }
        }
    }

    /// The editor tabs, then the body of the selected one. The tabs carry the
    /// row count of their table, the way parameters and headers are usually read
    /// at a glance. Import is an action (Cmd/Ctrl+Shift+V), not a button: used
    /// once per request, it belongs behind the overflow menu, not beside Send.
    fn render_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette.clone();
        let (tab, labels) = {
            let editor = self.editor.read(cx);
            (
                editor.tab,
                EditorTab::ALL.map(|each| editor.tab_label(each)),
            )
        };

        let tabs = TabBar::new("editor-tabs")
            .with_variant(TabVariant::Underline)
            .underline()
            // A narrow pane cannot hold all five tabs, so the strip scrolls and
            // offers the rest from its own overflow menu rather than painting the
            // tail over the band or clipping it at the pane's edge.
            .track_scroll(&self.editor_tabs_scroll)
            .menu(true)
            .selected_index(
                EditorTab::ALL
                    .iter()
                    .position(|each| *each == tab)
                    .unwrap_or(0),
            )
            .on_click(cx.listener(|this, index, _, cx| {
                if let Some(tab) = EditorTab::ALL.get(*index) {
                    this.editor.update(cx, |editor, cx| {
                        editor.tab = *tab;
                        cx.notify();
                    });
                }
            }))
            .children(labels);

        let editor = self.editor.clone();
        let body = match tab {
            EditorTab::Params => editor.update(cx, |editor, cx| editor.render_params_tab(cx)),
            EditorTab::Headers => editor.update(cx, |editor, cx| editor.render_headers_tab(cx)),
            EditorTab::Body => editor.update(cx, |editor, cx| editor.render_body_tab(cx)),
            EditorTab::Auth => editor.update(cx, |editor, cx| editor.render_auth_tab(cx)),
            EditorTab::Settings => editor.update(cx, |editor, cx| editor.render_settings_tab(cx)),
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                // The tab strip is a full-width band, not just the tabs: a
                // recessed surface behind the labels is what separates the
                // URL bar above from the editor below. The overflow button
                // lives in it too, so the band has one right edge.
                h_flex()
                    .items_center()
                    .h(px(self.metrics.header_height))
                    .flex_none()
                    .bg(palette.segmented)
                    .px(px(self.metrics.space_md))
                    .gap(px(self.metrics.space_xs))
                    // The strip takes the free width and may shrink: `TabBar` only
                    // scrolls once it has a bounded width, so this wrapper is what
                    // keeps the overflow inside the scrollport.
                    .child(div().flex_1().min_w_0().child(tabs)),
            )
            .child(
                div()
                    .id("editor-tab-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .px(px(self.metrics.space_md))
                    .py(px(self.metrics.space_sm))
                    .bg(palette.background)
                    .child(body),
            )
            .into_any_element()
    }
}

impl Render for RequestPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette.clone();

        // Read the cached derived state; the observers keep it in step with
        // the editor, so render never folds the request.
        let unresolved = &self.live_unresolved;
        // Every tab can be closed now, so the body below has two shapes: the
        // editor for an open request, an empty state when there is none.
        let has_tab = !self.tabs.is_empty();

        v_flex()
            .size_full()
            .bg(palette.background)
            .track_focus(&self.focus_handle)
            // `secondary-enter` sends from anywhere in the pane, including
            // while the URL field has focus: the action bubbles to this div.
            .on_action(cx.listener(|this, _: &SendRequest, _, cx| {
                if this.document.read(cx).is_sendable() && this.live_unresolved.is_empty() {
                    cx.emit(Event::Send);
                }
            }))
            .on_action(cx.listener(|_, _: &SaveRequest, _, cx| {
                cx.emit(Event::Save);
            }))
            .on_action(cx.listener(|this, _: &FocusUrl, window, cx| {
                this.focus_url(window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.url_popup_key(event, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PasteCurl, window, cx| {
                this.paste_curl(window, cx);
            }))
            .child(self.render_tab_strip(window, cx))
            .children(has_tab.then(|| self.render_url_row(window, unresolved, cx)))
            .children((has_tab && !unresolved.is_empty()).then(|| {
                h_flex()
                    .px(px(self.metrics.space_md))
                    .py(px(self.metrics.space_xs))
                    .gap(px(self.metrics.space_sm))
                    .items_center()
                    .bg(palette.danger.opacity(0.12))
                    .border_b_1()
                    .border_color(palette.danger.opacity(0.35))
                    .child(
                        icons::sized(IconName::CircleAlert, icons::SMALL)
                            .text_color(palette.danger),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.danger)
                            .child(format!("Unresolved variables: {}", unresolved.join(", "))),
                    )
            }))
            .children((!has_tab).then(|| self.render_empty(cx)))
            .children(has_tab.then(|| self.render_editor(cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::{is_unsaved, menu_tab, tab_label};
    use crate::workspace::TabEntry;
    use restbook_core::{Method, RequestDef};

    #[test]
    fn a_tab_that_was_never_saved_is_never_marked() {
        assert!(!is_unsaved(None, "anything"));
    }

    #[test]
    fn a_tab_matches_its_baseline_until_it_is_edited() {
        assert!(!is_unsaved(Some("same"), "same"));
        assert!(is_unsaved(Some("before"), "after"));
    }

    #[test]
    fn a_tab_without_a_url_is_named_for_being_new() {
        assert_eq!(
            &*tab_label(&RequestDef::new(Method::Get, "")),
            "(new request)"
        );
    }

    #[test]
    fn a_tab_label_drops_the_scheme() {
        let request = RequestDef::new(Method::Get, "https://api.test/v1/items");
        assert_eq!(&*tab_label(&request), "api.test/v1/items");
    }

    /// The pane menu names the tab on screen, and closing every tab leaves it
    /// with no target rather than a stale index.
    #[test]
    fn the_pane_menu_acts_on_the_active_tab() {
        let tabs = [TabEntry {
            id: 7,
            method: Method::Get,
            label: "one".into(),
            url: "".into(),
            current: String::new(),
            saved: None,
            node: None,
            draft: None,
        }];
        assert_eq!(menu_tab(&tabs, 0), Some(7));
        assert_eq!(menu_tab(&tabs, 1), None);
        assert_eq!(menu_tab(&[], 0), None);
    }
}

#[cfg(test)]
mod enter_tests {
    // Named imports only: `use super::*` would drag in GPUI's `test` macro and
    // shadow the built-in `#[test]`.
    use std::sync::Arc;

    use gpui_kit::{AppContext as _, TestAppContext};
    use restbook_core::{Body, Method};
    use restbook_store::MemoryStore;

    use crate::panels::editor::{BodyKind, EditorTab};

    use super::{Document, RequestPanel, TabEntry, Variables};

    /// One strip entry for a test that opens a window: a fresh GET tab with no
    /// persistence binding.
    fn blank_entry() -> TabEntry {
        TabEntry {
            id: 1,
            method: Method::Get,
            label: "(new request)".into(),
            url: "".into(),
            current: String::new(),
            saved: None,
            node: None,
            draft: None,
        }
    }

    /// Press Enter the way the platform delivers it: a `KeyDownEvent` with no
    /// text. The simulated IME would supply `"\n"`, which is not what a real
    /// keyboard sends for a control key.
    fn press_enter(cx: &mut gpui_kit::VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            window.dispatch_event(
                gpui_kit::PlatformInput::KeyDown(gpui_kit::KeyDownEvent {
                    keystroke: gpui_kit::Keystroke {
                        modifiers: gpui_kit::Modifiers::default(),
                        key: "enter".to_owned(),
                        key_char: None,
                    },
                    is_held: false,
                    prefer_character_input: false,
                }),
                cx,
            );
        });
        cx.run_until_parked();
    }

    /// Enter on the URL bar has to accept the highlighted row. No environment
    /// is configured, so the first row is the first generator and the accepted
    /// URL is exactly `{{$uuid}}`.
    #[gpui_kit::test]
    fn enter_accepts_a_url_completion(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let document = cx.update(|cx| cx.new(|_| Document::new()));
        let variables =
            cx.update(|cx| cx.new(|cx| Variables::new(Arc::new(MemoryStore::new()), cx)));

        let (panel, cx) = cx.add_window_view(|window, cx| {
            RequestPanel::new(document.clone(), variables.clone(), window, cx)
        });
        panel.update(cx, |panel, cx| {
            panel.set_tabs(vec![blank_entry()], 0, cx);
        });
        cx.update(|window, cx| {
            panel.update(cx, |panel, cx| panel.focus_url(window, cx));
        });
        cx.simulate_input("{{");
        // Dispatch the action directly: the simulated IME would send text for
        // Enter too, and the app's own press helper is behind a feature this
        // crate cannot enable.
        cx.dispatch_action(gpui_kit::base::input::Enter {
            secondary: false,
            shift: false,
        });

        let url = cx.update(|_, cx| panel.read(cx).editor.read(cx).url_value(cx));
        assert_eq!(url, "{{$uuid}}");
    }

    /// Enter in the real Body tab, with the request panel around it.
    ///
    /// The raw body is the kit's `Editor`, so the accept runs through the popover's
    /// action path or the editor's keystroke interceptor, which keeps the multi-line
    /// binding from inserting a newline first. Rendering the whole pane (not a bare
    /// editor) catches a missing interceptor; the body text is the assertion.
    #[gpui_kit::test]
    fn a_real_enter_key_accepts_a_generator_in_the_json_body(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let document = cx.update(|cx| cx.new(|_| Document::new()));
        let variables =
            cx.update(|cx| cx.new(|cx| Variables::new(Arc::new(MemoryStore::new()), cx)));

        let (panel, cx) = cx.add_window_view(|window, cx| {
            RequestPanel::new(document.clone(), variables.clone(), window, cx)
        });
        panel.update(cx, |panel, cx| {
            panel.set_tabs(vec![blank_entry()], 0, cx);
            panel.editor.update(cx, |editor, cx| {
                editor.tab = EditorTab::Body;
                editor.body_kind = BodyKind::Raw;
                cx.notify();
            });
        });
        cx.update(|window, cx| {
            panel.update(cx, |panel, cx| {
                let raw = panel.editor.read(cx).raw_text.clone();
                raw.update(cx, |state, cx| state.focus(window, cx));
            });
        });
        cx.simulate_input("{\"token\": \"{{$rand");
        press_enter(cx);

        let body = cx.update(|_, cx| panel.read(cx).current_request(cx).body);
        let Body::Raw { text, .. } = body else {
            panic!("the raw body kind should fold into a raw body, got {body:?}");
        };
        assert_eq!(text, "{\"token\": \"{{$randomInt}}\"}");
    }

    /// The same accept, driven by a real key-down: the URL bar is a plain
    /// `InputState`, so Enter has to reach its `InputEvent::PressEnter`
    /// subscription, not only the action a test dispatches directly.
    #[gpui_kit::test]
    fn a_real_enter_key_accepts_a_url_completion(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let document = cx.update(|cx| cx.new(|_| Document::new()));
        let variables =
            cx.update(|cx| cx.new(|cx| Variables::new(Arc::new(MemoryStore::new()), cx)));

        let (panel, cx) = cx.add_window_view(|window, cx| {
            RequestPanel::new(document.clone(), variables.clone(), window, cx)
        });
        panel.update(cx, |panel, cx| {
            panel.set_tabs(vec![blank_entry()], 0, cx);
        });
        cx.update(|window, cx| {
            panel.update(cx, |panel, cx| panel.focus_url(window, cx));
        });
        cx.simulate_input("{{");
        press_enter(cx);

        let url = cx.update(|_, cx| panel.read(cx).editor.read(cx).url_value(cx));
        assert_eq!(url, "{{$uuid}}");
    }

    /// Press Tab the way a keyboard sends it: no text, so the input's own
    /// `tab` binding is what would see it if the popup did not.
    fn press_tab(cx: &mut gpui_kit::VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            window.dispatch_event(
                gpui_kit::PlatformInput::KeyDown(gpui_kit::KeyDownEvent {
                    keystroke: gpui_kit::Keystroke {
                        modifiers: gpui_kit::Modifiers::default(),
                        key: "tab".to_owned(),
                        key_char: None,
                    },
                    is_held: false,
                    prefer_character_input: false,
                }),
                cx,
            );
        });
        cx.run_until_parked();
    }

    /// U35: Tab takes the highlighted row in the URL bar's list.
    ///
    /// `InputState` binds `tab` to `IndentInline`, so before this the keystroke
    /// moved the caret instead of accepting — and the accepted URL is the
    /// assertion because the two outcomes are different text.
    #[gpui_kit::test]
    fn tab_accepts_a_url_completion(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let document = cx.update(|cx| cx.new(|_| Document::new()));
        let variables =
            cx.update(|cx| cx.new(|cx| Variables::new(Arc::new(MemoryStore::new()), cx)));

        let (panel, cx) = cx.add_window_view(|window, cx| {
            RequestPanel::new(document.clone(), variables.clone(), window, cx)
        });
        panel.update(cx, |panel, cx| {
            panel.set_tabs(vec![blank_entry()], 0, cx);
        });
        cx.update(|window, cx| {
            panel.update(cx, |panel, cx| panel.focus_url(window, cx));
        });
        cx.simulate_input("{{");
        press_tab(cx);

        let url = cx.update(|_, cx| panel.read(cx).editor.read(cx).url_value(cx));
        assert_eq!(url, "{{$uuid}}");
    }
}
