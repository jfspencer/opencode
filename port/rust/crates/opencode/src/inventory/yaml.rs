//! YAML as markdown frontmatter reads it: a port of js-yaml 3.15.2's `safeLoad` (lib/js-yaml/loader.js with the
//! DEFAULT_SAFE_SCHEMA types of lib/js-yaml/type), the parser gray-matter 4.0.3 uses. The reader follows loader.js
//! function by function (same state machine, same checks in the same order), so it accepts, rejects and produces
//! exactly what js-yaml does, quirks included: `a: !!str\n  123` loads `{"a": 123}`, an anchor on a node that
//! starts on the next line (`a: &x\n  foo`) names an empty mapping, `yes`/`no`/`on` are strings, `012` is octal,
//! `1:30` is 90, merge keys (`<<`) and `%YAML`/`%TAG` directives work, duplicate keys and tab indentation fail.
//!
//! Values become [`Json`] the way `JSON.stringify` would see them:
//! - Empty input, or input of only whitespace, is `undefined` in js-yaml; a document of only comments or blank
//!   lines is `null`. Both are [`Json::Null`] here.
//! - Numbers are `f64` as in JS (integers parse like JavaScriptCore's `parseInt`, which bun runs); `.inf`/`.nan`
//!   are kept as non-finite numbers.
//! - A `!!timestamp` (implicit for `2001-12-14` and friends) is a JS `Date` in js-yaml; it is the string of its
//!   `Date#toISOString()` here (what `JSON.stringify` writes), so code that checks for a string accepts it where
//!   TS would see a `Date`. Used as a mapping key it is `Date#toString()` as in a UTC process.
//! - A `!!binary` is a Node `Buffer`; it is `{"type": "Buffer", "data": [bytes]}` here (its `toJSON`).
//! - Mapping keys are coerced with `String(key)` as js-yaml does (`1.0` is `"1"`, `[a, b]` is `"a,b"`, a mapping
//!   is `"[object Object]"`).
//!
//! Divergences, all on input no frontmatter has (each is an [`Error`] where js-yaml returns a value):
//! - TODO(port): a value that contains itself through an alias (`&a [*a]`) is a cyclic JS object; JSON cannot
//!   hold it.
//! - TODO(port): nesting deeper than [`MAX_DEPTH`] levels (JS fails only when its stack runs out), and aliases that
//!   expand to more than [`MAX_NODES`] values (JS shares the nodes instead of copying them).
//! - TODO(port): a double-quoted escape that yields a lone UTF-16 surrogate (`"\uD800"`) is U+FFFD: a Rust
//!   string cannot hold it.
//!
//! Error messages are js-yaml's reasons with a line and column; the snippet js-yaml appends is left out.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

use indexmap::IndexMap;
use opencode_protocol::Json;
use opencode_protocol::wire::number::js_number_string;

/// The deepest nesting of nodes loaded (see the module docs). The reader recurses like loader.js; a level of block
/// mapping takes several KiB of stack in a debug build, and this keeps the deepest document within a 2 MiB thread.
pub const MAX_DEPTH: usize = 100;
/// The most values one document may expand to through aliases (see the module docs).
pub const MAX_NODES: usize = 1_000_000;

/// The input is not YAML js-yaml loads: where it throws, and the divergences listed in the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// `yaml.safeLoad(text)`: the single document of `text` (see the module docs for how values map to JSON).
pub fn load(text: &str) -> Result<Json, Error> {
    let mut input = text.to_owned();
    if !input.is_empty() {
        if !input.ends_with(['\n', '\r']) {
            input.push('\n');
        }
        if input.starts_with('\u{feff}') {
            input.drain(..'\u{feff}'.len_utf8());
        }
    }
    let mut state = State::new(input);
    if let Some(at) = state.input.bytes().position(|b| b == 0) {
        state.position = at;
        return Err(state.error("null byte is not allowed in input"));
    }
    while state.ch(state.position) == b' ' {
        state.line_indent += 1;
        state.position += 1;
    }
    while (state.position as i64) < state.input.len() as i64 - 1 {
        state.read_document()?;
    }
    match state.documents.as_slice() {
        [] => Ok(Json::Null),
        [document] => Converter { path: Vec::new(), budget: MAX_NODES }.convert(document),
        _ => Err(Error { message: "expected a single document in the stream, but found more".to_owned() }),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Values while loading: JS values, with arrays and objects shared by reference as aliases share them in JS.

type Seq = Rc<RefCell<Vec<V>>>;
type Map = Rc<RefCell<IndexMap<String, V>>>;

#[derive(Clone, Default)]
enum V {
    #[default]
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    /// A JS `Date`: milliseconds since the epoch (always a valid time here).
    Date(i64),
    /// A Node `Buffer`.
    Bin(Rc<Vec<u8>>),
    Seq(Seq),
    Map(Map),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Scalar,
    Sequence,
    Mapping,
}

fn kind_name(kind: Option<Kind>) -> &'static str {
    match kind {
        None => "null",
        Some(Kind::Scalar) => "scalar",
        Some(Kind::Sequence) => "sequence",
        Some(Kind::Mapping) => "mapping",
    }
}

const CONTEXT_FLOW_IN: u8 = 1;
const CONTEXT_FLOW_OUT: u8 = 2;
const CONTEXT_BLOCK_IN: u8 = 3;
const CONTEXT_BLOCK_OUT: u8 = 4;

const CHOMPING_CLIP: u8 = 1;
const CHOMPING_STRIP: u8 = 2;
const CHOMPING_KEEP: u8 = 3;

const MERGE_TAG: &str = "tag:yaml.org,2002:merge";
const MAX_TOTAL_MERGE_KEYS: usize = 10000;

fn is_eol(c: u8) -> bool {
    c == b'\n' || c == b'\r'
}

fn is_white_space(c: u8) -> bool {
    c == b'\t' || c == b' '
}

fn is_ws_or_eol(c: u8) -> bool {
    is_white_space(c) || is_eol(c)
}

fn is_flow_indicator(c: u8) -> bool {
    matches!(c, b',' | b'[' | b']' | b'{' | b'}')
}

fn from_hex_code(c: u8) -> Option<u32> {
    (c as char).to_digit(16)
}

fn escaped_hex_len(c: u8) -> usize {
    match c {
        b'x' => 2,
        b'u' => 4,
        b'U' => 8,
        _ => 0,
    }
}

fn simple_escape(c: u8) -> Option<u16> {
    Some(match c {
        b'0' => 0x00,
        b'a' => 0x07,
        b'b' => 0x08,
        b't' | b'\t' => 0x09,
        b'n' => 0x0A,
        b'v' => 0x0B,
        b'f' => 0x0C,
        b'r' => 0x0D,
        b'e' => 0x1B,
        b' ' => 0x20,
        b'"' => 0x22,
        b'/' => 0x2F,
        b'\\' => 0x5C,
        b'N' => 0x85,
        b'_' => 0xA0,
        b'L' => 0x2028,
        b'P' => 0x2029,
        _ => return None,
    })
}

// PATTERN_NON_PRINTABLE (lone surrogates cannot occur in a Rust string).
fn is_non_printable(c: char) -> bool {
    matches!(c, '\x00'..='\x08' | '\x0B' | '\x0C' | '\x0E'..='\x1F' | '\x7F'..='\u{84}' | '\u{86}'..='\u{9F}' | '\u{FFFE}' | '\u{FFFF}')
}

// PATTERN_TAG_HANDLE: /^(?:!|!!|![a-z\-]+!)$/i
fn is_tag_handle(s: &str) -> bool {
    s == "!"
        || s == "!!"
        || (s.len() > 2
            && s.starts_with('!')
            && s.ends_with('!')
            && s[1..s.len() - 1].bytes().all(|b| b.is_ascii_alphabetic() || b == b'-'))
}

// PATTERN_TAG_URI: /^(?:!|[^,\[\]\{\}])(?:%[0-9a-f]{2}|[0-9a-z\-#;\/\?:@&=\+\$,_\.!~\*'\(\)\[\]])*$/i, on UTF-16
// code units: the first may be any BMP character but a flow indicator, the rest are ASCII.
fn is_tag_uri(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '!' || (!matches!(c, ',' | '[' | ']' | '{' | '}') && c.len_utf16() == 1) => {}
        _ => return false,
    }
    let rest = chars.as_str().as_bytes();
    let mut i = 0;
    while i < rest.len() {
        let b = rest[i];
        if b == b'%' && rest.get(i + 1).is_some_and(u8::is_ascii_hexdigit) && rest.get(i + 2).is_some_and(u8::is_ascii_hexdigit) {
            i += 3;
        } else if b.is_ascii_alphanumeric() || b"-#;/?:@&=+$,_.!~*'()[]".contains(&b) {
            i += 1;
        } else {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------------------------------------------
// The loader (loader.js `State` and the functions taking it).

struct State {
    input: String,
    position: usize,
    line: usize,
    line_start: usize,
    line_indent: i64,
    total_merge_keys: usize,
    depth: usize,
    documents: Vec<V>,
    version: Option<String>,
    tag_map: HashMap<String, String>,
    anchor_map: HashMap<String, V>,
    tag: Option<String>,
    anchor: Option<String>,
    kind: Option<Kind>,
    result: V,
    /// Every array and object made, emptied when loading ends so that values containing themselves are freed.
    containers: Vec<V>,
}

impl Drop for State {
    fn drop(&mut self) {
        for c in &self.containers {
            match c {
                V::Seq(s) => s.borrow_mut().clear(),
                V::Map(m) => m.borrow_mut().clear(),
                _ => {}
            }
        }
        self.anchor_map.clear();
    }
}

impl State {
    fn new(input: String) -> State {
        State {
            input,
            position: 0,
            line: 0,
            line_start: 0,
            line_indent: 0,
            total_merge_keys: 0,
            depth: 0,
            documents: Vec::new(),
            version: None,
            tag_map: HashMap::new(),
            anchor_map: HashMap::new(),
            tag: None,
            anchor: None,
            kind: None,
            result: V::Null,
            containers: Vec::new(),
        }
    }

    /// The code unit at `i`; 0 past the end (loader.js appends a NUL terminator).
    fn ch(&self, i: usize) -> u8 {
        self.input.as_bytes().get(i).copied().unwrap_or(0)
    }

    /// `input.slice(a, b)`. The readers only cut before ASCII characters, so the bytes are whole characters.
    fn slice(&self, a: usize, b: usize) -> String {
        String::from_utf8_lossy(&self.input.as_bytes()[a..b]).into_owned()
    }

    fn error(&self, message: impl Into<String>) -> Error {
        let bytes = self.input.as_bytes();
        let end = self.position.min(bytes.len());
        let start = self.line_start.min(end);
        let column = String::from_utf8_lossy(&bytes[start..end]).chars().count();
        Error { message: format!("{} at line {}, column {}", message.into(), self.line + 1, column + 1) }
    }

    fn new_seq(&mut self) -> Seq {
        let s: Seq = Rc::default();
        self.containers.push(V::Seq(s.clone()));
        s
    }

    fn new_map(&mut self) -> Map {
        let m: Map = Rc::default();
        self.containers.push(V::Map(m.clone()));
        m
    }

    // `state.anchorMap[name] = value` on a plain object: `__proto__` sets the prototype, not an entry.
    fn set_anchor(&mut self, name: &str, value: V) {
        if name != "__proto__" {
            self.anchor_map.insert(name.to_owned(), value);
        }
    }

    fn register_anchor(&mut self) {
        if let Some(anchor) = self.anchor.clone() {
            let result = self.result.clone();
            self.set_anchor(&anchor, result);
        }
    }

    fn capture_segment(&self, out: &mut String, start: usize, end: usize, check_json: bool) -> Result<(), Error> {
        if start < end {
            let segment = self.slice(start, end);
            if check_json {
                if segment.chars().any(|c| !(c == '\t' || c >= ' ')) {
                    return Err(self.error("expected valid JSON character"));
                }
            } else if segment.chars().any(is_non_printable) {
                return Err(self.error("the stream contains non-printable characters"));
            }
            out.push_str(&segment);
        }
        Ok(())
    }

    fn charge_merge_work(&mut self) -> Result<(), Error> {
        self.total_merge_keys += 1;
        if self.total_merge_keys > MAX_TOTAL_MERGE_KEYS {
            return Err(self.error(format!("merge keys exceeded maxTotalMergeKeys ({MAX_TOTAL_MERGE_KEYS})")));
        }
        Ok(())
    }

    fn merge_mappings(&mut self, destination: &Map, source: &V, overridable: &mut HashSet<String>) -> Result<(), Error> {
        // `Object.keys(source)` of anything `typeof "object"`.
        let entries: Vec<(String, V)> = match source {
            V::Map(m) => m.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            V::Seq(s) => s.borrow().iter().enumerate().map(|(i, v)| (i.to_string(), v.clone())).collect(),
            V::Bin(b) => b.iter().enumerate().map(|(i, &byte)| (i.to_string(), V::Num(byte as f64))).collect(),
            V::Date(_) => Vec::new(),
            _ => return Err(self.error("cannot merge mappings; the provided source object is unacceptable")),
        };
        self.charge_merge_work()?;
        for (key, value) in entries {
            self.charge_merge_work()?;
            if !destination.borrow().contains_key(&key) {
                destination.borrow_mut().insert(key.clone(), value);
                if key != "__proto__" {
                    overridable.insert(key);
                }
            }
        }
        Ok(())
    }

    fn store_mapping_pair(
        &mut self,
        result: &Map,
        overridable: &mut HashSet<String>,
        key_tag: Option<String>,
        key_node: V,
        value_node: V,
        start: Option<(usize, usize)>,
    ) -> Result<(), Error> {
        let key = self.key_string(&key_node)?;
        if key_tag.as_deref() == Some(MERGE_TAG) {
            if let V::Seq(items) = &value_node {
                let items = items.borrow().clone();
                if items.len() > 100 {
                    return Err(self.error("abnormal merge sequence size"));
                }
                for item in &items {
                    self.merge_mappings(result, item, overridable)?;
                }
            } else {
                self.merge_mappings(result, &value_node, overridable)?;
            }
        } else {
            if !overridable.contains(&key) && result.borrow().contains_key(&key) {
                if let Some((line, position)) = start {
                    self.line = line;
                    self.position = position;
                }
                return Err(self.error("duplicated mapping key"));
            }
            result.borrow_mut().insert(key.clone(), value_node);
            overridable.remove(&key);
        }
        Ok(())
    }

    /// `String(keyNode)` after storeMappingPair's guards.
    fn key_string(&self, key: &V) -> Result<String, Error> {
        match key {
            V::Seq(items) => {
                let items = items.borrow();
                let mut parts = Vec::with_capacity(items.len());
                for item in items.iter() {
                    parts.push(match item {
                        V::Seq(_) => return Err(self.error("nested arrays are not supported inside keys")),
                        V::Map(_) => "[object Object]".to_owned(),
                        V::Null => String::new(),
                        scalar => js_string(scalar),
                    });
                }
                Ok(parts.join(","))
            }
            V::Map(_) => Ok("[object Object]".to_owned()),
            scalar => Ok(js_string(scalar)),
        }
    }

    fn read_line_break(&mut self) -> Result<(), Error> {
        match self.ch(self.position) {
            b'\n' => self.position += 1,
            b'\r' => {
                self.position += 1;
                if self.ch(self.position) == b'\n' {
                    self.position += 1;
                }
            }
            _ => return Err(self.error("a line break is expected")),
        }
        self.line += 1;
        self.line_start = self.position;
        Ok(())
    }

    /// Skips white space, line breaks and (when allowed) comments; returns the line breaks crossed.
    fn skip_separation_space(&mut self, allow_comments: bool) -> usize {
        let mut line_breaks = 0;
        let mut ch = self.ch(self.position);
        while ch != 0 {
            while is_white_space(ch) {
                self.position += 1;
                ch = self.ch(self.position);
            }
            if allow_comments && ch == b'#' {
                loop {
                    self.position += 1;
                    ch = self.ch(self.position);
                    if ch == b'\n' || ch == b'\r' || ch == 0 {
                        break;
                    }
                }
            }
            if !is_eol(ch) {
                break;
            }
            // Cannot fail: `ch` is a line break.
            let _ = self.read_line_break();
            ch = self.ch(self.position);
            line_breaks += 1;
            self.line_indent = 0;
            while ch == b' ' {
                self.line_indent += 1;
                self.position += 1;
                ch = self.ch(self.position);
            }
        }
        line_breaks
    }

    fn test_document_separator(&self) -> bool {
        let p = self.position;
        let ch = self.ch(p);
        if (ch == b'-' || ch == b'.') && ch == self.ch(p + 1) && ch == self.ch(p + 2) {
            let next = self.ch(p + 3);
            return next == 0 || is_ws_or_eol(next);
        }
        false
    }

    fn read_plain_scalar(&mut self, node_indent: i64, within_flow: bool) -> Result<bool, Error> {
        let mut ch = self.ch(self.position);
        if is_ws_or_eol(ch)
            || is_flow_indicator(ch)
            || matches!(ch, b'#' | b'&' | b'*' | b'!' | b'|' | b'>' | b'\'' | b'"' | b'%' | b'@' | b'`')
        {
            return Ok(false);
        }
        if ch == b'?' || ch == b'-' {
            let following = self.ch(self.position + 1);
            if is_ws_or_eol(following) || (within_flow && is_flow_indicator(following)) {
                return Ok(false);
            }
        }
        let mut out = String::new();
        let mut capture_start = self.position;
        let mut capture_end = self.position;
        let mut has_pending_content = false;
        let mut saved_line = self.line;
        while ch != 0 {
            if ch == b':' {
                let following = self.ch(self.position + 1);
                if is_ws_or_eol(following) || (within_flow && is_flow_indicator(following)) {
                    break;
                }
            } else if ch == b'#' {
                if is_ws_or_eol(self.ch(self.position - 1)) {
                    break;
                }
            } else if (self.position == self.line_start && self.test_document_separator())
                || (within_flow && is_flow_indicator(ch))
            {
                break;
            } else if is_eol(ch) {
                saved_line = self.line;
                let (line_start, line_indent) = (self.line_start, self.line_indent);
                self.skip_separation_space(false);
                if self.line_indent >= node_indent {
                    has_pending_content = true;
                    ch = self.ch(self.position);
                    continue;
                }
                self.position = capture_end;
                self.line = saved_line;
                self.line_start = line_start;
                self.line_indent = line_indent;
                break;
            }
            if has_pending_content {
                self.capture_segment(&mut out, capture_start, capture_end, false)?;
                write_folded_lines(&mut out, self.line - saved_line);
                capture_start = self.position;
                capture_end = self.position;
                has_pending_content = false;
            }
            if !is_white_space(ch) {
                capture_end = self.position + 1;
            }
            self.position += 1;
            ch = self.ch(self.position);
        }
        self.capture_segment(&mut out, capture_start, capture_end, false)?;
        if out.is_empty() {
            return Ok(false);
        }
        self.kind = Some(Kind::Scalar);
        self.result = V::Str(out);
        Ok(true)
    }

    // js-yaml's quoted-scalar readers take the node indentation only to warn about it.
    fn read_single_quoted_scalar(&mut self) -> Result<bool, Error> {
        if self.ch(self.position) != b'\'' {
            return Ok(false);
        }
        let mut out = String::new();
        self.position += 1;
        let mut capture_start = self.position;
        let mut capture_end = self.position;
        loop {
            let ch = self.ch(self.position);
            if ch == 0 {
                break;
            }
            if ch == b'\'' {
                self.capture_segment(&mut out, capture_start, self.position, true)?;
                self.position += 1;
                if self.ch(self.position) == b'\'' {
                    capture_start = self.position;
                    self.position += 1;
                    capture_end = self.position;
                } else {
                    self.kind = Some(Kind::Scalar);
                    self.result = V::Str(out);
                    return Ok(true);
                }
            } else if is_eol(ch) {
                self.capture_segment(&mut out, capture_start, capture_end, true)?;
                let breaks = self.skip_separation_space(false);
                write_folded_lines(&mut out, breaks);
                capture_start = self.position;
                capture_end = self.position;
            } else if self.position == self.line_start && self.test_document_separator() {
                return Err(self.error("unexpected end of the document within a single quoted scalar"));
            } else {
                self.position += 1;
                capture_end = self.position;
            }
        }
        Err(self.error("unexpected end of the stream within a single quoted scalar"))
    }

    fn read_double_quoted_scalar(&mut self) -> Result<bool, Error> {
        if self.ch(self.position) != b'"' {
            return Ok(false);
        }
        let mut out = Utf16Out::default();
        self.position += 1;
        let mut capture_start = self.position;
        let mut capture_end = self.position;
        loop {
            let mut ch = self.ch(self.position);
            if ch == 0 {
                break;
            }
            if ch == b'"' {
                self.capture_segment(out.text(capture_start < self.position), capture_start, self.position, true)?;
                self.position += 1;
                self.kind = Some(Kind::Scalar);
                self.result = V::Str(out.finish());
                return Ok(true);
            } else if ch == b'\\' {
                self.capture_segment(out.text(capture_start < self.position), capture_start, self.position, true)?;
                self.position += 1;
                ch = self.ch(self.position);
                if is_eol(ch) {
                    self.skip_separation_space(false);
                } else if let Some(unit) = simple_escape(ch) {
                    out.unit(unit);
                    self.position += 1;
                } else if escaped_hex_len(ch) > 0 {
                    // JS bit arithmetic: `(hexResult << 4) + digit` on 32-bit integers, then
                    // `String.fromCharCode` (ToUint16) of the value or of its surrogate pair.
                    let mut hex: i32 = 0;
                    for _ in 0..escaped_hex_len(ch) {
                        self.position += 1;
                        match from_hex_code(self.ch(self.position)) {
                            Some(digit) => hex = (hex << 4).wrapping_add(digit as i32),
                            None => return Err(self.error("expected hexadecimal character")),
                        }
                    }
                    if hex <= 0xFFFF {
                        out.unit(hex as u16);
                    } else {
                        let c = hex - 0x10000;
                        out.unit(((c >> 10) + 0xD800) as u16);
                        out.unit(((c & 0x03FF) + 0xDC00) as u16);
                    }
                    self.position += 1;
                } else {
                    return Err(self.error("unknown escape sequence"));
                }
                capture_start = self.position;
                capture_end = self.position;
            } else if is_eol(ch) {
                self.capture_segment(out.text(capture_start < capture_end), capture_start, capture_end, true)?;
                let breaks = self.skip_separation_space(false);
                write_folded_lines(out.text(breaks > 0), breaks);
                capture_start = self.position;
                capture_end = self.position;
            } else if self.position == self.line_start && self.test_document_separator() {
                return Err(self.error("unexpected end of the document within a double quoted scalar"));
            } else {
                self.position += 1;
                capture_end = self.position;
            }
        }
        Err(self.error("unexpected end of the stream within a double quoted scalar"))
    }

    fn read_flow_collection(&mut self, node_indent: i64) -> Result<bool, Error> {
        let (terminator, is_mapping) = match self.ch(self.position) {
            b'[' => (b']', false),
            b'{' => (b'}', true),
            _ => return Ok(false),
        };
        let tag = self.tag.clone();
        let anchor = self.anchor.clone();
        let mut read_next = true;
        let mut overridable = HashSet::new();
        let result = if is_mapping { V::Map(self.new_map()) } else { V::Seq(self.new_seq()) };
        if let Some(anchor) = &anchor {
            self.set_anchor(anchor, result.clone());
        }
        self.position += 1;
        let mut ch = self.ch(self.position);
        while ch != 0 {
            self.skip_separation_space(true);
            ch = self.ch(self.position);
            if ch == terminator {
                self.position += 1;
                self.tag = tag;
                self.anchor = anchor;
                self.kind = Some(if is_mapping { Kind::Mapping } else { Kind::Sequence });
                self.result = result;
                return Ok(true);
            } else if !read_next {
                return Err(self.error("missed comma between flow collection entries"));
            }
            let mut value_node = V::Null;
            let mut is_pair = false;
            let mut is_explicit_pair = false;
            if ch == b'?' && is_ws_or_eol(self.ch(self.position + 1)) {
                is_pair = true;
                is_explicit_pair = true;
                self.position += 1;
                self.skip_separation_space(true);
            }
            let line = self.line;
            self.compose_node(node_indent, CONTEXT_FLOW_IN, false, true)?;
            let key_tag = self.tag.clone();
            let key_node = std::mem::take(&mut self.result);
            self.skip_separation_space(true);
            ch = self.ch(self.position);
            if (is_explicit_pair || self.line == line) && ch == b':' {
                is_pair = true;
                self.position += 1;
                self.skip_separation_space(true);
                self.compose_node(node_indent, CONTEXT_FLOW_IN, false, true)?;
                value_node = std::mem::take(&mut self.result);
            }
            if let V::Map(map) = &result {
                self.store_mapping_pair(map, &mut overridable, key_tag, key_node, value_node, None)?;
            } else if let V::Seq(seq) = &result {
                if is_pair {
                    let pair = self.new_map();
                    self.store_mapping_pair(&pair, &mut overridable, key_tag, key_node, value_node, None)?;
                    seq.borrow_mut().push(V::Map(pair));
                } else {
                    seq.borrow_mut().push(key_node);
                }
            }
            self.skip_separation_space(true);
            ch = self.ch(self.position);
            if ch == b',' {
                read_next = true;
                self.position += 1;
                ch = self.ch(self.position);
            } else {
                read_next = false;
            }
        }
        Err(self.error("unexpected end of the stream within a flow collection"))
    }

    fn read_block_scalar(&mut self, node_indent: i64) -> Result<bool, Error> {
        let folding = match self.ch(self.position) {
            b'|' => false,
            b'>' => true,
            _ => return Ok(false),
        };
        let mut chomping = CHOMPING_CLIP;
        let mut did_read_content = false;
        let mut detected_indent = false;
        let mut text_indent = node_indent;
        let mut empty_lines = 0usize;
        let mut at_more_indented = false;
        let mut out = String::new();
        let mut ch = self.ch(self.position);
        while ch != 0 {
            self.position += 1;
            ch = self.ch(self.position);
            if ch == b'+' || ch == b'-' {
                if chomping != CHOMPING_CLIP {
                    return Err(self.error("repeat of a chomping mode identifier"));
                }
                chomping = if ch == b'+' { CHOMPING_KEEP } else { CHOMPING_STRIP };
            } else if ch.is_ascii_digit() {
                let width = (ch - b'0') as i64;
                if width == 0 {
                    return Err(self.error(
                        "bad explicit indentation width of a block scalar; it cannot be less than one",
                    ));
                }
                if detected_indent {
                    return Err(self.error("repeat of an indentation width identifier"));
                }
                text_indent = node_indent + width - 1;
                detected_indent = true;
            } else {
                break;
            }
        }
        if is_white_space(ch) {
            loop {
                self.position += 1;
                ch = self.ch(self.position);
                if !is_white_space(ch) {
                    break;
                }
            }
            if ch == b'#' {
                loop {
                    self.position += 1;
                    ch = self.ch(self.position);
                    if is_eol(ch) || ch == 0 {
                        break;
                    }
                }
            }
        }
        while ch != 0 {
            self.read_line_break()?;
            self.line_indent = 0;
            ch = self.ch(self.position);
            while (!detected_indent || self.line_indent < text_indent) && ch == b' ' {
                self.line_indent += 1;
                self.position += 1;
                ch = self.ch(self.position);
            }
            if !detected_indent && self.line_indent > text_indent {
                text_indent = self.line_indent;
            }
            if is_eol(ch) {
                empty_lines += 1;
                continue;
            }
            // End of the scalar: chomp.
            if self.line_indent < text_indent {
                if chomping == CHOMPING_KEEP {
                    push_newlines(&mut out, if did_read_content { 1 + empty_lines } else { empty_lines });
                } else if chomping == CHOMPING_CLIP && did_read_content {
                    out.push('\n');
                }
                break;
            }
            if folding {
                if is_white_space(ch) {
                    // More-indented lines are not folded.
                    at_more_indented = true;
                    push_newlines(&mut out, if did_read_content { 1 + empty_lines } else { empty_lines });
                } else if at_more_indented {
                    at_more_indented = false;
                    push_newlines(&mut out, empty_lines + 1);
                } else if empty_lines == 0 {
                    if did_read_content {
                        out.push(' ');
                    }
                } else {
                    push_newlines(&mut out, empty_lines);
                }
            } else {
                push_newlines(&mut out, if did_read_content { 1 + empty_lines } else { empty_lines });
            }
            did_read_content = true;
            detected_indent = true;
            empty_lines = 0;
            let capture_start = self.position;
            while !is_eol(ch) && ch != 0 {
                self.position += 1;
                ch = self.ch(self.position);
            }
            self.capture_segment(&mut out, capture_start, self.position, false)?;
        }
        self.kind = Some(Kind::Scalar);
        self.result = V::Str(out);
        Ok(true)
    }

    fn read_block_sequence(&mut self, node_indent: i64) -> Result<bool, Error> {
        let tag = self.tag.clone();
        let anchor = self.anchor.clone();
        let result = self.new_seq();
        let mut detected = false;
        if let Some(anchor) = &anchor {
            self.set_anchor(anchor, V::Seq(result.clone()));
        }
        let mut ch = self.ch(self.position);
        while ch != 0 {
            if ch != b'-' || !is_ws_or_eol(self.ch(self.position + 1)) {
                break;
            }
            detected = true;
            self.position += 1;
            if self.skip_separation_space(true) > 0 && self.line_indent <= node_indent {
                result.borrow_mut().push(V::Null);
                ch = self.ch(self.position);
                continue;
            }
            let line = self.line;
            self.compose_node(node_indent, CONTEXT_BLOCK_IN, false, true)?;
            let item = std::mem::take(&mut self.result);
            result.borrow_mut().push(item);
            self.skip_separation_space(true);
            ch = self.ch(self.position);
            if (self.line == line || self.line_indent > node_indent) && ch != 0 {
                return Err(self.error("bad indentation of a sequence entry"));
            } else if self.line_indent < node_indent {
                break;
            }
        }
        if !detected {
            return Ok(false);
        }
        self.tag = tag;
        self.anchor = anchor;
        self.kind = Some(Kind::Sequence);
        self.result = V::Seq(result);
        Ok(true)
    }

    fn read_block_mapping(&mut self, node_indent: i64, flow_indent: i64) -> Result<bool, Error> {
        let tag = self.tag.clone();
        let anchor = self.anchor.clone();
        let result = self.new_map();
        let mut overridable = HashSet::new();
        let mut key_tag: Option<String> = None;
        let mut key_node = V::Null;
        let mut at_explicit_key = false;
        let mut detected = false;
        let mut allow_compact;
        if let Some(anchor) = &anchor {
            self.set_anchor(anchor, V::Map(result.clone()));
        }
        let mut ch = self.ch(self.position);
        while ch != 0 {
            let following = self.ch(self.position + 1);
            let line = self.line;
            let pos = self.position;
            if (ch == b'?' || ch == b':') && is_ws_or_eol(following) {
                // Explicit notation: `? key` then `: value`.
                if ch == b'?' {
                    if at_explicit_key {
                        let key = std::mem::take(&mut key_node);
                        self.store_mapping_pair(&result, &mut overridable, key_tag.take(), key, V::Null, None)?;
                    }
                    detected = true;
                    at_explicit_key = true;
                    allow_compact = true;
                } else if at_explicit_key {
                    at_explicit_key = false;
                    allow_compact = true;
                } else {
                    return Err(self.error(
                        "incomplete explicit mapping pair; a key node is missed; or followed by a non-tabulated empty line",
                    ));
                }
                self.position += 1;
                ch = following;
            } else if self.compose_node(flow_indent, CONTEXT_FLOW_OUT, false, true)? {
                // Implicit notation: a flow-style key, `:`, then the value.
                if self.line != line {
                    if detected {
                        return Err(self.error(
                            "can not read a block mapping entry; a multiline key may not be an implicit key",
                        ));
                    }
                    self.tag = tag;
                    self.anchor = anchor;
                    return Ok(true); // keep the result of compose_node
                }
                ch = self.ch(self.position);
                while is_white_space(ch) {
                    self.position += 1;
                    ch = self.ch(self.position);
                }
                if ch != b':' {
                    if detected {
                        return Err(self.error("can not read an implicit mapping pair; a colon is missed"));
                    }
                    self.tag = tag;
                    self.anchor = anchor;
                    return Ok(true); // keep the result of compose_node
                }
                self.position += 1;
                ch = self.ch(self.position);
                if !is_ws_or_eol(ch) {
                    return Err(self.error(
                        "a whitespace character is expected after the key-value separator within a block mapping",
                    ));
                }
                if at_explicit_key {
                    let key = std::mem::take(&mut key_node);
                    self.store_mapping_pair(&result, &mut overridable, key_tag.take(), key, V::Null, None)?;
                }
                detected = true;
                at_explicit_key = false;
                allow_compact = false;
                key_tag = self.tag.clone();
                key_node = std::mem::take(&mut self.result);
            } else {
                break;
            }

            if self.line == line || self.line_indent > node_indent {
                let mut value_node = V::Null;
                if self.compose_node(node_indent, CONTEXT_BLOCK_OUT, true, allow_compact)? {
                    if at_explicit_key {
                        key_node = std::mem::take(&mut self.result);
                    } else {
                        value_node = std::mem::take(&mut self.result);
                    }
                }
                if !at_explicit_key {
                    let key = std::mem::take(&mut key_node);
                    self.store_mapping_pair(&result, &mut overridable, key_tag.take(), key, value_node, Some((line, pos)))?;
                }
                self.skip_separation_space(true);
                ch = self.ch(self.position);
            }

            if self.line_indent > node_indent && ch != 0 {
                return Err(self.error("bad indentation of a mapping entry"));
            } else if self.line_indent < node_indent {
                break;
            }
        }
        // The last entry may be an explicit key alone.
        if at_explicit_key {
            self.store_mapping_pair(&result, &mut overridable, key_tag, key_node, V::Null, None)?;
        }
        if detected {
            self.tag = tag;
            self.anchor = anchor;
            self.kind = Some(Kind::Mapping);
            self.result = V::Map(result);
        }
        Ok(detected)
    }

    fn read_tag_property(&mut self) -> Result<bool, Error> {
        if self.ch(self.position) != b'!' {
            return Ok(false);
        }
        if self.tag.is_some() {
            return Err(self.error("duplication of a tag property"));
        }
        let mut is_verbatim = false;
        let mut is_named = false;
        let mut tag_handle = "!".to_owned();
        self.position += 1;
        let mut ch = self.ch(self.position);
        if ch == b'<' {
            is_verbatim = true;
            self.position += 1;
        } else if ch == b'!' {
            is_named = true;
            tag_handle = "!!".to_owned();
            self.position += 1;
        }
        ch = self.ch(self.position);
        let mut start = self.position;
        let tag_name;
        if is_verbatim {
            loop {
                self.position += 1;
                ch = self.ch(self.position);
                if ch == 0 || ch == b'>' {
                    break;
                }
            }
            if self.position >= self.input.len() {
                return Err(self.error("unexpected end of the stream within a verbatim tag"));
            }
            tag_name = self.slice(start, self.position);
            self.position += 1;
        } else {
            while ch != 0 && !is_ws_or_eol(ch) {
                if ch == b'!' {
                    if is_named {
                        return Err(self.error("tag suffix cannot contain exclamation marks"));
                    }
                    tag_handle = self.slice(start - 1, self.position + 1);
                    if !is_tag_handle(&tag_handle) {
                        return Err(self.error("named tag handle cannot contain such characters"));
                    }
                    is_named = true;
                    start = self.position + 1;
                }
                self.position += 1;
                ch = self.ch(self.position);
            }
            tag_name = self.slice(start, self.position);
            if tag_name.contains([',', '[', ']', '{', '}']) {
                return Err(self.error("tag suffix cannot contain flow indicator characters"));
            }
        }
        if !tag_name.is_empty() && !is_tag_uri(&tag_name) {
            return Err(self.error(format!("tag name cannot contain such characters: {tag_name}")));
        }
        self.tag = Some(if is_verbatim {
            tag_name
        } else if let Some(prefix) = self.tag_map.get(&tag_handle) {
            format!("{prefix}{tag_name}")
        } else if tag_handle == "!" {
            format!("!{tag_name}")
        } else if tag_handle == "!!" {
            format!("tag:yaml.org,2002:{tag_name}")
        } else {
            return Err(self.error(format!("undeclared tag handle \"{tag_handle}\"")));
        });
        Ok(true)
    }

    fn read_anchor_property(&mut self) -> Result<bool, Error> {
        if self.ch(self.position) != b'&' {
            return Ok(false);
        }
        if self.anchor.is_some() {
            return Err(self.error("duplication of an anchor property"));
        }
        let name = self.read_name();
        if name.is_empty() {
            return Err(self.error("name of an anchor node must contain at least one character"));
        }
        self.anchor = Some(name);
        Ok(true)
    }

    fn read_alias(&mut self) -> Result<bool, Error> {
        if self.ch(self.position) != b'*' {
            return Ok(false);
        }
        let alias = self.read_name();
        if alias.is_empty() {
            return Err(self.error("name of an alias node must contain at least one character"));
        }
        let Some(value) = self.anchor_map.get(&alias) else {
            return Err(self.error(format!("unidentified alias \"{alias}\"")));
        };
        self.result = value.clone();
        self.skip_separation_space(true);
        Ok(true)
    }

    // The name after `&` or `*`: up to white space, a line break or a flow indicator.
    fn read_name(&mut self) -> String {
        self.position += 1;
        let start = self.position;
        let mut ch = self.ch(self.position);
        while ch != 0 && !is_ws_or_eol(ch) && !is_flow_indicator(ch) {
            self.position += 1;
            ch = self.ch(self.position);
        }
        self.slice(start, self.position)
    }

    fn compose_node(&mut self, parent_indent: i64, context: u8, allow_to_seek: bool, allow_compact: bool) -> Result<bool, Error> {
        if self.depth >= MAX_DEPTH {
            return Err(self.error(format!("nesting deeper than {MAX_DEPTH} levels is not supported")));
        }
        self.depth += 1;
        let composed = self.compose_node_at_depth(parent_indent, context, allow_to_seek, allow_compact);
        self.depth -= 1;
        composed
    }

    fn compose_node_at_depth(
        &mut self,
        parent_indent: i64,
        context: u8,
        allow_to_seek: bool,
        allow_compact: bool,
    ) -> Result<bool, Error> {
        // 1: this > parent, 0: this = parent, -1: this < parent
        let indent_status = |line_indent: i64| match line_indent.cmp(&parent_indent) {
            std::cmp::Ordering::Greater => 1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Less => -1,
        };
        let mut status = 1;
        let mut at_new_line = false;
        let mut has_content = false;
        self.tag = None;
        self.anchor = None;
        self.kind = None;
        self.result = V::Null;
        let allow_block_styles = context == CONTEXT_BLOCK_OUT || context == CONTEXT_BLOCK_IN;
        let allow_block_scalars = allow_block_styles;
        let mut allow_block_collections = allow_block_styles;

        if allow_to_seek && self.skip_separation_space(true) > 0 {
            at_new_line = true;
            status = indent_status(self.line_indent);
        }
        if status == 1 {
            while self.read_tag_property()? || self.read_anchor_property()? {
                if self.skip_separation_space(true) > 0 {
                    at_new_line = true;
                    allow_block_collections = allow_block_styles;
                    status = indent_status(self.line_indent);
                } else {
                    allow_block_collections = false;
                }
            }
        }
        if allow_block_collections {
            allow_block_collections = at_new_line || allow_compact;
        }

        if status == 1 || context == CONTEXT_BLOCK_OUT {
            let flow_indent =
                if context == CONTEXT_FLOW_IN || context == CONTEXT_FLOW_OUT { parent_indent } else { parent_indent + 1 };
            let block_indent = (self.position - self.line_start) as i64;
            if status == 1 {
                if (allow_block_collections
                    && (self.read_block_sequence(block_indent)? || self.read_block_mapping(block_indent, flow_indent)?))
                    || self.read_flow_collection(flow_indent)?
                {
                    has_content = true;
                } else {
                    if (allow_block_scalars && self.read_block_scalar(flow_indent)?)
                        || self.read_single_quoted_scalar()?
                        || self.read_double_quoted_scalar()?
                    {
                        has_content = true;
                    } else if self.read_alias()? {
                        has_content = true;
                        if self.tag.is_some() || self.anchor.is_some() {
                            return Err(self.error("alias node should not have any properties"));
                        }
                    } else if self.read_plain_scalar(flow_indent, context == CONTEXT_FLOW_IN)? {
                        has_content = true;
                        if self.tag.is_none() {
                            self.tag = Some("?".to_owned());
                        }
                    }
                    self.register_anchor();
                }
            } else if status == 0 {
                // Block sequences may sit at their parent's indentation.
                has_content = allow_block_collections && self.read_block_sequence(block_indent)?;
            }
        }

        if let Some(tag) = self.tag.clone().filter(|t| t != "!") {
            if tag == "?" {
                // Implicit resolution, only for scalars.
                if !matches!(self.result, V::Null) && self.kind != Some(Kind::Scalar) {
                    return Err(self.error(format!(
                        "unacceptable node kind for !<?> tag; it should be \"scalar\", not \"{}\"",
                        kind_name(self.kind)
                    )));
                }
                let data = self.result.clone();
                for ty in IMPLICIT {
                    if let Some(value) = self.resolve(ty, &data).map_err(|m| self.error(m))? {
                        self.result = value;
                        self.tag = Some(ty.tag().to_owned());
                        self.register_anchor();
                        break;
                    }
                }
            } else if let Some(ty) = Type::from_tag(&tag).filter(|t| self.kind.is_none_or(|k| t.kind() == k)) {
                if !matches!(self.result, V::Null) && Some(ty.kind()) != self.kind {
                    return Err(self.error(format!(
                        "unacceptable node kind for !<{tag}> tag; it should be \"{}\", not \"{}\"",
                        kind_name(Some(ty.kind())),
                        kind_name(self.kind)
                    )));
                }
                let data = self.result.clone();
                match self.resolve(ty, &data).map_err(|m| self.error(m))? {
                    Some(value) => {
                        self.result = value;
                        self.register_anchor();
                    }
                    None => return Err(self.error(format!("cannot resolve a node with !<{tag}> explicit tag"))),
                }
            } else {
                return Err(self.error(format!("unknown tag !<{tag}>")));
            }
        }
        Ok(self.tag.is_some() || self.anchor.is_some() || has_content)
    }

    fn read_document(&mut self) -> Result<(), Error> {
        let mut has_directives = false;
        self.version = None;
        self.tag_map.clear();
        self.anchor_map.clear();

        while self.ch(self.position) != 0 {
            self.skip_separation_space(true);
            let mut ch = self.ch(self.position);
            if self.line_indent > 0 || ch != b'%' {
                break;
            }
            has_directives = true;
            self.position += 1;
            ch = self.ch(self.position);
            let start = self.position;
            while ch != 0 && !is_ws_or_eol(ch) {
                self.position += 1;
                ch = self.ch(self.position);
            }
            let name = self.slice(start, self.position);
            let mut args = Vec::new();
            if name.is_empty() {
                return Err(self.error("directive name must not be less than one character in length"));
            }
            while ch != 0 {
                while is_white_space(ch) {
                    self.position += 1;
                    ch = self.ch(self.position);
                }
                if ch == b'#' {
                    loop {
                        self.position += 1;
                        ch = self.ch(self.position);
                        if ch == 0 || is_eol(ch) {
                            break;
                        }
                    }
                    break;
                }
                if is_eol(ch) {
                    break;
                }
                let start = self.position;
                while ch != 0 && !is_ws_or_eol(ch) {
                    self.position += 1;
                    ch = self.ch(self.position);
                }
                args.push(self.slice(start, self.position));
            }
            if ch != 0 {
                self.read_line_break()?;
            }
            match name.as_str() {
                "YAML" => self.yaml_directive(&args)?,
                "TAG" => self.tag_directive(&args)?,
                _ => {} // js-yaml only warns
            }
        }

        self.skip_separation_space(true);
        if self.line_indent == 0
            && self.ch(self.position) == b'-'
            && self.ch(self.position + 1) == b'-'
            && self.ch(self.position + 2) == b'-'
        {
            self.position += 3;
            self.skip_separation_space(true);
        } else if has_directives {
            return Err(self.error("directives end mark is expected"));
        }

        self.compose_node(self.line_indent - 1, CONTEXT_BLOCK_OUT, false, true)?;
        self.skip_separation_space(true);
        let document = std::mem::take(&mut self.result);
        self.documents.push(document);

        if self.position == self.line_start && self.test_document_separator() {
            if self.ch(self.position) == b'.' {
                self.position += 3;
                self.skip_separation_space(true);
            }
            return Ok(());
        }
        if (self.position as i64) < self.input.len() as i64 - 1 {
            return Err(self.error("end of the stream or a document separator is expected"));
        }
        Ok(())
    }

    fn yaml_directive(&mut self, args: &[String]) -> Result<(), Error> {
        if self.version.is_some() {
            return Err(self.error("duplication of %YAML directive"));
        }
        let [version] = args else {
            return Err(self.error("YAML directive accepts exactly one argument"));
        };
        // The minor version decides only warnings.
        let Some((major, _)) = version.split_once('.').filter(|(major, minor)| {
            !major.is_empty() && !minor.is_empty() && major.bytes().chain(minor.bytes()).all(|b| b.is_ascii_digit())
        }) else {
            return Err(self.error("ill-formed argument of the YAML directive"));
        };
        if major.trim_start_matches('0') != "1" {
            return Err(self.error("unacceptable YAML version of the document"));
        }
        self.version = Some(version.clone());
        Ok(())
    }

    fn tag_directive(&mut self, args: &[String]) -> Result<(), Error> {
        let [handle, prefix] = args else {
            return Err(self.error("TAG directive accepts exactly two arguments"));
        };
        if !is_tag_handle(handle) {
            return Err(self.error("ill-formed tag handle (first argument) of the TAG directive"));
        }
        if self.tag_map.contains_key(handle) {
            return Err(self.error(format!("there is a previously declared suffix for \"{handle}\" tag handle")));
        }
        if !is_tag_uri(prefix) {
            return Err(self.error("ill-formed tag prefix (second argument) of the TAG directive"));
        }
        self.tag_map.insert(handle.clone(), prefix.clone());
        Ok(())
    }

    /// A type's `resolve` then `construct`: `None` where `resolve` is false, `Err` where JS throws a `TypeError`.
    fn resolve(&mut self, ty: Type, data: &V) -> Result<Option<V>, String> {
        let type_error = || format!("{} cannot construct this value", ty.tag());
        Ok(match ty {
            Type::Null => match data {
                V::Null => Some(V::Null),
                V::Str(s) if matches!(s.as_str(), "~" | "null" | "Null" | "NULL") => Some(V::Null),
                _ => None,
            },
            Type::Bool => match data {
                V::Str(s) => match s.as_str() {
                    "true" | "True" | "TRUE" => Some(V::Bool(true)),
                    "false" | "False" | "FALSE" => Some(V::Bool(false)),
                    _ => None,
                },
                _ => None,
            },
            Type::Int => match data {
                V::Str(s) => resolve_int(s).then(|| V::Num(construct_int(s))),
                V::Bin(b) if !b.is_empty() => return Err(type_error()),
                _ => None,
            },
            Type::Float => match data {
                V::Str(s) => (is_float(s) && !s.ends_with('_')).then(|| V::Num(construct_float(s))),
                V::Null => None,
                // `YAML_FLOAT_PATTERN.test(String(data))`, then `data.replace` throws
                other if is_float(&js_string(other)) => return Err(type_error()),
                _ => None,
            },
            Type::Timestamp => match data {
                V::Null => None,
                other => parse_timestamp(&js_string(other)).map(V::Date),
            },
            Type::Merge => match data {
                V::Null => Some(V::Null),
                V::Str(s) if s == "<<" => Some(data.clone()),
                _ => None,
            },
            Type::Str => Some(match data {
                V::Null => V::Str(String::new()),
                other => other.clone(),
            }),
            Type::Seq => Some(match data {
                V::Null => V::Seq(self.new_seq()),
                other => other.clone(),
            }),
            Type::Map => Some(match data {
                V::Null => V::Map(self.new_map()),
                other => other.clone(),
            }),
            Type::Binary => match data {
                V::Null => None,
                V::Str(s) => resolve_binary(s).then(|| V::Bin(Rc::new(construct_binary(s)))),
                _ => return Err(type_error()),
            },
            Type::Omap => match data {
                V::Null => Some(V::Seq(self.new_seq())),
                V::Seq(items) => {
                    let mut seen = HashSet::new();
                    let ok = items.borrow().iter().all(|pair| match pair {
                        V::Map(m) => {
                            let m = m.borrow();
                            m.len() == 1 && seen.insert(m.keys().next().cloned().unwrap_or_default())
                        }
                        _ => false,
                    });
                    ok.then(|| data.clone())
                }
                _ => None,
            },
            Type::Pairs => match data {
                V::Null => Some(V::Seq(self.new_seq())),
                V::Seq(items) => {
                    let items = items.borrow().clone();
                    let mut pairs = Vec::with_capacity(items.len());
                    for pair in &items {
                        let V::Map(m) = pair else { return Ok(None) };
                        let Some((key, value)) = m.borrow().iter().next().map(|(k, v)| (k.clone(), v.clone())) else {
                            return Ok(None);
                        };
                        if m.borrow().len() != 1 {
                            return Ok(None);
                        }
                        let entry = self.new_seq();
                        entry.borrow_mut().extend([V::Str(key), value]);
                        pairs.push(V::Seq(entry));
                    }
                    let result = self.new_seq();
                    *result.borrow_mut() = pairs;
                    Some(V::Seq(result))
                }
                _ => None,
            },
            Type::Set => match data {
                V::Null => Some(V::Map(self.new_map())),
                V::Map(m) => m.borrow().values().all(|v| matches!(v, V::Null)).then(|| data.clone()),
                _ => None,
            },
        })
    }
}

fn write_folded_lines(out: &mut String, count: usize) {
    if count == 1 {
        out.push(' ');
    } else if count > 1 {
        push_newlines(out, count - 1);
    }
}

fn push_newlines(out: &mut String, count: usize) {
    out.extend(std::iter::repeat_n('\n', count));
}

/// A double-quoted scalar's text, built from UTF-16 code units where escapes produce them: an escaped surrogate
/// pair joins into one character, a lone surrogate becomes U+FFFD.
#[derive(Default)]
struct Utf16Out {
    text: String,
    high: Option<u16>,
}

impl Utf16Out {
    fn unit(&mut self, unit: u16) {
        if let Some(high) = self.high.take() {
            if (0xDC00..0xE000).contains(&unit) {
                let c = 0x10000 + ((high as u32 - 0xD800) << 10) + (unit as u32 - 0xDC00);
                self.text.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                return;
            }
            self.text.push('\u{FFFD}');
        }
        if (0xD800..0xDC00).contains(&unit) {
            self.high = Some(unit);
        } else {
            self.text.push(char::from_u32(unit as u32).unwrap_or('\u{FFFD}'));
        }
    }

    /// The text to append to; `appending` says whether anything will be appended (which ends a pending pair).
    fn text(&mut self, appending: bool) -> &mut String {
        if appending && self.high.take().is_some() {
            self.text.push('\u{FFFD}');
        }
        &mut self.text
    }

    fn finish(mut self) -> String {
        self.text(true);
        self.text
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The DEFAULT_SAFE_SCHEMA types.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Type {
    Null,
    Bool,
    Int,
    Float,
    Timestamp,
    Merge,
    Str,
    Seq,
    Map,
    Binary,
    Omap,
    Pairs,
    Set,
}

/// The implicit types in resolution order (json schema, then default_safe's).
const IMPLICIT: [Type; 6] = [Type::Null, Type::Bool, Type::Int, Type::Float, Type::Timestamp, Type::Merge];

impl Type {
    const ALL: [Type; 13] = [
        Type::Null,
        Type::Bool,
        Type::Int,
        Type::Float,
        Type::Timestamp,
        Type::Merge,
        Type::Str,
        Type::Seq,
        Type::Map,
        Type::Binary,
        Type::Omap,
        Type::Pairs,
        Type::Set,
    ];

    fn tag(self) -> &'static str {
        match self {
            Type::Null => "tag:yaml.org,2002:null",
            Type::Bool => "tag:yaml.org,2002:bool",
            Type::Int => "tag:yaml.org,2002:int",
            Type::Float => "tag:yaml.org,2002:float",
            Type::Timestamp => "tag:yaml.org,2002:timestamp",
            Type::Merge => MERGE_TAG,
            Type::Str => "tag:yaml.org,2002:str",
            Type::Seq => "tag:yaml.org,2002:seq",
            Type::Map => "tag:yaml.org,2002:map",
            Type::Binary => "tag:yaml.org,2002:binary",
            Type::Omap => "tag:yaml.org,2002:omap",
            Type::Pairs => "tag:yaml.org,2002:pairs",
            Type::Set => "tag:yaml.org,2002:set",
        }
    }

    fn kind(self) -> Kind {
        match self {
            Type::Seq | Type::Omap | Type::Pairs => Kind::Sequence,
            Type::Map | Type::Set => Kind::Mapping,
            _ => Kind::Scalar,
        }
    }

    fn from_tag(tag: &str) -> Option<Type> {
        Type::ALL.into_iter().find(|t| t.tag() == tag)
    }
}

/// `String(value)` of a scalar.
fn js_string(v: &V) -> String {
    match v {
        V::Null => "null".to_owned(),
        V::Bool(b) => b.to_string(),
        V::Num(n) => js_number_string(*n),
        V::Str(s) => s.clone(),
        V::Date(t) => date_to_string(*t),
        V::Bin(b) => String::from_utf8_lossy(b).into_owned(),
        V::Seq(_) | V::Map(_) => "[object Object]".to_owned(),
    }
}

// type/int.js resolveYamlInteger
fn resolve_int(data: &str) -> bool {
    let b = data.as_bytes();
    let max = b.len();
    if max == 0 {
        return false;
    }
    let at = |i: usize| b.get(i).copied().unwrap_or(0);
    let mut index = 0;
    let mut ch = b[0];
    let mut has_digits = false;
    if ch == b'-' || ch == b'+' {
        index += 1;
        ch = at(index);
    }
    if ch == b'0' {
        if index + 1 == max {
            return true;
        }
        index += 1;
        ch = b[index];
        let digit: fn(u8) -> bool = match ch {
            b'b' => |c| c == b'0' || c == b'1',
            b'x' => |c| c.is_ascii_hexdigit(),
            _ => |c| (b'0'..=b'7').contains(&c),
        };
        if ch == b'b' || ch == b'x' {
            index += 1;
        }
        while index < max {
            ch = b[index];
            index += 1;
            if ch == b'_' {
                continue;
            }
            if !digit(ch) {
                return false;
            }
            has_digits = true;
        }
        return has_digits && ch != b'_';
    }
    if ch == b'_' {
        return false;
    }
    while index < max {
        ch = b[index];
        if ch == b'_' {
            index += 1;
            continue;
        }
        if ch == b':' {
            break;
        }
        if !ch.is_ascii_digit() {
            return false;
        }
        has_digits = true;
        index += 1;
    }
    if !has_digits || ch == b'_' {
        return false;
    }
    if ch != b':' {
        return true;
    }
    // /^(:[0-5]?[0-9])+$/
    let mut rest = &b[index..];
    while let [b':', tail @ ..] = rest {
        let digits = tail.iter().take_while(|c| c.is_ascii_digit()).count();
        if !(digits == 1 || (digits == 2 && tail[0] <= b'5')) {
            return false;
        }
        rest = &tail[digits..];
    }
    rest.is_empty()
}

// type/int.js constructYamlInteger
fn construct_int(data: &str) -> f64 {
    let value: String = data.chars().filter(|&c| c != '_').collect();
    let (sign, value) = match value.as_bytes()[0] {
        b'-' => (-1.0, &value[1..]),
        b'+' => (1.0, &value[1..]),
        _ => (1.0, &value[..]),
    };
    if value == "0" {
        return 0.0;
    }
    if let Some(rest) = value.strip_prefix('0') {
        if let Some(digits) = rest.strip_prefix('b') {
            return sign * js_parse_int(digits, 2);
        }
        if let Some(digits) = rest.strip_prefix('x') {
            return sign * js_parse_int(digits, 16);
        }
        return sign * js_parse_int(value, 8);
    }
    if value.contains(':') {
        let digits: Vec<f64> = value.split(':').map(|part| js_parse_int(part, 10)).collect();
        let (mut total, mut base) = (0.0, 1.0);
        for d in digits.iter().rev() {
            total += d * base;
            base *= 60.0;
        }
        return sign * total;
    }
    sign * js_parse_int(value, 10)
}

/// `parseInt(digits, radix)` as JavaScriptCore computes it: exact below 2^53; above, radix 10 parses the digits
/// as a double (correctly rounded) and powers of two sum the digits from the least significant one.
fn js_parse_int(digits: &str, radix: u32) -> f64 {
    let len = digits.bytes().take_while(|&b| (b as char).is_digit(radix)).count();
    if len == 0 {
        return f64::NAN;
    }
    let digits = &digits[..len];
    let mut number = 0.0f64;
    for c in digits.chars() {
        number = number * radix as f64 + c.to_digit(radix).unwrap_or(0) as f64;
    }
    if number >= 9_007_199_254_740_992.0 {
        if radix == 10 {
            number = digits.parse::<f64>().unwrap_or(number);
        } else if matches!(radix, 2 | 4 | 8 | 16 | 32) {
            number = 0.0;
            let mut multiplier = 1.0f64;
            for c in digits.chars().rev() {
                if multiplier == f64::INFINITY {
                    if c != '0' {
                        number = f64::INFINITY;
                        break;
                    }
                } else {
                    number += c.to_digit(radix).unwrap_or(0) as f64 * multiplier;
                }
                multiplier *= radix as f64;
            }
        }
    }
    number
}

// type/float.js YAML_FLOAT_PATTERN (without the trailing-underscore check).
fn is_float(s: &str) -> bool {
    let b = s.as_bytes();
    let signed = match b.first() {
        Some(b'-' | b'+') => &b[1..],
        _ => b,
    };
    let run = |s: &[u8], f: fn(&u8) -> bool| s.iter().take_while(|c| f(c)).count();
    let digit_or_underscore = |c: &u8| c.is_ascii_digit() || *c == b'_';
    // [eE][-+]?[0-9]+ to the end
    let exponent = |s: &[u8]| match s {
        [] => true,
        [b'e' | b'E', rest @ ..] => {
            let rest = match rest {
                [b'-' | b'+', r @ ..] => r,
                r => r,
            };
            let n = run(rest, u8::is_ascii_digit);
            n > 0 && n == rest.len()
        }
        _ => false,
    };
    // [-+]?(?:0|[1-9][0-9_]*)(?:\.[0-9_]*)?(?:[eE][-+]?[0-9]+)?
    let decimal = || {
        let rest = match signed {
            [b'0', r @ ..] => r,
            [b'1'..=b'9', r @ ..] => &r[run(r, digit_or_underscore)..],
            _ => return false,
        };
        let rest = match rest {
            [b'.', r @ ..] => &r[run(r, digit_or_underscore)..],
            r => r,
        };
        exponent(rest)
    };
    // \.[0-9_]+(?:[eE][-+]?[0-9]+)?
    let fraction = || match b {
        [b'.', r @ ..] => {
            let n = run(r, digit_or_underscore);
            n > 0 && exponent(&r[n..])
        }
        _ => false,
    };
    // [-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*
    let sexagesimal = || {
        let mut rest = match signed {
            [b'0'..=b'9', r @ ..] => &r[run(r, digit_or_underscore)..],
            _ => return false,
        };
        let mut groups = 0;
        while let [b':', tail @ ..] = rest {
            let n = run(tail, u8::is_ascii_digit);
            if !(n == 1 || (n == 2 && tail[0] <= b'5')) {
                return false;
            }
            rest = &tail[n..];
            groups += 1;
        }
        match rest {
            [b'.', r @ ..] => groups > 0 && run(r, digit_or_underscore) == r.len(),
            _ => false,
        }
    };
    let infinity = || matches!(signed, b".inf" | b".Inf" | b".INF");
    let nan = || matches!(b, b".nan" | b".NaN" | b".NAN");
    decimal() || fraction() || sexagesimal() || infinity() || nan()
}

// type/float.js constructYamlFloat
fn construct_float(data: &str) -> f64 {
    let value: String = data.chars().filter(|&c| c != '_').collect::<String>().to_ascii_lowercase();
    let sign = if value.starts_with('-') { -1.0 } else { 1.0 };
    let value = value.strip_prefix(['+', '-']).unwrap_or(&value);
    if value == ".inf" {
        return sign * f64::INFINITY;
    }
    if value == ".nan" {
        return f64::NAN;
    }
    let parse_float = |s: &str| s.parse::<f64>().unwrap_or(f64::NAN);
    if value.contains(':') {
        let digits: Vec<f64> = value.split(':').map(parse_float).collect();
        let (mut total, mut base) = (0.0, 1.0);
        for d in digits.iter().rev() {
            total += d * base;
            base *= 60.0;
        }
        return sign * total;
    }
    sign * parse_float(value)
}

// type/timestamp.js YAML_DATE_REGEXP / YAML_TIMESTAMP_REGEXP, then constructYamlTimestamp: the JS time value.
// Every `\d\d?` in the patterns is followed by a non-digit, so matching greedily is what the regexps match.
fn parse_timestamp(s: &str) -> Option<i64> {
    let mut c = Cursor { b: s.as_bytes(), i: 0 };
    let year = c.digits(4, 4)?;
    c.eat(b'-').then_some(())?;
    let month_start = c.i;
    let month = c.digits(1, 2)?;
    let month_len = c.i - month_start;
    c.eat(b'-').then_some(())?;
    let day_start = c.i;
    let day = c.digits(1, 2)?;
    let day_len = c.i - day_start;
    if c.done() {
        // YAML_DATE_REGEXP: two-digit month and day; YAML_TIMESTAMP_REGEXP needs a time.
        return (month_len == 2 && day_len == 2).then(|| date_utc(year, month - 1, day, 0, 0, 0, 0));
    }
    (c.eat(b'T') || c.eat(b't') || c.blanks() > 0).then_some(())?;
    let hour = c.digits(1, 2)?;
    c.eat(b':').then_some(())?;
    let minute = c.digits(2, 2)?;
    c.eat(b':').then_some(())?;
    let second = c.digits(2, 2)?;
    let mut fraction = 0;
    if c.eat(b'.') {
        let start = c.i;
        while c.b.get(c.i).is_some_and(u8::is_ascii_digit) {
            c.i += 1;
        }
        // `match[7].slice(0, 3)` padded to milliseconds; an empty fraction is 0
        for k in 0..3 {
            fraction = fraction * 10 + if start + k < c.i { (c.b[start + k] - b'0') as i64 } else { 0 };
        }
    }
    let blanks = c.blanks();
    let mut delta = 0;
    if c.done() {
        // `[ \t]*` belongs to the optional zone: trailing blanks without one do not match.
        if blanks > 0 {
            return None;
        }
    } else if c.eat(b'Z') {
        c.done().then_some(())?;
    } else {
        let negative = c.eat(b'-');
        (negative || c.eat(b'+')).then_some(())?;
        let tz_hour = c.digits(1, 2)?;
        let tz_minute = if c.eat(b':') { c.digits(2, 2)? } else { 0 };
        c.done().then_some(())?;
        delta = (tz_hour * 60 + tz_minute) * 60_000;
        if negative {
            delta = -delta;
        }
    }
    Some(date_utc(year, month - 1, day, hour, minute, second, fraction) - delta)
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    /// `min` to `max` digits, as a number.
    fn digits(&mut self, min: usize, max: usize) -> Option<i64> {
        let n = self.b[self.i..].iter().take(max).take_while(|c| c.is_ascii_digit()).count();
        if n < min {
            return None;
        }
        let v = self.b[self.i..self.i + n].iter().fold(0, |v, &d| v * 10 + (d - b'0') as i64);
        self.i += n;
        Some(v)
    }

    fn eat(&mut self, c: u8) -> bool {
        let found = self.b.get(self.i) == Some(&c);
        self.i += found as usize;
        found
    }

    fn blanks(&mut self) -> usize {
        let n = self.b[self.i..].iter().take_while(|c| matches!(c, b' ' | b'\t')).count();
        self.i += n;
        n
    }

    fn done(&self) -> bool {
        self.i == self.b.len()
    }
}

/// `Date.UTC(year, month, day, hours, minutes, seconds, ms)`: two-digit years are 19xx, fields overflow.
fn date_utc(year: i64, month: i64, day: i64, hours: i64, minutes: i64, seconds: i64, ms: i64) -> i64 {
    let year = if (0..=99).contains(&year) { 1900 + year } else { year };
    let days = days_from_civil(year + month.div_euclid(12), month.rem_euclid(12) + 1, 1) + day - 1;
    days * 86_400_000 + hours * 3_600_000 + minutes * 60_000 + seconds * 1000 + ms
}

// Days since 1970-01-01 of a proleptic Gregorian date (month 1..=12).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

struct DateParts {
    year: i64,
    month: i64,
    day: i64,
    weekday: i64,
    hour: i64,
    minute: i64,
    second: i64,
    ms: i64,
}

fn date_parts(t: i64) -> DateParts {
    let days = t.div_euclid(86_400_000);
    let ms = t.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    DateParts {
        year,
        month,
        day,
        weekday: (days + 4).rem_euclid(7),
        hour: ms / 3_600_000,
        minute: ms / 60_000 % 60,
        second: ms / 1000 % 60,
        ms: ms % 1000,
    }
}

/// `Date#toISOString()`.
fn date_to_iso(t: i64) -> String {
    let p = date_parts(t);
    let year = if (0..=9999).contains(&p.year) {
        format!("{:04}", p.year)
    } else {
        format!("{}{:06}", if p.year < 0 { '-' } else { '+' }, p.year.abs())
    };
    format!("{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", p.month, p.day, p.hour, p.minute, p.second, p.ms)
}

/// `Date#toString()` in a process whose time zone is UTC.
fn date_to_string(t: i64) -> String {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let p = date_parts(t);
    let year = if p.year < 0 { format!("-{:04}", -p.year) } else { format!("{:04}", p.year) };
    format!(
        "{} {} {:02} {year} {:02}:{:02}:{:02} GMT+0000 (Coordinated Universal Time)",
        WEEKDAYS[p.weekday as usize],
        MONTHS[(p.month - 1) as usize],
        p.day,
        p.hour,
        p.minute,
        p.second
    )
}

// type/binary.js resolveYamlBinary: base64 characters (CR and LF skipped, `=` counted) in whole bytes.
fn resolve_binary(data: &str) -> bool {
    let mut bits = 0usize;
    for c in data.chars() {
        match c {
            '\n' | '\r' => {}
            'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '/' | '=' => bits += 6,
            _ => return false,
        }
    }
    bits.is_multiple_of(8)
}

// type/binary.js constructYamlBinary
fn construct_binary(data: &str) -> Vec<u8> {
    let code = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            _ => 63,
        }
    };
    let input: Vec<u8> = data.bytes().filter(|b| !matches!(b, b'\r' | b'\n' | b'=')).collect();
    let mut result = Vec::new();
    let mut bits: u32 = 0;
    for (idx, &c) in input.iter().enumerate() {
        if idx % 4 == 0 && idx > 0 {
            result.extend([(bits >> 16) as u8, (bits >> 8) as u8, bits as u8]);
        }
        bits = (bits << 6) | code(c);
    }
    match (input.len() % 4) * 6 {
        0 => result.extend([(bits >> 16) as u8, (bits >> 8) as u8, bits as u8]),
        18 => result.extend([(bits >> 10) as u8, (bits >> 2) as u8]),
        12 => result.push((bits >> 4) as u8),
        _ => {}
    }
    result
}

// ---------------------------------------------------------------------------------------------------------------
// The loaded document as JSON.

struct Converter {
    /// The arrays and objects being converted, outermost first.
    path: Vec<*const ()>,
    budget: usize,
}

impl Converter {
    fn convert(&mut self, v: &V) -> Result<Json, Error> {
        if self.budget == 0 {
            return Err(Error { message: format!("the document expands to more than {MAX_NODES} values") });
        }
        self.budget -= 1;
        Ok(match v {
            V::Null => Json::Null,
            V::Bool(b) => Json::Bool(*b),
            V::Num(n) => Json::Number(*n),
            V::Str(s) => Json::String(s.clone()),
            V::Date(t) => Json::String(date_to_iso(*t)),
            V::Bin(bytes) => Json::Object(IndexMap::from([
                ("type".to_owned(), Json::String("Buffer".to_owned())),
                ("data".to_owned(), Json::Array(bytes.iter().map(|&b| Json::Number(b as f64)).collect())),
            ])),
            V::Seq(items) => {
                self.enter(Rc::as_ptr(items) as *const ())?;
                let items = items.borrow().iter().map(|item| self.convert(item)).collect::<Result<_, _>>()?;
                self.path.pop();
                Json::Array(items)
            }
            V::Map(entries) => {
                self.enter(Rc::as_ptr(entries) as *const ())?;
                let mut out = IndexMap::with_capacity(entries.borrow().len());
                for (key, value) in entries.borrow().iter() {
                    out.insert(key.clone(), self.convert(value)?);
                }
                self.path.pop();
                Json::Object(out)
            }
        })
    }

    fn enter(&mut self, container: *const ()) -> Result<(), Error> {
        if self.path.contains(&container) {
            return Err(Error { message: "the document contains itself through an alias".to_owned() });
        }
        if self.path.len() >= MAX_DEPTH {
            return Err(Error { message: format!("nesting deeper than {MAX_DEPTH} levels is not supported") });
        }
        self.path.push(container);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencode_protocol::wire::Out;

    fn json_text(v: &Json) -> String {
        let mut out = Out::new();
        out.json(v);
        out.finish()
    }

    /// Every case: `JSON.stringify(yaml.safeLoad(input))` (`undefined` as `null`), or `None` where it throws.
    /// Produced by running js-yaml 3.15.2 under bun with TZ=UTC; do not edit by hand.
    const CASES: &[(&str, Option<&str>)] = &[
    ("description: Uses: an unquoted colon", None),
    ("description: [unclosed", None),
    ("disable-model-invocation: yes", Some("{\"disable-model-invocation\":\"yes\"}")),
    ("steps: -4", Some("{\"steps\":-4}")),
    ("color: \"#00AA00\"", Some("{\"color\":\"#00AA00\"}")),
    ("metadata:\n  opencode/autoinvoke: false", Some("{\"metadata\":{\"opencode/autoinvoke\":false}}")),
    ("permissions:\n  - action: edit\n    resource: \"*\"\n    effect: deny", Some("{\"permissions\":[{\"action\":\"edit\",\"resource\":\"*\",\"effect\":\"deny\"}]}")),
    ("description: 42", Some("{\"description\":42}")),
    ("name: [unclosed\ndescription: 5", None),
    ("name: my-skill\ndescription: Does things\nlicense: MIT\nallowed-tools:\n  - Read\n  - Write", Some("{\"name\":\"my-skill\",\"description\":\"Does things\",\"license\":\"MIT\",\"allowed-tools\":[\"Read\",\"Write\"]}")),
    ("description: >\n  A folded\n  description\nmode: subagent\ntemperature: 0.1\ntools:\n  write: false\n  bash: true", Some("{\"description\":\"A folded description\\n\",\"mode\":\"subagent\",\"temperature\":0.1,\"tools\":{\"write\":false,\"bash\":true}}")),
    ("", Some("null")),
    (" ", Some("null")),
    ("\n", Some("null")),
    ("\n\n", Some("null")),
    ("#c", Some("null")),
    ("# only\n# comments\n", Some("null")),
    ("---", Some("null")),
    ("---\n", Some("null")),
    ("...", Some("null")),
    ("--- # c\n...", Some("null")),
    ("~", Some("null")),
    ("null", Some("null")),
    ("Null", Some("null")),
    ("NULL", Some("null")),
    ("nULL", Some("\"nULL\"")),
    ("a:", Some("{\"a\":null}")),
    ("a: ~\nb: null\nc:", Some("{\"a\":null,\"b\":null,\"c\":null}")),
    ("true", Some("true")),
    ("True", Some("true")),
    ("TRUE", Some("true")),
    ("tRUE", Some("\"tRUE\"")),
    ("false", Some("false")),
    ("False", Some("false")),
    ("FALSE", Some("false")),
    ("yes", Some("\"yes\"")),
    ("no", Some("\"no\"")),
    ("on", Some("\"on\"")),
    ("off", Some("\"off\"")),
    ("y", Some("\"y\"")),
    ("n", Some("\"n\"")),
    ("Yes", Some("\"Yes\"")),
    ("0", Some("0")),
    ("-0", Some("0")),
    ("+0", Some("0")),
    ("00", Some("0")),
    ("-00", Some("0")),
    ("012", Some("10")),
    ("08", Some("\"08\"")),
    ("0o17", Some("\"0o17\"")),
    ("0x1F", Some("31")),
    ("0X1F", Some("\"0X1F\"")),
    ("-0x1F", Some("-31")),
    ("0xff_ff", Some("65535")),
    ("0b101", Some("5")),
    ("-0b101", Some("-5")),
    ("0b2", Some("\"0b2\"")),
    ("0b", Some("\"0b\"")),
    ("0x", Some("\"0x\"")),
    ("0x_1", Some("1")),
    ("1_000", Some("1000")),
    ("_1", Some("\"_1\"")),
    ("1_", Some("\"1_\"")),
    ("1__0", Some("10")),
    ("+12", Some("12")),
    ("-4", Some("-4")),
    ("1:30", Some("90")),
    ("-1:30", Some("-90")),
    ("1:60", Some("\"1:60\"")),
    ("1:5", Some("65")),
    ("190:20:30", Some("685230")),
    ("1:2:3:4", Some("223384")),
    ("12345678901234567890", Some("12345678901234567000")),
    ("9007199254740993", Some("9007199254740992")),
    ("123456789012345678901234567890", Some("1.2345678901234568e+29")),
    ("0x400001000000020000001", Some("4.835704431380021e+24")),
    ("0xFFFFFFFFFFFFFFFFFFFF", Some("1.2089258196146292e+24")),
    ("0b111111111111111111111111111111111111111111111111111111111111", Some("1152921504606847000")),
    ("07777777777777777777777777", Some("3.777893186295716e+22")),
    ("10000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000", Some("null")),
    ("1.5", Some("1.5")),
    ("-1.5", Some("-1.5")),
    ("+1.5", Some("1.5")),
    (".5", Some("0.5")),
    ("-.5", Some("\"-.5\"")),
    ("1.", Some("1")),
    ("1.e5", Some("100000")),
    ("1e5", Some("100000")),
    ("1E5", Some("100000")),
    ("1e+5", Some("100000")),
    ("1e-5", Some("0.00001")),
    ("6.8523015e+5", Some("685230.15")),
    ("685.230_15e+03", Some("685230.15")),
    ("685_230.15", Some("685230.15")),
    ("190:20:30.15", Some("685230.15")),
    ("1:30.5", Some("90.5")),
    ("1:30.", Some("90")),
    (".inf", Some("null")),
    ("-.inf", Some("null")),
    ("+.inf", Some("null")),
    (".Inf", Some("null")),
    (".INF", Some("null")),
    (".iNf", Some("\".iNf\"")),
    (".nan", Some("null")),
    (".NaN", Some("null")),
    (".NAN", Some("null")),
    ("-.nan", Some("\"-.nan\"")),
    (".infinity", Some("\".infinity\"")),
    ("1e400", Some("null")),
    ("-1e400", Some("null")),
    ("0.0", Some("0")),
    ("-0.0", Some("0")),
    ("._1", Some("0.1")),
    ("._", Some("\"._\"")),
    ("1_.5", Some("1.5")),
    ("1.5_", Some("\"1.5_\"")),
    ("0.1e1_", Some("\"0.1e1_\"")),
    ("01.5", Some("\"01.5\"")),
    ("1e", Some("\"1e\"")),
    ("0.30000000000000004", Some("0.30000000000000004")),
    ("5e-324", Some("5e-324")),
    ("1.7976931348623157e308", Some("1.7976931348623157e+308")),
    ("2001-12-14", Some("\"2001-12-14T00:00:00.000Z\"")),
    ("2001-12-14t21:59:43.10-05:00", Some("\"2001-12-15T02:59:43.100Z\"")),
    ("2001-12-14 21:59:43.10 -5", Some("\"2001-12-15T02:59:43.100Z\"")),
    ("2001-12-15T02:59:43.1Z", Some("\"2001-12-15T02:59:43.100Z\"")),
    ("2001-12-15 2:59:43.10", Some("\"2001-12-15T02:59:43.100Z\"")),
    ("2001-1-1", Some("\"2001-1-1\"")),
    ("2001-1-1 1:02:03", Some("\"2001-01-01T01:02:03.000Z\"")),
    ("0050-01-01", Some("\"1950-01-01T00:00:00.000Z\"")),
    ("2001-13-45", Some("\"2002-02-14T00:00:00.000Z\"")),
    ("2001-12-14T21:59:43.123456Z", Some("\"2001-12-14T21:59:43.123Z\"")),
    ("2001-12-14T21:59:43.Z", Some("\"2001-12-14T21:59:43.000Z\"")),
    ("2001-12-14 21:59:43 +05:30", Some("\"2001-12-14T16:29:43.000Z\"")),
    ("2001-12-14 21:59:43+0530", Some("\"2001-12-14 21:59:43+0530\"")),
    ("2001-12-14 21:59:43 Z", Some("\"2001-12-14T21:59:43.000Z\"")),
    ("9999-99-99", Some("\"+010007-06-07T00:00:00.000Z\"")),
    ("1970-01-01", Some("\"1970-01-01T00:00:00.000Z\"")),
    ("1969-12-31T23:59:59.999Z", Some("\"1969-12-31T23:59:59.999Z\"")),
    ("2001-12-14T21:59:43", Some("\"2001-12-14T21:59:43.000Z\"")),
    ("2001-12-14  \t21:59:43", Some("\"2001-12-14T21:59:43.000Z\"")),
    ("2001-12-14T99:99:99+99:99", Some("\"2001-12-14T00:01:39.000Z\"")),
    ("2024-02-29", Some("\"2024-02-29T00:00:00.000Z\"")),
    ("2023-02-29", Some("\"2023-03-01T00:00:00.000Z\"")),
    ("d: 2001-12-14", Some("{\"d\":\"2001-12-14T00:00:00.000Z\"}")),
    ("hello world", Some("\"hello world\"")),
    ("a: b c", Some("{\"a\":\"b c\"}")),
    ("plain # comment", Some("\"plain\"")),
    ("plain#notcomment", Some("\"plain#notcomment\"")),
    ("a:b", Some("\"a:b\"")),
    ("http://x.y/z", Some("\"http://x.y/z\"")),
    ("-foo", Some("\"-foo\"")),
    ("?foo", Some("\"?foo\"")),
    (":foo", Some("\":foo\"")),
    ("- ", Some("[null]")),
    ("@foo", None),
    ("`foo", None),
    ("%foo", None),
    ("!foo bar", None),
    ("! foo", Some("\"foo\"")),
    ("& foo", None),
    ("a: foo bar  ", Some("{\"a\":\"foo bar\"}")),
    ("a: x\u{a0}y", Some("{\"a\":\"x\u{a0}y\"}")),
    ("\u{e9}: \u{fc}", Some("{\"\u{e9}\":\"\u{fc}\"}")),
    ("emoji: \u{1f600}", Some("{\"emoji\":\"\u{1f600}\"}")),
    ("a: foo\n  bar\n  baz", Some("{\"a\":\"foo bar baz\"}")),
    ("a: foo\n\n  bar", Some("{\"a\":\"foo\\nbar\"}")),
    ("a: foo\n  #c\nb: 1", Some("{\"a\":\"foo\",\"b\":1}")),
    ("a: foo\n\n\n  bar\n", Some("{\"a\":\"foo\\n\\nbar\"}")),
    ("foo\nbar", Some("\"foo bar\"")),
    ("- foo\n  bar", Some("[\"foo bar\"]")),
    ("a: [x\n  y]", Some("{\"a\":[\"x y\"]}")),
    ("a: 1 # comment", Some("{\"a\":1}")),
    ("a: 1 #comment\n# full\nb: 2", Some("{\"a\":1,\"b\":2}")),
    ("a: x#y", Some("{\"a\":\"x#y\"}")),
    ("a: 'x'#y", Some("{\"a\":\"x\"}")),
    ("a: \"x\"#y", Some("{\"a\":\"x\"}")),
    ("'single'", Some("\"single\"")),
    ("'it''s'", Some("\"it's\"")),
    ("''", Some("\"\"")),
    ("\"\"", Some("\"\"")),
    ("\"double\"", Some("\"double\"")),
    ("\"esc \\n \\t \\\\ \\\" \\/ \\x41 \\u00e9 \\U0001F600 \\N \\_ \\L \\P \\0 \\a \\e \\b \\f \\r \\v \\ \"", Some("\"esc \\n \\t \\\\ \\\" / A \u{e9} \u{1f600} \u{85} \u{a0} \u{2028} \u{2029} \\u0000 \\u0007 \\u001b \\b \\f \\r \\u000b  \"")),
    ("\"\\uD83D\\uDE00\"", Some("\"\u{1f600}\"")),
    ("\"\\U80000041\"", Some("\"A\"")),
    ("\"bad \\q\"", None),
    ("\"\\x4\"", None),
    ("\"\\xZZ\"", None),
    ("'unterminated", None),
    ("\"unterminated", None),
    ("a: 'foo\n  bar'", Some("{\"a\":\"foo bar\"}")),
    ("a: \"foo\n\n  bar\"", Some("{\"a\":\"foo\\nbar\"}")),
    ("a: \"foo \\\n  bar\"", Some("{\"a\":\"foo bar\"}")),
    ("a: 'x  \n  y'", Some("{\"a\":\"x   y\"}")),
    ("a: \"x\r\n  y\"", Some("{\"a\":\"x y\"}")),
    ("a: \"\u{1}\"", None),
    ("a: '\u{1}'", None),
    ("a: \"\u{7f}\"", Some("{\"a\":\"\u{7f}\"}")),
    ("a: \"tab\there\"", Some("{\"a\":\"tab\\there\"}")),
    ("a: '\u{e9}\n  \u{fc}'", Some("{\"a\":\"\u{e9} \u{fc}\"}")),
    ("'a\n---\nb'", None),
    ("\"a\n...\"", Some("\"a ...\"")),
    ("a: 'x'\n  'y'", None),
    ("a: |\n  line1\n  line2\n", Some("{\"a\":\"line1\\nline2\\n\"}")),
    ("a: |\n  line1\n  line2", Some("{\"a\":\"line1\\nline2\\n\"}")),
    ("a: |-\n  x\n\n", Some("{\"a\":\"x\"}")),
    ("a: |+\n  x\n\n", Some("{\"a\":\"x\\n\\n\"}")),
    ("a: |+\n  x\n\nb: 1", Some("{\"a\":\"x\\n\\n\",\"b\":1}")),
    ("a: >\n  folded\n  text\n\n  para\n", Some("{\"a\":\"folded text\\npara\\n\"}")),
    ("a: >-\n  x\n   more\n  y", Some("{\"a\":\"x\\n more\\ny\"}")),
    ("a: >\n  x\n\n\n  y\n", Some("{\"a\":\"x\\n\\ny\\n\"}")),
    ("a: |2\n   x\n", Some("{\"a\":\" x\\n\"}")),
    ("a: |0\n x", None),
    ("a: |--\n x", None),
    ("a: |1-\n  x", Some("{\"a\":\" x\"}")),
    ("a: |-1\n  x", Some("{\"a\":\" x\"}")),
    ("a: |12\n  x", None),
    ("a: | # comment\n  x", Some("{\"a\":\"x\\n\"}")),
    ("a: |x\n  y", None),
    ("|-\ntext", Some("\"text\\n\"")),
    ("|\ntext", Some("\"text\\n\"")),
    (">\n a\n b\n\n c", Some("\"a b\\nc\\n\"")),
    ("a: |\n\n  x\n", Some("{\"a\":\"\\nx\\n\"}")),
    ("a: |\n  x\n b", None),
    ("- |\n  x\n- y", Some("[\"x\\n\",\"y\"]")),
    ("a: |\n    deep\n  shallow", None),
    ("a: >\n\n  x", Some("{\"a\":\"\\nx\\n\"}")),
    ("a: |\n  x\n\n\n", Some("{\"a\":\"x\\n\"}")),
    ("a: >+\n  x\n\n", Some("{\"a\":\"x\\n\\n\"}")),
    ("a: |\n  \u{1}", None),
    ("a: >\n  a\n\n   b\n  c", Some("{\"a\":\"a\\n\\n b\\nc\\n\"}")),
    ("a: |\n  # not a comment\n  x", Some("{\"a\":\"# not a comment\\nx\\n\"}")),
    ("a: |\n\tx", None),
    ("a: |\n  x\n\ty", None),
    ("a: >\n x\n  y\n z", Some("{\"a\":\"x\\n y\\nz\\n\"}")),
    ("a: 1\nb: 2", Some("{\"a\":1,\"b\":2}")),
    ("a:\n  b:\n    c: 1", Some("{\"a\":{\"b\":{\"c\":1}}}")),
    ("a: 1\n b: 2", None),
    ("a: b: c", None),
    ("a: 1\na: 2", None),
    ("? a\n: b", Some("{\"a\":\"b\"}")),
    ("? [a, b]\n: c", Some("{\"a,b\":\"c\"}")),
    ("? a\n? b", Some("{\"a\":null,\"b\":null}")),
    ("a:\n", Some("{\"a\":null}")),
    ("a: 1\nb", None),
    ("  a: 1\n  b: 2", Some("{\"a\":1,\"b\":2}")),
    ("a: 1\n  # comment\nb: 2", Some("{\"a\":1,\"b\":2}")),
    ("key with spaces: v", Some("{\"key with spaces\":\"v\"}")),
    ("'quoted key': v", Some("{\"quoted key\":\"v\"}")),
    ("\"dq key\": v", Some("{\"dq key\":\"v\"}")),
    ("1: one\n2: two\n10: ten\nb: x\na: y", Some("{\"1\":\"one\",\"2\":\"two\",\"10\":\"ten\",\"b\":\"x\",\"a\":\"y\"}")),
    ("a : b", Some("{\"a\":\"b\"}")),
    ("a:b: c", Some("{\"a:b\":\"c\"}")),
    ("x: [1, 2]\ny: {a: 1}", Some("{\"x\":[1,2],\"y\":{\"a\":1}}")),
    ("a:\n- 1\n- 2", Some("{\"a\":[1,2]}")),
    ("a:\n  - 1\n  - 2", Some("{\"a\":[1,2]}")),
    ("- a: 1\n  b: 2\n- c: 3", Some("[{\"a\":1,\"b\":2},{\"c\":3}]")),
    ("- - 1\n  - 2\n- 3", Some("[[1,2],3]")),
    ("-\n  a: 1", Some("[{\"a\":1}]")),
    ("- \n- x", Some("[null,\"x\"]")),
    ("-", Some("[null]")),
    ("- a\n-b", None),
    ("a:\n\tb: 1", Some("{\"a\":null,\"b\":1}")),
    ("a:\n  b: 1\n\tc: 2", Some("{\"a\":{\"b\":1},\"c\":2}")),
    ("\ta: 1", Some("{\"a\":1}")),
    ("a:\t1", Some("{\"a\":1}")),
    ("a:\n  b: 1\n c: 2", None),
    ("a:\n  - b\n  c: d", None),
    ("a: - 1", None),
    ("a: [1, 2]: 3", None),
    ("? |\n  block key\n: v", Some("{\"block key\\n\":\"v\"}")),
    ("? - a\n  - b\n: c", Some("{\"a,b\":\"c\"}")),
    ("? a\n: b\n? c\n: d", Some("{\"a\":\"b\",\"c\":\"d\"}")),
    (": x", None),
    ("?", Some("{\"null\":null}")),
    ("? a", Some("{\"a\":null}")),
    ("a:\n  -\n  - b", Some("{\"a\":[null,\"b\"]}")),
    ("a: 1\n\n\nb: 2", Some("{\"a\":1,\"b\":2}")),
    ("  - a\n  - b", Some("[\"a\",\"b\"]")),
    ("- a\n - b", Some("[\"a - b\"]")),
    ("a: 1\n...\n", Some("{\"a\":1}")),
    ("a: 1\n---\nb: 2", None),
    ("--- a: 1", Some("{\"a\":1}")),
    ("---\na: 1\n...", Some("{\"a\":1}")),
    ("--- |\n  x", Some("\"x\\n\"")),
    ("---\n---", None),
    ("a: 1\n--- \n", None),
    ("%YAML 1.2\n---\na: 1", Some("{\"a\":1}")),
    ("%YAML 2.0\n---\na", None),
    ("%YAML 1.1\n%YAML 1.1\n---\n", None),
    ("%YAML 1.1 extra\n---\n", None),
    ("%YAML x\n---\n", None),
    ("%FOO bar\n---\nx", Some("\"x\"")),
    ("%YAML 1.2\nx", None),
    ("% x\n---\n", None),
    ("%TAG !e! tag:yaml.org,2002:\n---\n!e!str 1", Some("\"1\"")),
    ("%TAG !e! tag:yaml.org,2002:\n%TAG !e! x\n---\n", None),
    ("%TAG e x\n---\n", None),
    ("__proto__: 1", Some("{\"__proto__\":1}")),
    ("constructor: 1", Some("{\"constructor\":1}")),
    ("toString: x\nhasOwnProperty: y", Some("{\"toString\":\"x\",\"hasOwnProperty\":\"y\"}")),
    ("a: [b, c]\nd: e", Some("{\"a\":[\"b\",\"c\"],\"d\":\"e\"}")),
    ("a: {b: c}: d", None),
    ("a\u{0}b", None),
    ("a: \u{1}", None),
    ("a: \u{85}x", Some("{\"a\":\"\u{85}x\"}")),
    ("\u{feff}a: 1", Some("{\"a\":1}")),
    ("a: 1\r\nb: 2\r\n", Some("{\"a\":1,\"b\":2}")),
    ("a: 1\rb: 2", Some("{\"a\":1,\"b\":2}")),
    ("a: |\r\n  x\r\n  y\r\n", Some("{\"a\":\"x\\ny\\n\"}")),
    ("a:\n  b\n  c: d", None),
    ("a: b\n  c", Some("{\"a\":\"b c\"}")),
    ("a:\n  b: c\n    d: e", None),
    ("x:\n  y: 1\n z: 2", None),
    ("-a: 1", Some("{\"-a\":1}")),
    ("a: -b", Some("{\"a\":\"-b\"}")),
    ("a: 1\nb:\n  - x\n  -\n  - y: 1\n    z:\n      - 2", Some("{\"a\":1,\"b\":[\"x\",null,{\"y\":1,\"z\":[2]}]}")),
    ("[1, 2, 3]", Some("[1,2,3]")),
    ("[a, b: c, ? d : e]", Some("[\"a\",{\"b\":\"c\"},{\"d\":\"e\"}]")),
    ("{a: 1, b: [2, 3]}", Some("{\"a\":1,\"b\":[2,3]}")),
    ("[]", Some("[]")),
    ("{}", Some("{}")),
    ("[,]", Some("[null]")),
    ("[a,,b]", Some("[\"a\",null,\"b\"]")),
    ("[a b]", Some("[\"a b\"]")),
    ("[a\n, b]", Some("[\"a\",\"b\"]")),
    ("{a: 1, a: 2}", None),
    ("{ ? a }", Some("{\"a\":null}")),
    ("{a}", Some("{\"a\":null}")),
    ("{a, b: c}", Some("{\"a\":null,\"b\":\"c\"}")),
    ("[a: 1]", Some("[{\"a\":1}]")),
    ("[{a: 1}, {b: 2}]", Some("[{\"a\":1},{\"b\":2}]")),
    ("[1, 2", None),
    ("{a: 1", None),
    ("[1 2] x", None),
    ("{a: [1, {b: 2}]}", Some("{\"a\":[1,{\"b\":2}]}")),
    ("[\"a\", 'b']", Some("[\"a\",\"b\"]")),
    ("[a, b]: c", Some("{\"a,b\":\"c\"}")),
    ("{x: 1}: y", Some("{\"[object Object]\":\"y\"}")),
    ("[a, [b]]: c", None),
    ("{a: b}c", None),
    ("[a]]", None),
    ("{\"a\":1}", Some("{\"a\":1}")),
    ("{a: 1,}", Some("{\"a\":1}")),
    ("[1,]", Some("[1]")),
    ("[ :x ]", Some("[\":x\"]")),
    ("[a:b]", Some("[\"a:b\"]")),
    ("[a: b]", Some("[{\"a\":\"b\"}]")),
    ("{a:b}", Some("{\"a:b\":null}")),
    ("{\"a\":b}", Some("{\"a\":\"b\"}")),
    ("[-1, -a, - a]", None),
    ("[? ]", Some("[{\"null\":null}]")),
    ("[a, ? b]", Some("[\"a\",{\"b\":null}]")),
    ("{a: 1\n}", Some("{\"a\":1}")),
    ("{a: 1\nb: 2}", None),
    ("[a,\n  b]", Some("[\"a\",\"b\"]")),
    ("a: [\n  1,\n  2\n]", Some("{\"a\":[1,2]}")),
    ("[a, #c\n b]", Some("[\"a\",\"b\"]")),
    ("{ a : b }", Some("{\"a\":\"b\"}")),
    ("[*]", None),
    ("[&]", None),
    ("[a]: b", Some("{\"a\":\"b\"}")),
    ("{[a]: b}", Some("{\"a\":\"b\"}")),
    ("[[a]]", Some("[[\"a\"]]")),
    ("{a: {b: {c: [d]}}}", Some("{\"a\":{\"b\":{\"c\":[\"d\"]}}}")),
    ("[a: b, c: d]: e", Some("{\"[object Object],[object Object]\":\"e\"}")),
    ("[\n- a]", None),
    ("[a, - ]", None),
    ("a: &x 1\nb: *x", Some("{\"a\":1,\"b\":1}")),
    ("a: &x [1, 2]\nb: *x", Some("{\"a\":[1,2],\"b\":[1,2]}")),
    ("a: &x {k: v}\nb: *x", Some("{\"a\":{\"k\":\"v\"},\"b\":{\"k\":\"v\"}}")),
    ("b: *x", None),
    ("a: &x\n  foo\nb: *x", Some("{\"a\":\"foo\",\"b\":{}}")),
    ("&a { *a : 1 }", Some("{\"[object Object]\":1}")),
    ("a: &x 1\nb: &x 2\nc: *x", Some("{\"a\":1,\"b\":2,\"c\":2}")),
    ("&x !!str 1", Some("\"1\"")),
    ("!!str &x 1", Some("\"1\"")),
    ("a: &x !!int '5'\nb: *x", Some("{\"a\":5,\"b\":5}")),
    ("a: &x\nb: *x", Some("{\"a\":null,\"b\":[]}")),
    ("- &a a\n- *a", Some("[\"a\",\"a\"]")),
    ("a: & x", None),
    ("a: *", None),
    ("&a [ *a: 1 ]", None),
    ("a: &x [1]\nb: [*x, *x]", Some("{\"a\":[1],\"b\":[[1],[1]]}")),
    ("a: &x 1\n*x : 2", Some("{\"1\":2,\"a\":1}")),
    ("a: &x b\n? *x\n: c", Some("{\"a\":\"b\",\"b\":\"c\"}")),
    ("&a\n- 1", Some("[1]")),
    ("a: !!str *x", None),
    ("a: &x &y 1", None),
    ("a: &x\n  - 1\nb: *x", Some("{\"a\":[1],\"b\":[1]}")),
    ("a: &x\n  k: v\nb: *x", Some("{\"a\":{\"k\":\"v\"},\"b\":{\"k\":\"v\"}}")),
    ("a: &__proto__ 1\nb: *__proto__", None),
    ("a: &x {b: 1}\n*x : 2", Some("{\"a\":{\"b\":1},\"[object Object]\":2}")),
    ("base: &b {a: 1, b: 2}\nderived:\n  <<: *b\n  b: 3", Some("{\"base\":{\"a\":1,\"b\":2},\"derived\":{\"a\":1,\"b\":3}}")),
    ("x: &x {a: 1}\ny: &y {b: 2}\nz:\n  <<: [*x, *y]\n  c: 3", Some("{\"x\":{\"a\":1},\"y\":{\"b\":2},\"z\":{\"a\":1,\"b\":2,\"c\":3}}")),
    ("<<: {a: 1}\na: 2", Some("{\"a\":2}")),
    ("a: 2\n<<: {a: 1}", Some("{\"a\":2}")),
    ("<<: 1", None),
    ("<<: [1]", None),
    ("'<<': {a: 1}", Some("{\"<<\":{\"a\":1}}")),
    ("!!merge <<: {a: 1}", None),
    ("<<: 2001-01-01", Some("{}")),
    ("<<: [[1, 2]]", Some("{\"0\":1,\"1\":2}")),
    ("<<: {a: 1}\n<<: {b: 2}", Some("{\"a\":1,\"b\":2}")),
    ("<<: {a: 1}\n<<: {a: 2}", Some("{\"a\":1}")),
    ("<<: {a: 1}\na: 2\na: 3", None),
    ("<<: ~", None),
    ("<<:", None),
    ("<<: [{a: 1}, {a: 2, b: 3}]", Some("{\"a\":1,\"b\":3}")),
    ("<<: !!binary aGk=", Some("{\"0\":104,\"1\":105}")),
    ("<<: {2: x, 1: y, b: z}", Some("{\"1\":\"y\",\"2\":\"x\",\"b\":\"z\"}")),
    ("{<<: {a: 1}, b: 2}", Some("{\"a\":1,\"b\":2}")),
    ("[<<: {a: 1}]", Some("[{\"a\":1}]")),
    ("a: &a {x: 1}\nb: &b\n  <<: *a\n  y: 2\nc:\n  <<: *b", Some("{\"a\":{\"x\":1},\"b\":{\"x\":1,\"y\":2},\"c\":{\"x\":1,\"y\":2}}")),
    ("<<: {a: 1}\n<<: [{b: 2}]\nb: 3", Some("{\"a\":1,\"b\":3}")),
    ("a: {<<: {__proto__: 1}, __proto__: 2}", None),
    ("<< : {a: 1}", Some("{\"a\":1}")),
    ("\"<<\": {a: 1}\nx: 1", Some("{\"<<\":{\"a\":1},\"x\":1}")),
    ("!!str 123", Some("\"123\"")),
    ("!!int '123'", Some("123")),
    ("!!int 12.5", None),
    ("!!float 1", Some("1")),
    ("!!float '1.5'", Some("1.5")),
    ("!!bool yes", None),
    ("!!bool 'true'", Some("true")),
    ("!!null ''", None),
    ("!!null", Some("null")),
    ("!!str", Some("\"\"")),
    ("!!seq [1]", Some("[1]")),
    ("!!map {a: 1}", Some("{\"a\":1}")),
    ("!!seq {a: 1}", None),
    ("!!str [1]", None),
    ("!!map", Some("{}")),
    ("!!seq", Some("[]")),
    ("!!binary aGVsbG8=", Some("{\"type\":\"Buffer\",\"data\":[104,101,108,108,111]}")),
    ("!!binary |\n  aGVs\n  bG8=", Some("{\"type\":\"Buffer\",\"data\":[104,101,108,108,111]}")),
    ("!!binary !", None),
    ("!!binary ''", Some("{\"type\":\"Buffer\",\"data\":[0,0,0]}")),
    ("!!binary 'AA=='", Some("{\"type\":\"Buffer\",\"data\":[0]}")),
    ("!!binary =", None),
    ("!!binary 'a b'", None),
    ("!!set {a, b}", Some("{\"a\":null,\"b\":null}")),
    ("!!set {a: 1}", None),
    ("!!set ? a\n? b", None),
    ("!!omap [a: 1, b: 2]", Some("[{\"a\":1},{\"b\":2}]")),
    ("!!omap [a: 1, a: 2]", None),
    ("!!omap [a]", None),
    ("!!omap [{a: 1, b: 2}]", None),
    ("!!pairs [a: 1, a: 2]", Some("[[\"a\",1],[\"a\",2]]")),
    ("!!pairs [a]", None),
    ("!!pairs", Some("[]")),
    ("!!omap", Some("[]")),
    ("!!set", Some("{}")),
    ("!!timestamp 2001-12-14", Some("\"2001-12-14T00:00:00.000Z\"")),
    ("!!timestamp foo", None),
    ("!<tag:yaml.org,2002:str> 1", Some("\"1\"")),
    ("!<!> 1", Some("\"1\"")),
    ("!<?> \"123\"", Some("123")),
    ("!<?> [1]", None),
    ("!<?>\n123", None),
    ("!foo x", None),
    ("!!foo x", None),
    ("!e!str 1", None),
    ("!!str !!int 1", None),
    ("!!str, 1", None),
    ("!<> x", None),
    ("!<tag:yaml.org,2002:int> 0x10", Some("16")),
    ("!<x", None),
    ("!a!b!c x", None),
    ("!a{ x", None),
    ("!%41 x", None),
    ("!!str\n123", Some("123")),
    ("a: !!str\n  123", Some("{\"a\":123}")),
    ("- !!str\n  123", Some("[123]")),
    ("!!int\n123", None),
    ("key: !!int\n  \"12\"", Some("{\"key\":12}")),
    ("a: !!str\nb: 1", Some("{\"a\":\"\",\"b\":1}")),
    ("a: !!str\n  ~", Some("{\"a\":\"\"}")),
    ("a: !!null\n  ~", Some("{\"a\":null}")),
    ("!!float\n.5", None),
    ("!!int\n'7'", Some("7")),
    ("!!bool\n'true'", Some("true")),
    ("!!str\n[1]", None),
    ("!!seq\n- 1", Some("[1]")),
    ("!!map\na: 1", Some("{\"a\":1}")),
    ("!!binary\n!!binary aGk=", None),
    ("!!str\n!!binary aGk=", None),
    ("a: !!timestamp\n  2001-12-14", None),
    ("? !!binary aGVsbG8=\n: x", Some("{\"hello\":\"x\"}")),
    ("!!str 2001-12-14", Some("\"2001-12-14\"")),
    ("!!str .inf", Some("\".inf\"")),
    ("! 12", Some("\"12\"")),
    ("!!float 0x1F", None),
    ("!!int 1:30", Some("90")),
    ("!!float 1:30", None),
    ("2001-12-14: x", Some("{\"Fri Dec 14 2001 00:00:00 GMT+0000 (Coordinated Universal Time)\":\"x\"}")),
    ("[2001-12-14]: x", Some("{\"Fri Dec 14 2001 00:00:00 GMT+0000 (Coordinated Universal Time)\":\"x\"}")),
    ("0x1F: a\n1.0: b\n~: c\ntrue: d\n.inf: e\n.nan: f\n[1, 2]: g\n[null, ~]: h\n{a: 1}: i", Some("{\"1\":\"b\",\"31\":\"a\",\"null\":\"c\",\"true\":\"d\",\"Infinity\":\"e\",\"NaN\":\"f\",\"1,2\":\"g\",\",\":\"h\",\"[object Object]\":\"i\"}")),
    ("1e21: a\n1e-7: b\n0.000001: c\n-0: d\n0.1: e", Some("{\"0\":\"d\",\"1e+21\":\"a\",\"1e-7\":\"b\",\"0.000001\":\"c\",\"0.1\":\"e\"}")),
    ("? !!binary 4pyT\n: x", Some("{\"\u{2713}\":\"x\"}")),
    ("? !!binary /w==\n: x", Some("{\"\u{fffd}\":\"x\"}")),
    ("[!!binary aGk=]: x", Some("{\"hi\":\"x\"}")),
    ("[1.5, true, ~]: x", Some("{\"1.5,true,\":\"x\"}")),
    ("[[1]]: x", None),
    ("[{a: 1}, b]: x", Some("{\"[object Object],b\":\"x\"}")),
    ("{}: x", Some("{\"[object Object]\":\"x\"}")),
    ("[]: x", Some("{\"\":\"x\"}")),
    ("!!omap [a: 1]: x", None),
    ("!!pairs [a: 1]: x", None),
    ("? 2001-12-14 21:59:43.10 -5\n: x", Some("{\"Sat Dec 15 2001 02:59:43 GMT+0000 (Coordinated Universal Time)\":\"x\"}")),
    ];

    /// The cases that load to a number: `String(n)` of it (`-0` for negative zero), from the same run.
    const NUMBERS: &[(&str, &str)] = &[
    ("0", "0"),
    ("-0", "0"),
    ("+0", "0"),
    ("00", "0"),
    ("-00", "-0"),
    ("012", "10"),
    ("0x1F", "31"),
    ("-0x1F", "-31"),
    ("0xff_ff", "65535"),
    ("0b101", "5"),
    ("-0b101", "-5"),
    ("0x_1", "1"),
    ("1_000", "1000"),
    ("1__0", "10"),
    ("+12", "12"),
    ("-4", "-4"),
    ("1:30", "90"),
    ("-1:30", "-90"),
    ("1:5", "65"),
    ("190:20:30", "685230"),
    ("1:2:3:4", "223384"),
    ("12345678901234567890", "12345678901234567000"),
    ("9007199254740993", "9007199254740992"),
    ("123456789012345678901234567890", "1.2345678901234568e+29"),
    ("0x400001000000020000001", "4.835704431380021e+24"),
    ("0xFFFFFFFFFFFFFFFFFFFF", "1.2089258196146292e+24"),
    ("0b111111111111111111111111111111111111111111111111111111111111", "1152921504606847000"),
    ("07777777777777777777777777", "3.777893186295716e+22"),
    ("10000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000", "Infinity"),
    ("1.5", "1.5"),
    ("-1.5", "-1.5"),
    ("+1.5", "1.5"),
    (".5", "0.5"),
    ("1.", "1"),
    ("1.e5", "100000"),
    ("1e5", "100000"),
    ("1E5", "100000"),
    ("1e+5", "100000"),
    ("1e-5", "0.00001"),
    ("6.8523015e+5", "685230.15"),
    ("685.230_15e+03", "685230.15"),
    ("685_230.15", "685230.15"),
    ("190:20:30.15", "685230.15"),
    ("1:30.5", "90.5"),
    ("1:30.", "90"),
    (".inf", "Infinity"),
    ("-.inf", "-Infinity"),
    ("+.inf", "Infinity"),
    (".Inf", "Infinity"),
    (".INF", "Infinity"),
    (".nan", "NaN"),
    (".NaN", "NaN"),
    (".NAN", "NaN"),
    ("1e400", "Infinity"),
    ("-1e400", "-Infinity"),
    ("0.0", "0"),
    ("-0.0", "-0"),
    ("._1", "0.1"),
    ("1_.5", "1.5"),
    ("0.30000000000000004", "0.30000000000000004"),
    ("5e-324", "5e-324"),
    ("1.7976931348623157e308", "1.7976931348623157e+308"),
    ("!!int '123'", "123"),
    ("!!float 1", "1"),
    ("!!float '1.5'", "1.5"),
    ("!<?> \"123\"", "123"),
    ("!<tag:yaml.org,2002:int> 0x10", "16"),
    ("!!str\n123", "123"),
    ("!!int\n'7'", "7"),
    ("!!int 1:30", "90"),
    ];

    #[test]
    fn matches_js_yaml() {
        let failures: Vec<String> = CASES
            .iter()
            .filter_map(|&(input, expected)| {
                let got = load(input);
                let text = got.as_ref().ok().map(json_text);
                (text.as_deref() != expected).then(|| format!("{input:?}: js-yaml {expected:?}, port {got:?}"))
            })
            .collect();
        assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), CASES.len(), failures.join("\n"));
    }

    #[test]
    fn numbers_are_the_same_doubles() {
        for &(input, expected) in NUMBERS {
            let Ok(Json::Number(n)) = load(input) else { panic!("{input:?} is not a number") };
            let got = if n == 0.0 && n.is_sign_negative() { "-0".to_owned() } else { js_number_string(n) };
            assert_eq!(got, expected, "{input:?}");
        }
    }

    // Where the port differs from js-yaml on purpose (see the module docs). js-yaml, from the same run:
    // `&a [*a]` loads an array containing itself (JSON.stringify throws), `"\uD83D"`, `a: "\uDE00x"` and
    // `"\U00110000"` (JSON.stringify: `"\udc00\udc00"`) keep their lone surrogates, and 600 nested `[` load fine.
    #[test]
    fn documented_divergences() {
        assert!(load("&a [*a]").is_err());
        assert!(load("a: &a\n  b: [*a]").is_err());
        assert_eq!(load("\"\\uD83D\""), Ok(Json::String("\u{fffd}".to_owned())));
        let object = |k: &str, v: &str| Json::Object(IndexMap::from([(k.to_owned(), Json::String(v.to_owned()))]));
        assert_eq!(load("a: \"\\uDE00x\""), Ok(object("a", "\u{fffd}x")));
        assert_eq!(load("\"\\U00110000\""), Ok(Json::String("\u{fffd}\u{fffd}".to_owned())));
        let deep = format!("{}{}", "[".repeat(600), "]".repeat(600));
        assert!(load(&deep).is_err());
    }

    #[test]
    fn deep_nesting_within_the_limit_fits_a_small_stack() {
        let depth = MAX_DEPTH - 1;
        let flow = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let block: String = (0..depth).map(|i| format!("{}k:\n", " ".repeat(i))).collect();
        let loaded = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || (load(&flow).is_ok(), load(&block).is_ok()))
            .expect("spawn")
            .join()
            .expect("no stack overflow");
        assert_eq!(loaded, (true, true));
    }

    #[test]
    fn alias_expansion_is_bounded() {
        // 10 levels of 10 aliases each: 10^10 values once copied.
        let mut doc = "a0: &a0 [x]\n".to_owned();
        for i in 1..=10 {
            let p = i - 1;
            doc += &format!("a{i}: &a{i} [*a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}]\n");
        }
        assert!(load(&doc).is_err());
    }
}
