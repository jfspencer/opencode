//! Markdown files with frontmatter: gray-matter 4.0.3's `matter(content, {})` (index.js, lib/*.js) under opencode's
//! wrapper, packages/core/src/config/markdown.ts (`ConfigMarkdown.parse`, `parseOption`, `sanitize`).
//!
//! gray-matter, step by step: an empty input has no frontmatter; a leading BOM is dropped; the input must start
//! with `---` not followed by another `-`; the rest of that line names the language (its trimmed text, `yaml` when
//! blank; with no line break at all, every character but the last); the frontmatter ends at the first `\n---`
//! (whatever follows on that line), else at the end of the input, which then leaves no content; the content after
//! the closing delimiter drops one leading `\r` and then one leading `\n`. A frontmatter that is blank once lines
//! of comments are removed (`/^\s*#[^\n]+/gm`) is `{}` without being parsed; otherwise the language's engine parses
//! it, and a `null` or `undefined` result is `{}`.
//!
//! Without frontmatter the data is `{}` and the content the whole input (less a BOM).
//!
//! The data is a JS value ([`jsvalue::Value`]): what the consumers make of a `Date`, a `Buffer`, an array, a string or
//! a value that contains itself is theirs to reproduce (inventory/agents.rs, commands.rs, skills.rs).
//!
//! Engines (lib/engine.js): `options.engines[name] || options.engines[aliase(name)]`, where the engines are an
//! ordinary object, so a name is looked up as written, then lower-cased for the aliases (`js`, `javascript`, `yaml`,
//! `yml`). `yaml` is [`yaml::load`] (js-yaml's `safeLoad`); `json` (lower case only) is `JSON.parse`; `javascript`
//! `eval`s the frontmatter, which the port does not ([`frontmatter_js`]: the file is skipped). A name of an
//! `Object.prototype` member finds that member: a function engine is
//! called as `{ parse: member }.parse(matter, options)` (`constructor` makes a `String` object of the frontmatter,
//! `toString` gives `"[object Object]"`, `valueOf` the engine object itself, `hasOwnProperty` `false`, and so on),
//! and `__proto__` is no function. Any other language is an [`Error`] where gray-matter throws (`coffee`, `toml`,
//! `JSON`, ...).

use opencode_protocol::wire::number::{is_js_whitespace, js_trim};

use super::jsvalue::{self, Arena, Class, Graph, Obj, Value};
use super::{frontmatter_js, yaml};

/// gray-matter's result: `file.data` (what the frontmatter parsed to; `{}` without frontmatter) and `file.content`.
pub struct Markdown {
    pub data: Graph,
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

fn empty_object() -> Graph {
    Graph::of(Value::Object(Obj::plain()))
}

/// `matter(input, {})`.
fn matter(input: &str) -> Result<Markdown, Error> {
    let none = |content: &str| Markdown { data: empty_object(), content: content.to_owned() };
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
    let data = if js_trim(&strip_comment_lines(block)).is_empty() { empty_object() } else { engine(language, block)? };
    // `excerpt()`: `if (file.data == null) file.data = {}`; then a truthy `data.excerpt_separator` is looked for in
    // the content (`content.indexOf(separator)`), which throws where the separator has no string form.
    let data = if matches!(data.root, Value::Null | Value::Undefined) { empty_object() } else { data };
    if jsvalue::get(&data.root, "excerpt_separator").is_some_and(|sep| sep.truthy() && jsvalue::to_string_throws(&sep)) {
        return Err(Error { message: "Cannot convert object to primitive value".to_owned() });
    }
    Ok(Markdown { data, content: content.to_owned() })
}

/// lib/engine.js `getEngine(name)`, then its `parse(str, opts)` (lib/parse.js).
fn engine(name: &str, block: &str) -> Result<Graph, Error> {
    let alias = match name.to_lowercase().as_str() {
        "js" | "javascript" => "javascript",
        "coffee" | "coffeescript" | "cson" => "coffee",
        "yaml" | "yml" => "yaml",
        _ => name,
    };
    let found = |key: &str| matches!(key, "yaml" | "json" | "javascript") || jsvalue::OBJECT_PROTOTYPE.contains(&key);
    let Some(engine) = [name, alias].into_iter().find(|n| found(n)) else {
        return Err(not_registered(name));
    };
    let thrown = |message: &str| Err(Error { message: message.to_owned() });
    match engine {
        "yaml" => yaml::load(block).map_err(|e| Error { message: e.to_string() }),
        "json" => opencode_protocol::wire::parse(block)
            .map(|json| Graph::of(jsvalue::from_json(&json)))
            .map_err(|e| Error { message: e.to_string() }),
        "javascript" => frontmatter_js::evaluate(block).map_err(|message| Error { message }),
        // An `Object.prototype` member: a function is `{ parse: member }.parse(block, options)`.
        "__proto__" => thrown("expected \"__proto__.parse\" to be a function"),
        "toString" | "toLocaleString" => Ok(Graph::of(Value::string("[object Object]"))),
        "valueOf" => {
            let arena = Arena::default();
            let engine = arena.object(Class::Plain);
            engine.define("parse", Value::Object(arena.object(Class::Function)));
            Ok(Graph::new(Value::Object(engine), arena))
        }
        "hasOwnProperty" | "propertyIsEnumerable" => Ok(Graph::of(Value::Bool(block == "parse"))),
        "isPrototypeOf" => Ok(Graph::of(Value::Bool(false))),
        "__defineGetter__" => thrown("invalid getter usage"),
        "__defineSetter__" => thrown("invalid setter usage"),
        "__lookupGetter__" | "__lookupSetter__" => Ok(Graph::of(Value::Undefined)),
        // `constructor`: `Object(block)`.
        _ => Ok(Graph::of(Value::Object(Obj::new(Class::Boxed(Value::string(block)))))),
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

#[cfg(test)]
mod tests {
    use super::*;
    use opencode_protocol::wire::Out;

    type File = Option<(&'static str, &'static str)>;

    /// Every case: the input; gray-matter 4.0.3's `matter(input, {})` as `(JSON.stringify(file.data),
    /// file.content)`, `None` where it throws; `ConfigMarkdown.sanitize(input)`; and `ConfigMarkdown.parse(input)`
    /// like `matter`. Produced by running gray-matter and packages/core/src/config/markdown.ts under bun with
    /// TZ=UTC; do not edit by hand.
    #[rustfmt::skip]
    const CASES: &[(&str, File, &str, File)] = &[
        ("---\ndescription: Uses: an unquoted colon\n---\nBody text\n", None, "---\ndescription: |-\n  Uses: an unquoted colon\n---\nBody text\n", Some(("{\"description\":\"Uses: an unquoted colon\"}", "Body text\n"))),
        ("---\ndescription: [unclosed\n---\nBody text\n", None, "---\ndescription: [unclosed\n---\nBody text\n", None),
        ("---\ndisable-model-invocation: yes\n---\nBody text\n", Some(("{\"disable-model-invocation\":\"yes\"}", "Body text\n")), "---\ndisable-model-invocation: yes\n---\nBody text\n", Some(("{\"disable-model-invocation\":\"yes\"}", "Body text\n"))),
        ("---\nsteps: -4\n---\nBody text\n", Some(("{\"steps\":-4}", "Body text\n")), "---\nsteps: -4\n---\nBody text\n", Some(("{\"steps\":-4}", "Body text\n"))),
        ("---\ncolor: \"#00AA00\"\n---\nBody text\n", Some(("{\"color\":\"#00AA00\"}", "Body text\n")), "---\ncolor: \"#00AA00\"\n---\nBody text\n", Some(("{\"color\":\"#00AA00\"}", "Body text\n"))),
        ("---\nmetadata:\n  opencode/autoinvoke: false\n---\nBody text\n", Some(("{\"metadata\":{\"opencode/autoinvoke\":false}}", "Body text\n")), "---\nmetadata:\n  opencode/autoinvoke: false\n---\nBody text\n", Some(("{\"metadata\":{\"opencode/autoinvoke\":false}}", "Body text\n"))),
        ("---\npermissions:\n  - action: edit\n    resource: \"*\"\n    effect: deny\n---\nBody text\n", Some(("{\"permissions\":[{\"action\":\"edit\",\"resource\":\"*\",\"effect\":\"deny\"}]}", "Body text\n")), "---\npermissions:\n  - action: edit\n    resource: \"*\"\n    effect: deny\n---\nBody text\n", Some(("{\"permissions\":[{\"action\":\"edit\",\"resource\":\"*\",\"effect\":\"deny\"}]}", "Body text\n"))),
        ("---\ndescription: 42\n---\nBody text\n", Some(("{\"description\":42}", "Body text\n")), "---\ndescription: 42\n---\nBody text\n", Some(("{\"description\":42}", "Body text\n"))),
        ("---\nname: [unclosed\ndescription: 5\n---\nBody text\n", None, "---\nname: [unclosed\ndescription: 5\n---\nBody text\n", None),
        ("---\nname: my-skill\ndescription: Does things\nlicense: MIT\nallowed-tools:\n  - Read\n  - Write\n---\nBody text\n", Some(("{\"name\":\"my-skill\",\"description\":\"Does things\",\"license\":\"MIT\",\"allowed-tools\":[\"Read\",\"Write\"]}", "Body text\n")), "---\nname: my-skill\ndescription: Does things\nlicense: MIT\nallowed-tools:\n  - Read\n  - Write\n---\nBody text\n", Some(("{\"name\":\"my-skill\",\"description\":\"Does things\",\"license\":\"MIT\",\"allowed-tools\":[\"Read\",\"Write\"]}", "Body text\n"))),
        ("---\ndescription: >\n  A folded\n  description\nmode: subagent\ntemperature: 0.1\ntools:\n  write: false\n  bash: true\n---\nBody text\n", Some(("{\"description\":\"A folded description\\n\",\"mode\":\"subagent\",\"temperature\":0.1,\"tools\":{\"write\":false,\"bash\":true}}", "Body text\n")), "---\ndescription: >\n  A folded\n  description\nmode: subagent\ntemperature: 0.1\ntools:\n  write: false\n  bash: true\n---\nBody text\n", Some(("{\"description\":\"A folded description\\n\",\"mode\":\"subagent\",\"temperature\":0.1,\"tools\":{\"write\":false,\"bash\":true}}", "Body text\n"))),
        ("", Some(("{}", "")), "", Some(("{}", ""))),
        ("no frontmatter", Some(("{}", "no frontmatter")), "no frontmatter", Some(("{}", "no frontmatter"))),
        ("\u{feff}no fm", Some(("{}", "no fm")), "\u{feff}no fm", Some(("{}", "no fm"))),
        ("\u{feff}", Some(("{}", "")), "\u{feff}", Some(("{}", ""))),
        ("---\n---\nbody", Some(("{}", "body")), "---\n---\nbody", Some(("{}", "body"))),
        ("---\ntitle: x\n---\nbody", Some(("{\"title\":\"x\"}", "body")), "---\ntitle: x\n---\nbody", Some(("{\"title\":\"x\"}", "body"))),
        ("---\r\ntitle: x\r\n---\r\nbody", Some(("{\"title\":\"x\"}", "body")), "---\r\ntitle: x\r\n---\r\nbody", Some(("{\"title\":\"x\"}", "body"))),
        ("----\ntitle: x\n----\n", Some(("{}", "----\ntitle: x\n----\n")), "----\ntitle: x\n----\n", Some(("{}", "----\ntitle: x\n----\n"))),
        ("---\ntitle: x\n", Some(("{\"title\":\"x\"}", "")), "---\ntitle: x\n", Some(("{\"title\":\"x\"}", ""))),
        ("---\ntitle: x", Some(("{\"title\":\"x\"}", "")), "---\ntitle: x", Some(("{\"title\":\"x\"}", ""))),
        ("---yaml\ntitle: x\n---\nb", Some(("{\"title\":\"x\"}", "b")), "---yaml\ntitle: x\n---\nb", Some(("{\"title\":\"x\"}", "b"))),
        ("---YAML\na: 1\n---\n", Some(("{\"a\":1}", "")), "---YAML\na: 1\n---\n", Some(("{\"a\":1}", ""))),
        ("---yml\na: 1\n---\n", Some(("{\"a\":1}", "")), "---yml\na: 1\n---\n", Some(("{\"a\":1}", ""))),
        ("---json\n{\"a\": 1}\n---\nb", Some(("{\"a\":1}", "b")), "---json\n{\"a\": 1}\n---\nb", Some(("{\"a\":1}", "b"))),
        ("---json\nnull\n---\nb", Some(("{}", "b")), "---json\nnull\n---\nb", Some(("{}", "b"))),
        ("---json\n{bad}\n---\nb", None, "---json\n{bad}\n---\nb", None),
        ("---JSON\n{}\n---\n", None, "---JSON\n{}\n---\n", None),
        ("---coffee\na: 1\n---\n", None, "---coffee\na: 1\n---\n", None),
        ("---toml\na = 1\n---\n", None, "---toml\na = 1\n---\n", None),
        ("--- \na: 1\n---\n", Some(("{\"a\":1}", "")), "--- \na: 1\n---\n", Some(("{\"a\":1}", ""))),
        ("---  yaml  \na: 1\n---\n", Some(("{\"a\":1}", "")), "---  yaml  \na: 1\n---\n", Some(("{\"a\":1}", ""))),
        ("---\n# just a comment\n---\nbody", Some(("{}", "body")), "---\n# just a comment\n---\nbody", Some(("{}", "body"))),
        ("---\n#\n---\nb", Some(("{}", "b")), "---\n#\n---\nb", Some(("{}", "b"))),
        ("---\n  # indented comment\n   \n---\nb", Some(("{}", "b")), "---\n  # indented comment\n   \n---\nb", Some(("{}", "b"))),
        ("---\n\n---\n", Some(("{}", "")), "---\n\n---\n", Some(("{}", ""))),
        ("---\n# a\n#\n---\n", Some(("{}", "")), "---\n# a\n#\n---\n", Some(("{}", ""))),
        ("---\nnull\n---\nbody", Some(("{}", "body")), "---\nnull\n---\nbody", Some(("{}", "body"))),
        ("---\n~\n---\n", Some(("{}", "")), "---\n~\n---\n", Some(("{}", ""))),
        ("---\nhello\n---\n", Some(("\"hello\"", "")), "---\nhello\n---\n", Some(("\"hello\"", ""))),
        ("---\n42\n---\n", Some(("42", "")), "---\n42\n---\n", Some(("42", ""))),
        ("---\nfalse\n---\n", Some(("false", "")), "---\nfalse\n---\n", Some(("false", ""))),
        ("---\n[1, 2]\n---\n", Some(("[1,2]", "")), "---\n[1, 2]\n---\n", Some(("[1,2]", ""))),
        ("---\n''\n---\n", Some(("\"\"", "")), "---\n''\n---\n", Some(("\"\"", ""))),
        ("---\na: 1\n---", Some(("{\"a\":1}", "")), "---\na: 1\n---", Some(("{\"a\":1}", ""))),
        ("---\na: 1\n---\r\nbody", Some(("{\"a\":1}", "body")), "---\na: 1\n---\r\nbody", Some(("{\"a\":1}", "body"))),
        ("---\na: 1\n---\n\nbody", Some(("{\"a\":1}", "\nbody")), "---\na: 1\n---\n\nbody", Some(("{\"a\":1}", "\nbody"))),
        ("---\na: 1\n----\nbody", Some(("{\"a\":1}", "-\nbody")), "---\na: 1\n----\nbody", Some(("{\"a\":1}", "-\nbody"))),
        ("---\na: 1\n--- trailing\nbody", Some(("{\"a\":1}", " trailing\nbody")), "---\na: 1\n--- trailing\nbody", Some(("{\"a\":1}", " trailing\nbody"))),
        ("---\na: 1\n---body", Some(("{\"a\":1}", "body")), "---\na: 1\n---body", Some(("{\"a\":1}", "body"))),
        ("\u{feff}---\na: 1\n---\nb", Some(("{\"a\":1}", "b")), "\u{feff}---\na: 1\n---\nb", Some(("{\"a\":1}", "b"))),
        ("---\n---\n---\n", Some(("{}", "---\n")), "---\n---\n---\n", Some(("{}", "---\n"))),
        ("---a: 1", None, "---a: 1", None),
        ("---x", Some(("\"x\"", "")), "---x", Some(("\"x\"", ""))),
        ("---yamlx", Some(("\"x\"", "")), "---yamlx", Some(("\"x\"", ""))),
        ("---yaml ", Some(("{}", "")), "---yaml ", Some(("{}", ""))),
        ("---\n", Some(("{}", "")), "---\n", Some(("{}", ""))),
        ("---", Some(("{}", "")), "---", Some(("{}", ""))),
        ("---\na: 1\n...\n---\n", Some(("{\"a\":1}", "")), "---\na: 1\n...\n---\n", Some(("{\"a\":1}", ""))),
        ("text\n---\na: 1\n---\n", Some(("{}", "text\n---\na: 1\n---\n")), "text\n---\na: 1\n---\n", Some(("{}", "text\n---\na: 1\n---\n"))),
        ("  ---\na: 1\n---\n", Some(("{}", "  ---\na: 1\n---\n")), "  ---\na: 1\n---\n", Some(("{}", "  ---\na: 1\n---\n"))),
        (" ---", Some(("{}", " ---")), " ---", Some(("{}", " ---"))),
        ("---\na: 1\n---\n---\nb: 2\n---\n", Some(("{\"a\":1}", "---\nb: 2\n---\n")), "---\na: 1\n---\n---\nb: 2\n---\n", Some(("{\"a\":1}", "---\nb: 2\n---\n"))),
        ("---\r\na: 1\r\n---\r\n\r\nbody", Some(("{\"a\":1}", "\r\nbody")), "---\r\na: 1\r\n---\r\n\r\nbody", Some(("{\"a\":1}", "\r\nbody"))),
        ("---\na: 1\n---\r\rbody", Some(("{\"a\":1}", "\rbody")), "---\na: 1\n---\r\rbody", Some(("{\"a\":1}", "\rbody"))),
        ("---\na: 1\n---\n\r\nbody", Some(("{\"a\":1}", "\r\nbody")), "---\na: 1\n---\n\r\nbody", Some(("{\"a\":1}", "\r\nbody"))),
        ("---\ndescription: Uses: an unquoted colon\n---\nbody", None, "---\ndescription: |-\n  Uses: an unquoted colon\n---\nbody", Some(("{\"description\":\"Uses: an unquoted colon\"}", "body"))),
        ("---\nname: x\ndescription: [Tag] text here\n---\n", None, "---\nname: x\ndescription: |-\n  [Tag] text here\n---\n", Some(("{\"name\":\"x\",\"description\":\"[Tag] text here\"}", ""))),
        ("---\ndescription: {a} b\n---\n", None, "---\ndescription: |-\n  {a} b\n---\n", Some(("{\"description\":\"{a} b\"}", ""))),
        ("---\ndescription: \"quoted: colon\"\n---\n", Some(("{\"description\":\"quoted: colon\"}", "")), "---\ndescription: \"quoted: colon\"\n---\n", Some(("{\"description\":\"quoted: colon\"}", ""))),
        ("---\ndescription: 'x: y'\n---\n", Some(("{\"description\":\"x: y\"}", "")), "---\ndescription: 'x: y'\n---\n", Some(("{\"description\":\"x: y\"}", ""))),
        ("---\ndescription: >\n  folded: x\n---\n", Some(("{\"description\":\"folded: x\\n\"}", "")), "---\ndescription: >\n  folded: x\n---\n", Some(("{\"description\":\"folded: x\\n\"}", ""))),
        ("---\n  indented: a: b\n---\n", None, "---\n  indented: a: b\n---\n", None),
        ("---\n# comment: x\n---\n", Some(("{}", "")), "---\n# comment: x\n---\n", Some(("{}", ""))),
        ("---\ndesc-ription: a: b\n---\n", None, "---\ndesc-ription: a: b\n---\n", None),
        ("---\ndescription: a: b\r\nname: c\r\n---\r\nbody", None, "---\ndescription: |-\n  a: b\nname: c\r\n---\r\nbody", Some(("{\"description\":\"a: b\",\"name\":\"c\"}", "body"))),
        ("---\nkey  :  a: b  \n---\n", None, "---\nkey: |-\n  a: b\n---\n", Some(("{\"key\":\"a: b\"}", ""))),
        ("---\n_k: a:b\n---\n", Some(("{\"_k\":\"a:b\"}", "")), "---\n_k: |-\n  a:b\n---\n", Some(("{\"_k\":\"a:b\"}", ""))),
        ("---\na: x\na: y\n---\n", None, "---\na: x\na: y\n---\n", None),
        ("---\ndescription: [a, b]\n---\n", Some(("{\"description\":[\"a\",\"b\"]}", "")), "---\ndescription: [a, b]\n---\n", Some(("{\"description\":[\"a\",\"b\"]}", ""))),
        ("---\ndescription: [a] b: c\n---", Some(("{\"description\":[\"a\"],\"b\":\"c\"}", "")), "---\ndescription: |-\n  [a] b: c\n---", Some(("{\"description\":[\"a\"],\"b\":\"c\"}", ""))),
        ("---\ndescription: [a]\n---\n", Some(("{\"description\":[\"a\"]}", "")), "---\ndescription: [a]\n---\n", Some(("{\"description\":[\"a\"]}", ""))),
        ("---\ndescription: [a]]\n---\n", None, "---\ndescription: |-\n  [a]]\n---\n", Some(("{\"description\":\"[a]]\"}", ""))),
        ("---\ndescription: [a] b\nname: [unclosed\n---\n", None, "---\ndescription: |-\n  [a] b\nname: [unclosed\n---\n", None),
        ("---\nd: x: y\nother: |\n  keep: this\n---\nbody", None, "---\nd: |-\n  x: y\nother: |\n  keep: this\n---\nbody", Some(("{\"d\":\"x: y\",\"other\":\"keep: this\\n\"}", "body"))),
        ("---\nd:   spaced: out   \n---\n", None, "---\nd: |-\n  spaced: out\n---\n", Some(("{\"d\":\"spaced: out\"}", ""))),
        ("---\nd: a: b\n---\nrest\n---\nmore", None, "---\nd: |-\n  a: b\n---\nrest\n---\nmore", Some(("{\"d\":\"a: b\"}", "rest\n---\nmore"))),
        ("---\nd: x\u{2028}y: z\n---\n", None, "---\nd: x\u{2028}y: z\n---\n", None),
        ("---\nd:\u{2028}a: b\n---\n", Some(("{\"d:\u{2028}a\":\"b\"}", "")), "---\nd: |-\n  a: b\n---\n", Some(("{\"d:\u{2028}a\":\"b\"}", ""))),
        ("---\n\u{feff}d: a: b\n---\n", None, "---\n\u{feff}d: a: b\n---\n", None),
        ("---\nd: a: b", None, "---\nd: a: b", None),
        ("---\n\n---\nd: a: b\n---\n", Some(("{}", "d: a: b\n---\n")), "---\n\n---\nd: a: b\n---\n", Some(("{}", "d: a: b\n---\n"))),
        ("---\r\nd: a: b\r\n---", None, "---\r\nd: |-\n  a: b\r\n---", Some(("{\"d\":\"a: b\"}", ""))),
        ("---\nd: |\n---\n", Some(("{\"d\":\"\"}", "")), "---\nd: |\n---\n", Some(("{\"d\":\"\"}", ""))),
        ("---\nd: >\n---\n", Some(("{\"d\":\"\"}", "")), "---\nd: >\n---\n", Some(("{\"d\":\"\"}", ""))),
        ("---\nd: 'a: b\n---\n", None, "---\nd: 'a: b\n---\n", None),
        ("---\nd: \"a: b\n---\n", None, "---\nd: \"a: b\n---\n", None),
        ("---\nd: [x] y\n---\n", None, "---\nd: |-\n  [x] y\n---\n", Some(("{\"d\":\"[x] y\"}", ""))),
        ("---\nd: {x}y\n---\n", None, "---\nd: |-\n  {x}y\n---\n", Some(("{\"d\":\"{x}y\"}", ""))),
        ("---\nd: [x]\t\n---\n", Some(("{\"d\":[\"x\"]}", "")), "---\nd: [x]\t\n---\n", Some(("{\"d\":[\"x\"]}", ""))),
        ("---\nd: x # c: y\n---\n", Some(("{\"d\":\"x\"}", "")), "---\nd: |-\n  x # c: y\n---\n", Some(("{\"d\":\"x\"}", ""))),
        ("---\n9d: a: b\n---\n", None, "---\n9d: a: b\n---\n", None),
        ("---\nd: a:b\n  e: f\n---\n", None, "---\nd: |-\n  a:b\n  e: f\n---\n", Some(("{\"d\":\"a:b\\ne: f\"}", ""))),
        ("---\nD_1: a: b\n---\n", None, "---\nD_1: |-\n  a: b\n---\n", Some(("{\"D_1\":\"a: b\"}", ""))),
        ("---\nd : : :\n---\n", None, "---\nd: |-\n  : :\n---\n", Some(("{\"d\":\": :\"}", ""))),
        ("---\nd: @x: y\n---\n", None, "---\nd: |-\n  @x: y\n---\n", Some(("{\"d\":\"@x: y\"}", ""))),
        ("---\nd: *x: y\n---\n", None, "---\nd: |-\n  *x: y\n---\n", Some(("{\"d\":\"*x: y\"}", ""))),
        ("---\nd: 2001-12-14\n---\n", Some(("{\"d\":\"2001-12-14T00:00:00.000Z\"}", "")), "---\nd: 2001-12-14\n---\n", Some(("{\"d\":\"2001-12-14T00:00:00.000Z\"}", ""))),
        ("---\nd: !!binary aGk=\n---\n", Some(("{\"d\":{\"type\":\"Buffer\",\"data\":[104,105]}}", "")), "---\nd: !!binary aGk=\n---\n", Some(("{\"d\":{\"type\":\"Buffer\",\"data\":[104,105]}}", ""))),
        ("---\nmodel: anthropic/claude: latest\n---\nPrompt", None, "---\nmodel: |-\n  anthropic/claude: latest\n---\nPrompt", Some(("{\"model\":\"anthropic/claude: latest\"}", "Prompt"))),
    ];

    fn file(result: Result<Markdown, Error>) -> Option<(String, String)> {
        result.ok().map(|m| {
            let mut out = Out::new();
            out.json(&jsvalue::stringify(&m.data.root).unwrap().unwrap_or_default());
            (out.finish(), m.content)
        })
    }

    fn owned(expected: File) -> Option<(String, String)> {
        expected.map(|(data, content)| (data.to_owned(), content.to_owned()))
    }

    #[test]
    fn matches_gray_matter_and_config_markdown() {
        let mut failures = Vec::new();
        for &(input, matter_expected, sanitize_expected, parse_expected) in CASES {
            let got = file(matter(input));
            if got != owned(matter_expected) {
                failures.push(format!("matter {input:?}: gray-matter {matter_expected:?}, port {got:?}"));
            }
            let got = sanitize(input);
            if got != sanitize_expected {
                failures.push(format!("sanitize {input:?}: TS {sanitize_expected:?}, port {got:?}"));
            }
            let got = file(parse(input));
            if got != owned(parse_expected) {
                failures.push(format!("parse {input:?}: TS {parse_expected:?}, port {got:?}"));
            }
            if parse_option(input).is_some() != parse_expected.is_some() {
                failures.push(format!("parse_option {input:?}"));
            }
        }
        assert!(failures.is_empty(), "{} differences:\n{}", failures.len(), failures.join("\n"));
    }

    // From the same run: the engines of `Object.prototype` members (`String(data)` of their result in a JSON array, or
    // `null` where gray-matter throws).
    #[test]
    fn object_prototype_names_are_engines() {
        let data = |name: &str| -> Option<String> {
            let parsed = matter(&format!("---{name}\na: 1\n---\nbody")).ok()?;
            Some(match &parsed.data.root {
                Value::String(s) => format!("string {s}"),
                Value::Bool(b) => format!("boolean {b}"),
                Value::Object(o) => format!("{:?} {:?}", o.class(), jsvalue::keys(&parsed.data.root)),
                other => format!("{other:?}"),
            })
        };
        assert_eq!(data("toString").as_deref(), Some("string [object Object]"));
        assert_eq!(data("toLocaleString").as_deref(), Some("string [object Object]"));
        assert_eq!(data("valueOf").as_deref(), Some("Plain [\"parse\"]"));
        assert_eq!(data("hasOwnProperty").as_deref(), Some("boolean false"));
        assert_eq!(data("propertyIsEnumerable").as_deref(), Some("boolean false"));
        assert_eq!(data("isPrototypeOf").as_deref(), Some("boolean false"));
        assert_eq!(data("__lookupGetter__").as_deref(), Some("Plain []"));
        assert_eq!(data("__defineGetter__"), None);
        assert_eq!(data("__proto__"), None);
        assert_eq!(data("constructor").as_deref(), Some("Boxed(String(\"\\na: 1\")) [\"0\", \"1\", \"2\", \"3\", \"4\"]"));
        assert_eq!(data("Constructor"), None);
    }
}
