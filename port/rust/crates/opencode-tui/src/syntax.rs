//! The theme's syntax style (`useThemes().currentSyntax`: `generateSyntax(tokens)` through
//! `SyntaxStyle.fromTheme`), as a memo over the theme, and `generateThinkingSyntax` (routes/session/thinking-syntax.ts).
//!
//! TODO(port-tui): the theme context: `currentSyntax` belongs to ThemeProvider; until it carries one, the memo
//! lives in a root of its own, created on first use.
use std::cell::RefCell;
use std::rc::Rc;

use opencode_tui_core::color::RGBA;
use opencode_tui_core::reactive::*;
use opencode_tui_markup::style::{Rgba, StyleDefinition, SyntaxStyle};

use crate::ctx::tui;
use crate::theme::ResolvedTheme;
use crate::util::rgba;

fn to_rgba(color: RGBA) -> Rgba {
    Rgba(color.0)
}

/// `SyntaxStyle.fromTheme(generateSyntax(theme))` (`convertThemeToStyles`: a later rule for a scope replaces the
/// earlier one in its first place).
pub fn syntax_style(theme: &ResolvedTheme) -> SyntaxStyle {
    let mut style = SyntaxStyle::new();
    for rule in crate::theme::syntax::generate_syntax(theme) {
        let definition = StyleDefinition {
            fg: rule.style.foreground.map(|c| to_rgba(rgba(c))),
            bg: rule.style.background.map(|c| to_rgba(rgba(c))),
            bold: rule.style.bold.then_some(true),
            italic: rule.style.italic.then_some(true),
            underline: rule.style.underline.then_some(true),
            dim: None,
        };
        for scope in &rule.scope {
            style.register_style(scope, definition);
        }
    }
    style
}

thread_local! {
    static CURRENT: RefCell<Option<Memo<Rc<SyntaxStyle>>>> = const { RefCell::new(None) };
}

/// `currentSyntax()`: the theme's syntax style, a new object when the theme changes.
pub fn current_syntax() -> Rc<SyntaxStyle> {
    let memo = CURRENT.with(|cell| {
        if let Some(memo) = *cell.borrow() {
            return memo;
        }
        let theme = tui().theme;
        let memo = create_root_unowned(move || create_memo_equals(move || Rc::new(syntax_style(&theme.get())), None));
        *cell.borrow_mut() = Some(memo);
        memo
    });
    memo.get()
}

/// `generateThinkingSyntax(syntax, foreground)`: every registered style with the given foreground.
pub fn thinking_syntax(syntax: &SyntaxStyle, foreground: RGBA) -> SyntaxStyle {
    let fg = to_rgba(foreground);
    SyntaxStyle::from_styles(syntax.registered_names().iter().map(|name| {
        let style = syntax.get_style(name).unwrap_or_default();
        (name.as_str(), StyleDefinition { fg: Some(fg), ..style })
    }))
}
