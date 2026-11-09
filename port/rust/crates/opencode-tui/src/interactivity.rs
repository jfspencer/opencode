//! `context/interactivity.tsx`: whether a subtree takes input (keymap layers, native input and focus handlers).
//! Disabling a subtree also disables every nested provider.
use std::rc::Rc;

use opencode_tui_core::reactive::{Context, create_context, create_memo, provide_context, untrack, use_context};
use opencode_tui_core::solid::{View, memo_view};

pub type Interactive = Rc<dyn Fn() -> bool>;

fn enabled() -> Interactive {
    Rc::new(|| true)
}

thread_local! {
    static CONTEXT: Context<Interactive> = create_context(Some(enabled));
}

fn context() -> Context<Interactive> {
    CONTEXT.with(|c| *c)
}

/// `useInteractivity()`: enabled by default.
pub fn use_interactivity() -> Interactive {
    use_context(context()).unwrap_or_else(enabled)
}

/// `<InteractivityProvider enabled>`: its children see `parent() && enabled()` (the provider's computation owns
/// them, as Solid's `Context.Provider`).
pub fn interactivity_provider(enabled: impl Fn() -> bool + 'static, children: impl Fn() -> View + 'static) -> View {
    let parent = use_interactivity();
    let memo = create_memo(move || parent() && enabled());
    memo_view(move || {
        provide_context(context(), Rc::new(move || memo.get()) as Interactive);
        untrack(&children)
    })
}
