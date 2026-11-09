//! The resolved theme (`ResolvedTheme`, `packages/theme/src/tui/types.ts`) as a plain struct tree: a component port
//! reads `theme.text.muted` as `theme.text.muted`, `theme.text.action.primary.selected` as
//! `theme.text.action.primary.selected`, `theme.hue.accent[200]` as `theme.hue.accent[200]`. Token names are
//! snake_case (`hunk_header`, `line_number`, `link_text`, ...), `type` is `type_`.
//!
//! A [`ResolvedTheme`] is one view (the base surface, `dialog` or `dialog.footer`) of one resolution; it is cheap to
//! clone (an `Rc`) and derefs to its [`Tokens`].

use std::rc::Rc;

use opencode_protocol::Json;
use opencode_protocol::wire::Object;

use super::resolve::HueTable;
use super::rgba::RGBA;
use super::schema::HUE_STEPS;
use super::value::{Color, Value};

/// A hue's nine steps, indexed by step: `scale[200]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HueScale(pub [Color; 9]);

impl HueScale {
    /// The color of a step (`100`..`900`), if it is one.
    pub fn step(&self, step: u16) -> Option<Color> {
        HUE_STEPS.iter().position(|s| *s == step).map(|i| self.0[i])
    }
}

impl std::ops::Index<u16> for HueScale {
    type Output = Color;
    fn index(&self, step: u16) -> &Color {
        let i = HUE_STEPS.iter().position(|s| *s == step).unwrap_or_else(|| panic!("no hue step {step}"));
        &self.0[i]
    }
}

/// `ResolvedThemeTokens.hue`: the three semantic hues, and every hue by name.
#[derive(Clone, Debug)]
pub struct Hue {
    pub accent: HueScale,
    pub interactive: HueScale,
    pub neutral: HueScale,
    /// Every hue of the theme in property order (the semantic ones included).
    pub all: Vec<(String, HueScale)>,
}

impl Hue {
    pub fn get(&self, name: &str) -> Option<&HueScale> {
        self.all.iter().find(|(n, _)| n == name).map(|(_, s)| s)
    }
}

/// `ActionStates`: which states apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActionStates {
    pub disabled: bool,
    pub pressed: bool,
    pub focused: bool,
    pub selected: bool,
    pub hovered: bool,
}

/// `StatefulColor`: a color per action state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatefulColor {
    pub base: Color,
    pub disabled: Color,
    pub pressed: Color,
    pub focused: Color,
    pub selected: Color,
    pub hovered: Color,
}

impl StatefulColor {
    /// `state(states)`: the first state that applies, in `disabled`, `pressed`, `focused`, `selected`, `hovered`
    /// order, else the base.
    pub fn state(&self, states: ActionStates) -> Color {
        if states.disabled {
            self.disabled
        } else if states.pressed {
            self.pressed
        } else if states.focused {
            self.focused
        } else if states.selected {
            self.selected
        } else if states.hovered {
            self.hovered
        } else {
            self.base
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Actions {
    pub primary: StatefulColor,
    pub secondary: StatefulColor,
    pub destructive: StatefulColor,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Feedback<T> {
    pub error: T,
    pub warning: T,
    pub success: T,
    pub info: T,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextFeedback {
    pub base: Color,
    pub muted: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BackgroundFeedback {
    pub base: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Text {
    pub base: Color,
    pub muted: Color,
    pub faint: Color,
    pub action: Actions,
    pub formfield: StatefulColor,
    pub feedback: Feedback<TextFeedback>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Raised {
    pub base: Color,
    pub high: Color,
    pub max: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Background {
    pub base: Color,
    pub raised: Raised,
    pub action: Actions,
    pub formfield: StatefulColor,
    pub feedback: Feedback<BackgroundFeedback>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Single {
    pub base: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiffText {
    pub added: Color,
    pub removed: Color,
    pub context: Color,
    pub hunk_header: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiffBackground {
    pub added: Color,
    pub removed: Color,
    pub context: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AddedRemoved {
    pub added: Color,
    pub removed: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineNumber {
    pub text: Color,
    pub background: AddedRemoved,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Diff {
    pub text: DiffText,
    pub background: DiffBackground,
    pub highlight: AddedRemoved,
    pub line_number: LineNumber,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Syntax {
    pub comment: Color,
    pub keyword: Color,
    pub function: Color,
    pub variable: Color,
    pub string: Color,
    pub number: Color,
    pub type_: Color,
    pub operator: Color,
    pub punctuation: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Markdown {
    pub text: Color,
    pub heading: Color,
    pub link: Color,
    pub link_text: Color,
    pub code: Color,
    pub block_quote: Color,
    pub emphasis: Color,
    pub strong: Color,
    pub horizontal_rule: Color,
    pub list_item: Color,
    pub list_enumeration: Color,
    pub image: Color,
    pub image_text: Color,
    pub code_block: Color,
}

/// One view's tokens (`ResolvedThemeTokens` without the functions, which are methods of [`ResolvedTheme`]).
#[derive(Clone, Debug)]
pub struct Tokens {
    pub hue: Hue,
    pub categorical: Vec<HueScale>,
    pub text: Text,
    pub background: Background,
    pub border: Single,
    pub scrollbar: Single,
    pub diff: Diff,
    pub syntax: Syntax,
    pub markdown: Markdown,
}

/// A raised surface a view re-resolves the theme on (`SurfaceName`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SurfaceName {
    Dialog,
    DialogFooter,
}

impl SurfaceName {
    pub fn parse(name: &str) -> Option<SurfaceName> {
        match name {
            "dialog" => Some(SurfaceName::Dialog),
            "dialog.footer" => Some(SurfaceName::DialogFooter),
            _ => None,
        }
    }
}

/// The views of one resolution.
pub(crate) struct Views {
    pub base: Tokens,
    pub dialog: Tokens,
    /// `None`: the footer is the dialog view itself.
    pub footer: Option<Tokens>,
    pub hue: HueTable,
    pub theme: u32,
}

/// `ResolvedTheme`: a view of a resolved theme.
#[derive(Clone)]
pub struct ResolvedTheme {
    views: Rc<Views>,
    view: u8,
}

/// `HueSource`: the hue and step of a scale color.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HueSource {
    pub hue: String,
    pub step: u16,
}

impl ResolvedTheme {
    pub(crate) fn new(views: Rc<Views>) -> ResolvedTheme {
        ResolvedTheme { views, view: 0 }
    }

    pub fn tokens(&self) -> &Tokens {
        match self.view {
            0 => &self.views.base,
            1 => &self.views.dialog,
            _ => self.views.footer.as_ref().unwrap_or(&self.views.dialog),
        }
    }

    /// `surface(name)`: the theme re-resolved on a raised surface. Absolute: every view's surfaces are the base
    /// theme's.
    pub fn surface(&self, name: SurfaceName) -> ResolvedTheme {
        ResolvedTheme { views: self.views.clone(), view: if name == SurfaceName::Dialog { 1 } else { 2 } }
    }

    /// `source(color)`: the hue scale step a color is, when it is one of this theme's scale colors.
    pub fn source(&self, color: Color) -> Option<HueSource> {
        let origin = color.origin.filter(|o| o.theme == self.views.theme)?;
        Some(HueSource {
            hue: self.views.hue.names[origin.hue as usize].clone(),
            step: HUE_STEPS[origin.position as usize],
        })
    }

    fn shift(&self, color: Color, amount: f64) -> Color {
        let Some(origin) = color.origin.filter(|o| o.theme == self.views.theme) else { return color };
        let offset = if amount.is_finite() { amount.trunc() } else { 0.0 };
        let position = (origin.position as f64 + offset).clamp(0.0, (HUE_STEPS.len() - 1) as f64) as usize;
        self.views.hue.scales[origin.hue as usize].0[position]
    }

    /// `increase(color, amount = 1)`: the step `amount` above, within the color's scale.
    pub fn increase(&self, color: Color, amount: f64) -> Color {
        self.shift(color, amount)
    }

    /// `decrease(color, amount = 1)`: the step `amount` below, within the color's scale.
    pub fn decrease(&self, color: Color, amount: f64) -> Color {
        self.shift(color, -amount)
    }

    /// Every token of the view as JSON (hex colors, with `{hue, step}` for scale colors), for tests and the
    /// differential harness.
    pub fn to_json(&self) -> Json {
        let t = self.tokens();
        let mut o = Object::new();
        let c = |color: Color| self.color_json(color);
        let scale = |s: &HueScale| Json::Array(s.0.iter().map(|x| Json::String(x.to_hex())).collect());
        o.insert("hue".into(), Json::Object(t.hue.all.iter().map(|(n, s)| (n.clone(), scale(s))).collect()));
        o.insert("categorical".into(), Json::Array(t.categorical.iter().map(scale).collect()));
        let stateful = |s: &StatefulColor| {
            obj([
                ("base", c(s.base)),
                ("disabled", c(s.disabled)),
                ("pressed", c(s.pressed)),
                ("focused", c(s.focused)),
                ("selected", c(s.selected)),
                ("hovered", c(s.hovered)),
            ])
        };
        let actions = |a: &Actions| {
            obj([("primary", stateful(&a.primary)), ("secondary", stateful(&a.secondary)), ("destructive", stateful(&a.destructive))])
        };
        let feedback_text = |f: &Feedback<TextFeedback>| {
            let item = |x: &TextFeedback| obj([("base", c(x.base)), ("muted", c(x.muted))]);
            obj([("error", item(&f.error)), ("warning", item(&f.warning)), ("success", item(&f.success)), ("info", item(&f.info))])
        };
        let feedback_bg = |f: &Feedback<BackgroundFeedback>| {
            let item = |x: &BackgroundFeedback| obj([("base", c(x.base))]);
            obj([("error", item(&f.error)), ("warning", item(&f.warning)), ("success", item(&f.success)), ("info", item(&f.info))])
        };
        o.insert(
            "text".into(),
            obj([
                ("base", c(t.text.base)),
                ("muted", c(t.text.muted)),
                ("faint", c(t.text.faint)),
                ("action", actions(&t.text.action)),
                ("formfield", stateful(&t.text.formfield)),
                ("feedback", feedback_text(&t.text.feedback)),
            ]),
        );
        o.insert(
            "background".into(),
            obj([
                ("base", c(t.background.base)),
                ("raised", obj([("base", c(t.background.raised.base)), ("high", c(t.background.raised.high)), ("max", c(t.background.raised.max))])),
                ("action", actions(&t.background.action)),
                ("formfield", stateful(&t.background.formfield)),
                ("feedback", feedback_bg(&t.background.feedback)),
            ]),
        );
        o.insert("border".into(), obj([("base", c(t.border.base))]));
        o.insert("scrollbar".into(), obj([("base", c(t.scrollbar.base))]));
        let d = &t.diff;
        o.insert(
            "diff".into(),
            obj([
                (
                    "text",
                    obj([("added", c(d.text.added)), ("removed", c(d.text.removed)), ("context", c(d.text.context)), ("hunkHeader", c(d.text.hunk_header))]),
                ),
                ("background", obj([("added", c(d.background.added)), ("removed", c(d.background.removed)), ("context", c(d.background.context))])),
                ("highlight", obj([("added", c(d.highlight.added)), ("removed", c(d.highlight.removed))])),
                (
                    "lineNumber",
                    obj([
                        ("text", c(d.line_number.text)),
                        ("background", obj([("added", c(d.line_number.background.added)), ("removed", c(d.line_number.background.removed))])),
                    ]),
                ),
            ]),
        );
        let s = &t.syntax;
        o.insert(
            "syntax".into(),
            obj([
                ("comment", c(s.comment)),
                ("keyword", c(s.keyword)),
                ("function", c(s.function)),
                ("variable", c(s.variable)),
                ("string", c(s.string)),
                ("number", c(s.number)),
                ("type", c(s.type_)),
                ("operator", c(s.operator)),
                ("punctuation", c(s.punctuation)),
            ]),
        );
        let m = &t.markdown;
        o.insert(
            "markdown".into(),
            obj([
                ("text", c(m.text)),
                ("heading", c(m.heading)),
                ("link", c(m.link)),
                ("linkText", c(m.link_text)),
                ("code", c(m.code)),
                ("blockQuote", c(m.block_quote)),
                ("emphasis", c(m.emphasis)),
                ("strong", c(m.strong)),
                ("horizontalRule", c(m.horizontal_rule)),
                ("listItem", c(m.list_item)),
                ("listEnumeration", c(m.list_enumeration)),
                ("image", c(m.image)),
                ("imageText", c(m.image_text)),
                ("codeBlock", c(m.code_block)),
            ]),
        );
        Json::Object(o)
    }

    /// Every token color of the view with its TS path (`text.action.primary.base`, `diff.lineNumber.text`, ...), in
    /// the views' field order.
    pub fn colors(&self) -> Vec<(String, Color)> {
        let t = self.tokens();
        let mut out: Vec<(String, Color)> = Vec::new();
        let mut put = |path: &str, color: Color| out.push((path.to_owned(), color));
        let stateful = |put: &mut dyn FnMut(&str, Color), prefix: &str, s: &StatefulColor| {
            for (state, color) in [
                ("base", s.base),
                ("disabled", s.disabled),
                ("pressed", s.pressed),
                ("focused", s.focused),
                ("selected", s.selected),
                ("hovered", s.hovered),
            ] {
                put(&format!("{prefix}.{state}"), color);
            }
        };
        put("text.base", t.text.base);
        put("text.muted", t.text.muted);
        put("text.faint", t.text.faint);
        for (variant, s) in [("primary", &t.text.action.primary), ("secondary", &t.text.action.secondary), ("destructive", &t.text.action.destructive)] {
            stateful(&mut put, &format!("text.action.{variant}"), s);
        }
        stateful(&mut put, "text.formfield", &t.text.formfield);
        let f = &t.text.feedback;
        for (kind, x) in [("error", &f.error), ("warning", &f.warning), ("success", &f.success), ("info", &f.info)] {
            put(&format!("text.feedback.{kind}.base"), x.base);
            put(&format!("text.feedback.{kind}.muted"), x.muted);
        }
        put("background.base", t.background.base);
        put("background.raised.base", t.background.raised.base);
        put("background.raised.high", t.background.raised.high);
        put("background.raised.max", t.background.raised.max);
        for (variant, s) in [
            ("primary", &t.background.action.primary),
            ("secondary", &t.background.action.secondary),
            ("destructive", &t.background.action.destructive),
        ] {
            stateful(&mut put, &format!("background.action.{variant}"), s);
        }
        stateful(&mut put, "background.formfield", &t.background.formfield);
        let f = &t.background.feedback;
        for (kind, x) in [("error", &f.error), ("warning", &f.warning), ("success", &f.success), ("info", &f.info)] {
            put(&format!("background.feedback.{kind}.base"), x.base);
        }
        put("border.base", t.border.base);
        put("scrollbar.base", t.scrollbar.base);
        let d = &t.diff;
        put("diff.text.added", d.text.added);
        put("diff.text.removed", d.text.removed);
        put("diff.text.context", d.text.context);
        put("diff.text.hunkHeader", d.text.hunk_header);
        put("diff.background.added", d.background.added);
        put("diff.background.removed", d.background.removed);
        put("diff.background.context", d.background.context);
        put("diff.highlight.added", d.highlight.added);
        put("diff.highlight.removed", d.highlight.removed);
        put("diff.lineNumber.text", d.line_number.text);
        put("diff.lineNumber.background.added", d.line_number.background.added);
        put("diff.lineNumber.background.removed", d.line_number.background.removed);
        let y = &t.syntax;
        for (name, color) in [
            ("comment", y.comment),
            ("keyword", y.keyword),
            ("function", y.function),
            ("variable", y.variable),
            ("string", y.string),
            ("number", y.number),
            ("type", y.type_),
            ("operator", y.operator),
            ("punctuation", y.punctuation),
        ] {
            put(&format!("syntax.{name}"), color);
        }
        let m = &t.markdown;
        for (name, color) in [
            ("text", m.text),
            ("heading", m.heading),
            ("link", m.link),
            ("linkText", m.link_text),
            ("code", m.code),
            ("blockQuote", m.block_quote),
            ("emphasis", m.emphasis),
            ("strong", m.strong),
            ("horizontalRule", m.horizontal_rule),
            ("listItem", m.list_item),
            ("listEnumeration", m.list_enumeration),
            ("image", m.image),
            ("imageText", m.image_text),
            ("codeBlock", m.code_block),
        ] {
            put(&format!("markdown.{name}"), color);
        }
        out
    }

    fn color_json(&self, color: Color) -> Json {
        let hex = Json::String(color.to_hex());
        match self.source(color) {
            None => hex,
            Some(source) => obj([("hex", hex), ("hue", Json::String(source.hue)), ("step", Json::Number(source.step as f64))]),
        }
    }
}

impl std::ops::Deref for ResolvedTheme {
    type Target = Tokens;
    fn deref(&self) -> &Tokens {
        self.tokens()
    }
}

impl std::fmt::Debug for ResolvedTheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedTheme").field("view", &self.view).finish_non_exhaustive()
    }
}

fn obj<const N: usize>(entries: [(&str, Json); N]) -> Json {
    Json::Object(entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

/// Builds a view's tokens from its resolved tree (`resolveView`'s result). A token the tree lacks (TS: `undefined`)
/// is transparent black.
pub(crate) fn tokens_of(resolved: &Value, hue: &HueTable, categorical: &[HueScale]) -> Tokens {
    let color = |path: &[&str]| -> Color {
        let mut value = resolved;
        for key in path {
            value = value.at(key);
        }
        match value {
            Value::Color(c) => *c,
            _ => Color::plain(RGBA::default()),
        }
    };
    let stateful = |prefix: &[&str]| -> StatefulColor {
        let at = |state: &str| {
            let mut path = prefix.to_vec();
            path.push(state);
            color(&path)
        };
        StatefulColor {
            base: at("base"),
            disabled: at("disabled"),
            pressed: at("pressed"),
            focused: at("focused"),
            selected: at("selected"),
            hovered: at("hovered"),
        }
    };
    let actions = |group: &str| Actions {
        primary: stateful(&[group, "action", "primary"]),
        secondary: stateful(&[group, "action", "secondary"]),
        destructive: stateful(&[group, "action", "destructive"]),
    };
    let scale_of = |name: &str| hue.get(name).copied().unwrap_or(HueScale([Color::plain(RGBA::default()); 9]));
    let text_feedback =
        |kind: &str| TextFeedback { base: color(&["text", "feedback", kind, "base"]), muted: color(&["text", "feedback", kind, "muted"]) };
    let background_feedback = |kind: &str| BackgroundFeedback { base: color(&["background", "feedback", kind, "base"]) };
    Tokens {
        hue: Hue {
            accent: scale_of("accent"),
            interactive: scale_of("interactive"),
            neutral: scale_of("neutral"),
            all: hue.names.iter().cloned().zip(hue.scales.iter().copied()).collect(),
        },
        categorical: categorical.to_vec(),
        text: Text {
            base: color(&["text", "base"]),
            muted: color(&["text", "muted"]),
            faint: color(&["text", "faint"]),
            action: actions("text"),
            formfield: stateful(&["text", "formfield"]),
            feedback: Feedback {
                error: text_feedback("error"),
                warning: text_feedback("warning"),
                success: text_feedback("success"),
                info: text_feedback("info"),
            },
        },
        background: Background {
            base: color(&["background", "base"]),
            raised: Raised {
                base: color(&["background", "raised", "base"]),
                high: color(&["background", "raised", "high"]),
                max: color(&["background", "raised", "max"]),
            },
            action: actions("background"),
            formfield: stateful(&["background", "formfield"]),
            feedback: Feedback {
                error: background_feedback("error"),
                warning: background_feedback("warning"),
                success: background_feedback("success"),
                info: background_feedback("info"),
            },
        },
        border: Single { base: color(&["border", "base"]) },
        scrollbar: Single { base: color(&["scrollbar", "base"]) },
        diff: Diff {
            text: DiffText {
                added: color(&["diff", "text", "added"]),
                removed: color(&["diff", "text", "removed"]),
                context: color(&["diff", "text", "context"]),
                hunk_header: color(&["diff", "text", "hunkHeader"]),
            },
            background: DiffBackground {
                added: color(&["diff", "background", "added"]),
                removed: color(&["diff", "background", "removed"]),
                context: color(&["diff", "background", "context"]),
            },
            highlight: AddedRemoved { added: color(&["diff", "highlight", "added"]), removed: color(&["diff", "highlight", "removed"]) },
            line_number: LineNumber {
                text: color(&["diff", "lineNumber", "text"]),
                background: AddedRemoved {
                    added: color(&["diff", "lineNumber", "background", "added"]),
                    removed: color(&["diff", "lineNumber", "background", "removed"]),
                },
            },
        },
        syntax: Syntax {
            comment: color(&["syntax", "comment"]),
            keyword: color(&["syntax", "keyword"]),
            function: color(&["syntax", "function"]),
            variable: color(&["syntax", "variable"]),
            string: color(&["syntax", "string"]),
            number: color(&["syntax", "number"]),
            type_: color(&["syntax", "type"]),
            operator: color(&["syntax", "operator"]),
            punctuation: color(&["syntax", "punctuation"]),
        },
        markdown: Markdown {
            text: color(&["markdown", "text"]),
            heading: color(&["markdown", "heading"]),
            link: color(&["markdown", "link"]),
            link_text: color(&["markdown", "linkText"]),
            code: color(&["markdown", "code"]),
            block_quote: color(&["markdown", "blockQuote"]),
            emphasis: color(&["markdown", "emphasis"]),
            strong: color(&["markdown", "strong"]),
            horizontal_rule: color(&["markdown", "horizontalRule"]),
            list_item: color(&["markdown", "listItem"]),
            list_enumeration: color(&["markdown", "listEnumeration"]),
            image: color(&["markdown", "image"]),
            image_text: color(&["markdown", "imageText"]),
            code_block: color(&["markdown", "codeBlock"]),
        },
    }
}
