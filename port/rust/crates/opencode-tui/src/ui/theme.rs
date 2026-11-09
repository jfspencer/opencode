//! `useTheme()` with `ThemeContextProvider` (`context/theme.tsx`): the theme of the enclosing surface. A
//! component inside a dialog reads the dialog surface without asking for it; `surface(name)` re-resolves on a
//! surface explicitly (absolute: every view's surfaces are the base theme's).
use opencode_tui_core::reactive::{Context, create_context, provide_context, use_context};

use crate::ctx::tui;
use crate::theme::{ResolvedTheme, SurfaceName};

thread_local! {
    static SURFACE: Context<Option<SurfaceName>> = create_context(None);
}

fn context() -> Context<Option<SurfaceName>> {
    SURFACE.with(|c| *c)
}

/// `useTheme()`: reads the theme signal (tracked) on the enclosing surface.
#[derive(Clone, Copy)]
pub struct ComponentTheme {
    surface: Option<SurfaceName>,
}

impl ComponentTheme {
    /// `theme` (the current tokens), tracked.
    pub fn get(&self) -> ResolvedTheme {
        let theme = tui().theme.get();
        match self.surface {
            Some(name) => theme.surface(name),
            None => theme,
        }
    }

    /// `useTheme().surface(name)`.
    pub fn surface(&self, name: SurfaceName) -> ComponentTheme {
        ComponentTheme { surface: Some(name) }
    }
}

pub fn use_theme() -> ComponentTheme {
    ComponentTheme { surface: use_context(context()).flatten() }
}

/// `<ThemeContextProvider context>` for what the current owner creates from now on.
pub fn provide_surface(name: Option<SurfaceName>) {
    let current = use_context(context()).flatten();
    provide_context(context(), name.or(current));
}
