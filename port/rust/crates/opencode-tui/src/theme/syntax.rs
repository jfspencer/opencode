//! `generateSyntax(theme, owner)` (`packages/theme/src/tui/syntax.ts`): the syntax-highlighting rules of a theme.
//! The rules are returned as data, in order; building a `SyntaxStyle` from them (`SyntaxStyle.fromTheme`) is the
//! core's.

use super::resolved::Tokens;
use super::value::Color;

/// `ThemeTokenStyle["style"]`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TokenStyle {
    pub foreground: Option<Color>,
    pub background: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// `ThemeTokenStyle`: scopes and their style.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeTokenStyle {
    pub scope: Vec<&'static str>,
    pub style: TokenStyle,
}

#[derive(Default, Clone, Copy)]
struct Flags {
    background: Option<Color>,
    bold: bool,
    italic: bool,
    underline: bool,
}

fn rule(scope: &[&'static str], foreground: Color, flags: Flags) -> ThemeTokenStyle {
    ThemeTokenStyle {
        scope: scope.to_vec(),
        style: TokenStyle {
            foreground: Some(foreground),
            background: flags.background,
            bold: flags.bold,
            italic: flags.italic,
            underline: flags.underline,
        },
    }
}

const NONE: Flags = Flags { background: None, bold: false, italic: false, underline: false };
const BOLD: Flags = Flags { bold: true, ..NONE };
const ITALIC: Flags = Flags { italic: true, ..NONE };
const UNDERLINE: Flags = Flags { underline: true, ..NONE };
const BOLD_ITALIC: Flags = Flags { bold: true, italic: true, ..NONE };

/// The rules, in `generateSyntax`'s order.
pub fn generate_syntax(theme: &Tokens) -> Vec<ThemeTokenStyle> {
    let step = 200;
    let syntax = &theme.syntax;
    let markdown = &theme.markdown;
    let feedback = &theme.text.feedback;
    let categorical = |i: usize| theme.categorical.get(i).or_else(|| theme.categorical.first()).map(|s| s[step]);
    let fallback = theme.text.base;
    let bg = |color: Color| Flags { background: Some(color), ..NONE };
    vec![
        rule(&["default"], theme.text.base, NONE),
        rule(&["prompt"], theme.hue.accent[step], NONE),
        rule(&["extmark.file"], feedback.warning.base, BOLD),
        rule(&["extmark.agent"], categorical(0).unwrap_or(fallback), BOLD),
        rule(&["extmark.skill"], categorical(1).unwrap_or(fallback), BOLD),
        rule(&["extmark.paste"], theme.text.action.primary.focused, Flags { background: Some(feedback.warning.base), bold: true, ..NONE }),
        rule(&["comment", "comment.documentation"], syntax.comment, ITALIC),
        rule(&["string", "symbol", "character.special", "character"], syntax.string, NONE),
        rule(&["number", "boolean", "constant", "float"], syntax.number, NONE),
        rule(&["keyword.return", "keyword.conditional", "keyword.repeat", "keyword.coroutine"], syntax.keyword, ITALIC),
        rule(&["keyword.type"], syntax.type_, BOLD_ITALIC),
        rule(&["keyword.function", "function.method"], syntax.function, NONE),
        rule(&["keyword"], syntax.keyword, ITALIC),
        rule(&["keyword.import", "string.escape", "string.regexp", "tag.attribute", "keyword.export"], syntax.keyword, NONE),
        rule(&["operator", "keyword.operator", "punctuation.delimiter", "keyword.conditional.ternary"], syntax.operator, NONE),
        rule(
            &["variable", "variable.parameter", "function.method.call", "function.call", "property", "parameter", "field"],
            syntax.variable,
            NONE,
        ),
        rule(&["variable.member", "function", "constructor"], syntax.function, NONE),
        rule(&["type", "module", "class", "namespace"], syntax.type_, NONE),
        rule(&["type.definition"], syntax.type_, BOLD),
        rule(&["punctuation", "punctuation.bracket"], syntax.punctuation, NONE),
        rule(
            &["variable.builtin", "type.builtin", "function.builtin", "module.builtin", "constant.builtin", "variable.super"],
            feedback.error.base,
            NONE,
        ),
        rule(&["keyword.directive", "keyword.modifier", "keyword.exception"], syntax.keyword, ITALIC),
        rule(&["punctuation.special", "tag.delimiter"], syntax.operator, NONE),
        rule(
            &["markup.heading", "markup.heading.2", "markup.heading.3", "markup.heading.4", "markup.heading.5", "markup.heading.6"],
            markdown.heading,
            BOLD,
        ),
        rule(&["markup.heading.1"], markdown.heading, Flags { bold: true, underline: true, ..NONE }),
        rule(&["markup.bold", "markup.strong"], markdown.strong, BOLD),
        rule(&["markup.italic"], markdown.emphasis, ITALIC),
        rule(&["markup.list"], markdown.list_item, NONE),
        rule(&["markup.quote"], markdown.block_quote, ITALIC),
        rule(&["markup.raw", "markup.raw.block"], markdown.code, NONE),
        rule(&["markup.raw.inline"], markdown.code, bg(theme.background.base)),
        rule(&["markup.link", "markup.link.url", "string.special", "string.special.url"], markdown.link, UNDERLINE),
        rule(&["markup.link.label"], markdown.link_text, UNDERLINE),
        rule(&["label"], markdown.link_text, NONE),
        rule(&["spell", "nospell"], theme.text.base, NONE),
        rule(&["markup.underline"], theme.text.base, UNDERLINE),
        rule(&["comment.error"], feedback.error.base, BOLD_ITALIC),
        rule(&["comment.warning"], feedback.warning.base, BOLD_ITALIC),
        rule(&["comment.todo", "comment.note"], feedback.info.base, BOLD_ITALIC),
        rule(&["attribute", "annotation"], feedback.warning.base, NONE),
        rule(&["tag"], feedback.error.base, NONE),
        rule(&["markup.strikethrough", "markup.list.unchecked", "debug"], theme.text.muted, NONE),
        rule(&["markup.list.checked"], feedback.success.base, NONE),
        rule(&["diff.plus"], theme.diff.text.added, bg(theme.diff.background.added)),
        rule(&["diff.minus"], theme.diff.text.removed, bg(theme.diff.background.removed)),
        rule(&["diff.delta"], theme.diff.text.context, bg(theme.diff.background.context)),
        rule(&["error"], feedback.error.base, BOLD),
        rule(&["warning"], feedback.warning.base, BOLD),
        rule(&["info"], feedback.info.base, NONE),
    ]
}
