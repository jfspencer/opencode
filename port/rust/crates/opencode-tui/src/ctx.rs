//! The TUI's contexts (the providers app.tsx nests around `<App />`) as one [`Tui`] value in a Solid context:
//! `useTuiApp`, `useArgs`, `useRoute`, `useTheme`, `useTerminalDimensions`, `useExit`, `usePromptRef`, `useToast`,
//! and the data and local selection the home screen reads. Components call [`tui()`].
//!
//! TODO(port-tui): opencode-tui-sync's data store (data.ts) and the TUI's data contexts. Until they land,
//! [`crate::data::Data`] loads what the home screen shows with direct client calls.
use std::cell::RefCell;
use std::rc::Rc;

use opencode_tui_core::reactive::{Context, Signal, create_context, create_signal_with, use_context};
use opencode_tui_core::tree::Element;

use crate::data::Data;
use crate::theme::ResolvedTheme;

/// `TuiApp`: the build identity.
#[derive(Clone, Debug, PartialEq)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub channel: String,
}

/// The TUI arguments (`context/args.tsx`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Args {
    pub continue_: bool,
    pub session_id: Option<String>,
    pub new_session_id: Option<String>,
    pub prompt: Option<String>,
    pub auto: bool,
    pub model: Option<String>,
    pub agent: Option<String>,
    pub fork: bool,
}

/// `route.data` (`context/route.tsx`).
#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    Home,
    Session { session_id: String },
}

/// The paths the TUI reads (`TuiPathsProvider`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Paths {
    pub cwd: String,
    pub home: String,
    pub state: String,
}

/// Everything the providers give the components.
#[derive(Clone)]
pub struct Tui {
    pub app: Rc<AppInfo>,
    pub args: Rc<Args>,
    pub paths: Rc<Paths>,
    pub route: Signal<Route>,
    pub theme: Signal<ResolvedTheme>,
    /// `useTerminalDimensions()`: (width, height).
    pub dimensions: Signal<(u32, u32)>,
    pub data: Rc<Data>,
    pub config: Rc<crate::config::Resolved>,
    /// `usePromptRef()`: the prompt's textarea.
    pub prompt: Rc<RefCell<Option<Element>>>,
    /// `useExit()`.
    pub exit: Rc<dyn Fn()>,
    /// `useToast()`.
    pub toast: crate::toast::Toasts,
}

thread_local! {
    static TUI: Context<Tui> = create_context(None);
}

pub fn context() -> Context<Tui> {
    TUI.with(|c| *c)
}

/// The TUI's contexts (`useX()` of each provider).
pub fn tui() -> Tui {
    use_context(context()).expect("tui() outside the TUI's providers")
}

pub fn theme_signal(theme: ResolvedTheme) -> Signal<ResolvedTheme> {
    create_signal_with(theme, None).0
}
