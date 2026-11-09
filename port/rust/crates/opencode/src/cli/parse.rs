//! effect/cli 4.1's command-line parsing (`internal/lexer.ts`, `internal/parser.ts`, `Param.ts`, `Primitive.ts`,
//! `Command.runWith`): the lexer, global flag extraction across the command tree, the per-level scan with its
//! subcommand detection, then each parameter's parse in declaration order. Error texts are `CliError`'s messages.

use super::quote;
use super::spec::{BUILT_INS, Check, Command, Global, Kind, Mode, Param};

#[derive(Clone, Debug)]
enum Token {
    Long { name: String, raw: String, value: Option<String> },
    Short { flag: String, raw: String, value: Option<String> },
    Value(String),
}

impl Token {
    fn is_flag(&self) -> bool {
        !matches!(self, Token::Value(_))
    }
    fn flag_name(&self) -> &str {
        match self {
            Token::Long { name, .. } => name,
            Token::Short { flag, .. } => flag,
            Token::Value(value) => value,
        }
    }
    fn raw(&self) -> &str {
        match self {
            Token::Long { raw, .. } | Token::Short { raw, .. } => raw,
            Token::Value(value) => value,
        }
    }
    fn inline(&self) -> Option<&String> {
        match self {
            Token::Long { value, .. } | Token::Short { value, .. } => value.as_ref(),
            Token::Value(_) => None,
        }
    }
}

/// `-3`, `-3.70`, `-.5`, `-1e3`: values, not options.
fn negative_number(arg: &str) -> bool {
    let Some(rest) = arg.strip_prefix('-') else { return false };
    let bytes = rest.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > start
    };
    if digits(&mut i) {
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            digits(&mut i);
        }
    } else if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        if !digits(&mut i) {
            return false;
        }
    } else {
        return false;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        if !digits(&mut i) {
            return false;
        }
    }
    i == bytes.len()
}

fn lex(argv: &[String]) -> (Vec<Token>, Vec<String>) {
    let end = argv.iter().position(|a| a == "--");
    let (head, trailing) = match end {
        Some(index) => (&argv[..index], argv[index + 1..].to_vec()),
        None => (argv, vec![]),
    };
    let mut tokens = vec![];
    for arg in head {
        if !arg.starts_with('-') || negative_number(arg) {
            tokens.push(Token::Value(arg.clone()));
        } else if let Some(body) = arg.strip_prefix("--") {
            match body.find('=') {
                Some(0) => tokens.push(Token::Value(arg.clone())),
                Some(index) => tokens.push(Token::Long {
                    name: body[..index].to_owned(),
                    raw: arg.clone(),
                    value: Some(body[index + 1..].to_owned()),
                }),
                None => tokens.push(Token::Long { name: body.to_owned(), raw: arg.clone(), value: None }),
            }
        } else if arg.chars().count() > 1 {
            let flags = &arg[1..];
            match flags.find('=') {
                Some(index) => {
                    let mut chars: Vec<char> = flags[..index].chars().collect();
                    let last = chars.pop().map(String::from).unwrap_or_default();
                    for ch in chars {
                        tokens.push(Token::Short { flag: ch.to_string(), raw: format!("-{ch}"), value: None });
                    }
                    tokens.push(Token::Short {
                        raw: format!("-{last}"),
                        flag: last,
                        value: Some(flags[index + 1..].to_owned()),
                    });
                }
                None => {
                    for ch in flags.chars() {
                        tokens.push(Token::Short { flag: ch.to_string(), raw: format!("-{ch}"), value: None });
                    }
                }
            }
        } else {
            tokens.push(Token::Value(arg.clone()));
        }
    }
    (tokens, trailing)
}

/// One of `CliError`'s parse failures.
#[derive(Clone, Debug)]
pub enum CliError {
    UnrecognizedOption { option: String, command: Vec<String>, suggestions: Vec<String> },
    MissingOption { option: String },
    MissingArgument { argument: String },
    UnexpectedArgument { arguments: Vec<String> },
    InvalidValue { option: String, value: String, expected: String, flag: bool },
    UnknownSubcommand { subcommand: String, parent: Vec<String>, suggestions: Vec<String> },
}

impl CliError {
    pub fn tag(&self) -> &'static str {
        match self {
            CliError::UnrecognizedOption { .. } => "UnrecognizedOption",
            CliError::MissingOption { .. } => "MissingOption",
            CliError::MissingArgument { .. } => "MissingArgument",
            CliError::UnexpectedArgument { .. } => "UnexpectedArgument",
            CliError::InvalidValue { .. } => "InvalidValue",
            CliError::UnknownSubcommand { .. } => "UnknownSubcommand",
        }
    }

    pub fn message(&self) -> String {
        let suggest = |suggestions: &[String]| {
            if suggestions.is_empty() {
                String::new()
            } else {
                format!("\n\n  Did you mean this?\n    {}", suggestions.join("\n    "))
            }
        };
        match self {
            CliError::UnrecognizedOption { option, command, suggestions } => {
                format!("Unrecognized flag: {option} in command {}{}", command.join(" "), suggest(suggestions))
            }
            CliError::MissingOption { option } => format!("Missing required flag: --{option}"),
            CliError::MissingArgument { argument } => format!("Missing required argument: {argument}"),
            CliError::UnexpectedArgument { arguments } => {
                let label = if arguments.len() == 1 { "argument" } else { "arguments" };
                let list: Vec<String> = arguments.iter().map(|a| quote(a)).collect();
                format!("Unexpected positional {label}: {}", list.join(", "))
            }
            CliError::InvalidValue { option, value, expected, flag } => {
                let expectation = if expected.starts_with("Expected ") || expected.starts_with("Expected:") {
                    expected.clone()
                } else {
                    format!("Expected: {expected}")
                };
                if !flag {
                    format!("Invalid value for argument <{option}>: \"{value}\". {expectation}")
                } else if value.is_empty() {
                    format!("Missing value for flag --{option}. {expectation}")
                } else {
                    format!("Invalid value for flag --{option}: \"{value}\". {expectation}")
                }
            }
            CliError::UnknownSubcommand { subcommand, parent, suggestions } => {
                format!("Unknown subcommand \"{subcommand}\" for \"{}\"{}", parent.join(" "), suggest(suggestions))
            }
        }
    }
}

/// A parsed parameter value.
#[derive(Clone, Debug)]
pub enum Val {
    Bool(bool),
    Str(String),
    Int(i64),
    List(Vec<String>),
    Pairs(Vec<(String, String)>),
    /// An optional parameter that was not given.
    None,
}

/// A command's parsed input, by parameter name.
#[derive(Clone, Debug, Default)]
pub struct Input {
    values: Vec<(&'static str, Val)>,
}

impl Input {
    fn get(&self, name: &str) -> Option<&Val> {
        self.values.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }
    pub fn flag(&self, name: &str) -> bool {
        matches!(self.get(name), Some(Val::Bool(true)))
    }
    pub fn str(&self, name: &str) -> Option<&str> {
        match self.get(name) {
            Some(Val::Str(value)) => Some(value),
            _ => None,
        }
    }
    pub fn int(&self, name: &str) -> Option<i64> {
        match self.get(name) {
            Some(Val::Int(value)) => Some(*value),
            _ => None,
        }
    }
    pub fn list(&self, name: &str) -> &[String] {
        match self.get(name) {
            Some(Val::List(values)) => values,
            _ => &[],
        }
    }
    pub fn pairs(&self, name: &str) -> Option<&[(String, String)]> {
        match self.get(name) {
            Some(Val::Pairs(values)) => Some(values),
            _ => None,
        }
    }
    /// A `CliFlag.optionalValue` flag: `Some(None)` for the bare flag, `Some(Some(value))` with a value.
    pub fn optional_value(&self, name: &str) -> Option<Option<&str>> {
        match self.get(name) {
            Some(Val::Bool(true)) => Some(None),
            Some(Val::Str(value)) => Some(Some(value)),
            _ => None,
        }
    }
}

/// The values of opencode's global settings.
#[derive(Clone, Debug, Default)]
pub struct Globals {
    pub print_logs: bool,
    pub standalone: bool,
    pub server: Option<String>,
}

pub enum Outcome {
    /// `CliError.ShowHelp`: the help of `path`, then the errors (exit 1 with errors, 0 without).
    Help { path: Vec<&'static str>, errors: Vec<CliError> },
    Version,
    Completions(&'static str),
    /// `--wizard`. TODO(port): the interactive wizard.
    Wizard { path: Vec<&'static str> },
    Run { path: Vec<&'static str>, input: Input, globals: Globals },
}

type FlagMap = Vec<(&'static str, Vec<String>)>;

fn add(map: &mut FlagMap, name: &'static str, value: String) {
    if let Some((_, values)) = map.iter_mut().find(|(n, _)| *n == name) {
        values.push(value);
    }
}

fn values<'a>(map: &'a FlagMap, name: &str) -> &'a [String] {
    map.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_slice()).unwrap_or(&[])
}

struct Registry<'p> {
    params: Vec<&'p Param>,
}

struct Resolved<'p> {
    param: &'p Param,
    negated: bool,
}

impl<'p> Registry<'p> {
    fn new(params: impl IntoIterator<Item = &'p Param>) -> Self {
        Self { params: params.into_iter().collect() }
    }

    fn empty_map(&self) -> FlagMap {
        self.params.iter().map(|p| (p.name, vec![])).collect()
    }

    fn resolve(&self, token: &Token) -> Option<Resolved<'p>> {
        let name = token.flag_name();
        let direct = self.params.iter().find(|p| p.name == name || p.alias == Some(name)).copied();
        if let Some(param) = direct.filter(|p| p.name == name) {
            return Some(Resolved { param, negated: false });
        }
        if let Token::Long { name, .. } = token {
            if let Some(param) = self.params.iter().find(|p| p.negation().as_deref() == Some(name)) {
                return Some(Resolved { param, negated: true });
            }
        }
        direct.map(|param| Resolved { param, negated: false })
    }
}

fn is_bool_literal(value: &str) -> bool {
    matches!(value, "true" | "yes" | "on" | "1" | "y" | "false" | "no" | "off" | "0" | "n")
}

fn missing_value(param: &Param) -> CliError {
    let expected = match param.kind {
        Kind::Choice(choices) => choices.join(" | "),
        _ => param.type_name().to_owned(),
    };
    CliError::InvalidValue { option: param.name.into(), value: String::new(), expected, flag: true }
}

/// `consumeFlagValueWithTokens`: the value a flag token takes, and the tokens it consumed.
fn consume(tokens: &[Token], at: &mut usize, token: &Token, param: &Param, negated: bool) -> Result<Option<String>, CliError> {
    let peek_literal = |at: &usize| match tokens.get(*at) {
        Some(Token::Value(value)) if is_bool_literal(value) => Some(value.clone()),
        _ => None,
    };
    if negated {
        let invalid = |value: String| CliError::InvalidValue {
            option: param.name.into(),
            value,
            expected: format!("omit the value and use {} by itself to set --{} to false", token.raw(), param.name),
            flag: true,
        };
        if let Some(value) = token.inline() {
            return Err(invalid(value.clone()));
        }
        if param.optional_value.is_none() {
            if let Some(literal) = peek_literal(at) {
                *at += 1;
                return Err(invalid(literal));
            }
        }
        return Ok(Some("false".into()));
    }
    if let Some(value) = token.inline() {
        return Ok(Some(value.clone()));
    }
    if let Some(value) = param.optional_value {
        return Ok(Some(value.into()));
    }
    if param.kind == Kind::Bool {
        let literal = peek_literal(at);
        if literal.is_some() {
            *at += 1;
        }
        return Ok(Some(literal.unwrap_or_else(|| "true".into())));
    }
    if let Some(Token::Value(value)) = tokens.get(*at) {
        *at += 1;
        return Ok(Some(value.clone()));
    }
    Err(missing_value(param))
}

struct GlobalState<'p> {
    registry: &'p Registry<'p>,
    map: FlagMap,
    errors: Vec<CliError>,
}

/// `consumeGlobalFlags`: takes global flags out of the token stream, level by level, leaving a command's own flag
/// of the same name to it unless it stands before a subcommand.
fn consume_level(state: &mut GlobalState, tokens: &[Token], command: &Command) -> Vec<Token> {
    let local = Registry::new(command.flags());
    let mut remainder = vec![];
    let mut awaiting = true;
    let mut at = 0;
    while at < tokens.len() {
        let token = &tokens[at];
        at += 1;
        if token.is_flag() {
            let local_hit = local.resolve(token);
            let global_hit = state.registry.resolve(token);
            if let Some(hit) = &local_hit {
                if global_hit.is_none()
                    || !awaiting
                    || !precedes_subcommand(&tokens[at..], token, hit, command, &local, state.registry)
                {
                    remainder.push(token.clone());
                    let start = at;
                    let _ = consume(tokens, &mut at, token, hit.param, hit.negated);
                    remainder.extend_from_slice(&tokens[start..at]);
                    continue;
                }
            }
            if let Some(hit) = global_hit {
                match consume(tokens, &mut at, token, hit.param, hit.negated) {
                    Err(error) => state.errors.push(error),
                    Ok(Some(value)) => add(&mut state.map, hit.param.name, value),
                    Ok(None) => {}
                }
                continue;
            }
            remainder.push(token.clone());
            continue;
        }
        if let Token::Value(value) = token {
            if awaiting {
                if let Some(sub) = command.sub(value) {
                    remainder.push(token.clone());
                    remainder.extend(consume_level(state, &tokens[at..], sub));
                    return remainder;
                }
                awaiting = false;
            }
        }
        remainder.push(token.clone());
    }
    remainder
}

fn precedes_subcommand(rest: &[Token], token: &Token, hit: &Resolved, command: &Command, local: &Registry, global: &Registry) -> bool {
    let mut at = 0;
    let _ = consume(rest, &mut at, token, hit.param, hit.negated);
    while at < rest.len() {
        let next = &rest[at];
        at += 1;
        if next.is_flag() {
            if let Some(known) = local.resolve(next).or_else(|| global.resolve(next)) {
                let _ = consume(rest, &mut at, next, known.param, known.negated);
            }
            continue;
        }
        if let Token::Value(value) = next {
            return command.sub(value).is_some();
        }
    }
    false
}

/// The parse of one command level (`ParsedTokens`).
struct Parsed {
    flags: FlagMap,
    arguments: Vec<String>,
    sub: Option<(&'static str, Box<Parsed>)>,
}

/// `parseArgs` / `scanCommandLevel`.
fn parse_level(tokens: &[Token], trailing: &[String], command: &Command, parent: &[String], errors: &mut Vec<CliError>) -> Parsed {
    let mut path = parent.to_vec();
    path.push(command.name.to_owned());
    let registry = Registry::new(command.flags());
    let mut flags = registry.empty_map();
    let mut arguments = vec![];
    let mut awaiting = true;
    let mut at = 0;
    while at < tokens.len() {
        let token = &tokens[at];
        at += 1;
        if token.is_flag() {
            let Some(hit) = registry.resolve(token) else {
                errors.push(unrecognized(token, &registry, &path));
                continue;
            };
            match consume(tokens, &mut at, token, hit.param, hit.negated) {
                Err(error) => errors.push(error),
                Ok(Some(value)) => add(&mut flags, hit.param.name, value),
                Ok(None) => {}
            }
            continue;
        }
        let Token::Value(value) = token else { continue };
        if awaiting {
            if let Some(sub) = command.sub(value) {
                let mut selected = path.clone();
                selected.push(sub.name.to_owned());
                // Local flags are not inherited by subcommands.
                for (name, values) in &flags {
                    if !values.is_empty() {
                        errors.push(CliError::UnrecognizedOption {
                            option: format!("--{name}"),
                            command: selected.clone(),
                            suggestions: vec![],
                        });
                    }
                }
                let child = parse_level(&tokens[at..], trailing, sub, &path, errors);
                return Parsed { flags, arguments: vec![], sub: Some((sub.name, Box::new(child))) };
            }
            if command.arguments().next().is_none() && !command.subcommands.is_empty() {
                let suggestions = suggest(value, &command.sub_keys());
                errors.push(CliError::UnknownSubcommand { subcommand: value.clone(), parent: path.clone(), suggestions });
            }
            awaiting = false;
        }
        arguments.push(value.clone());
    }
    arguments.extend_from_slice(trailing);
    Parsed { flags, arguments, sub: None }
}

fn unrecognized(token: &Token, registry: &Registry, path: &[String]) -> CliError {
    let printable = match token {
        Token::Long { name, .. } => format!("--{name}"),
        Token::Short { flag, .. } => format!("-{flag}"),
        Token::Value(value) => value.clone(),
    };
    let mut names: Vec<String> = vec![];
    for param in registry.params.iter().filter(|p| !p.hidden) {
        names.push(param.name.to_owned());
        if let Some(negation) = param.negation() {
            names.push(negation);
        }
        names.extend(param.alias.map(String::from));
    }
    let candidates: Vec<&str> = names.iter().map(String::as_str).collect();
    let suggestions = suggest(token.flag_name(), &candidates)
        .into_iter()
        .map(|n| if n.encode_utf16().count() == 1 { format!("-{n}") } else { format!("--{n}") })
        .collect();
    CliError::UnrecognizedOption { option: printable, command: path.to_vec(), suggestions }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<u16> = a.encode_utf16().collect();
    let b: Vec<u16> = b.encode_utf16().collect();
    let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=b.len() {
        dp[0][j] = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1).min(dp[i][j - 1] + 1).min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[a.len()][b.len()]
}

/// `auto-suggest.ts`: the candidates at the smallest distance, up to 2.
fn suggest(input: &str, candidates: &[&str]) -> Vec<String> {
    let mut distances: Vec<(usize, &str)> =
        candidates.iter().map(|c| (levenshtein(input, c), *c)).filter(|(d, _)| *d <= 2).collect();
    distances.sort_by_key(|(d, _)| *d);
    let Some(&(min, _)) = distances.first() else { return vec![] };
    distances.into_iter().filter(|(d, _)| *d == min).map(|(_, c)| c.to_owned()).collect()
}

/// `Schema.Int` under the string codec: a finite-number string whose value is a safe integer.
fn parse_int(text: &str) -> Result<i64, String> {
    let finite = {
        let bytes = text.as_bytes();
        let mut i = 0;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let digits = |i: &mut usize| {
            let start = *i;
            while *i < bytes.len() && bytes[*i].is_ascii_digit() {
                *i += 1;
            }
            *i > start
        };
        let mut ok = if digits(&mut i) {
            if i < bytes.len() && bytes[i] == b'.' {
                i += 1;
                digits(&mut i)
            } else {
                true
            }
        } else if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            digits(&mut i)
        } else {
            false
        };
        if ok && i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
            i += 1;
            if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                i += 1;
            }
            ok = digits(&mut i);
        }
        ok && i == bytes.len()
    };
    if !finite {
        return Err("Expected a string representing a finite number".into());
    }
    let value: f64 = text.parse().map_err(|_| "Expected a string representing a finite number".to_owned())?;
    if !value.is_finite() || value.trunc() != value || value.abs() > 9_007_199_254_740_991.0 {
        return Err("Expected an integer".into());
    }
    Ok(value as i64)
}

fn primitive(param: &Param, raw: &str) -> Result<Val, String> {
    match param.kind {
        Kind::Bool => match raw {
            "true" | "yes" | "on" | "1" | "y" => Ok(Val::Bool(true)),
            "false" | "no" | "off" | "0" | "n" => Ok(Val::Bool(false)),
            _ => Err(r#"Expected "true" | "yes" | "on" | "1" | "y" | "false" | "no" | "off" | "0" | "n""#.into()),
        },
        Kind::Str => Ok(Val::Str(raw.to_owned())),
        Kind::Int => parse_int(raw).map(Val::Int),
        Kind::Choice(choices) => {
            if choices.contains(&raw) {
                Ok(Val::Str(raw.to_owned()))
            } else {
                Err(choices.iter().map(|c| quote(c)).collect::<Vec<_>>().join(" | "))
            }
        }
        Kind::KeyValue => match raw.find('=') {
            None => Err(format!("Invalid key=value format. Expected format: key=value, got: {raw}")),
            Some(index) if index == 0 || index + 1 == raw.len() => {
                Err(format!("Invalid key=value format. Both key and value must be non-empty. Got: {raw}"))
            }
            Some(index) => Ok(Val::Pairs(vec![(raw[..index].to_owned(), raw[index + 1..].to_owned())])),
        },
    }
}

/// One value through the primitive, the schema check and the optional-value mapping.
fn parse_one(param: &Param, raw: &str) -> Result<Val, CliError> {
    let invalid = |value: String, expected: String| CliError::InvalidValue {
        option: param.name.into(),
        value,
        expected,
        flag: param.flag,
    };
    let value = primitive(param, raw).map_err(|expected| invalid(raw.to_owned(), expected))?;
    let failed = match (&value, param.check) {
        (Val::Int(n), Check::AtLeast(min)) if *n < min => {
            Some((n.to_string(), format!("Expected a value greater than or equal to {min}")))
        }
        (Val::Int(n), Check::Between(min, max)) if *n < min || *n > max => {
            Some((n.to_string(), format!("Expected a value between {min} and {max}")))
        }
        (Val::Str(s), Check::NonEmpty) if s.is_empty() => {
            Some((s.clone(), "Expected a value with a length of at least 1".to_owned()))
        }
        _ => None,
    };
    if let Some((shown, message)) = failed {
        return Err(invalid(shown, format!("Schema validation failed: {message}")));
    }
    // `CliFlag.optionalValue` maps the empty value to `true`.
    if param.optional_value.is_some() && matches!(&value, Val::Str(s) if s.is_empty()) {
        return Ok(Val::Bool(true));
    }
    Ok(value)
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn default_value(param: &Param, raw: &'static str) -> Val {
    match param.kind {
        Kind::Bool => Val::Bool(raw == "true"),
        Kind::Int => Val::Int(raw.parse().unwrap_or_default()),
        _ => Val::Str(raw.to_owned()),
    }
}

fn parse_flag(param: &Param, given: &[String]) -> Result<Val, CliError> {
    if let Mode::Many { min, max, optional } = param.mode {
        let count = given.len();
        if let Some(min) = min.filter(|min| count < *min) {
            if count == 0 && optional {
                return Ok(Val::None);
            }
            return Err(if count == 0 {
                CliError::MissingOption { option: param.name.into() }
            } else {
                CliError::InvalidValue {
                    option: param.name.into(),
                    value: format!("{count} occurrences"),
                    expected: format!("at least {min} value{}", plural(min)),
                    flag: true,
                }
            });
        }
        if let Some(max) = max.filter(|max| count > *max) {
            return Err(CliError::InvalidValue {
                option: param.name.into(),
                value: format!("{count} occurrences"),
                expected: format!("at most {max} value{}", plural(max)),
                flag: true,
            });
        }
        let mut list = vec![];
        let mut pairs = vec![];
        for raw in given {
            match parse_one(param, raw)? {
                Val::Pairs(found) => pairs.extend(found),
                Val::Str(value) => list.push(value),
                Val::Int(value) => list.push(value.to_string()),
                _ => {}
            }
        }
        // `KeyValuePair` merges its objects; a repeated key keeps the last value in the first key's place.
        if param.kind == Kind::KeyValue {
            let mut merged: Vec<(String, String)> = vec![];
            for (key, value) in pairs {
                match merged.iter_mut().find(|(k, _)| *k == key) {
                    Some(entry) => entry.1 = value,
                    None => merged.push((key, value)),
                }
            }
            return Ok(Val::Pairs(merged));
        }
        return Ok(Val::List(list));
    }
    if given.is_empty() {
        return match param.mode {
            Mode::Optional => Ok(Val::None),
            Mode::Default(raw) => Ok(default_value(param, raw)),
            _ => Err(CliError::MissingOption { option: param.name.into() }),
        };
    }
    if given.len() > 1 {
        return Err(CliError::InvalidValue {
            option: param.name.into(),
            value: format!("{} occurrences", given.len()),
            expected: "at most 1 value".into(),
            flag: true,
        });
    }
    parse_one(param, &given[0])
}

fn parse_argument(param: &Param, arguments: &mut Vec<String>) -> Result<Val, CliError> {
    if let Mode::Many { min, max, .. } = param.mode {
        let mut list = vec![];
        while !arguments.is_empty() && max.is_none_or(|max| list.len() < max) {
            match parse_one(param, &arguments.remove(0))? {
                Val::Str(value) => list.push(value),
                Val::Int(value) => list.push(value.to_string()),
                _ => {}
            }
        }
        let min = min.unwrap_or(0);
        if list.len() < min {
            return Err(if list.is_empty() {
                CliError::MissingArgument { argument: param.name.into() }
            } else {
                CliError::InvalidValue {
                    option: param.name.into(),
                    value: format!("{} values", list.len()),
                    expected: format!("at least {min} value{}", plural(min)),
                    flag: false,
                }
            });
        }
        return Ok(Val::List(list));
    }
    if arguments.is_empty() {
        return match param.mode {
            Mode::Optional => Ok(Val::None),
            Mode::Default(raw) => Ok(default_value(param, raw)),
            _ => Err(CliError::MissingArgument { argument: param.name.into() }),
        };
    }
    parse_one(param, &arguments.remove(0))
}

/// `makeParser`: every parameter in declaration order; the first failure wins, then leftovers are unexpected.
fn parse_command(command: &Command, parsed: &Parsed) -> Result<Input, CliError> {
    let mut arguments = parsed.arguments.clone();
    let mut input = Input::default();
    for param in &command.params {
        let value = if param.flag {
            parse_flag(param, values(&parsed.flags, param.name))?
        } else {
            parse_argument(param, &mut arguments)?
        };
        input.values.push((param.name, value));
    }
    if !arguments.is_empty() {
        return Err(CliError::UnexpectedArgument { arguments });
    }
    Ok(input)
}

/// Every global flag of the tree (`getGlobalFlagsForCommandTree`): built-ins, then each command's declarations.
fn tree_globals(root: &Command) -> Vec<Global> {
    fn visit(command: &Command, out: &mut Vec<Global>) {
        for global in &command.globals {
            if !out.contains(global) {
                out.push(*global);
            }
        }
        for sub in &command.subcommands {
            visit(sub, out);
        }
    }
    let mut out = BUILT_INS.to_vec();
    visit(root, &mut out);
    out
}

/// The commands along a path, by name.
pub fn commands_for<'c>(root: &'c Command, path: &[&str]) -> Vec<&'c Command> {
    let mut commands = vec![root];
    let mut current = root;
    for name in path.iter().skip(1) {
        let Some(sub) = current.subcommands.iter().find(|c| c.name == *name) else { break };
        commands.push(sub);
        current = sub;
    }
    commands
}

/// The global flags active on a path (`getGlobalFlagsForCommandPath`).
pub fn path_globals(root: &Command, path: &[&str]) -> Vec<Global> {
    let mut out = BUILT_INS.to_vec();
    for command in commands_for(root, path) {
        for global in &command.globals {
            if !out.contains(global) {
                out.push(*global);
            }
        }
    }
    out
}

/// `Command.runWith` up to the handler.
pub fn run(root: &Command, argv: &[String]) -> Outcome {
    let (tokens, trailing) = lex(argv);
    let all = tree_globals(root);
    let params: Vec<Param> = all.iter().map(|g| g.param()).collect();
    let registry = Registry::new(params.iter());
    let mut state = GlobalState { registry: &registry, map: registry.empty_map(), errors: vec![] };
    let remainder = consume_level(&mut state, &tokens, root);
    let GlobalState { map: global_map, errors: global_errors, .. } = state;

    let mut parse_errors = vec![];
    let parsed = parse_level(&remainder, &trailing, root, &[], &mut parse_errors);
    let mut path = vec![root.name];
    let mut level = &parsed;
    while let Some((name, child)) = &level.sub {
        path.push(name);
        level = child;
    }
    let active = path_globals(root, &path);

    let mut out_of_scope = vec![];
    for global in &all {
        if active.contains(global) {
            continue;
        }
        let param = global.param();
        if !values(&global_map, param.name).is_empty() {
            out_of_scope.push(CliError::UnrecognizedOption {
                option: format!("--{}", param.name),
                command: path.iter().map(|s| s.to_string()).collect(),
                suggestions: vec![],
            });
        }
    }
    if !out_of_scope.is_empty() || !global_errors.is_empty() {
        let errors = global_errors.into_iter().chain(out_of_scope).chain(parse_errors).collect();
        return Outcome::Help { path, errors };
    }

    let global_value = |global: Global| -> Result<Val, CliError> {
        let param = global.param();
        parse_flag(&param, values(&global_map, param.name))
    };
    for global in active.iter().filter(|g| g.is_action()) {
        let param = global.param();
        if values(&global_map, param.name).is_empty() {
            continue;
        }
        let value = match global_value(*global) {
            Ok(value) => value,
            Err(error) => return Outcome::Help { path, errors: vec![error] },
        };
        return match (global, value) {
            (Global::Help, _) => Outcome::Help { path, errors: vec![] },
            (Global::Version, _) => Outcome::Version,
            (Global::Wizard, _) => Outcome::Wizard { path },
            (Global::Completions, Val::Str(shell)) => {
                Outcome::Completions(super::spec::SHELLS.iter().find(|s| **s == shell).copied().unwrap_or("bash"))
            }
            _ => Outcome::Run { path, input: Input::default(), globals: Globals::default() },
        };
    }

    if !parse_errors.is_empty() {
        return Outcome::Help { path, errors: parse_errors };
    }
    let commands = commands_for(root, &path);
    let leaf = commands.last().copied().unwrap_or(root);
    let input = match parse_command(leaf, level) {
        Ok(input) => input,
        Err(error) => return Outcome::Help { path, errors: vec![error] },
    };
    let mut globals = Globals::default();
    for global in &active {
        if global.is_action() {
            continue;
        }
        let value = match global_value(*global) {
            Ok(value) => value,
            Err(error) => return Outcome::Help { path, errors: vec![error] },
        };
        match (global, value) {
            (Global::PrintLogs, Val::Bool(on)) => globals.print_logs = on,
            (Global::Standalone, Val::Bool(on)) => globals.standalone = on,
            (Global::Server, Val::Str(url)) => globals.server = Some(url),
            _ => {}
        }
    }
    if !leaf.handled {
        return Outcome::Help { path, errors: vec![] };
    }
    Outcome::Run { path, input, globals }
}
