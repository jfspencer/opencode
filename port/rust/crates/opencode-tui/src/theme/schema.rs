//! The theme document schema (`packages/theme/src/tui/schema.ts`) and the part of Effect 4.1's decoder the theme
//! code observes: whether a document decodes, what the decoded value is (schema keys only, excess properties
//! dropped), and the text of the first issues, from which `themeDecodeError` takes the `got <input>` value.
//!
//! Decoding follows Effect's rules for these schemas: a struct checks its fields in schema order and stops at the
//! first failing one (`errors: "first"`); a union tries the members whose type matches the input (a literal only
//! when equal) and reports each tried member's issues in order, or `Expected <members>, got <input>` when none
//! matches; records check their entries in property order; refinements report `Expected <check>, got <input>`.
//! Messages are rendered as `SchemaIssue`'s default formatter does (`<message>\n  at ["path"][0]`, joined by
//! newlines). The `Expected ...` texts of union and type issues are reproduced for these schemas; nothing reads
//! them but the `got` extraction.

use std::sync::LazyLock;

use regex::Regex;

use super::js;
use super::value::{Rec, Value};

/// A path segment of an issue: a property name, or a number (array index, literal record key).
#[derive(Clone, Debug)]
enum Seg {
    Key(String),
    Index(u64),
}

#[derive(Clone, Debug)]
pub struct Issue {
    path: Vec<Seg>,
    message: String,
}

/// The formatted message of a list of issues (`SchemaError.message`).
pub fn message(issues: &[Issue]) -> String {
    issues
        .iter()
        .map(|issue| {
            if issue.path.is_empty() {
                issue.message.clone()
            } else {
                let path: String = issue
                    .path
                    .iter()
                    .map(|seg| match seg {
                        Seg::Key(k) => format!("[{}]", js::quote(k)),
                        Seg::Index(i) => format!("[{i}]"),
                    })
                    .collect();
                format!("{}\n  at {}", issue.message, path)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A schema of the theme document.
pub enum Schema {
    /// `Schema.String`.
    Str,
    /// `Schema.String.check(isPattern(re))`; `source` is the RegExp's `source` as printed.
    Pattern(&'static LazyLock<Regex>, &'static str),
    /// `Schema.NonEmptyString`.
    NonEmpty,
    /// `Schema.Literal(text)`.
    Literal(&'static str),
    /// `Schema.TemplateLiteral(["$", Schema.NonEmptyString])`.
    Dollar,
    /// `Schema.Union(members)`.
    Union(Vec<Schema>),
    /// `Schema.Struct(fields)`: name, schema, whether the key is required.
    Struct(Vec<(&'static str, Schema, bool)>),
    /// `Schema.Array(item)`, with `isMinLength(n)`.
    Array(Box<Schema>, usize),
    /// `Schema.Record(NonEmptyString, value)`, with a filter on the whole record.
    Record(Box<Schema>, Option<fn(&Rec) -> Option<String>>),
    /// `Schema.Record(Literals(keys), value)`: every key required (`optionalKey` values: optional).
    Keys(&'static [&'static str], Box<Schema>, bool),
    /// `Schema.Undefined` (the second member of `Schema.optional`).
    Undefined,
}

impl Schema {
    /// `Schema.optional(x)`: the key may be missing, the value may be `undefined`.
    fn optional(inner: Schema) -> Schema {
        Schema::Union(vec![inner, Schema::Undefined])
    }

    fn expected(&self, out: &mut Vec<String>) {
        let push = |out: &mut Vec<String>, text: String| {
            if !out.contains(&text) {
                out.push(text)
            }
        };
        match self {
            Schema::Str | Schema::Pattern(..) | Schema::NonEmpty | Schema::Dollar => push(out, "string".into()),
            Schema::Literal(text) => push(out, js::quote(text)),
            Schema::Union(members) => members.iter().for_each(|m| m.expected(out)),
            Schema::Struct(_) | Schema::Record(..) | Schema::Keys(..) => push(out, "object".into()),
            Schema::Array(..) => push(out, "array".into()),
            Schema::Undefined => push(out, "undefined".into()),
        }
    }

    /// Whether a union tries this member for the input.
    fn candidate(&self, input: &Value) -> bool {
        match self {
            Schema::Str | Schema::Pattern(..) | Schema::NonEmpty | Schema::Dollar => matches!(input, Value::Str(_)),
            Schema::Literal(text) => input.as_str() == Some(text),
            Schema::Union(members) => members.iter().any(|m| m.candidate(input)),
            Schema::Struct(_) | Schema::Record(..) | Schema::Keys(..) => matches!(input, Value::Rec(_)),
            Schema::Array(..) => matches!(input, Value::Arr(_)),
            Schema::Undefined => input.is_undefined(),
        }
    }

    fn invalid_type(&self, input: &Value) -> Vec<Issue> {
        let mut expected = Vec::new();
        self.expected(&mut expected);
        vec![Issue { path: vec![], message: format!("Expected {}, got {}", expected.join(" | "), got(input)) }]
    }

    /// Decodes `input`: the decoded value, or the issues Effect reports.
    pub fn decode(&self, input: &Value) -> Result<Value, Vec<Issue>> {
        match self {
            Schema::Str => match input {
                Value::Str(_) => Ok(input.clone()),
                _ => Err(self.invalid_type(input)),
            },
            Schema::Pattern(re, source) => match input {
                Value::Str(s) if re.is_match(s) => Ok(input.clone()),
                Value::Str(_) => Err(check(format!("a string matching the RegExp {source}"), input)),
                _ => Err(self.invalid_type(input)),
            },
            Schema::NonEmpty => match input {
                Value::Str(s) if !s.is_empty() => Ok(input.clone()),
                Value::Str(_) => Err(check("a value with a length of at least 1".into(), input)),
                _ => Err(self.invalid_type(input)),
            },
            Schema::Literal(text) => match input {
                Value::Str(s) if s == text => Ok(input.clone()),
                _ => Err(self.invalid_type(input)),
            },
            Schema::Dollar => match input {
                Value::Str(s) if s.len() > 1 && s.starts_with('$') => Ok(input.clone()),
                Value::Str(_) => Err(vec![Issue {
                    path: vec![],
                    message: format!("Expected a string matching template literal parts, got {}", got(input)),
                }]),
                _ => Err(self.invalid_type(input)),
            },
            Schema::Undefined => match input {
                Value::Undefined => Ok(Value::Undefined),
                _ => Err(self.invalid_type(input)),
            },
            Schema::Union(members) => {
                let mut issues = Vec::new();
                let mut tried = false;
                for member in flatten(members) {
                    if !member.candidate(input) {
                        continue;
                    }
                    tried = true;
                    match member.decode(input) {
                        Ok(value) => return Ok(value),
                        Err(found) => issues.extend(found),
                    }
                }
                if !tried {
                    return Err(self.invalid_type(input));
                }
                Err(issues)
            }
            Schema::Struct(fields) => {
                let Value::Rec(rec) = input else { return Err(self.invalid_type(input)) };
                let mut out = Rec::new();
                for (name, schema, required) in fields {
                    match rec.get(name) {
                        None => {
                            if *required {
                                return Err(vec![Issue { path: vec![Seg::Key((*name).into())], message: "Missing key".into() }]);
                            }
                        }
                        Some(value) => match schema.decode(value) {
                            Ok(decoded) => out.set(*name, decoded),
                            Err(issues) => return Err(prefix(Seg::Key((*name).into()), issues)),
                        },
                    }
                }
                Ok(Value::Rec(out))
            }
            Schema::Array(item, min) => {
                let Value::Arr(items) = input else { return Err(self.invalid_type(input)) };
                let mut out = Vec::new();
                for (i, value) in items.iter().enumerate() {
                    match item.decode(value) {
                        Ok(decoded) => out.push(decoded),
                        Err(issues) => return Err(prefix(Seg::Index(i as u64), issues)),
                    }
                }
                if out.len() < *min {
                    return Err(check(format!("a value with a length of at least {min}"), input));
                }
                Ok(Value::Arr(out))
            }
            Schema::Record(value_schema, filter) => {
                let Value::Rec(rec) = input else { return Err(self.invalid_type(input)) };
                let mut out = Rec::new();
                for (key, value) in rec.entries() {
                    // An entry whose key is not a `HueName` (the empty string) is left out, unchecked.
                    if key.is_empty() {
                        continue;
                    }
                    match value_schema.decode(value) {
                        Ok(decoded) => out.set(key, decoded),
                        Err(issues) => return Err(prefix(Seg::Key(key.into()), issues)),
                    }
                }
                if let Some(filter) = filter
                    && let Some(message) = filter(&out)
                {
                    return Err(vec![Issue { path: vec![], message }]);
                }
                Ok(Value::Rec(out))
            }
            Schema::Keys(keys, value_schema, optional) => {
                let Value::Rec(rec) = input else { return Err(self.invalid_type(input)) };
                let mut out = Rec::new();
                for key in *keys {
                    let seg = || match key.parse::<u64>() {
                        Ok(n) => Seg::Index(n),
                        Err(_) => Seg::Key((*key).into()),
                    };
                    match rec.get(key) {
                        None => {
                            if !optional {
                                return Err(vec![Issue { path: vec![seg()], message: "Missing key".into() }]);
                            }
                        }
                        Some(value) => match value_schema.decode(value) {
                            Ok(decoded) => out.set(*key, decoded),
                            Err(issues) => return Err(prefix(seg(), issues)),
                        },
                    }
                }
                Ok(Value::Rec(out))
            }
        }
    }
}

/// Union members, nested unions flattened (Effect flattens `Union([Union([a, b]), c])`).
fn flatten(members: &[Schema]) -> Vec<&Schema> {
    let mut out = Vec::new();
    for member in members {
        match member {
            Schema::Union(inner) => out.extend(flatten(inner)),
            _ => out.push(member),
        }
    }
    out
}

fn got(input: &Value) -> String {
    match input {
        Value::Undefined => "undefined".into(),
        Value::Color(c) => c.to_hex(),
        other => js::format(&other.to_json()),
    }
}

fn check(expected: String, input: &Value) -> Vec<Issue> {
    vec![Issue { path: vec![], message: format!("Expected {expected}, got {}", got(input)) }]
}

fn prefix(seg: Seg, mut issues: Vec<Issue>) -> Vec<Issue> {
    for issue in &mut issues {
        issue.path.insert(0, seg.clone());
    }
    issues
}

// JS `\d` is ASCII and `.` stops at line terminators: the Rust patterns spell both out.
static HEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$").unwrap());
const HEX_SOURCE: &str = r"^#(?:[\da-f]{3}|[\da-f]{4}|[\da-f]{6}|[\da-f]{8})$";
static HUE_STEP_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\$hue\.[^\n\r\u{2028}\u{2029}]+\.(?:100|200|300|400|500|600|700|800|900)$").unwrap());
const HUE_STEP_REF_SOURCE: &str = r"^\$hue\..+\.(?:100|200|300|400|500|600|700|800|900)$";
static HUE_REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\$hue\.[^\n\r\u{2028}\u{2029}]+$").unwrap());
const HUE_REF_SOURCE: &str = r"^\$hue\..+$";

/// `/^#(?:[\da-f]{3}|[\da-f]{4}|[\da-f]{6}|[\da-f]{8})$/i` (`isHex` of resolve.ts).
pub fn is_hex(value: &str) -> bool {
    HEX.is_match(value)
}

pub const HUE_STEPS: [u16; 9] = [100, 200, 300, 400, 500, 600, 700, 800, 900];
const HUE_STEP_KEYS: &[&str] = &["100", "200", "300", "400", "500", "600", "700", "800", "900"];
pub const SYNTAX_TOKENS: &[&str] =
    &["comment", "keyword", "function", "variable", "string", "number", "type", "operator", "punctuation"];
pub const MARKDOWN_TOKENS: &[&str] = &[
    "text",
    "heading",
    "link",
    "linkText",
    "code",
    "blockQuote",
    "emphasis",
    "strong",
    "horizontalRule",
    "listItem",
    "listEnumeration",
    "image",
    "imageText",
    "codeBlock",
];
/// `ActionState.literals`.
pub const ACTION_STATES: [&str; 5] = ["disabled", "pressed", "focused", "selected", "hovered"];
/// `ActionVariant.literals`.
pub const ACTION_VARIANTS: [&str; 3] = ["primary", "secondary", "destructive"];
pub const FEEDBACK_KINDS: [&str; 4] = ["error", "warning", "success", "info"];

fn hex() -> Schema {
    Schema::Pattern(&HEX, HEX_SOURCE)
}

/// `ColorValue`: a hex color, `"transparent"` or `$<reference>`.
fn color_value() -> Schema {
    Schema::Union(vec![hex(), Schema::Literal("transparent"), Schema::Dollar])
}

fn opt(schema: Schema) -> Schema {
    Schema::optional(schema)
}

/// `HueColorValue`: a hex color or `$hue.<name>.<step>`.
fn hue_color_value() -> Schema {
    Schema::Union(vec![hex(), Schema::Pattern(&HUE_STEP_REF, HUE_STEP_REF_SOURCE)])
}

fn hue_definition() -> Schema {
    let scale = Schema::Keys(HUE_STEP_KEYS, Box::new(hex()), false);
    let value = Schema::Union(vec![Schema::Pattern(&HUE_REF, HUE_REF_SOURCE), scale]);
    Schema::Record(
        Box::new(value),
        Some(|hues| {
            let missing: Vec<&str> =
                ["accent", "interactive", "neutral"].into_iter().filter(|name| hues.defined(name).is_none()).collect();
            (!missing.is_empty()).then(|| format!("Missing required semantic hues: {}", missing.join(", ")))
        }),
    )
}

fn categorical() -> Schema {
    Schema::Array(Box::new(Schema::NonEmpty), 1)
}

fn stateful(complete: bool) -> Schema {
    let mut fields = vec![("base", if complete { color_value() } else { opt(color_value()) }, complete)];
    for state in ["$hovered", "$focused", "$pressed", "$selected", "$disabled"] {
        fields.push((state, opt(color_value()), false));
    }
    Schema::Struct(fields)
}

fn actions(complete: bool) -> Schema {
    Schema::Struct(
        ACTION_VARIANTS
            .iter()
            .map(|v| (*v, if complete { stateful(true) } else { opt(stateful(false)) }, complete))
            .collect(),
    )
}

fn field(name: &'static str, schema: Schema, complete: bool) -> (&'static str, Schema, bool) {
    if complete { (name, schema, true) } else { (name, opt(schema), false) }
}

fn color_field(name: &'static str, complete: bool) -> (&'static str, Schema, bool) {
    field(name, color_value(), complete)
}

/// `ThemeTokensDefinition` (`complete: false`) or `CompleteThemeTokensDefinition` (`complete: true`) fields.
fn token_fields(complete: bool) -> Vec<(&'static str, Schema, bool)> {
    let c = complete;
    let text_feedback = || Schema::Struct(vec![color_field("base", c), ("muted", opt(color_value()), false)]);
    let background_feedback = || Schema::Struct(vec![color_field("base", c)]);
    let feedback = |item: &dyn Fn() -> Schema| {
        Schema::Struct(FEEDBACK_KINDS.iter().map(|k| field(k, item(), c)).collect())
    };
    let text = Schema::Struct(vec![
        color_field("base", c),
        color_field("muted", c),
        ("faint", opt(color_value()), false),
        field("action", actions(c), c),
        field("formfield", stateful(c), c),
        field("feedback", feedback(&text_feedback), c),
    ]);
    let background = Schema::Struct(vec![
        color_field("base", c),
        field("raised", Schema::Struct(vec![color_field("base", c), color_field("high", c), color_field("max", c)]), c),
        field("action", actions(c), c),
        field("formfield", stateful(c), c),
        field("feedback", feedback(&background_feedback), c),
    ]);
    let diff = Schema::Struct(vec![
        field(
            "text",
            Schema::Struct(vec![
                color_field("added", c),
                color_field("removed", c),
                color_field("context", c),
                color_field("hunkHeader", c),
            ]),
            c,
        ),
        field(
            "background",
            Schema::Struct(vec![color_field("added", c), color_field("removed", c), color_field("context", c)]),
            c,
        ),
        field("highlight", Schema::Struct(vec![color_field("added", c), color_field("removed", c)]), c),
        field(
            "lineNumber",
            Schema::Struct(vec![
                color_field("text", c),
                field("background", Schema::Struct(vec![color_field("added", c), color_field("removed", c)]), c),
            ]),
            c,
        ),
    ]);
    vec![
        field("text", text, c),
        field("background", background, c),
        field("border", Schema::Struct(vec![color_field("base", c)]), c),
        field("scrollbar", Schema::Struct(vec![color_field("base", c)]), c),
        field("diff", diff, c),
        field("syntax", Schema::Keys(SYNTAX_TOKENS, Box::new(hue_color_value()), !c), c),
        field("markdown", Schema::Keys(MARKDOWN_TOKENS, Box::new(hue_color_value()), !c), c),
    ]
}

fn dialog_tokens() -> Schema {
    let mut fields = token_fields(false);
    fields.push(("@footer", opt(Schema::Struct(token_fields(false))), false));
    Schema::Struct(fields)
}

/// `BaseThemeDefinition`.
fn base_theme() -> Schema {
    let mut fields = vec![("categorical", categorical(), true)];
    fields.extend(token_fields(true));
    fields.push(("@dialog", opt(dialog_tokens()), false));
    Schema::Struct(fields)
}

/// `ModeDefinition`.
fn mode_definition() -> Schema {
    let mut fields = vec![("hue", hue_definition(), true), ("categorical", opt(categorical()), false)];
    fields.extend(token_fields(false));
    fields.push(("@dialog", opt(dialog_tokens()), false));
    Schema::Struct(fields)
}

/// `ThemeDocument`.
pub static THEME_DOCUMENT: LazyLock<Schema> = LazyLock::new(|| {
    let member = |light: bool| {
        Schema::Struct(vec![
            ("$schema", opt(Schema::Str), false),
            ("base", base_theme(), true),
            ("light", if light { mode_definition() } else { opt(mode_definition()) }, light),
            ("dark", if light { opt(mode_definition()) } else { mode_definition() }, !light),
        ])
    };
    Schema::Union(vec![member(true), member(false)])
});
