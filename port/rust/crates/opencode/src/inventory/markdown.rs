//! Markdown files with frontmatter: gray-matter 4.0.3's `matter(content, {})` (index.js, lib/*.js) under opencode's
//! wrapper, packages/core/src/config/markdown.ts (`ConfigMarkdown.parse`, `parseOption`, `sanitize`).
//!
//! gray-matter, step by step: an empty input has no frontmatter; a leading BOM is dropped; the input must start
//! with `---` not followed by another `-`; the rest of that line names the language (its trimmed text, `yaml` when
//! blank; with no line break at all, every character but the last); the frontmatter ends at the first `\n---`
//! (whatever follows on that line), else at the end of the input, which then leaves no content; the content after
//! the closing delimiter drops one leading `\r` and then one leading `\n`. A frontmatter that is blank once lines
//! of comments are removed (`/^\s*#[^\n]+/gm`) is `{}` without being parsed; otherwise the language's engine parses
//! it, and a `null` result is `{}`.
//!
//! Engines: `yaml` (also `yml`, any case) is [`yaml::load`] (js-yaml's `safeLoad`); `json` is `JSON.parse`. Any
//! other language is an [`Error`] where gray-matter throws (`coffee`, `toml`, ...).
//! TODO(port): gray-matter's `javascript`/`js` engine `eval`s the frontmatter, and names of `Object.prototype`
//! members (`constructor`, `toString`, ...) resolve to those functions; both are an [`Error`] here.

use indexmap::IndexMap;
use opencode_protocol::Json;
use opencode_protocol::wire::number::{is_js_whitespace, js_trim};

use super::yaml;

/// gray-matter's result: `file.data` (what the frontmatter parsed to; `{}` without frontmatter) and `file.content`.
#[derive(Debug, Clone, PartialEq)]
pub struct Markdown {
    pub data: Json,
    pub content: String,
}

/// The frontmatter could not be parsed (also after [`sanitize`], for [`parse`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// `ConfigMarkdown.parse`: gray-matter, and when that throws, gray-matter on the [`sanitize`]d content.
pub fn parse(content: &str) -> Result<Markdown, Error> {
    matter(content).or_else(|_| matter(&sanitize(content)))
}

/// `ConfigMarkdown.parseOption`: [`parse`], or `None` where it fails.
pub fn parse_option(content: &str) -> Option<Markdown> {
    parse(content).ok()
}

/// `ConfigMarkdown.sanitize`: frontmatter lines `key: value` whose unquoted value holds a `:` or starts with a
/// bracketed tag (`[Tag] text`) are rewritten as block scalars (`key: |-` then the value indented), which YAML
/// accepts where the plain value would not parse.
pub fn sanitize(content: &str) -> String {
    let Some(frontmatter) = frontmatter_block(content) else {
        return content.to_owned();
    };
    let lines: Vec<String> = split_lines(frontmatter).into_iter().flat_map(sanitize_line).collect();
    // `content.replace(frontmatter, () => ...)`: the first occurrence, replaced literally.
    content.replacen(frontmatter, &lines.join("\n"), 1)
}

/// `matter(input, {})`.
fn matter(input: &str) -> Result<Markdown, Error> {
    let none = |content: &str| Markdown { data: Json::Object(IndexMap::new()), content: content.to_owned() };
    if input.is_empty() {
        return Ok(none(""));
    }
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let Some(rest) = input.strip_prefix("---") else {
        return Ok(none(input));
    };
    // "if the next character after the opening delimiter is a character from the delimiter, then it's not a
    // front-matter delimiter"
    if rest.starts_with('-') {
        return Ok(none(input));
    }

    // `matter.language`: `str.slice(0, str.search(/\r?\n/))`, which without a line break is all but the last
    // UTF-16 code unit.
    let raw = match rest.find('\n') {
        Some(nl) => rest[..nl].strip_suffix('\r').unwrap_or(&rest[..nl]),
        None => match rest.chars().next_back() {
            None => "",
            // Cutting a surrogate pair in half leaves a language name no engine has.
            Some(last) if last.len_utf16() == 2 => {
                return Err(not_registered(&format!("{}\u{fffd}", js_trim(&rest[..rest.len() - last.len_utf8()]))));
            }
            Some(last) => &rest[..rest.len() - last.len_utf8()],
        },
    };
    let name = js_trim(raw);
    let (language, rest) = if name.is_empty() { ("yaml", rest) } else { (name, &rest[raw.len()..]) };

    let (block, content) = match rest.find("\n---") {
        Some(close) => {
            let after = &rest[close + "\n---".len()..];
            let after = after.strip_prefix('\r').unwrap_or(after);
            (&rest[..close], after.strip_prefix('\n').unwrap_or(after))
        }
        None => (rest, ""),
    };
    let data = if js_trim(&strip_comment_lines(block)).is_empty() { Json::Null } else { engine(language, block)? };
    // `excerpt()`: `if (file.data == null) file.data = {}`
    let data = if data == Json::Null { Json::Object(IndexMap::new()) } else { data };
    Ok(Markdown { data, content: content.to_owned() })
}

/// lib/engine.js `getEngine(name)`, then its `parse`.
fn engine(name: &str, block: &str) -> Result<Json, Error> {
    let alias = match name.to_lowercase().as_str() {
        "js" | "javascript" => "javascript",
        "yaml" | "yml" => "yaml",
        _ => name,
    };
    let engine = [name, alias].into_iter().find(|n| matches!(*n, "yaml" | "json" | "javascript"));
    match engine {
        Some("yaml") => yaml::load(block).map_err(|e| Error { message: e.to_string() }),
        Some("json") => opencode_protocol::wire::parse(block).map_err(|e| Error { message: e.to_string() }),
        Some(_) => Err(Error { message: "TODO(port): gray-matter's javascript engine is not supported".to_owned() }),
        None => Err(not_registered(name)),
    }
}

fn not_registered(name: &str) -> Error {
    Error { message: format!("gray-matter engine \"{name}\" is not registered") }
}

/// `block.replace(/^\s*#[^\n]+/gm, '')`: drops each run of white space at a line start that leads to a `#` and the
/// rest of that line.
fn strip_comment_lines(block: &str) -> String {
    let mut out = String::new();
    let mut last = 0;
    let mut at = 0;
    let is_line_terminator = |c: char| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
    while at < block.len() {
        let line_start = at == 0 || block[..at].chars().next_back().is_some_and(is_line_terminator);
        if line_start {
            let hash = at + block[at..].len() - block[at..].trim_start_matches(is_js_whitespace).len();
            let after = &block[hash..];
            if after.starts_with('#') && after[1..].chars().next().is_some_and(|c| c != '\n') {
                let end = hash + 1 + after[1..].find('\n').unwrap_or(after.len() - 1);
                out.push_str(&block[last..at]);
                last = end;
                at = end;
                continue;
            }
        }
        at += block[at..].chars().next().map_or(1, char::len_utf8);
    }
    out.push_str(&block[last..]);
    out
}

/// The frontmatter `content.match(/^---\r?\n([\s\S]*?)\r?\n---/)` captures.
fn frontmatter_block(content: &str) -> Option<&str> {
    let rest = content.strip_prefix("---")?;
    let start = content.len() - rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n'))?.len();
    let bytes = content.as_bytes();
    // The lazy group ends at the first place `\r?\n---` matches.
    (start..bytes.len())
        .find(|&i| bytes[i..].starts_with(b"\r\n---") || bytes[i..].starts_with(b"\n---"))
        .map(|end| &content[start..end])
}

/// `text.split(/\r?\n/)`.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    let last = lines.len() - 1;
    for line in &mut lines[..last] {
        *line = line.strip_suffix('\r').unwrap_or(line);
    }
    lines
}

/// One line of [`sanitize`]: kept, or rewritten as a block scalar.
fn sanitize_line(line: &str) -> Vec<String> {
    let keep = vec![line.to_owned()];
    let trimmed = js_trim(line);
    if trimmed.starts_with('#') || trimmed.is_empty() || line.starts_with(is_js_whitespace) {
        return keep;
    }
    // /^([a-zA-Z_][a-zA-Z0-9_]*)\s*:\s*(.*)$/ (`.` stops at line terminators, `$` is the end of the line)
    if !line.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return keep;
    }
    let key_len = line.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(line.len());
    let (key, rest) = line.split_at(key_len);
    let Some(rest) = rest.trim_start_matches(is_js_whitespace).strip_prefix(':') else {
        return keep;
    };
    let rest = rest.trim_start_matches(is_js_whitespace);
    if rest.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return keep;
    }
    let value = js_trim(rest);
    if value.is_empty() || value == ">" || value == "|" || value.starts_with('"') || value.starts_with('\'') {
        return keep;
    }
    if !value.contains(':') && !starts_with_bracketed_tag(value) {
        return keep;
    }
    vec![format!("{key}: |-"), format!("  {value}")]
}

/// `/^[[{].*[\]}]\s*\S/.test(value)` for a trimmed value without line terminators: a `]` or `}` after the opening
/// bracket that is not the last character.
fn starts_with_bracketed_tag(value: &str) -> bool {
    if !value.starts_with(['[', '{']) {
        return false;
    }
    let inner = &value[1..];
    let last = inner.char_indices().next_back().map(|(i, _)| i);
    inner.char_indices().any(|(i, c)| matches!(c, ']' | '}') && Some(i) != last)
}
