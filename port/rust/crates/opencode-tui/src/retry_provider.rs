//! `component/retry-provider.tsx` (`<RetryProvider value enabled>`): the web search provider's name inside a tool
//! row's text, as an inline span.
//!
//! TODO(port-tui): the transition (fade out, resize, fade in over 220 ms on the frame callbacks) when a running
//! search retries with another provider while on screen. Without it the span shows the target name at once,
//! which is what TS shows with animations off and whenever the transition does not start.
use std::rc::Rc;

use opencode_tui_core::elements::span;
use opencode_tui_core::reactive::*;
use opencode_tui_core::solid::*;

/// `Value`: the search's identity, its provider and whether it is still running.
#[derive(Clone, Debug, PartialEq)]
pub struct RetryValue {
    pub id: String,
    pub provider: String,
    pub running: bool,
}

/// `webSearchProviderName(provider)` (util/tool-display.ts).
pub fn web_search_provider_name(provider: &str) -> String {
    if provider.is_empty() {
        return String::new();
    }
    if provider == "opencode" {
        return "OpenCode".into();
    }
    let mut chars = provider.chars();
    let first: String = chars.next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
    format!("{first}{}", chars.as_str())
}

/// `<RetryProvider value enabled>`.
pub fn retry_provider(value: Rc<dyn Fn() -> RetryValue>, enabled: Rc<dyn Fn() -> bool>) -> View {
    let _ = enabled;
    let el = span();
    let name = create_memo(move || web_search_provider_name(&value().provider));
    insert(el, View::dynamic(move || View::from(name.get())), At::Only);
    View::Node(el)
}
