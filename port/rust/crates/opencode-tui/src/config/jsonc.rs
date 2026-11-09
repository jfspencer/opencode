//! `jsonc-parser` 3.3.1 (the CLI config's `parse`, `modify` and `applyEdits`, packages/cli/src/config/config.ts):
//! the scanner, the visitor-based parser (`parse` with its errors, `parseTree`, `findNodeAtLocation`), `setProperty`
//! with formatting and the formatter, ported line for line.
//!
//! Offsets are UTF-16 code units, as in JS; texts are converted at the boundary. Divergence: a string with an
//! escaped lone surrogate (`"\ud800"`) holds U+FFFD here (a Rust `String` cannot hold the surrogate).

use opencode_protocol::Json;
use opencode_protocol::wire::Object;

// SyntaxKind
const OPEN_BRACE: u8 = 1;
const CLOSE_BRACE: u8 = 2;
const OPEN_BRACKET: u8 = 3;
const CLOSE_BRACKET: u8 = 4;
const COMMA: u8 = 5;
const COLON: u8 = 6;
const NULL_KEYWORD: u8 = 7;
const TRUE_KEYWORD: u8 = 8;
const FALSE_KEYWORD: u8 = 9;
const STRING_LITERAL: u8 = 10;
const NUMERIC_LITERAL: u8 = 11;
const LINE_COMMENT: u8 = 12;
const BLOCK_COMMENT: u8 = 13;
const LINE_BREAK: u8 = 14;
const TRIVIA: u8 = 15;
const UNKNOWN: u8 = 16;
const EOF: u8 = 17;

// ScanError
const SCAN_NONE: u8 = 0;
const SCAN_END_OF_COMMENT: u8 = 1;
const SCAN_END_OF_STRING: u8 = 2;
const SCAN_END_OF_NUMBER: u8 = 3;
const SCAN_INVALID_UNICODE: u8 = 4;
const SCAN_INVALID_ESCAPE: u8 = 5;
const SCAN_INVALID_CHARACTER: u8 = 6;

fn is_white_space(ch: u16) -> bool {
    ch == 32 || ch == 9
}

fn is_line_break(ch: u16) -> bool {
    ch == 10 || ch == 13
}

fn is_digit(ch: u16) -> bool {
    (48..=57).contains(&ch)
}

/// `createScanner(text, false)`.
pub(crate) struct Scanner<'a> {
    text: &'a [u16],
    pos: usize,
    value: Vec<u16>,
    token_offset: usize,
    token: u8,
    scan_error: u8,
}

impl<'a> Scanner<'a> {
    pub fn new(text: &'a [u16]) -> Scanner<'a> {
        Scanner { text, pos: 0, value: Vec::new(), token_offset: 0, token: UNKNOWN, scan_error: SCAN_NONE }
    }

    /// `text.charCodeAt(i)`: NaN past the end, which matches no test.
    fn at(&self, i: usize) -> Option<u16> {
        self.text.get(i).copied()
    }

    fn is(&self, i: usize, ch: u16) -> bool {
        self.at(i) == Some(ch)
    }

    pub fn token(&self) -> u8 {
        self.token
    }

    pub fn token_offset(&self) -> usize {
        self.token_offset
    }

    pub fn token_length(&self) -> usize {
        self.pos - self.token_offset
    }

    pub fn token_error(&self) -> u8 {
        self.scan_error
    }

    pub fn token_value(&self) -> &[u16] {
        &self.value
    }

    fn scan_hex_digits(&mut self, count: usize) -> i64 {
        let mut digits = 0;
        let mut value: i64 = 0;
        while digits < count {
            let Some(ch) = self.at(self.pos) else { break };
            let digit = match ch {
                48..=57 => ch - 48,
                65..=70 => ch - 65 + 10,
                97..=102 => ch - 97 + 10,
                _ => break,
            };
            value = value * 16 + digit as i64;
            self.pos += 1;
            digits += 1;
        }
        if digits < count { -1 } else { value }
    }

    fn scan_number(&mut self) -> Vec<u16> {
        let start = self.pos;
        if self.is(self.pos, 48) {
            self.pos += 1;
        } else {
            self.pos += 1;
            while self.pos < self.text.len() && is_digit(self.text[self.pos]) {
                self.pos += 1;
            }
        }
        if self.pos < self.text.len() && self.text[self.pos] == 46 {
            self.pos += 1;
            if self.pos < self.text.len() && is_digit(self.text[self.pos]) {
                self.pos += 1;
                while self.pos < self.text.len() && is_digit(self.text[self.pos]) {
                    self.pos += 1;
                }
            } else {
                self.scan_error = SCAN_END_OF_NUMBER;
                return self.text[start..self.pos].to_vec();
            }
        }
        let mut end = self.pos;
        if self.pos < self.text.len() && (self.text[self.pos] == 69 || self.text[self.pos] == 101) {
            self.pos += 1;
            // `pos < len && c === plus || c === minus` (precedence as written in jsonc-parser)
            if (self.pos < self.text.len() && self.text[self.pos] == 43) || self.is(self.pos, 45) {
                self.pos += 1;
            }
            if self.pos < self.text.len() && is_digit(self.text[self.pos]) {
                self.pos += 1;
                while self.pos < self.text.len() && is_digit(self.text[self.pos]) {
                    self.pos += 1;
                }
                end = self.pos;
            } else {
                self.scan_error = SCAN_END_OF_NUMBER;
            }
        }
        self.text[start..end].to_vec()
    }

    fn scan_string(&mut self) -> Vec<u16> {
        let len = self.text.len();
        let mut result: Vec<u16> = Vec::new();
        let mut start = self.pos;
        loop {
            if self.pos >= len {
                result.extend_from_slice(&self.text[start..self.pos]);
                self.scan_error = SCAN_END_OF_STRING;
                break;
            }
            let ch = self.text[self.pos];
            if ch == 34 {
                result.extend_from_slice(&self.text[start..self.pos]);
                self.pos += 1;
                break;
            }
            if ch == 92 {
                result.extend_from_slice(&self.text[start..self.pos]);
                self.pos += 1;
                if self.pos >= len {
                    self.scan_error = SCAN_END_OF_STRING;
                    break;
                }
                let ch2 = self.text[self.pos];
                self.pos += 1;
                match ch2 {
                    34 => result.push(34),
                    92 => result.push(92),
                    47 => result.push(47),
                    98 => result.push(8),
                    102 => result.push(12),
                    110 => result.push(10),
                    114 => result.push(13),
                    116 => result.push(9),
                    117 => {
                        let ch3 = self.scan_hex_digits(4);
                        if ch3 >= 0 {
                            result.push(ch3 as u16);
                        } else {
                            self.scan_error = SCAN_INVALID_UNICODE;
                        }
                    }
                    _ => self.scan_error = SCAN_INVALID_ESCAPE,
                }
                start = self.pos;
                continue;
            }
            if ch <= 0x1f {
                if is_line_break(ch) {
                    result.extend_from_slice(&self.text[start..self.pos]);
                    self.scan_error = SCAN_END_OF_STRING;
                    break;
                } else {
                    self.scan_error = SCAN_INVALID_CHARACTER;
                }
            }
            self.pos += 1;
        }
        result
    }

    /// `scan()` (trivia included).
    pub fn scan(&mut self) -> u8 {
        self.token = self.scan_next();
        self.token
    }

    fn scan_next(&mut self) -> u8 {
        let len = self.text.len();
        self.value.clear();
        self.scan_error = SCAN_NONE;
        self.token_offset = self.pos;
        if self.pos >= len {
            self.token_offset = len;
            return EOF;
        }
        let mut code = self.text[self.pos];
        if is_white_space(code) {
            loop {
                self.pos += 1;
                self.value.push(code);
                match self.at(self.pos) {
                    Some(c) if is_white_space(c) => code = c,
                    _ => break,
                }
            }
            return TRIVIA;
        }
        if is_line_break(code) {
            self.pos += 1;
            self.value.push(code);
            if code == 13 && self.is(self.pos, 10) {
                self.pos += 1;
                self.value.push(10);
            }
            return LINE_BREAK;
        }
        match code {
            123 => {
                self.pos += 1;
                OPEN_BRACE
            }
            125 => {
                self.pos += 1;
                CLOSE_BRACE
            }
            91 => {
                self.pos += 1;
                OPEN_BRACKET
            }
            93 => {
                self.pos += 1;
                CLOSE_BRACKET
            }
            58 => {
                self.pos += 1;
                COLON
            }
            44 => {
                self.pos += 1;
                COMMA
            }
            34 => {
                self.pos += 1;
                self.value = self.scan_string();
                STRING_LITERAL
            }
            47 => {
                let start = self.pos;
                if self.is(self.pos + 1, 47) {
                    self.pos += 2;
                    while self.pos < len {
                        if is_line_break(self.text[self.pos]) {
                            break;
                        }
                        self.pos += 1;
                    }
                    self.value = self.text[start..self.pos].to_vec();
                    return LINE_COMMENT;
                }
                if self.is(self.pos + 1, 42) {
                    self.pos += 2;
                    let safe_length = len - 1;
                    let mut closed = false;
                    while self.pos < safe_length {
                        let ch = self.text[self.pos];
                        if ch == 42 && self.is(self.pos + 1, 47) {
                            self.pos += 2;
                            closed = true;
                            break;
                        }
                        self.pos += 1;
                        if is_line_break(ch) && ch == 13 && self.is(self.pos, 10) {
                            self.pos += 1;
                        }
                    }
                    if !closed {
                        self.pos += 1;
                        self.scan_error = SCAN_END_OF_COMMENT;
                    }
                    self.value = self.text[start..self.pos.min(len)].to_vec();
                    return BLOCK_COMMENT;
                }
                self.value.push(code);
                self.pos += 1;
                UNKNOWN
            }
            45 => {
                self.value.push(code);
                self.pos += 1;
                if self.pos == len || !is_digit(self.text[self.pos]) {
                    return UNKNOWN;
                }
                let number = self.scan_number();
                self.value.extend(number);
                NUMERIC_LITERAL
            }
            48..=57 => {
                let number = self.scan_number();
                self.value.extend(number);
                NUMERIC_LITERAL
            }
            _ => {
                while self.pos < len && is_unknown_content_character(code) {
                    self.pos += 1;
                    match self.at(self.pos) {
                        Some(c) => code = c,
                        None => break,
                    }
                }
                if self.token_offset != self.pos {
                    self.value = self.text[self.token_offset..self.pos].to_vec();
                    return match String::from_utf16_lossy(&self.value).as_str() {
                        "true" => TRUE_KEYWORD,
                        "false" => FALSE_KEYWORD,
                        "null" => NULL_KEYWORD,
                        _ => UNKNOWN,
                    };
                }
                self.value.push(code);
                self.pos += 1;
                UNKNOWN
            }
        }
    }
}

fn is_unknown_content_character(code: u16) -> bool {
    if is_white_space(code) || is_line_break(code) {
        return false;
    }
    !matches!(code, 125 | 93 | 123 | 91 | 34 | 58 | 44 | 47)
}

/// A node of `parseTree`.
#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub kind: NodeKind,
    pub offset: usize,
    pub length: usize,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
    /// The value of a string node (a property key).
    pub value: Option<Vec<u16>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeKind {
    Object,
    Array,
    Property,
    String,
    Number,
    Boolean,
    Null,
}

/// The visitor events of `visit`.
enum Event {
    ObjectBegin(usize),
    ObjectProperty(Vec<u16>, usize, usize),
    ObjectEnd(usize, usize),
    ArrayBegin(usize),
    ArrayEnd(usize, usize),
    Literal(Json, NodeKind, usize, usize),
    Separator(u16, usize),
    Error,
}

/// `visit(text, visitor, { allowTrailingComma })`.
fn visit(text: &[u16], allow_trailing_comma: bool, on: &mut dyn FnMut(Event)) {
    struct Visit<'a, 'b> {
        scanner: Scanner<'a>,
        allow_trailing_comma: bool,
        on: &'b mut dyn FnMut(Event),
    }
    impl Visit<'_, '_> {
        fn scan_next(&mut self) -> u8 {
            loop {
                let token = self.scanner.scan();
                match self.scanner.token_error() {
                    SCAN_INVALID_UNICODE | SCAN_INVALID_ESCAPE | SCAN_END_OF_NUMBER | SCAN_END_OF_COMMENT | SCAN_END_OF_STRING
                    | SCAN_INVALID_CHARACTER => self.handle_error(&[], &[]),
                    _ => {}
                }
                match token {
                    LINE_COMMENT | BLOCK_COMMENT | TRIVIA | LINE_BREAK => {}
                    UNKNOWN => self.handle_error(&[], &[]),
                    _ => return token,
                }
            }
        }

        fn handle_error(&mut self, skip_until_after: &[u8], skip_until: &[u8]) {
            (self.on)(Event::Error);
            if skip_until_after.len() + skip_until.len() > 0 {
                let mut token = self.scanner.token();
                while token != EOF {
                    if skip_until_after.contains(&token) {
                        self.scan_next();
                        break;
                    } else if skip_until.contains(&token) {
                        break;
                    }
                    token = self.scan_next();
                }
            }
        }

        fn parse_string(&mut self, is_value: bool) -> bool {
            let value = self.scanner.token_value().to_vec();
            let (offset, length) = (self.scanner.token_offset(), self.scanner.token_length());
            if is_value {
                (self.on)(Event::Literal(Json::String(String::from_utf16_lossy(&value)), NodeKind::String, offset, length));
            } else {
                (self.on)(Event::ObjectProperty(value, offset, length));
            }
            self.scan_next();
            true
        }

        fn parse_literal(&mut self) -> bool {
            let (offset, length) = (self.scanner.token_offset(), self.scanner.token_length());
            let (value, kind) = match self.scanner.token() {
                NUMERIC_LITERAL => {
                    let text = String::from_utf16_lossy(self.scanner.token_value());
                    let mut value = opencode_protocol::wire::number::js_string_to_number(&text);
                    if value.is_nan() {
                        self.handle_error(&[], &[]);
                        value = 0.0;
                    }
                    (Json::Number(value), NodeKind::Number)
                }
                NULL_KEYWORD => (Json::Null, NodeKind::Null),
                TRUE_KEYWORD => (Json::Bool(true), NodeKind::Boolean),
                FALSE_KEYWORD => (Json::Bool(false), NodeKind::Boolean),
                _ => return false,
            };
            (self.on)(Event::Literal(value, kind, offset, length));
            self.scan_next();
            true
        }

        fn parse_property(&mut self) -> bool {
            if self.scanner.token() != STRING_LITERAL {
                self.handle_error(&[], &[CLOSE_BRACE, COMMA]);
                return false;
            }
            self.parse_string(false);
            if self.scanner.token() == COLON {
                (self.on)(Event::Separator(58, self.scanner.token_offset()));
                self.scan_next();
                if !self.parse_value() {
                    self.handle_error(&[], &[CLOSE_BRACE, COMMA]);
                }
            } else {
                self.handle_error(&[], &[CLOSE_BRACE, COMMA]);
            }
            true
        }

        fn parse_object(&mut self) -> bool {
            (self.on)(Event::ObjectBegin(self.scanner.token_offset()));
            self.scan_next();
            let mut needs_comma = false;
            while self.scanner.token() != CLOSE_BRACE && self.scanner.token() != EOF {
                if self.scanner.token() == COMMA {
                    if !needs_comma {
                        self.handle_error(&[], &[]);
                    }
                    (self.on)(Event::Separator(44, self.scanner.token_offset()));
                    self.scan_next();
                    if self.scanner.token() == CLOSE_BRACE && self.allow_trailing_comma {
                        break;
                    }
                } else if needs_comma {
                    self.handle_error(&[], &[]);
                }
                if !self.parse_property() {
                    self.handle_error(&[], &[CLOSE_BRACE, COMMA]);
                }
                needs_comma = true;
            }
            (self.on)(Event::ObjectEnd(self.scanner.token_offset(), self.scanner.token_length()));
            if self.scanner.token() != CLOSE_BRACE {
                self.handle_error(&[CLOSE_BRACE], &[]);
            } else {
                self.scan_next();
            }
            true
        }

        fn parse_array(&mut self) -> bool {
            (self.on)(Event::ArrayBegin(self.scanner.token_offset()));
            self.scan_next();
            let mut needs_comma = false;
            while self.scanner.token() != CLOSE_BRACKET && self.scanner.token() != EOF {
                if self.scanner.token() == COMMA {
                    if !needs_comma {
                        self.handle_error(&[], &[]);
                    }
                    (self.on)(Event::Separator(44, self.scanner.token_offset()));
                    self.scan_next();
                    if self.scanner.token() == CLOSE_BRACKET && self.allow_trailing_comma {
                        break;
                    }
                } else if needs_comma {
                    self.handle_error(&[], &[]);
                }
                if !self.parse_value() {
                    self.handle_error(&[], &[CLOSE_BRACKET, COMMA]);
                }
                needs_comma = true;
            }
            (self.on)(Event::ArrayEnd(self.scanner.token_offset(), self.scanner.token_length()));
            if self.scanner.token() != CLOSE_BRACKET {
                self.handle_error(&[CLOSE_BRACKET], &[]);
            } else {
                self.scan_next();
            }
            true
        }

        fn parse_value(&mut self) -> bool {
            match self.scanner.token() {
                OPEN_BRACKET => self.parse_array(),
                OPEN_BRACE => self.parse_object(),
                STRING_LITERAL => self.parse_string(true),
                _ => self.parse_literal(),
            }
        }
    }
    let mut v = Visit { scanner: Scanner::new(text), allow_trailing_comma, on };
    v.scan_next();
    if v.scanner.token() == EOF {
        v.handle_error(&[], &[]);
        return;
    }
    if !v.parse_value() {
        v.handle_error(&[], &[]);
        return;
    }
    if v.scanner.token() != EOF {
        v.handle_error(&[], &[]);
    }
}

pub(crate) fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// `parse(text, errors, { allowTrailingComma: true })`: the value and whether any error was reported. Objects are
/// built as JS builds them (a repeated key keeps its first position and its last value; `__proto__` sets the
/// prototype and is not a property).
pub fn parse(text: &str) -> (Option<Json>, bool) {
    let units = utf16(text);
    // The stack of containers being built: (value, current property).
    let mut stack: Vec<(Json, Option<String>)> = vec![(Json::Array(Vec::new()), None)];
    let mut errors = false;
    fn push_value(stack: &mut [(Json, Option<String>)], value: Json) {
        let (parent, property) = stack.last_mut().unwrap();
        match parent {
            Json::Array(items) => items.push(value),
            Json::Object(object) => {
                if let Some(property) = property.clone()
                    && property != "__proto__"
                {
                    object.insert(property, value);
                }
            }
            _ => {}
        }
    }
    visit(&units, true, &mut |event| match event {
        Event::ObjectBegin(_) => stack.push((Json::Object(Object::new()), None)),
        Event::ArrayBegin(_) => stack.push((Json::Array(Vec::new()), None)),
        Event::ObjectProperty(name, _, _) => stack.last_mut().unwrap().1 = Some(String::from_utf16_lossy(&name)),
        Event::ObjectEnd(..) | Event::ArrayEnd(..) => {
            let (value, _) = stack.pop().unwrap();
            push_value(&mut stack, value);
        }
        Event::Literal(value, ..) => push_value(&mut stack, value),
        Event::Separator(..) => {}
        Event::Error => errors = true,
    });
    // Containers left open by errors are attached where they began.
    while stack.len() > 1 {
        let (value, _) = stack.pop().unwrap();
        push_value(&mut stack, value);
    }
    let root = match stack.pop() {
        Some((Json::Array(mut items), _)) if !items.is_empty() => Some(items.remove(0)),
        _ => None,
    };
    (root, errors)
}

/// `parseTree(text)`: the node arena and the root's index.
pub(crate) fn parse_tree(text: &[u16]) -> (Vec<Node>, Option<usize>) {
    // Index 0 is the artificial root array.
    let mut nodes: Vec<Node> =
        vec![Node { kind: NodeKind::Array, offset: 0, length: 0, children: Vec::new(), parent: None, value: None }];
    let mut current = 0usize;
    fn on_value(nodes: &mut Vec<Node>, current: usize, node: Node) -> usize {
        let index = nodes.len();
        nodes.push(node);
        nodes[current].children.push(index);
        index
    }
    fn ensure_property_complete(nodes: &mut [Node], current: &mut usize, end: usize) {
        if nodes[*current].kind == NodeKind::Property {
            nodes[*current].length = end - nodes[*current].offset;
            *current = nodes[*current].parent.unwrap();
        }
    }
    visit(text, false, &mut |event| match event {
        Event::ObjectBegin(offset) => {
            current = on_value(&mut nodes, current, Node {
                kind: NodeKind::Object,
                offset,
                length: 0,
                children: Vec::new(),
                parent: Some(current),
                value: None,
            });
        }
        Event::ObjectProperty(name, offset, length) => {
            current = on_value(&mut nodes, current, Node {
                kind: NodeKind::Property,
                offset,
                length: 0,
                children: Vec::new(),
                parent: Some(current),
                value: None,
            });
            on_value(&mut nodes, current, Node {
                kind: NodeKind::String,
                offset,
                length,
                children: Vec::new(),
                parent: Some(current),
                value: Some(name),
            });
        }
        Event::ObjectEnd(offset, length) => {
            ensure_property_complete(&mut nodes, &mut current, offset + length);
            nodes[current].length = offset + length - nodes[current].offset;
            current = nodes[current].parent.unwrap();
            ensure_property_complete(&mut nodes, &mut current, offset + length);
        }
        Event::ArrayBegin(offset) => {
            current = on_value(&mut nodes, current, Node {
                kind: NodeKind::Array,
                offset,
                length: 0,
                children: Vec::new(),
                parent: Some(current),
                value: None,
            });
        }
        Event::ArrayEnd(offset, length) => {
            nodes[current].length = offset + length - nodes[current].offset;
            current = nodes[current].parent.unwrap();
            ensure_property_complete(&mut nodes, &mut current, offset + length);
        }
        Event::Literal(_, kind, offset, length) => {
            on_value(&mut nodes, current, Node { kind, offset, length, children: Vec::new(), parent: Some(current), value: None });
            ensure_property_complete(&mut nodes, &mut current, offset + length);
        }
        Event::Separator(sep, offset) => {
            if nodes[current].kind == NodeKind::Property && sep == 44 {
                ensure_property_complete(&mut nodes, &mut current, offset);
            }
        }
        Event::Error => {}
    });
    let root = nodes[0].children.first().copied();
    if let Some(root) = root {
        nodes[root].parent = None;
    }
    (nodes, root)
}

/// A path segment of `modify`.
#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    Key(String),
    Index(i64),
}

/// `findNodeAtLocation(root, path)`.
fn find_node_at_location(nodes: &[Node], root: Option<usize>, path: &[Segment]) -> Option<usize> {
    let mut node = root?;
    for segment in path {
        match segment {
            Segment::Key(key) => {
                if nodes[node].kind != NodeKind::Object {
                    return None;
                }
                let key = utf16(key);
                let found = nodes[node].children.iter().find(|&&property| {
                    let children = &nodes[property].children;
                    !children.is_empty() && nodes[children[0]].value.as_deref() == Some(&key[..]) && children.len() == 2
                })?;
                node = nodes[*found].children[1];
            }
            Segment::Index(index) => {
                if nodes[node].kind != NodeKind::Array || *index < 0 || *index as usize >= nodes[node].children.len() {
                    return None;
                }
                node = nodes[node].children[*index as usize];
            }
        }
    }
    Some(node)
}

/// An edit (`EditResult` item): replace `length` units at `offset` with `content`.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub offset: usize,
    pub length: usize,
    pub content: Vec<u16>,
}

/// The formatting options the CLI config passes: `{ tabSize: 2, insertSpaces: true }`.
#[derive(Clone, Copy, Debug)]
pub struct FormattingOptions {
    pub tab_size: usize,
    pub insert_spaces: bool,
}

fn stringify(value: &Json) -> Vec<u16> {
    utf16(&opencode_protocol::to_string(value))
}

/// `modify(text, path, value, { formattingOptions })` (`setProperty`): `value: None` removes.
pub fn modify(text: &str, path: &[Segment], value: Option<&Json>, options: FormattingOptions) -> Result<Vec<Edit>, String> {
    let units = utf16(text);
    let (nodes, root) = parse_tree(&units);
    let mut path = path.to_vec();
    let mut value: Option<Json> = value.cloned();
    let mut parent: Option<usize> = None;
    let mut last_segment: Option<Segment> = None;
    while let Some(segment) = path.pop() {
        parent = find_node_at_location(&nodes, root, &path);
        last_segment = Some(segment.clone());
        if parent.is_none() && value.is_some() {
            let inner = value.take().unwrap();
            value = Some(match segment {
                Segment::Key(key) => {
                    let mut object = Object::new();
                    object.insert(key, inner);
                    Json::Object(object)
                }
                Segment::Index(_) => Json::Array(vec![inner]),
            });
        } else {
            break;
        }
    }
    let Some(parent) = parent else {
        let Some(value) = value else { return Err("Can not delete in empty document".into()) };
        let (offset, length) = root.map(|r| (nodes[r].offset, nodes[r].length)).unwrap_or((0, 0));
        return Ok(with_formatting(&units, Edit { offset, length, content: stringify(&value) }, options));
    };
    let node = &nodes[parent];
    match (&last_segment, node.kind) {
        (Some(Segment::Key(key)), NodeKind::Object) => {
            let existing = find_node_at_location(&nodes, Some(parent), &[Segment::Key(key.clone())]);
            if let Some(existing) = existing {
                match value {
                    None => {
                        let property = nodes[existing].parent.ok_or("Malformed AST")?;
                        let property_index = node.children.iter().position(|c| *c == property).unwrap();
                        let mut remove_end = nodes[property].offset + nodes[property].length;
                        let remove_begin = if property_index > 0 {
                            let previous = &nodes[node.children[property_index - 1]];
                            previous.offset + previous.length
                        } else {
                            if node.children.len() > 1 {
                                remove_end = nodes[node.children[1]].offset;
                            }
                            node.offset + 1
                        };
                        Ok(with_formatting(&units, Edit { offset: remove_begin, length: remove_end - remove_begin, content: Vec::new() }, options))
                    }
                    Some(value) => Ok(with_formatting(
                        &units,
                        Edit { offset: nodes[existing].offset, length: nodes[existing].length, content: stringify(&value) },
                        options,
                    )),
                }
            } else {
                let Some(value) = value else { return Ok(Vec::new()) };
                let mut property = stringify(&Json::String(key.clone()));
                property.extend(utf16(": "));
                property.extend(stringify(&value));
                let index = node.children.len();
                let edit = if index > 0 {
                    let previous = &nodes[node.children[index - 1]];
                    let mut content = utf16(",");
                    content.extend(property);
                    Edit { offset: previous.offset + previous.length, length: 0, content }
                } else {
                    Edit { offset: node.offset + 1, length: 0, content: property }
                };
                Ok(with_formatting(&units, edit, options))
            }
        }
        (Some(Segment::Index(index)), NodeKind::Array) => {
            let index = *index;
            let children = &node.children;
            let len = children.len() as i64;
            // `parent.children[i]` of JS: undefined out of range, which jsonc-parser then reads `.offset` of.
            let child = |i: i64| (0..len).contains(&i).then(|| &nodes[children[i as usize]]);
            let undefined_offset = |name: &str| format!("undefined is not an object (evaluating '{name}.offset')");
            if index == -1 {
                // `${JSON.stringify(value)}`: a removal inserts the text `undefined`.
                let content = value.as_ref().map(stringify).unwrap_or_else(|| utf16("undefined"));
                let edit = if children.is_empty() {
                    Edit { offset: node.offset + 1, length: 0, content }
                } else {
                    let previous = child(len - 1).unwrap();
                    let mut c = utf16(",");
                    c.extend(content);
                    Edit { offset: previous.offset + previous.length, length: 0, content: c }
                };
                return Ok(with_formatting(&units, edit, options));
            }
            match value {
                None => {
                    let edit = if len == 1 {
                        Edit { offset: node.offset + 1, length: node.length - 2, content: Vec::new() }
                    } else if len - 1 == index {
                        let previous = child(index - 1).ok_or_else(|| undefined_offset("previous"))?;
                        let offset = previous.offset + previous.length;
                        let parent_end = node.offset + node.length;
                        Edit { offset, length: parent_end - 2 - offset, content: Vec::new() }
                    } else {
                        let to_remove = child(index).ok_or_else(|| undefined_offset("toRemove"))?;
                        let next = child(index + 1).ok_or_else(|| undefined_offset("parent.children[(removalIndex + 1)]"))?;
                        Edit { offset: to_remove.offset, length: next.offset - to_remove.offset, content: Vec::new() }
                    };
                    Ok(with_formatting(&units, edit, options))
                }
                Some(value) => {
                    let content = stringify(&value);
                    let edit = if len > index {
                        let to_modify = child(index).ok_or_else(|| undefined_offset("toModify"))?;
                        Edit { offset: to_modify.offset, length: to_modify.length, content }
                    } else if len == 0 || index == 0 {
                        let mut c = content;
                        if len != 0 {
                            c.extend(utf16(","));
                        }
                        Edit { offset: node.offset + 1, length: 0, content: c }
                    } else {
                        let i = index.min(len);
                        let previous = child(i - 1).ok_or_else(|| undefined_offset("previous"))?;
                        let mut c = utf16(",");
                        c.extend(content);
                        Edit { offset: previous.offset + previous.length, length: 0, content: c }
                    };
                    Ok(with_formatting(&units, edit, options))
                }
            }
        }
        (segment, kind) => Err(format!(
            "Can not add {} to parent of type {}",
            if matches!(segment, Some(Segment::Index(_))) { "property" } else { "index" },
            match kind {
                NodeKind::Object => "object",
                NodeKind::Array => "array",
                NodeKind::Property => "property",
                NodeKind::String => "string",
                NodeKind::Number => "number",
                NodeKind::Boolean => "boolean",
                NodeKind::Null => "null",
            }
        )),
    }
}

fn apply_edit(text: &[u16], edit: &Edit) -> Vec<u16> {
    let mut out = text[..edit.offset].to_vec();
    out.extend_from_slice(&edit.content);
    out.extend_from_slice(&text[edit.offset + edit.length..]);
    out
}

fn is_eol(text: &[u16], offset: usize) -> bool {
    matches!(text.get(offset), Some(10 | 13))
}

fn with_formatting(text: &[u16], edit: Edit, options: FormattingOptions) -> Vec<Edit> {
    let mut new_text = apply_edit(text, &edit);
    let mut begin = edit.offset;
    let mut end = edit.offset + edit.content.len();
    if edit.length == 0 || edit.content.is_empty() {
        while begin > 0 && !is_eol(&new_text, begin - 1) {
            begin -= 1;
        }
        while end < new_text.len() && !is_eol(&new_text, end) {
            end += 1;
        }
    }
    let edits = format(&new_text, Some((begin, end - begin)), options);
    for edit in edits.iter().rev() {
        new_text = apply_edit(&new_text, edit);
        begin = begin.min(edit.offset);
        end = end.max(edit.offset + edit.length);
        end = (end + edit.content.len()) - edit.length;
    }
    let edit_length = text.len() - (new_text.len() - end) - begin;
    vec![Edit { offset: begin, length: edit_length, content: new_text[begin..end].to_vec() }]
}

/// `format(documentText, range, options)` with `keepLines: false` (no final newline).
fn format(document: &[u16], range: Option<(usize, usize)>, options: FormattingOptions) -> Vec<Edit> {
    let (range_start, range_end, format_start, format_text, initial_indent_level) = match range {
        Some((offset, length)) => {
            let range_start = offset;
            let range_end = offset + length;
            let mut format_start = range_start;
            while format_start > 0 && !is_eol(document, format_start - 1) {
                format_start -= 1;
            }
            let mut end_offset = range_end;
            while end_offset < document.len() && !is_eol(document, end_offset) {
                end_offset += 1;
            }
            let text = &document[format_start..end_offset];
            (range_start, range_end, format_start, text, compute_indent_level(text, options))
        }
        None => (0, document.len(), 0, document, 0),
    };
    let eol = get_eol(document);
    let indent_value: Vec<u16> = if options.insert_spaces {
        vec![32; if options.tab_size == 0 { 4 } else { options.tab_size }]
    } else {
        vec![9]
    };
    let mut indent_level: i64 = 0;
    let mut scanner = Scanner::new(format_text);
    let mut has_error = false;
    let mut number_line_breaks = 0usize;
    let repeat = |s: &[u16], count: i64| -> Vec<u16> {
        let mut out = Vec::new();
        for _ in 0..count.max(0) {
            out.extend_from_slice(s);
        }
        out
    };
    let new_lines_and_indent = |number_line_breaks: usize, indent_level: i64| -> Vec<u16> {
        if number_line_breaks > 1 {
            let mut out = repeat(&eol, number_line_breaks as i64);
            out.extend(repeat(&indent_value, initial_indent_level as i64 + indent_level));
            return out;
        }
        let mut out = eol.clone();
        out.extend(repeat(&indent_value, initial_indent_level as i64 + indent_level));
        out
    };
    let scan_next = |scanner: &mut Scanner, number_line_breaks: &mut usize, has_error: &mut bool| -> u8 {
        let mut token = scanner.scan();
        *number_line_breaks = 0;
        while token == TRIVIA || token == LINE_BREAK {
            if token == LINE_BREAK {
                *number_line_breaks = 1;
            }
            token = scanner.scan();
        }
        *has_error = token == UNKNOWN || scanner.token_error() != SCAN_NONE;
        token
    };
    let mut edits: Vec<Edit> = Vec::new();
    let add_edit = |edits: &mut Vec<Edit>, has_error: bool, text: Vec<u16>, start: usize, end: usize| {
        if !has_error && (range.is_none() || (start < range_end && end > range_start)) && document[start..end] != text[..] {
            edits.push(Edit { offset: start, length: end - start, content: text });
        }
    };
    let mut first_token = scan_next(&mut scanner, &mut number_line_breaks, &mut has_error);
    if first_token != EOF {
        let first_token_start = scanner.token_offset() + format_start;
        let initial_indent = repeat(&indent_value, initial_indent_level as i64);
        add_edit(&mut edits, has_error, initial_indent, format_start, first_token_start);
    }
    while first_token != EOF {
        let mut first_token_end = scanner.token_offset() + scanner.token_length() + format_start;
        let mut second_token = scan_next(&mut scanner, &mut number_line_breaks, &mut has_error);
        let mut replace_content: Vec<u16> = Vec::new();
        let mut needs_line_break = false;
        while number_line_breaks == 0 && (second_token == LINE_COMMENT || second_token == BLOCK_COMMENT) {
            let comment_token_start = scanner.token_offset() + format_start;
            add_edit(&mut edits, has_error, vec![32], first_token_end, comment_token_start);
            first_token_end = scanner.token_offset() + scanner.token_length() + format_start;
            needs_line_break = second_token == LINE_COMMENT;
            replace_content = if needs_line_break { new_lines_and_indent(number_line_breaks, indent_level) } else { Vec::new() };
            second_token = scan_next(&mut scanner, &mut number_line_breaks, &mut has_error);
        }
        if second_token == CLOSE_BRACE {
            if first_token != OPEN_BRACE {
                indent_level -= 1;
            }
            if first_token != OPEN_BRACE {
                replace_content = new_lines_and_indent(number_line_breaks, indent_level);
            }
        } else if second_token == CLOSE_BRACKET {
            if first_token != OPEN_BRACKET {
                indent_level -= 1;
            }
            if first_token != OPEN_BRACKET {
                replace_content = new_lines_and_indent(number_line_breaks, indent_level);
            }
        } else {
            match first_token {
                OPEN_BRACKET | OPEN_BRACE => {
                    indent_level += 1;
                    replace_content = new_lines_and_indent(number_line_breaks, indent_level);
                }
                COMMA => replace_content = new_lines_and_indent(number_line_breaks, indent_level),
                LINE_COMMENT => replace_content = new_lines_and_indent(number_line_breaks, indent_level),
                BLOCK_COMMENT => {
                    if number_line_breaks > 0 {
                        replace_content = new_lines_and_indent(number_line_breaks, indent_level);
                    } else if !needs_line_break {
                        replace_content = vec![32];
                    }
                }
                COLON => {
                    if !needs_line_break {
                        replace_content = vec![32];
                    }
                }
                STRING_LITERAL => {
                    if second_token == COLON && !needs_line_break {
                        replace_content = Vec::new();
                    }
                }
                NULL_KEYWORD | TRUE_KEYWORD | FALSE_KEYWORD | NUMERIC_LITERAL | CLOSE_BRACE | CLOSE_BRACKET => {
                    if (second_token == LINE_COMMENT || second_token == BLOCK_COMMENT) && !needs_line_break {
                        replace_content = vec![32];
                    } else if second_token != COMMA && second_token != EOF {
                        has_error = true;
                    }
                }
                UNKNOWN => has_error = true,
                _ => {}
            }
            if number_line_breaks > 0 && (second_token == LINE_COMMENT || second_token == BLOCK_COMMENT) {
                replace_content = new_lines_and_indent(number_line_breaks, indent_level);
            }
        }
        if second_token == EOF {
            replace_content = Vec::new();
        }
        let second_token_start = scanner.token_offset() + format_start;
        add_edit(&mut edits, has_error, replace_content, first_token_end, second_token_start);
        first_token = second_token;
    }
    edits
}

fn compute_indent_level(content: &[u16], options: FormattingOptions) -> usize {
    let tab_size = if options.tab_size == 0 { 4 } else { options.tab_size };
    let mut n_chars = 0;
    for &ch in content {
        if ch == 32 {
            n_chars += 1;
        } else if ch == 9 {
            n_chars += tab_size;
        } else {
            break;
        }
    }
    n_chars / tab_size
}

fn get_eol(text: &[u16]) -> Vec<u16> {
    for (i, &ch) in text.iter().enumerate() {
        if ch == 13 {
            if text.get(i + 1) == Some(&10) {
                return vec![13, 10];
            }
            return vec![13];
        } else if ch == 10 {
            return vec![10];
        }
    }
    vec![10]
}

/// `applyEdits(text, edits)`.
pub fn apply_edits(text: &str, edits: &[Edit]) -> Result<String, String> {
    let mut units = utf16(text);
    let mut sorted = edits.to_vec();
    sorted.sort_by(|a, b| a.offset.cmp(&b.offset).then(a.length.cmp(&b.length)));
    let mut last_modified = units.len();
    for edit in sorted.iter().rev() {
        if edit.offset + edit.length <= last_modified {
            units = apply_edit(&units, edit);
        } else {
            return Err("Overlapping edit".into());
        }
        last_modified = edit.offset;
    }
    Ok(String::from_utf16_lossy(&units))
}
