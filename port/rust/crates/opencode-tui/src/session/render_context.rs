//! `routes/session/render-context.tsx`: what the Session route gives its transcript rows.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::reactive::{Context, create_context, use_context};
use opencode_tui_core::tree::Element;

use super::anchors::TimelineAnchors;
use super::grouping::GroupKind;

/// `ThinkingMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThinkingMode {
    Show,
    Hide,
}

impl ThinkingMode {
    pub fn parse(value: Option<&str>) -> ThinkingMode {
        match value {
            Some("show") => ThinkingMode::Show,
            _ => ThinkingMode::Hide,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ThinkingMode::Show => "show",
            ThinkingMode::Hide => "hide",
        }
    }

    /// `nextThinkingMode(current)`: show → hide → show.
    pub fn next(self) -> ThinkingMode {
        match self {
            ThinkingMode::Show => ThinkingMode::Hide,
            ThinkingMode::Hide => ThinkingMode::Show,
        }
    }
}

/// `"source" | "rendered"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkdownMode {
    Source,
    Rendered,
}

/// `"word" | "none"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffWrapMode {
    Word,
    None,
}

/// `PendingAction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingAction {
    Steer,
    Queue,
    Cancel,
}

/// The context value (`context` in render-context.tsx).
#[derive(Clone)]
pub struct RenderCtx {
    /// Content width: terminal width minus vertical tabs, sidebar, and padding.
    pub width: Rc<dyn Fn() -> i32>,
    /// The shared reactive terminal size; transcript rows read these instead of their own resize listener.
    pub terminal_width: Rc<dyn Fn() -> u32>,
    pub terminal_height: Rc<dyn Fn() -> u32>,
    pub session_id: String,
    pub anchors: TimelineAnchors,
    /// Saved disclosure, falling back to the verbosity default for the group kind.
    pub group_expanded: Rc<dyn Fn(&str, GroupKind) -> bool>,
    /// An anchor keeps its viewport row through the layout change the toggle causes.
    pub set_group_expanded: Rc<dyn Fn(&str, bool, Option<Element>)>,
    pub thinking_mode: Rc<dyn Fn() -> ThinkingMode>,
    pub markdown_mode: Rc<dyn Fn() -> MarkdownMode>,
    pub group_exploration: Rc<dyn Fn() -> bool>,
    pub diff_wrap_mode: Rc<dyn Fn() -> DiffWrapMode>,
    pub models: Rc<dyn Fn() -> Rc<Vec<Rc<Json>>>>,
    pub message_index: Rc<dyn Fn(&str) -> Option<usize>>,
    /// True when the session has no idle markers, so turn footers end at the next prompt.
    pub legacy_turns: Rc<dyn Fn() -> bool>,
    pub config: Rc<dyn Fn() -> Rc<crate::config::Resolved>>,
    pub mutate_pending: Rc<dyn Fn(PendingAction, &str) -> opencode_tui_sync::runtime::Deferred<bool>>,
    pub pending_delivery: Rc<dyn Fn(&str) -> Option<String>>,
    /// Opens the pending-steer dialog of a user message (move to queue, delete).
    pub open_pending_steer: Rc<dyn Fn(String)>,
}

thread_local! {
    static CONTEXT: Context<RenderCtx> = create_context(None);
}

pub fn context() -> Context<RenderCtx> {
    CONTEXT.with(|c| *c)
}

/// `use()`.
pub fn use_ctx() -> RenderCtx {
    use_context(context()).expect("useContext must be used within a Session component")
}
