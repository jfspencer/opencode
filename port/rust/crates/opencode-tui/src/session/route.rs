//! The session route (`routes/session/index.tsx` `Session`, `SessionRowView`, `SessionEntryView`): the layout, the
//! transcript scrollbox with sticky follow, tail-first mounting, message navigation, the session commands and
//! keybindings, and the composer area (prompt, permission and form prompts).
//!
//! TODO(port-tui): of Session's providers and features, not here yet: the epilogue (`epilogue.session`), the
//! location context (`currentLocation`), session tabs (scroll anchors and group disclosure are kept per route
//! instance), the composer (subagents and terminals), the go-upsell offer on free-limit failures, queued prompts and
//! their dialog, and the dialogs the commands open (rename, timeline, fork, export) and the clipboard.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::elements::{box_, scrollbox, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::scrollbox::{
    ScrollBarOptions, ScrollUnit, StickyStart,
};
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element, FlexDirection, Justify, MouseEventType, ScrollDirection};
use opencode_tui_sync::connection::Status;
use opencode_tui_sync::data::LoadMoreOptions;
use opencode_tui_sync::json::JsonExt;

use super::anchors::{TimelineAnchors, entry_contains_anchor, group_contains_anchor, group_id, use_entry_anchor};
use super::grouping::{DEFAULT_VERBOSITY, GroupKind, SessionEntry, SessionRow, Verbosity};
use super::group_view::session_group_view;
use super::message_navigation::{find_message_boundary, message_navigation_slack};
use super::messages::{Message, assistant_footer, compaction_queued, session_message_view};
use super::mount_budget::{row_weight, rows_after, rows_before};
use super::parts::{form_prompt, permission_prompt, same_record, session_part_view};
use super::render_context::{DiffWrapMode, MarkdownMode, PendingAction, RenderCtx, ThinkingMode, context};
use super::rows::{Rows, create_session_rows, message_boundary_ids, session_row_id};
use crate::ctx::{Route, tui};
use crate::keymap::{Bind, Command, Enabled, Layer, create_layer};
use crate::prompt::{PromptProps, PromptRef, prompt};
use crate::util::rgba;

/// Exclude temporary bottom space when measuring the real transcript height.
const NAVIGATION_SLACK_ID: &str = "session-navigation-slack";
/// Budgets count rendered entries (see mount_budget); the tail comfortably overfills a tall viewport.
const TRANSCRIPT_TAIL_ROWS: usize = 40;
const TRANSCRIPT_BACKFILL_CHUNK: usize = 60;

/// The props of `<Session>` (SessionFrame passes them).
#[derive(Clone)]
pub struct SessionProps {
    pub scroll_ref: Option<Rc<dyn Fn(Option<Element>)>>,
    pub vertical_tabs_width: Rc<dyn Fn() -> u32>,
    pub prompt_muted: Rc<dyn Fn() -> bool>,
    pub sidebar_visible: Rc<dyn Fn() -> bool>,
    pub on_toggle_sidebar: Rc<dyn Fn()>,
    pub terminals: Rc<dyn Fn() -> bool>,
    pub visible_terminal_id: Rc<dyn Fn() -> Option<String>>,
    pub on_terminal_picker: Option<Rc<dyn Fn(Option<Rc<dyn Fn()>>)>>,
    pub width: Rc<dyn Fn() -> Option<f32>>,
}

impl Default for SessionProps {
    fn default() -> Self {
        SessionProps {
            scroll_ref: None,
            vertical_tabs_width: Rc::new(|| 0),
            prompt_muted: Rc::new(|| false),
            sidebar_visible: Rc::new(|| false),
            on_toggle_sidebar: Rc::new(|| {}),
            terminals: Rc::new(|| false),
            visible_terminal_id: Rc::new(|| None),
            on_terminal_picker: None,
            width: Rc::new(|| None),
        }
    }
}

/// A function in a signal, compared by identity (`firstJump`).
#[derive(Clone)]
struct Callback(Rc<dyn Fn()>);

impl PartialEq for Callback {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

type Continuations = Rc<RefCell<Vec<Box<dyn FnOnce()>>>>;

/// The route's state, shared by its handlers (TS keeps these in the component's closure).
struct State {
    t: crate::ctx::Tui,
    dialog: crate::ui::dialog::DialogContext,
    session_id: String,
    scroll: Cell<Option<Element>>,
    rows: Rows,
    weights: Memo<Vec<usize>>,
    hidden_rows: Signal<Option<usize>>,
    visible_rows_end: Signal<Option<usize>>,
    hidden: Memo<usize>,
    visible_end: Memo<usize>,
    navigation_message: Signal<Option<String>>,
    navigation_slack: Signal<f64>,
    first_jump: Signal<Option<Callback>>,
    away_from_bottom: Signal<bool>,
    ensure_all_rows_pending: RefCell<Option<Continuations>>,
    revealing_older_rows: Cell<bool>,
    revealing_newer_rows: Cell<bool>,
    restored: Cell<bool>,
    away_timer: Cell<Option<u64>>,
    prepend_history: RefCell<Option<super::history::HistoryPrepend>>,
    anchors: TimelineAnchors,
    /// TODO(port-tui): session tabs: the saved anchor and group disclosure live in the tab state.
    scroll_anchor: RefCell<Option<(super::anchors::AnchorTarget, f64)>>,
    group_expanded: Signal<HashMap<String, bool>>,
}

impl State {
    fn scroll(&self) -> Option<Element> {
        self.scroll.get().filter(|s| !s.is_destroyed())
    }

    fn history(&self) -> super::history::HistoryPrepend {
        self.prepend_history.borrow().clone().expect("history prepend")
    }

    /// `afterLayout(continuation)`: after the next frame's layout and before it paints, unless torn down.
    fn after_layout(self: &Rc<Self>, continuation: impl FnOnce() + 'static) {
        let Some(scroll) = self.scroll() else { return };
        scroll.after_layout(move || {
            if !scroll.is_destroyed() {
                continuation()
            }
        });
    }

    /// `pinAbove()`: pin an existing row before publication.
    fn pin_above(self: &Rc<Self>) {
        let Some(scroll) = self.scroll() else { return };
        let viewport = scroll.scroll_viewport();
        if scroll.sticky_scroll() && scroll.scroll_top() >= scroll.scroll_height() - viewport.height {
            return;
        }
        let node = scroll.children().into_iter().find(|n| n.height() > 0.0 && (n.y() as f64 + n.height() as f64) > viewport.y);
        if let Some(node) = node {
            let release = scroll.anchor(node);
            self.after_layout(release);
        }
    }

    fn clear_message_navigation(self: &Rc<Self>) {
        if let Some(pending) = self.ensure_all_rows_pending.borrow().as_ref() {
            pending.borrow_mut().clear();
        }
        self.history().cancel();
        if let Some(jump) = self.first_jump.get_untracked() {
            (jump.0)();
        }
        self.first_jump.set(None);
        self.navigation_slack.set(0.0);
        self.navigation_message.set(None);
    }

    fn reveal_older_rows(self: &Rc<Self>, scroll_by: f64) -> bool {
        let current = self.hidden.get_untracked();
        let Some(scroll) = self.scroll() else { return false };
        if self.revealing_older_rows.get() || scroll.scroll_top() > scroll.scroll_viewport().height {
            return false;
        }
        if current == 0 {
            return self.history().prepend(scroll_by, None);
        }
        self.revealing_older_rows.set(true);
        scroll.set_sticky_scroll(false);
        self.pin_above();
        self.hidden_rows.set(Some(rows_before(&self.weights.get_untracked(), current, TRANSCRIPT_BACKFILL_CHUNK)));
        let this = self.clone();
        self.after_layout(move || {
            scroll.scroll_by(scroll_by, ScrollUnit::Absolute);
            scroll.set_sticky_scroll(this.navigation_message.get_untracked().is_none());
            this.revealing_older_rows.set(false);
        });
        true
    }

    fn reveal_newer_rows(self: &Rc<Self>, scroll_by: f64) -> bool {
        let current = self.visible_end.get_untracked();
        let Some(scroll) = self.scroll() else { return false };
        let viewport = scroll.scroll_viewport().height;
        if self.revealing_newer_rows.get()
            || current == self.rows.with_untracked(Vec::len)
            || scroll.scroll_top() + viewport < scroll.scroll_height() - viewport
        {
            return false;
        }
        self.revealing_newer_rows.set(true);
        let next = rows_after(&self.weights.get_untracked(), current, TRANSCRIPT_BACKFILL_CHUNK);
        let len = self.rows.with_untracked(Vec::len);
        self.visible_rows_end.set(if next == len { None } else { Some(next) });
        let this = self.clone();
        self.after_layout(move || {
            this.revealing_newer_rows.set(false);
            scroll.scroll_by(scroll_by, ScrollUnit::Absolute);
            this.update_away_from_bottom();
        });
        true
    }

    /// `ensureAllRows(continuation)`: message navigation needs the full transcript mounted.
    fn ensure_all_rows(self: &Rc<Self>, continuation: Box<dyn FnOnce()>) {
        if self.first_jump.get_untracked().is_some() {
            self.clear_message_navigation();
        }
        let len = self.rows.with_untracked(Vec::len);
        if self.ensure_all_rows_pending.borrow().is_none() && self.hidden.get_untracked() == 0 && self.visible_end.get_untracked() == len {
            return continuation();
        }
        if let Some(pending) = self.ensure_all_rows_pending.borrow().as_ref() {
            pending.borrow_mut().push(continuation);
            return;
        }
        let pending: Continuations = Rc::new(RefCell::new(vec![continuation]));
        *self.ensure_all_rows_pending.borrow_mut() = Some(pending.clone());
        self.hidden_rows.set(Some(0));
        self.visible_rows_end.set(None);
        let this = self.clone();
        self.after_layout(move || {
            let same = this.ensure_all_rows_pending.borrow().as_ref().is_some_and(|p| Rc::ptr_eq(p, &pending));
            if same {
                *this.ensure_all_rows_pending.borrow_mut() = None;
            }
            let continuations: Vec<_> = pending.borrow_mut().drain(..).collect();
            continuations.into_iter().for_each(|c| c());
            this.update_away_from_bottom();
        });
    }

    fn is_away_from_bottom(&self) -> bool {
        if self.revealing_older_rows.get()
            || self.revealing_newer_rows.get()
            || self.ensure_all_rows_pending.borrow().is_some()
            || self.navigation_message.get_untracked().is_some()
            || self.navigation_slack.get_untracked() != 0.0
            || self.first_jump.get_untracked().is_some()
        {
            return true;
        }
        if self.visible_end.get_untracked() < self.rows.with_untracked(Vec::len) {
            return true;
        }
        let Some(scroll) = self.scroll() else { return false };
        scroll.scroll_top() < (scroll.scroll_height() - scroll.scroll_viewport().height).max(0.0)
    }

    fn update_away_from_bottom(self: &Rc<Self>) {
        let preserve = self.revealing_older_rows.get()
            || self.revealing_newer_rows.get()
            || self.ensure_all_rows_pending.borrow().is_some()
            || self.first_jump.get_untracked().is_some();
        if self.is_away_from_bottom() {
            let hidden = self.hidden.get_untracked();
            self.hidden_rows.update(|current| {
                current.get_or_insert(hidden);
            });
        }
        if let Some(timer) = self.away_timer.take() {
            renderer::clear_timeout(timer);
        }
        let this = self.clone();
        let timer = renderer::set_timeout(0, move || {
            this.away_timer.set(None);
            let Some(scroll) = this.scroll() else { return };
            let away = preserve || this.is_away_from_bottom();
            this.away_from_bottom.set(away);
            if !away {
                // TODO(port-tui): the renderer's selection (`renderer.getSelection()`).
                this.hidden_rows.set(None);
                scroll.set_sticky_scroll(true);
            }
            this.save_scroll_anchor(false);
        });
        self.away_timer.set(Some(timer));
    }

    fn save_scroll_anchor(&self, unmounting: bool) {
        // Initial layout must not overwrite the saved position before synchronization restores it.
        if !self.restored.get() {
            return;
        }
        let mounted = self.anchors.list();
        if unmounting && mounted.is_empty() {
            return;
        }
        if !self.is_away_from_bottom() {
            *self.scroll_anchor.borrow_mut() = None;
            return;
        }
        let Some(scroll) = self.scroll() else { return };
        let viewport_y = scroll.scroll_viewport().y;
        let data = self.t.data.clone();
        let mut first = None;
        let mut anchor: Option<(super::anchors::AnchorTarget, f64)> = None;
        for child in mounted {
            let screen_y = child.node.y() as f64 - viewport_y;
            let item = (child.target.clone(), screen_y);
            if first.is_none() {
                first = Some(item.clone());
            }
            let inset = match &child.target {
                super::anchors::AnchorTarget::Group(_) => 1.0,
                super::anchors::AnchorTarget::Part(r) => {
                    let assistant = untrack(|| data.store.session.message.get(&self.session_id, &r.message_id))
                        .is_some_and(|m| m.str("type") == Some("assistant"));
                    if assistant { 1.0 } else { 0.0 }
                }
            };
            if screen_y <= inset && anchor.as_ref().is_none_or(|a| screen_y > a.1) {
                anchor = Some(item);
            }
        }
        *self.scroll_anchor.borrow_mut() = anchor.or(first);
    }

    fn restore_scroll_position(self: &Rc<Self>) {
        let Some(scroll) = self.scroll() else { return };
        let anchor = self.scroll_anchor.borrow().clone();
        let index = anchor.as_ref().and_then(|(target, _)| {
            self.rows.with_untracked(|rows| {
                rows.iter().position(|row| match row {
                    SessionRow::Entry(entry) => entry_contains_anchor(entry, target),
                    SessionRow::Group(group) => group_contains_anchor(&group.node(), target, 0),
                })
            })
        });
        let (Some((target, screen_y)), Some(index)) = (anchor, index) else {
            scroll.scroll_to(scroll.scroll_height());
            self.away_from_bottom.set(false);
            return;
        };
        let weights = self.weights.get_untracked();
        self.hidden_rows.set(Some(rows_before(&weights, index, TRANSCRIPT_BACKFILL_CHUNK)));
        let end = rows_after(&weights, index, TRANSCRIPT_BACKFILL_CHUNK);
        let len = self.rows.with_untracked(Vec::len);
        self.visible_rows_end.set(if end == len { None } else { Some(end) });
        scroll.set_sticky_scroll(false);
        fn restore(this: Rc<State>, scroll: Element, target: super::anchors::AnchorTarget, screen_y: f64) {
            let me = this.clone();
            this.after_layout(move || {
                let Some(boundary) = me.anchors.get(&target) else {
                    *me.scroll_anchor.borrow_mut() = None;
                    scroll.set_sticky_scroll(true);
                    scroll.scroll_to(scroll.scroll_height());
                    me.away_from_bottom.set(false);
                    return;
                };
                let viewport = scroll.scroll_viewport();
                let content_y = scroll.scroll_top() + boundary.node.y() as f64 - viewport.y;
                let target_y = content_y - screen_y;
                let maximum = (scroll.scroll_height() - viewport.height).max(0.0);
                let len = me.rows.with_untracked(Vec::len);
                if target_y > maximum && me.visible_end.get_untracked() < len {
                    let next = rows_after(&me.weights.get_untracked(), me.visible_end.get_untracked(), TRANSCRIPT_BACKFILL_CHUNK);
                    me.visible_rows_end.set(if next == len { None } else { Some(next) });
                    return restore(me.clone(), scroll, target, screen_y);
                }
                if target_y > maximum {
                    let slack = scroll.find_descendant_by_id(NAVIGATION_SLACK_ID).map_or(0.0, |s| s.height() as f64);
                    me.navigation_slack.set(message_navigation_slack(target_y, viewport.height, scroll.scroll_height(), slack));
                    return restore(me.clone(), scroll, target, screen_y);
                }
                scroll.scroll_to(target_y);
                me.update_away_from_bottom();
            });
        }
        restore(self.clone(), scroll, target, screen_y);
    }

    fn align_message(self: &Rc<Self>, message_id: String, top: f64) {
        let Some(scroll) = self.scroll() else { return };
        scroll.set_sticky_scroll(false);
        self.navigation_message.set(Some(message_id.clone()));
        self.update_away_from_bottom();
        let slack = scroll.find_descendant_by_id(NAVIGATION_SLACK_ID).map_or(0.0, |s| s.height() as f64);
        self.navigation_slack.set(message_navigation_slack(top, scroll.scroll_viewport().height, scroll.scroll_height(), slack));
        let this = self.clone();
        self.after_layout(move || {
            if this.navigation_message.get_untracked().as_deref() != Some(message_id.as_str()) {
                return;
            }
            scroll.scroll_to(top);
        });
    }

    fn jump_to_message(self: &Rc<Self>, message_id: String) {
        let this = self.clone();
        self.ensure_all_rows(Box::new(move || {
            let Some(child) = this.anchors.for_message(&message_id) else { return };
            let Some(scroll) = this.scroll() else { return };
            let y = scroll.scroll_top() + child.node.y() as f64 - scroll.scroll_viewport().y;
            let data = this.t.data.clone();
            let assistant = untrack(|| data.store.session.message.get(&this.session_id, &message_id))
                .is_some_and(|m| m.str("type") == Some("assistant"));
            this.align_message(message_id, (y - if assistant { 1.0 } else { 0.0 }).max(0.0));
        }));
    }

    fn scroll_to_message(self: &Rc<Self>, next: bool, user_only: bool) {
        let this = self.clone();
        self.ensure_all_rows(Box::new(move || {
            let Some(scroll) = this.scroll() else { return };
            let data = this.t.data.clone();
            let messages = untrack(|| data.store.session.message.list(&this.session_id));
            let target = find_message_boundary(
                next,
                &this.anchors.message_positions(),
                &messages,
                scroll.scroll_top(),
                scroll.scroll_viewport().y,
                this.navigation_message.get_untracked().as_deref(),
                user_only,
            );
            if let Some(target) = target {
                this.jump_to_message(target.id);
                this.dialog.clear();
                return;
            }
            if !next && untrack(|| data.store.session.message.more(&this.session_id)) {
                let me = this.clone();
                this.history().prepend(0.0, Some(Box::new(move || me.scroll_to_message(next, user_only))));
                return;
            }
            this.dialog.clear();
        }));
    }

    fn to_bottom(self: &Rc<Self>) {
        self.clear_message_navigation();
        *self.ensure_all_rows_pending.borrow_mut() = None;
        if let Some(timer) = self.away_timer.take() {
            renderer::clear_timeout(timer);
        }
        self.away_from_bottom.set(false);
        *self.scroll_anchor.borrow_mut() = None;
        self.hidden_rows.set(None);
        self.visible_rows_end.set(None);
        let this = self.clone();
        renderer::set_timeout(50, move || {
            let Some(scroll) = this.scroll() else { return };
            scroll.set_sticky_scroll(true);
            scroll.scroll_to(scroll.scroll_height());
        });
    }

    fn move_transcript(self: &Rc<Self>, delta: f64) {
        self.clear_message_navigation();
        if delta < 0.0 && self.reveal_older_rows(delta) {
            self.dialog.clear();
            return;
        }
        if delta > 0.0 && self.reveal_newer_rows(delta) {
            self.dialog.clear();
            return;
        }
        let Some(scroll) = self.scroll() else { return };
        scroll.scroll_by(delta, ScrollUnit::Absolute);
        self.update_away_from_bottom();
        self.dialog.clear();
    }

    /// `session.first`.
    fn first(self: &Rc<Self>) {
        if self.first_jump.get_untracked().is_some() {
            return;
        }
        self.clear_message_navigation();
        let aborted = Rc::new(Cell::new(false));
        let signal = opencode_tui_sync::abort::AbortController::new();
        let cancel = {
            let (aborted, signal) = (aborted.clone(), signal.clone());
            Callback(Rc::new(move || {
                aborted.set(true);
                signal.abort(None);
            }))
        };
        self.first_jump.set(Some(cancel.clone()));
        let this = self.clone();
        let start: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let start_ref = start.clone();
        *start.borrow_mut() = Some(Rc::new(move || {
            let Some(scroll) = this.scroll() else { return };
            if this.first_jump.get_untracked() != Some(cancel.clone()) {
                return;
            }
            if this.revealing_older_rows.get() || this.revealing_newer_rows.get() || this.ensure_all_rows_pending.borrow().is_some() {
                let again = start_ref.borrow().clone();
                if let Some(again) = again {
                    this.after_layout(move || again());
                }
                return;
            }
            let previous = (this.hidden_rows.get_untracked(), this.visible_rows_end.get_untracked());
            let restore = {
                let (this, cancel) = (this.clone(), cancel.clone());
                Callback(Rc::new(move || {
                    (cancel.0)();
                    batch(|| {
                        this.hidden_rows.set(previous.0);
                        this.visible_rows_end.set(previous.1);
                    });
                }))
            };
            let commit = {
                let (this, cancel, restore) = (this.clone(), cancel.clone(), restore.clone());
                Rc::new(move || {
                    if this.first_jump.get_untracked() != Some(restore.clone()) || scroll.is_destroyed() {
                        return;
                    }
                    scroll.set_sticky_scroll(false);
                    batch(|| {
                        this.hidden_rows.set(Some(0));
                        this.visible_rows_end.set(Some(TRANSCRIPT_BACKFILL_CHUNK));
                        this.first_jump.set(Some(cancel.clone()));
                    });
                })
            };
            // Pin both ends until the head budget commits in the same batch as history.
            batch(|| {
                this.first_jump.set(Some(restore.clone()));
                this.hidden_rows.set(Some(this.hidden.get_untracked()));
                this.visible_rows_end.set(Some(this.visible_end.get_untracked()));
            });
            let data = this.t.data.clone();
            let before: Rc<dyn Fn()> = {
                let commit = commit.clone();
                Rc::new(move || commit())
            };
            let load = data.store.session.message.load_more(
                &this.session_id,
                LoadMoreOptions { all: true, signal: Some(signal.signal().clone()), before_publish: Some(before) },
            );
            let (me, cancel, restore) = (this.clone(), cancel.clone(), restore.clone());
            tokio::task::spawn_local(async move {
                match load.wait().await {
                    Ok(()) => {
                        commit();
                        if me.first_jump.get_untracked() != Some(cancel.clone()) || scroll.is_destroyed() {
                            return;
                        }
                        if me.rows.with_untracked(Vec::len) <= TRANSCRIPT_BACKFILL_CHUNK {
                            me.visible_rows_end.set(None);
                        }
                        scroll.scroll_to(0.0);
                        let me2 = me.clone();
                        me.after_layout(move || {
                            if me2.first_jump.get_untracked() != Some(cancel) {
                                return;
                            }
                            scroll.scroll_to(0.0);
                            me2.first_jump.set(None);
                            me2.update_away_from_bottom();
                        });
                    }
                    Err(error) => {
                        if me.first_jump.get_untracked() != Some(restore) || scroll.is_destroyed() {
                            return;
                        }
                        me.clear_message_navigation();
                        me.t.toast.error(error.message());
                        me.update_away_from_bottom();
                    }
                }
            });
        }));
        let start = start.borrow().clone().expect("start");
        self.history().after(Box::new(move || start()));
        self.dialog.clear();
    }
}

fn init_parsers() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    // `addDefaultParsers(parsers.parsers)`, as importing routes/session/index.tsx does.
    ONCE.call_once(|| opencode_tui_markup::tree_sitter::add_default_parsers(opencode_tui_markup::tree_sitter::tui_parsers()));
}

/// The route as app.tsx mounts it (keyed on the session ID).
pub fn session_route() -> View {
    session(SessionProps::default())
}

/// `<Session>`.
pub fn session(props: SessionProps) -> View {
    init_parsers();
    let t = tui();
    let data = t.data.clone();
    let config_state = t.config_context.clone();
    let theme = t.theme;
    let dimensions = t.dimensions;
    let session_id = match t.route.get_untracked() {
        Route::Session { session_id } => session_id,
        Route::Home => String::new(),
    };
    let sid = session_id.clone();
    let session = {
        let data = data.clone();
        let sid = sid.clone();
        create_memo_equals(move || data.store.session.get(&sid), Some(same_record))
    };
    let messages = {
        let data = data.clone();
        let sid = sid.clone();
        move || data.store.session.message.list(&sid)
    };
    let message_indexes = {
        let messages = messages.clone();
        create_memo_equals(
            move || Rc::new(messages().iter().enumerate().map(|(i, m)| (m.str("id").unwrap_or_default().to_string(), i)).collect::<HashMap<_, _>>()),
            None,
        )
    };
    let legacy = {
        let messages = messages.clone();
        create_memo(move || !messages().iter().any(|m| m.str("type") == Some("idle")))
    };
    // `location`: the session's, else the current one; the location context follows it.
    let current_location = data.location.clone();
    let location = {
        let current_location = current_location.clone();
        create_memo(move || session.get().and_then(|s| s.get("location").cloned()).or_else(|| current_location.r#ref()))
    };
    {
        let current_location = current_location.clone();
        create_effect(move || {
            let location = location.get();
            untrack(|| current_location.set(location));
        });
    }
    // TODO(port-tui): the epilogue context (`epilogue.session({ title, sessionID })`).
    let descendant_session_ids = {
        let (data, sid) = (data.clone(), sid.clone());
        create_memo(move || {
            if session.get().is_some_and(|s| s.get("parentID").is_some_and(|p| !matches!(p, Json::Null))) {
                return Vec::new();
            }
            data.store.session.family(&sid).iter().filter(|id| **id != sid).cloned().collect::<Vec<String>>()
        })
    };
    let is_child = move || session.get().is_some_and(|s| s.str("parentID").is_some());
    let permissions = {
        let (data, sid) = (data.clone(), sid.clone());
        create_memo_equals(
            move || {
                if is_child() {
                    return Vec::new();
                }
                std::iter::once(sid.clone())
                    .chain(descendant_session_ids.get())
                    .flat_map(|id| data.store.session.permission.list(&id).map(|l| l.to_vec()).unwrap_or_default())
                    .collect::<Vec<Rc<Json>>>()
            },
            None,
        )
    };
    // TODO(port-tui): `local.permission.mode` ("autoaccept" replies to every request).
    let prompted_permissions = create_memo_equals(move || permissions.get(), None);
    let forms = {
        let (data, sid) = (data.clone(), sid.clone());
        create_memo_equals(
            move || {
                let location = location.get();
                let global = data.store.session.form.list("global", location.as_ref()).map(|l| l.to_vec()).unwrap_or_default();
                if is_child() {
                    return global;
                }
                let mut out: Vec<Rc<Json>> = std::iter::once(sid.clone())
                    .chain(descendant_session_ids.get())
                    .flat_map(|id| data.store.session.form.list(&id, None).map(|l| l.to_vec()).unwrap_or_default())
                    .collect();
                out.extend(global);
                out
            },
            None,
        )
    };
    let pending_users = {
        let (data, sid) = (data.clone(), sid.clone());
        create_memo_equals(
            move || data.store.session.pending.list(&sid).iter().filter(|i| i.str("type") == Some("user")).cloned().collect::<Vec<_>>(),
            None,
        )
    };
    let pending_deliveries = create_memo(move || {
        pending_users
            .get()
            .iter()
            .map(|i| (i.str("id").unwrap_or_default().to_string(), i.str("delivery").unwrap_or_default().to_string()))
            .collect::<Vec<_>>()
    });
    let disabled = create_memo(move || !prompted_permissions.with(Vec::is_empty) || !forms.with(Vec::is_empty));

    let local = crate::local::local();
    let config = move || config_state.data();
    let thinking_mode = {
        let config = config.clone();
        create_memo(move || ThinkingMode::parse(config().session.thinking.as_deref()))
    };
    let show_scrollbar = {
        let config = config.clone();
        create_memo(move || config().session.scrollbar.unwrap_or(false))
    };
    let markdown_mode = {
        let config = config.clone();
        create_memo(move || if config().session.markdown.as_deref() == Some("source") { MarkdownMode::Source } else { MarkdownMode::Rendered })
    };
    let diff_wrap_mode = {
        let config = config.clone();
        create_memo(move || {
            if config().diffs.as_ref().and_then(|d| d.wrap.as_deref()) == Some("none") { DiffWrapMode::None } else { DiffWrapMode::Word }
        })
    };
    let group_exploration = {
        let config = config.clone();
        create_memo(move || config().session.grouping.as_deref() != Some("none"))
    };
    let verbosity = {
        let config = config.clone();
        create_memo(move || Verbosity::parse(config().session.verbosity.as_deref()))
    };
    let _ = DEFAULT_VERBOSITY;
    let group_expanded_map: Signal<HashMap<String, bool>> = create_signal_with(HashMap::new(), None).0;
    // High opens exploration and instruction summaries by default; everything else starts collapsed.
    let group_expanded: Rc<dyn Fn(&str, GroupKind) -> bool> = Rc::new(move |id: &str, kind: GroupKind| {
        group_expanded_map
            .with(|m| m.get(id).copied())
            .unwrap_or_else(|| verbosity.get() == Verbosity::High && matches!(kind, GroupKind::Exploration | GroupKind::Instructions))
    });
    let grouped_kind = move |kind: GroupKind| match kind {
        GroupKind::Reasoning => thinking_mode.get() == ThinkingMode::Hide,
        GroupKind::Exploration => group_exploration.get(),
        _ => true,
    };

    {
        let (sidebar_visible, tabs_width, toggle) = (props.sidebar_visible.clone(), props.vertical_tabs_width.clone(), props.on_toggle_sidebar.clone());
        create_layer(move || {
            let (sidebar_visible, tabs_width, toggle) = (sidebar_visible.clone(), tabs_width.clone(), toggle.clone());
            Layer {
                priority: Some(10),
                enabled: Some(Enabled::Fn(Rc::new(move || {
                    sidebar_visible() && (dimensions.get().0 as i64 - tabs_width() as i64) <= 120 && !disabled.get()
                }))),
                commands: vec![Command::inline("escape,ctrl+c", move || {
                    toggle();
                    true
                })
                .title("Close sidebar")
                .group("Session")],
                ..Layer::default()
            }
        });
    }
    let content_width = {
        let (width, tabs_width) = (props.width.clone(), props.vertical_tabs_width.clone());
        create_memo(move || width().map(|w| w as i64).unwrap_or(dimensions.get().0 as i64 - tabs_width() as i64) as i32 - 4)
    };
    // A session whose location is unavailable never loads its models: name them from the default location's.
    let models = {
        let data = data.clone();
        create_memo_equals(
            move || {
                let location = location.get();
                let list = data.store.location.model.list(location.as_ref()).or_else(|| data.store.location.model.list(None));
                Rc::new(list.map(|l| match &*l {
                    Json::Array(items) => items.iter().cloned().map(Rc::new).collect(),
                    other => opencode_tui_sync::json::JsonExt::arr(other, "data").cloned().unwrap_or_default().into_iter().map(Rc::new).collect(),
                }).unwrap_or_default())
            },
            None,
        )
    };

    let (rows_synced, set_rows_synced) = create_signal(false);
    let anchors = TimelineAnchors::new();
    let state_cell: Rc<RefCell<Option<Rc<State>>>> = Rc::new(RefCell::new(None));
    let rows = {
        let sid_for = sid.clone();
        let target = sid.clone();
        let state_cell = state_cell.clone();
        create_session_rows(
            Rc::new(move || sid_for.clone()),
            Rc::new(move |id: &str| {
                if id == target {
                    set_rows_synced.set(true)
                }
            }),
            // A scroll-triggered history load joins this boundary load instead of publishing its own page.
            Rc::new(move || {
                if let Some(state) = state_cell.borrow().clone() {
                    state.pin_above();
                }
            }),
        )
    };
    let boundaries = {
        let messages = messages.clone();
        create_memo(move || message_boundary_ids(&rows.get(), &messages()))
    };
    let weights = {
        let group_expanded = group_expanded.clone();
        create_memo(move || rows.get().iter().map(|row| row_weight(row, &*group_expanded, &grouped_kind)).collect::<Vec<usize>>())
    };
    let hidden_rows: Signal<Option<usize>> = create_signal(None).0;
    let visible_rows_end: Signal<Option<usize>> = create_signal(None).0;
    let hidden = create_memo(move || {
        hidden_rows.get().unwrap_or(usize::MAX).min(rows_before(&weights.get(), rows.with(Vec::len), TRANSCRIPT_TAIL_ROWS))
    });
    let visible_end = create_memo(move || hidden.get().max(visible_rows_end.get().unwrap_or(usize::MAX).min(rows.with(Vec::len))));
    let visible_rows = create_memo(move || {
        let (start, end) = (hidden.get(), visible_end.get());
        rows.with(|rows| rows[start.min(rows.len())..end.min(rows.len())].to_vec())
    });
    let (synced, set_synced) = create_signal(false);
    let (latest_hovered, set_latest_hovered) = create_signal(false);
    let state = Rc::new(State {
        t: t.clone(),
        dialog: crate::ui::dialog::use_dialog(),
        session_id: sid.clone(),
        scroll: Cell::new(None),
        rows,
        weights,
        hidden_rows,
        visible_rows_end,
        hidden,
        visible_end,
        navigation_message: create_signal(None).0,
        navigation_slack: create_signal(0.0).0,
        first_jump: create_signal(None).0,
        away_from_bottom: create_signal(false).0,
        ensure_all_rows_pending: RefCell::new(None),
        revealing_older_rows: Cell::new(false),
        revealing_newer_rows: Cell::new(false),
        restored: Cell::new(false),
        away_timer: Cell::new(None),
        prepend_history: RefCell::new(None),
        anchors: anchors.clone(),
        scroll_anchor: RefCell::new(None),
        group_expanded: group_expanded_map,
    });
    *state_cell.borrow_mut() = Some(state.clone());
    {
        let away = state.away_from_bottom;
        create_effect(move || {
            if !away.get() {
                set_latest_hovered.set(false);
            }
        });
    }
    {
        let (data, sid) = (data.clone(), sid.clone());
        let st = state.clone();
        let st2 = state.clone();
        let st3 = state.clone();
        let loader = data.clone();
        *state.prepend_history.borrow_mut() = Some(super::history::HistoryPrepend::new(super::history::HistoryInput {
            session_id: Rc::new(move || sid.clone()),
            more: Rc::new(move |id: &str| untrack(|| data.store.session.message.more(id))),
            load_more: Rc::new(move |id: &str| {
                let (loader, id) = (loader.clone(), id.to_string());
                let st = st.clone();
                Box::pin(async move {
                    let before: Rc<dyn Fn()> = {
                        let (st, id) = (st.clone(), id.clone());
                        Rc::new(move || {
                            if st.session_id == id {
                                st.pin_above();
                            }
                        })
                    };
                    let load = loader.store.session.message.load_more(&id, LoadMoreOptions { before_publish: Some(before.clone()), ..LoadMoreOptions::default() });
                    load.wait().await.map_err(|_| ())?;
                    let (l1, l2, l3) = (loader.clone(), loader.clone(), loader.clone());
                    let (i1, i2, i3) = (id.clone(), id.clone(), id.clone());
                    super::rows::complete_group_boundary(
                        st.rows,
                        move || untrack(|| l1.store.session.message.list(&i1).len()),
                        move || untrack(|| l2.store.session.message.more(&i2)),
                        move || {
                            let before = before.clone();
                            l3.store.session.message.load_more(&i3, LoadMoreOptions { before_publish: Some(before), ..LoadMoreOptions::default() })
                        },
                        || true,
                    )
                    .await
                })
            }),
            after_layout: Rc::new(move |continuation| st2.after_layout(continuation)),
            active: Rc::new(move |id: &str| st3.session_id == id && st3.scroll().is_some()),
            scroll_by: {
                let st = state.clone();
                Rc::new(move |amount: f64| {
                    if let Some(scroll) = st.scroll() {
                        scroll.scroll_by(amount, ScrollUnit::Absolute);
                        st.update_away_from_bottom();
                    }
                })
            },
        }));
    }

    {
        let st = state.clone();
        create_effect(on(
            move || (dimensions.get(), 0u32),
            move |_, previous| {
                if previous.is_none() {
                    return;
                }
                st.clear_message_navigation();
                if st.scroll().is_some() {
                    st.update_away_from_bottom();
                }
            },
            false,
        ));
    }

    {
        let (data, connection) = (data.clone(), data.connection.clone());
        create_effect(on(
            move || (descendant_session_ids.get(), connection.status()),
            move |(ids, status), _| {
                if *status != Status::Connected {
                    return;
                }
                for id in ids {
                    let (p, f) = (data.store.session.permission.sync(id), data.store.session.form.sync(id, None));
                    tokio::task::spawn_local(async move {
                        let _ = p.wait().await;
                        let _ = f.wait().await;
                    });
                }
            },
            false,
        ));
    }

    {
        let (data, sid) = (data.clone(), sid.clone());
        let connection = data.connection.clone();
        let tui_for_tasks = t.clone();
        create_effect(move || {
            if connection.status() != Status::Connected {
                return;
            }
            set_synced.set(false);
            let id = sid.clone();
            let all = (data.store.session.sync(&id, true), data.store.session.permission.sync(&id), data.store.session.form.sync(&id, None));
            let data = data.clone();
            let t = Some(tui_for_tasks.clone());
            tokio::task::spawn_local(async move {
                let result = all.0.wait().await;
                let _ = all.1.wait().await;
                let _ = all.2.wait().await;
                match result {
                    Ok(()) => {
                        if untrack(|| data.store.session.get(&id)).is_none() {
                            if let Some(t) = &t {
                                t.toast.show(crate::toast::ToastInput::new(crate::toast::Variant::Error, format!("Session not found: {id}")).duration(5000));
                                t.route.set(Route::Home);
                            }
                            return;
                        }
                        // TODO(port-tui): `editor.reconnect(info.location.directory)`.
                        set_synced.set(true);
                    }
                    Err(error) => {
                        if let Some(t) = &t {
                            t.toast.show(crate::toast::ToastInput::new(crate::toast::Variant::Error, error.message()).duration(5000));
                            t.route.set(Route::Home);
                        }
                    }
                }
            });
        });
    }

    {
        let st = state.clone();
        create_effect(move || {
            if st.restored.get() || !synced.get() || !rows_synced.get() || st.scroll().is_none() {
                return;
            }
            st.restored.set(true);
            // Initial synchronization can finish after the reader has already navigated.
            if !untrack(|| st.is_away_from_bottom()) {
                untrack(|| st.restore_scroll_position());
            }
        });
    }
    {
        let st = state.clone();
        let scroll_ref = props.scroll_ref.clone();
        on_cleanup(move || {
            if let Some(timer) = st.away_timer.take() {
                renderer::clear_timeout(timer);
            }
            if let Some(scroll_ref) = &scroll_ref {
                scroll_ref(None);
            }
            st.history().cancel();
            if let Some(jump) = st.first_jump.get_untracked() {
                (jump.0)();
            }
            if st.scroll().is_none() {
                return;
            }
            st.save_scroll_anchor(true);
        });
    }

    let prompt_ref: Rc<RefCell<Option<PromptRef>>> = Rc::new(RefCell::new(None));
    let pending = super::pending::Pending::new(t.clone(), sid.clone());

    // ---- commands ----
    let global_commands = {
        let st = state.clone();
        move || -> Vec<Command> {
            let mk = |id: &str, title: &str, f: Rc<dyn Fn(&Rc<State>)>| {
                let st = st.clone();
                Command::new(id, move || f(&st)).title(title).group("Session").bind(Bind::Off)
            };
            let height = |st: &Rc<State>| st.scroll().map_or(0.0, |s| s.height() as f64);
            vec![
                mk("session.page.up", "Page up", Rc::new(move |st| st.move_transcript(-height(st) / 2.0))),
                mk("session.page.down", "Page down", Rc::new(move |st| st.move_transcript(height(st) / 2.0))),
                mk("session.line.up", "Line up", Rc::new(|st| st.move_transcript(-1.0))),
                mk("session.line.down", "Line down", Rc::new(|st| st.move_transcript(1.0))),
                mk("session.half.page.up", "Half page up", Rc::new(move |st| st.move_transcript(-height(st) / 4.0))),
                mk("session.half.page.down", "Half page down", Rc::new(move |st| st.move_transcript(height(st) / 4.0))),
            ]
        }
    };
    let base_and_unfocused_commands = {
        let st = state.clone();
        move || -> Vec<Command> {
            let (a, b) = (st.clone(), st.clone());
            vec![
                Command::new("session.first", move || a.first()).title("First message").group("Session").bind(Bind::Off),
                Command::new("session.last", move || {
                    b.to_bottom();
                    b.dialog.clear();
                }).title("Last message").group("Session").bind(Bind::Off),
            ]
        }
    };
    let base_commands = {
        let st = state.clone();
        let config_state = t.config_context.clone();
        let data = data.clone();
        let sid = sid.clone();
        let pending = pending.clone();
        move || -> Vec<Command> {
            let toggle = |id: &str, title: String, mutate: Rc<dyn Fn(&mut Json)>| {
                let (config_state, toast, dialog) = (config_state.clone(), st.t.toast.clone(), st.dialog.clone());
                Command::new(id, move || {
                    let mutate = mutate.clone();
                    if let Err(error) = config_state.update(move |draft| mutate(draft)) {
                        toast.error(error);
                    }
                    dialog.clear();
                })
                .title(title)
                .group("Session")
                .bind(Bind::Off)
            };
            let session_field = |key: &'static str, value: Json| -> Rc<dyn Fn(&mut Json)> {
                Rc::new(move |draft: &mut Json| {
                    let mut session = draft.get("session").cloned().filter(|s| matches!(s, Json::Object(_))).unwrap_or(Json::Object(Default::default()));
                    opencode_tui_sync::json::JsonExt::set(&mut session, key, value.clone());
                    opencode_tui_sync::json::JsonExt::set(draft, "session", session);
                })
            };
            let next = thinking_mode.get().next();
            let messages_last_user = {
                let (st, data, sid) = (st.clone(), data.clone(), sid.clone());
                move || {
                    let messages = untrack(|| data.store.session.message.list(&sid));
                    if let Some(message) = messages
                        .iter()
                        .rev()
                        .find(|m| m.str("type") == Some("user") && !m.str("text").unwrap_or_default().trim().is_empty())
                    {
                        st.jump_to_message(message.str("id").unwrap_or_default().to_string());
                    }
                }
            };
            let (n1, n2, n3, n4) = (st.clone(), st.clone(), st.clone(), st.clone());
            let levels = [Verbosity::Low, Verbosity::Medium, Verbosity::High];
            let next_level = levels[(levels.iter().position(|l| *l == verbosity.get()).unwrap_or(1) + 1) % 3];
            let session_command = |id: &str, title: &str| {
                let dialog = st.dialog.clone();
                Command::new(id, move || dialog.clear()).title(title).group("Session").palette(true).bind(Bind::Off)
            };
            let call = {
                let (client, toast) = (st.t.data.client.clone(), st.t.toast.clone());
                move |name: &'static str, input: Json| {
                    let (client, toast) = (client.clone(), toast.clone());
                    tokio::task::spawn_local(async move {
                        if let Err(error) = client.call_named(name, &input, Default::default()).await {
                            toast.show(crate::toast::ToastInput::new(crate::toast::Variant::Error, error.message()).duration(5000));
                        }
                    });
                }
            };
            let sid_json = || Json::Object([("sessionID".to_string(), Json::String(sid.clone()))].into_iter().collect());
            let rename = {
                let (st, sid) = (st.clone(), sid.clone());
                let call = call.clone();
                Command::with_input("session.rename", move |input| {
                    let Some(input) = input else {
                        let title = untrack(|| st.t.data.store.session.get(&sid)).and_then(|s| s.str("title").map(str::to_string));
                        crate::component::dialog_session_rename::show(&st.dialog, &sid, title);
                        return true;
                    };
                    let title = input.trim().to_string();
                    if title.is_empty() {
                        let request = st.t.data.store.session.title.generate(&sid);
                        let toast = st.t.toast.clone();
                        tokio::task::spawn_local(async move {
                            if let Err(error) = request.wait().await {
                                toast.error(error.message());
                            }
                        });
                    } else {
                        let input = Json::Object(
                            [("sessionID".to_string(), Json::String(sid.clone())), ("title".to_string(), Json::String(title))].into_iter().collect(),
                        );
                        call("session.update", input);
                    }
                    true
                })
                .title("Rename session")
                .group("Session")
                .palette(true)
                .bind(Bind::Off)
                .slash_arguments("rename")
            };
            let compact = {
                let (st, sid, local) = (st.clone(), sid.clone(), local.clone());
                Command::new("session.compact", move || {
                    let model = untrack(|| local.model_current().map(|m| (m.provider_id, m.model_id, local.variant_current()))).map(|(provider, id, variant)| {
                        let mut fields = vec![("providerID".to_string(), Json::String(provider)), ("id".to_string(), Json::String(id))];
                        if let Some(variant) = variant {
                            fields.push(("variant".to_string(), Json::String(variant)));
                        }
                        Json::Object(fields.into_iter().collect())
                    });
                    let request = st.t.data.store.session.compact(&sid, model);
                    let toast = st.t.toast.clone();
                    tokio::task::spawn_local(async move {
                        if let Err(error) = request.wait().await {
                            toast.show(crate::toast::ToastInput::new(crate::toast::Variant::Error, error.message()));
                        }
                    });
                    st.dialog.clear();
                })
                .title("Compact session")
                .group("Session")
                .palette(true)
                .bind(Bind::Off)
                .slash("compact", &[])
            };
            let redo = {
                let (client, toast, dialog) = (st.t.data.client.clone(), st.t.toast.clone(), st.dialog.clone());
                let input = sid_json();
                Command::new("session.redo", move || {
                    let (client, toast, dialog, input) = (client.clone(), toast.clone(), dialog.clone(), input.clone());
                    tokio::task::spawn_local(async move {
                        if let Err(error) = client.call_named("session.revert.clear", &input, Default::default()).await {
                            toast.show(crate::toast::ToastInput::new(crate::toast::Variant::Error, error.message()).duration(5000));
                        }
                        dialog.clear();
                    });
                })
                    .title("Redo")
                    .group("Session")
                    .palette(true)
                    .bind(Bind::Off)
                    .enabled(session.get().and_then(|s| s.get("revert").and_then(|r| r.str("messageID")).map(|_| ())).is_some())
                    .slash("redo", &[])
            };
            let background = {
                let (call, dialog) = (call.clone(), st.dialog.clone());
                let input = sid_json();
                Command::new("session.background", move || {
                    call("session.background", input.clone());
                    dialog.clear();
                })
                    .title("Background blocking tools")
                    .group("Session")
                    .bind(Bind::Off)
            };
            let parent = {
                let st = st.clone();
                let parent_id = session.get().and_then(|s| s.str("parentID").map(str::to_string));
                let enabled = parent_id.is_some();
                Command::new("session.parent", move || {
                    if let Some(id) = parent_id.clone() {
                        st.t.route.set(Route::Session { session_id: id });
                    }
                    st.dialog.clear();
                })
                .title("Go to parent session")
                .group("Session")
                .bind(Bind::Off)
                .enabled(enabled)
            };
            let queued = !pending_users.with(|p| p.iter().all(|i| i.str("delivery") != Some("queue")));
            let queued_list = move || {
                pending_users.with(|p| {
                    p.iter()
                        .filter(|i| i.str("delivery") == Some("queue"))
                        .map(|i| {
                            (i.str("id").unwrap_or_default().to_string(), i.get("payload").and_then(|p| p.str("text")).unwrap_or_default().to_string())
                        })
                        .collect::<Vec<_>>()
                })
            };
            vec![
                rename,
                // TODO(port-tui): the dialogs slice: DialogTimeline, DialogFork.
                session_command("session.timeline", "Jump to message").slash("timeline", &[]),
                session_command("session.fork", "Fork session").slash("fork", &[]),
                compact,
                // TODO(port-tui): the prompt slice: usePromptRestore and appendToSessionPrompt for the undo.
                session_command("session.undo", "Undo previous message").slash("undo", &[]),
                redo,
                toggle(
                    "session.toggle.thinking",
                    if next == ThinkingMode::Hide { "Collapse thinking".into() } else { "Expand thinking".into() },
                    session_field("thinking", Json::String(next.as_str().into())),
                ),
                toggle("session.toggle.scrollbar", "Toggle session scrollbar".into(), session_field("scrollbar", Json::Bool(!show_scrollbar.get()))),
                toggle(
                    "session.toggle.exploration_grouping",
                    if group_exploration.get() { "Show tool calls individually".into() } else { "Group related tool calls".into() },
                    session_field("grouping", Json::String(if group_exploration.get() { "none".into() } else { "auto".into() })),
                ),
                toggle(
                    "session.verbosity.cycle",
                    format!("Verbosity: {}", crate::util::titlecase(verbosity.get().as_str())),
                    session_field("verbosity", Json::String(next_level.as_str().into())),
                )
                .palette(true),
                Command::new("session.messages_last_user", messages_last_user).title("Jump to last user message").group("Session").bind(Bind::Off),
                Command::new("session.message.next", move || n1.scroll_to_message(true, false)).title("Next message").group("Session").bind(Bind::Off),
                Command::new("session.message.previous", move || n2.scroll_to_message(false, false)).title("Previous message").group("Session").bind(Bind::Off),
                Command::new("session.message.user.next", move || n3.scroll_to_message(true, true)).title("Next user message").group("Session").bind(Bind::Off),
                Command::new("session.message.user.previous", move || n4.scroll_to_message(false, true)).title("Previous user message").group("Session").bind(Bind::Off),
                // TODO(port-tui): the clipboard context and the export dialogs.
                session_command("messages.copy", "Copy last assistant message").slash("copy", &[]),
                session_command("session.copy.id", "Copy session ID"),
                session_command("session.copy", "Copy session transcript"),
                session_command("session.export", "Export session transcript").slash("export", &[]),
                background,
                // TODO(port-tui): the prompt slice's composer (subagent picker) and the queued prompts dialog.
                session_command("session.child.first", "Toggle subagent picker"),
                Command::new("session.queued_prompts", {
                    let (pending, dialog) = (pending.clone(), st.dialog.clone());
                    move || pending.open_queued(&dialog, Rc::new(queued_list))
                })
                .title("View queued prompts").group("Prompt").palette(true).bind(Bind::Off).enabled(queued),
                parent,
            ]
        }
    };
    {
        let (g, b, c) = (global_commands.clone(), base_and_unfocused_commands.clone(), base_commands.clone());
        create_layer(move || {
            let mut commands = g();
            commands.extend(b());
            commands.extend(c());
            Layer {
                mode: Some("global"),
                commands,
                bindings: vec![
                    "session.page.up",
                    "session.page.down",
                    "session.line.up",
                    "session.line.down",
                    "session.half.page.up",
                    "session.half.page.down",
                ],
                ..Layer::default()
            }
        });
    }
    create_layer(move || Layer {
        mode: Some("composer"),
        enabled: Some(Enabled::Fn(Rc::new(|| renderer::current_focused_renderable().is_none_or(|el| !el.is::<opencode_tui_core::renderables::textarea::TextareaKind>())))),
        bindings: vec!["session.first", "session.last"],
        ..Layer::default()
    });
    create_layer(move || Layer {
        bindings: vec![
            "session.first",
            "session.last",
            "session.rename",
            "session.timeline",
            "session.fork",
            "session.compact",
            "session.undo",
            "session.redo",
            "session.toggle.thinking",
            "session.toggle.scrollbar",
            "session.toggle.exploration_grouping",
            "session.verbosity.cycle",
            "session.messages_last_user",
            "session.message.next",
            "session.message.previous",
            "session.message.user.next",
            "session.message.user.previous",
            "messages.copy",
            "session.copy.id",
            "session.copy",
            "session.export",
            "session.background",
            "session.child.first",
            "session.queued_prompts",
            "session.parent",
        ],
        ..Layer::default()
    });

    // ---- the view ----
    let st = state.clone();
    let ctx = RenderCtx {
        width: Rc::new(move || content_width.get()),
        terminal_width: Rc::new(move || dimensions.get().0),
        terminal_height: Rc::new(move || dimensions.get().1),
        session_id: sid.clone(),
        anchors: anchors.clone(),
        group_expanded: group_expanded.clone(),
        set_group_expanded: {
            let st = st.clone();
            Rc::new(move |id: &str, expanded: bool, anchor: Option<Element>| {
                // A group that ends the transcript would open off screen while the reader follows the bottom.
                let ending = st.rows.with_untracked(|rows| {
                    rows.iter()
                        .rev()
                        .find(|row| !matches!(row, SessionRow::Entry(SessionEntry::AssistantFooter { .. } | SessionEntry::TurnUsage { .. })))
                        .and_then(|row| match row {
                            SessionRow::Group(group) => group_id(&group.node(), 0),
                            _ => None,
                        })
                        .as_deref()
                        == Some(id)
                });
                // Keeps the toggled disclosure's header on its viewport row through the next layout.
                if let (Some(anchor), Some(scroll)) = (anchor, st.scroll()) {
                    if !(ending && !st.is_away_from_bottom()) {
                        let release = scroll.anchor(anchor);
                        st.after_layout(release);
                    }
                }
                let id = id.to_string();
                st.group_expanded.update(|m| {
                    m.insert(id, expanded);
                });
                let me = st.clone();
                st.after_layout(move || me.save_scroll_anchor(false));
            })
        },
        thinking_mode: Rc::new(move || thinking_mode.get()),
        markdown_mode: Rc::new(move || markdown_mode.get()),
        group_exploration: Rc::new(move || group_exploration.get()),
        diff_wrap_mode: Rc::new(move || diff_wrap_mode.get()),
        models: Rc::new(move || models.get()),
        message_index: Rc::new(move |id: &str| message_indexes.with(|m| m.get(id).copied())),
        legacy_turns: Rc::new(move || legacy.get()),
        config: {
            let config = t.config_context.clone();
            Rc::new(move || config.data())
        },
        mutate_pending: {
            let pending = pending.clone();
            Rc::new(move |action: PendingAction, id: &str| pending.mutate(action, id))
        },
        open_pending_steer: {
            let (pending, dialog) = (pending.clone(), state.dialog.clone());
            let render_filter = !t.config.new_designs();
            Rc::new(move |id: String| pending.open_steer(&dialog, id, render_filter))
        },
        pending_delivery: Rc::new(move |id: &str| pending_deliveries.with(|list| list.iter().find(|(i, _)| i == id).map(|(_, d)| d.clone()))),
    };
    let mut view = View::Empty;
    let props_for = props.clone();
    let prompt_for = prompt_ref.clone();
    provide_context(context(), ctx);
    let outer = box_();
    outer.set_flex_direction(FlexDirection::Row);
    outer.set_flex_grow(Some(1.0));
    outer.set_min_height(Some(Value::Point(0.0)));
    let column = box_();
    insert_node(outer, column, None);
    column.set_flex_grow(Some(1.0));
    column.set_min_height(Some(Value::Point(0.0)));
    column.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
    bind(move || if dimensions.get().0 < 44 { 1.0 } else { 2.0 }, move |p| {
        column.set_padding(Edge::Left, Some(Value::Point(p)));
        column.set_padding(Edge::Right, Some(Value::Point(p)));
    });
    let st_view = state.clone();
    let messages_for_view = messages.clone();
    insert(
        column,
        show(
            move || session.get().is_some(),
            move || {
                let st = st_view.clone();
                let holder = box_();
                holder.set_flex_grow(Some(1.0));
                holder.set_min_height(Some(Value::Point(0.0)));
                holder.set_position_type(false);
                let scroll = scrollbox();
                insert_node(holder, scroll, None);
                st.scroll.set(Some(scroll));
                if let Some(scroll_ref) = &props_for.scroll_ref {
                    scroll_ref(Some(scroll));
                }
                {
                    let st = st.clone();
                    scroll.on_scroll_change(true, Rc::new(move |_| st.update_away_from_bottom()));
                }
                bind(move || show_scrollbar.get(), move |show| scroll.set_viewport_padding("paddingRight", Some(if show { 1 } else { 0 })));
                bind_always(
                    move || (show_scrollbar.get(), theme.with(|th| (rgba(th.decrease(th.background.raised.base, 1.0)), rgba(th.border.base)))),
                    move |(show, (bg, fg))| {
                        scroll.set_scrollbar_options(
                            Some(true),
                            &ScrollBarOptions {
                                gap: Some(1.0),
                                visible: Some(Some(show)),
                                track_background: Some(bg),
                                track_foreground: Some(fg),
                                ..ScrollBarOptions::default()
                            },
                        )
                    },
                );
                {
                    let st = st.clone();
                    bind(move || st.navigation_message.get().is_none() && st.navigation_slack.get() == 0.0, move |s| scroll.set_sticky_scroll(s));
                }
                scroll.set_sticky_start(Some(StickyStart::Bottom));
                scroll.set_flex_grow(Some(1.0));
                bind_always(move || crate::ui::scroll::scroll_acceleration(&tui().config_context.data()), move |a| scroll.set_scroll_acceleration(a));
                {
                    let st = st.clone();
                    scroll.set_on_mouse(
                        Some(MouseEventType::Scroll),
                        Some(Rc::new(move |event| {
                            if st.first_jump.get_untracked().is_some() {
                                st.clear_message_navigation();
                            }
                            let direction = event.scroll.map(|s| s.direction);
                            if direction == Some(ScrollDirection::Up) && st.reveal_older_rows(0.0) {
                                return;
                            }
                            if direction == Some(ScrollDirection::Down) && st.reveal_newer_rows(0.0) {
                                return;
                            }
                            st.update_away_from_bottom();
                        })),
                    );
                }
                let hidden = st.hidden;
                insert(
                    scroll,
                    for_each(
                        move || visible_rows.get(),
                        super::rows::row_key,
                        move |row, index| {
                            let boundary = Rc::new(move || boundaries.with(|b| b.get(index.get() + hidden.get()).cloned().flatten()));
                            session_row_view(row, boundary)
                        },
                        None,
                    ),
                    At::End,
                );
                {
                    let messages = messages_for_view.clone();
                    insert(scroll, super::extras::background_tool_hint(Rc::new(move || messages())), At::End);
                }
                {
                    let messages = messages_for_view.clone();
                    let revert = move || session.get().and_then(|s| s.get("revert").and_then(|r| r.str("messageID")).map(str::to_string));
                    let files = move || {
                        session.get().and_then(|s| s.get("revert").and_then(|r| r.arr("files").cloned())).unwrap_or_default()
                    };
                    let count = move || {
                        let Some(boundary) = revert() else { return 0 };
                        let list = messages();
                        let index = list.iter().position(|m| m.str("id") == Some(boundary.as_str()));
                        index.map_or(0, |i| list[i..].iter().filter(|m| m.str("type") == Some("user")).count())
                    };
                    let (count, files): (Rc<dyn Fn() -> usize>, Rc<dyn Fn() -> Vec<Json>>) = (Rc::new(count), Rc::new(files));
                    insert(
                        scroll,
                        show(move || revert().is_some(), move || super::extras::revert_message(count.clone(), files.clone()), None),
                        At::End,
                    );
                }
                insert(
                    scroll,
                    show(
                        { let st = st.clone(); move || st.navigation_slack.get() > 0.0 },
                        {
                            let st = st.clone();
                            move || {
                                let slack = box_();
                                slack.set_id(NAVIGATION_SLACK_ID);
                                let st = st.clone();
                                bind(move || st.navigation_slack.get(), move |h| slack.set_height(Some(Value::Point(h as f32))));
                                slack.set_flex_shrink(Some(0.0));
                                View::Node(slack)
                            }
                        },
                        None,
                    ),
                    At::End,
                );
                let jump_row = box_();
                jump_row.set_height(Some(Value::Point(1.0)));
                jump_row.set_flex_shrink(Some(0.0));
                jump_row.set_flex_direction(FlexDirection::Row);
                jump_row.set_justify_content(Justify::FlexEnd);
                let st2 = st_view.clone();
                insert(
                    jump_row,
                    show(
                        move || st2.first_jump.get().is_some(),
                        move || {
                            let label = text();
                            bind_always(move || rgba(theme.with(|th| th.text.feedback.info.base)), move |c| label.set_fg(Some(c)));
                            insert(label, View::from("Loading session history…"), At::Only);
                            View::Node(label)
                        },
                        None,
                    ),
                    At::End,
                );
                let st3 = st_view.clone();
                let st4 = st_view.clone();
                insert(
                    jump_row,
                    show(
                        move || st3.first_jump.get().is_none() && st3.away_from_bottom.get(),
                        move || {
                            let jump = box_();
                            jump.set_id("session-jump-to-latest");
                            jump.set_padding(Edge::Left, Some(Value::Point(1.0)));
                            jump.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| set_latest_hovered.set(true))));
                            jump.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| set_latest_hovered.set(false))));
                            let st = st4.clone();
                            jump.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| st.to_bottom())));
                            let label = text();
                            insert_node(jump, label, None);
                            bind_always(
                                move || theme.with(|th| if latest_hovered.get() { rgba(th.text.action.secondary.hovered) } else { rgba(th.text.action.secondary.base) }),
                                move |c| label.set_fg(Some(c)),
                            );
                            insert(label, View::from("Jump to latest ↓"), At::Only);
                            View::Node(jump)
                        },
                        None,
                    ),
                    At::End,
                );
                let composer = box_();
                composer.set_flex_shrink(Some(0.0));
                let st5 = st_view.clone();
                let prompt_for = prompt_for.clone();
                let sid = st_view.session_id.clone();
                // `<Show when={!composer.open && !disabled() && queuedPrompts().length > 0}><QueuedPromptDock/>`.
                let (pending_for_dock, st_dock) = (pending.clone(), st_view.clone());
                let queued_prompts = move || {
                    pending_users.with(|p| {
                        p.iter()
                            .filter(|i| i.str("delivery") == Some("queue"))
                            .map(|i| {
                                let text = i.get("payload").and_then(|p| p.str("text")).unwrap_or_default().to_string();
                                (i.str("id").unwrap_or_default().to_string(), text)
                            })
                            .collect::<Vec<_>>()
                    })
                };
                insert(
                    composer,
                    show(
                        move || !disabled.get() && !queued_prompts().is_empty(),
                        move || {
                            // TODO(port-tui): the dialogs slice: the queued prompts DialogSelect (`openQueuedPrompts`).
                            let (pending, dialog) = (pending_for_dock.clone(), st_dock.dialog.clone());
                            super::extras::queued_prompt_dock(
                                Rc::new(queued_prompts),
                                Rc::new(move || pending.open_queued(&dialog, Rc::new(queued_prompts))),
                            )
                        },
                        None,
                    ),
                    At::End,
                );
                insert(
                    composer,
                    switch(
                        vec![
                            when(move || !prompted_permissions.with(Vec::is_empty), move || {
                                show_keyed(
                                    move || prompted_permissions.with(|p| p.first().and_then(|r| r.str("id").map(str::to_string))),
                                    move |_| {
                                        let request = untrack(|| prompted_permissions.with(|p| p.first().cloned()));
                                        let directory = untrack(|| session.get()).and_then(|s| s.get("location").and_then(|l| l.str("directory")).map(str::to_string));
                                        request.map(|r| permission_prompt(r, directory)).unwrap_or_default()
                                    },
                                    None,
                                )
                            }),
                            when(move || !forms.with(Vec::is_empty), move || {
                                show_keyed(
                                    move || forms.with(|f| f.first().and_then(|r| r.str("id").map(str::to_string))),
                                    move |_| untrack(|| forms.with(|f| f.first().cloned())).map(form_prompt).unwrap_or_default(),
                                    None,
                                )
                            }),
                            when(move || !disabled.get(), move || {
                                let prompt_for = prompt_for.clone();
                                let st = st5.clone();
                                let submit_scroll = tui().config_context.clone();
                                let sid = sid.clone();
                                component(move || {
                                    prompt(PromptProps {
                                        session_id: Some(sid.clone()),
                                        visible: Some(Rc::new(|| true)),
                                        disabled: Rc::new(|| false),
                                        on_ref: {
                                            let prompt_for = prompt_for.clone();
                                            let shared = tui().prompt.clone();
                                            Rc::new(move |r: PromptRef| {
                                                *shared.borrow_mut() = Some(r.input);
                                                *prompt_for.borrow_mut() = Some(r);
                                            })
                                        },
                                        on_submit: {
                                            let st = st.clone();
                                            let submit_scroll = submit_scroll.clone();
                                            Rc::new(move |_value: String| {
                                                if submit_scroll.data_untracked().session.submit_scroll.as_deref() != Some("keep") {
                                                    st.to_bottom();
                                                }
                                            })
                                        },
                                    })
                                })
                            }),
                        ],
                        None,
                    ),
                    At::End,
                );
                View::list([View::Node(holder), View::Node(jump_row), View::Node(composer)])
            },
            None,
        ),
        At::Only,
    );
    view = View::list([view, View::Node(outer)]);
    view
}

/// `<SessionRowView row message boundaryID>`.
fn session_row_view(row: Signal<SessionRow>, boundary: Rc<dyn Fn() -> Option<String>>) -> View {
    let (target, set_target) = create_signal(None::<Element>);
    let _ = target;
    use_entry_anchor(
        move || match row.get() {
            SessionRow::Entry(entry) => Some(entry),
            SessionRow::Group(_) => None,
        },
        set_target,
    );
    let el = box_();
    set_target.set(Some(el));
    {
        let boundary = boundary.clone();
        bind(move || session_row_id(&row.get(), boundary().as_deref()), move |id| {
            if let Some(id) = id {
                el.set_id(&id)
            }
        });
    }
    el.set_margin(Edge::Top, Some(Value::Point(1.0)));
    el.set_flex_shrink(Some(0.0));
    let is_group = create_memo(move || matches!(row.get(), SessionRow::Group(_)));
    insert(
        el,
        switch(
            vec![
                when(move || is_group.get(), move || {
                    let group = create_memo(move || match row.get() {
                        SessionRow::Group(group) => Some(group),
                        _ => None,
                    });
                    session_group_view(Rc::new(move || group.get()), Rc::new(|entry, images| session_entry_view(entry, images)))
                }),
                when(move || !is_group.get(), move || {
                    let entry = create_memo(move || match row.get() {
                        SessionRow::Entry(entry) => Some(entry),
                        _ => None,
                    });
                    session_entry_view(Rc::new(move || entry.get()), None)
                }),
            ],
            None,
        ),
        At::Only,
    );
    View::Node(el)
}

/// The transcript's message accessor: `data.session.message.get(sessionID, messageID)`.
fn message_of(message_id: String) -> Rc<dyn Fn() -> Option<Rc<Json>>> {
    let t = tui();
    let data = t.data.clone();
    let ctx = super::render_context::use_ctx();
    Rc::new(move || data.store.session.message.get(&ctx.session_id, &message_id))
}

/// `<SessionEntryView row message images>`.
pub fn session_entry_view(entry: Rc<dyn Fn() -> Option<SessionEntry>>, images: Option<bool>) -> View {
    let kind = {
        let entry = entry.clone();
        create_memo(move || match entry() {
            Some(SessionEntry::Message { .. }) => 1,
            Some(SessionEntry::CompactionQueued { .. }) => 2,
            Some(SessionEntry::Part { .. }) => 3,
            Some(SessionEntry::AssistantFooter { .. }) => 4,
            Some(SessionEntry::TurnUsage { .. }) => 5,
            None => 0,
        })
    };
    let (e1, e2, e3, e4) = (entry.clone(), entry.clone(), entry.clone(), entry.clone());
    switch(
        vec![
            when(move || kind.get() == 1, move || {
                let id = match e1() {
                    Some(SessionEntry::Message { message_id }) => message_id,
                    _ => String::new(),
                };
                let message = message_of(id);
                show_value(
                    move || message(),
                    |message| {
                        let current: Message = Rc::new(move || message.get().unwrap_or_else(|| Rc::new(Json::Null)));
                        session_message_view(current)
                    },
                    None,
                )
            }),
            when(move || kind.get() == 2, compaction_queued),
            when(move || kind.get() == 3, move || {
                let Some(SessionEntry::Part { r#ref }) = e2() else { return View::Empty };
                session_part_view(message_of(r#ref.message_id), r#ref.part_id, images)
            }),
            when(move || kind.get() == 4, move || {
                let id = match e3() {
                    Some(SessionEntry::AssistantFooter { message_id }) => message_id,
                    _ => String::new(),
                };
                let message = message_of(id);
                show_value(
                    move || message(),
                    |message| {
                        let assistant = create_memo(move || message.get().is_some_and(|m| m.str("type") == Some("assistant")));
                        show(
                            move || assistant.get(),
                            move || assistant_footer(Rc::new(move || message.get().unwrap_or_else(|| Rc::new(Json::Null)))),
                            None,
                        )
                    },
                    None,
                )
            }),
            when(move || kind.get() == 5, move || match e4() {
                Some(SessionEntry::TurnUsage { message_ids, previous_cache }) => {
                    super::extras::turn_token_usage(message_ids, previous_cache)
                }
                _ => View::Empty,
            }),
        ],
        None,
    )
}
