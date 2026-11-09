//! JSON with comments, as `jsonc-parser` 3.3.1 reads it for config files (`parse(text, errors, { allowTrailingComma:
//! true })`, packages/core/src/config.ts): `//` and `/* */` comments, trailing commas in objects and arrays. opencode
//! rejects a document with any parse error, so [`parse`] answers `None` wherever jsonc-parser reports one.
//!
//! The value is built as jsonc-parser builds it: a repeated key keeps its first position and its last value, and
//! object keys are then iterated in JS property order (array-index keys first), as `Object.entries` does. A
//! `__proto__` key sets the prototype in JS instead of a property, so it is dropped.
//!
//! [`set_root_property`] is `applyEdits(text, modify(text, [key], value, { formattingOptions: { tabSize: 2,
//! insertSpaces: true } }))` for a property of the root object (the only edit the server makes, `Config.update`),
//! on any text, readable or not: the library's own scanner and error-tolerant `parseTree` find the property (the
//! first complete one of that name), `setProperty` makes the edit, and its formatter rewrites the whitespace of the
//! lines the edit touched (the document's first line break as EOL, two-space indent from the first line's
//! indentation, comments kept, the gaps next to an invalid token left alone). `harness/jsonc/run.sh` compares it
//! with the reference's jsonc-parser on generated documents and edits.

use opencode_protocol::Json;
use opencode_protocol::wire::Object;

/// The parsed value, or `None` when jsonc-parser reports any error (an empty document included).
pub fn parse(text: &str) -> Option<Json> {
    let mut p = Parser { s: text.as_bytes(), text, i: 0 };
    p.skip()?;
    if p.i >= p.s.len() {
        return None;
    }
    let value = p.value()?;
    p.skip()?;
    (p.i == p.s.len()).then_some(value)
}

struct Parser<'a> {
    s: &'a [u8],
    text: &'a str,
    i: usize,
}

impl Parser<'_> {
    /// Skips whitespace (space, tab, CR, LF) and comments; an unterminated block comment is an error.
    fn skip(&mut self) -> Option<()> {
        loop {
            match self.s.get(self.i) {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.i += 1,
                Some(b'/') if self.s.get(self.i + 1) == Some(&b'/') => {
                    self.i += 2;
                    while let Some(&c) = self.s.get(self.i) {
                        if c == b'\n' || c == b'\r' {
                            break;
                        }
                        self.i += 1;
                    }
                }
                Some(b'/') if self.s.get(self.i + 1) == Some(&b'*') => {
                    self.i += 2;
                    loop {
                        if self.i + 1 >= self.s.len() {
                            return None;
                        }
                        if self.s[self.i] == b'*' && self.s[self.i + 1] == b'/' {
                            self.i += 2;
                            break;
                        }
                        self.i += 1;
                    }
                }
                _ => return Some(()),
            }
        }
    }

    fn value(&mut self) -> Option<Json> {
        match self.s.get(self.i)? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => self.string().map(Json::String),
            b'-' | b'0'..=b'9' => self.number(),
            _ => {
                // A word: true, false, null, anything else is an invalid symbol.
                let start = self.i;
                while let Some(&c) = self.s.get(self.i) {
                    if matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'{' | b'}' | b'[' | b']' | b'"' | b':' | b',' | b'/') {
                        break;
                    }
                    self.i += 1;
                }
                match &self.text[start..self.i] {
                    "true" => Some(Json::Bool(true)),
                    "false" => Some(Json::Bool(false)),
                    "null" => Some(Json::Null),
                    _ => None,
                }
            }
        }
    }

    fn object(&mut self) -> Option<Json> {
        self.i += 1;
        let mut map = Object::new();
        let mut needs_comma = false;
        loop {
            self.skip()?;
            match self.s.get(self.i)? {
                b'}' => {
                    self.i += 1;
                    break;
                }
                b',' => {
                    if !needs_comma {
                        return None;
                    }
                    self.i += 1;
                    self.skip()?;
                    if self.s.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        break;
                    }
                }
                _ if needs_comma => return None,
                _ => {}
            }
            if self.s.get(self.i) != Some(&b'"') {
                return None;
            }
            let key = self.string()?;
            self.skip()?;
            if self.s.get(self.i) != Some(&b':') {
                return None;
            }
            self.i += 1;
            self.skip()?;
            let value = self.value()?;
            if key != "__proto__" {
                map.insert(key, value);
            }
            needs_comma = true;
        }
        Some(Json::Object(js_order(map)))
    }

    fn array(&mut self) -> Option<Json> {
        self.i += 1;
        let mut items = Vec::new();
        let mut needs_comma = false;
        loop {
            self.skip()?;
            match self.s.get(self.i)? {
                b']' => {
                    self.i += 1;
                    break;
                }
                b',' => {
                    if !needs_comma {
                        return None;
                    }
                    self.i += 1;
                    self.skip()?;
                    if self.s.get(self.i) == Some(&b']') {
                        self.i += 1;
                        break;
                    }
                }
                _ if needs_comma => return None,
                _ => {}
            }
            items.push(self.value()?);
            needs_comma = true;
        }
        Some(Json::Array(items))
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        if self.s[self.i] == b'-' {
            self.i += 1;
            if !self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
                return None;
            }
        }
        let digits = |p: &mut Self| {
            while p.s.get(p.i).is_some_and(u8::is_ascii_digit) {
                p.i += 1;
            }
        };
        if self.s[self.i] == b'0' {
            self.i += 1;
        } else {
            digits(self);
        }
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            digits(self);
        }
        if let Some(b'e' | b'E') = self.s.get(self.i) {
            self.i += 1;
            if let Some(b'+' | b'-') = self.s.get(self.i) {
                self.i += 1;
            }
            if !self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            digits(self);
        }
        self.text[start..self.i].parse::<f64>().ok().map(Json::Number)
    }

    fn string(&mut self) -> Option<String> {
        self.i += 1;
        let mut out = String::new();
        let mut units: Vec<u16> = Vec::new();
        let flush = |units: &mut Vec<u16>, out: &mut String| {
            if !units.is_empty() {
                // Lone surrogates are valid in a JS string; a Rust string holds their replacement.
                out.push_str(&String::from_utf16_lossy(units));
                units.clear();
            }
        };
        loop {
            let start = self.i;
            while let Some(&c) = self.s.get(self.i) {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.i += 1;
            }
            if self.i > start {
                flush(&mut units, &mut out);
                out.push_str(&self.text[start..self.i]);
            }
            match *self.s.get(self.i)? {
                b'"' => {
                    self.i += 1;
                    flush(&mut units, &mut out);
                    return Some(out);
                }
                b'\\' => {
                    self.i += 1;
                    let c = *self.s.get(self.i)?;
                    self.i += 1;
                    let simple = match c {
                        b'"' => Some('"'),
                        b'\\' => Some('\\'),
                        b'/' => Some('/'),
                        b'b' => Some('\u{8}'),
                        b'f' => Some('\u{c}'),
                        b'n' => Some('\n'),
                        b'r' => Some('\r'),
                        b't' => Some('\t'),
                        b'u' => None,
                        _ => return None,
                    };
                    match simple {
                        Some(ch) => {
                            flush(&mut units, &mut out);
                            out.push(ch);
                        }
                        None => {
                            let hex = self.text.get(self.i..self.i + 4)?;
                            let unit = u16::from_str_radix(hex, 16).ok().filter(|_| hex.bytes().all(|b| b.is_ascii_hexdigit()))?;
                            self.i += 4;
                            units.push(unit);
                        }
                    }
                }
                // A control character (a line break ends the string): an error either way.
                _ => return None,
            }
        }
    }
}

/// Reorders an object's keys as JS iterates them: array-index keys first, ascending, then the rest in insertion order.
pub fn js_order(map: Object) -> Object {
    if !map.keys().any(|k| opencode_protocol::wire::out::array_index(k).is_some()) {
        return map;
    }
    let mut indexed: Vec<(u32, String, Json)> = Vec::new();
    let mut rest = Object::new();
    for (k, v) in map {
        match opencode_protocol::wire::out::array_index(&k) {
            Some(n) => indexed.push((n, k, v)),
            None => {
                rest.insert(k, v);
            }
        }
    }
    indexed.sort_by_key(|(n, _, _)| *n);
    let mut out = Object::new();
    for (_, k, v) in indexed {
        out.insert(k, v);
    }
    out.extend(rest);
    out
}

/// `applyEdits(text, modify(text, [key], value, { formattingOptions: { tabSize: 2, insertSpaces: true } }))`: sets
/// (or with `None` removes) a property of the root object, keeping the rest of the text. `None` where jsonc-parser
/// throws: deleting from a document without a value, a root value that is not an object.
pub fn set_root_property(text: &str, key: &str, value: Option<&Json>) -> Option<String> {
    let edit = edit::set_property(text, key, value)?;
    Some(match edit {
        Some(edit) => edit::apply_edit(text, &edit),
        None => text.to_owned(),
    })
}

/// jsonc-parser 3.3.1's edit path (`impl/scanner.ts`, `parseTree` and `visit` in `impl/parser.ts`, `setProperty` and
/// `withFormatting` in `impl/edit.ts`, `impl/format.ts`), followed step by step. Offsets are bytes where the library
/// counts UTF-16 units: every offset it computes is a token boundary, which is an ASCII character or the end of the
/// text, so the two agree.
mod edit {
    use opencode_protocol::Json;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Token {
        OpenBrace,
        CloseBrace,
        OpenBracket,
        CloseBracket,
        Comma,
        Colon,
        Null,
        True,
        False,
        Str,
        Number,
        LineComment,
        BlockComment,
        LineBreak,
        Trivia,
        Unknown,
        Eof,
    }

    fn is_break(c: u8) -> bool {
        c == b'\n' || c == b'\r'
    }

    /// `isUnknownContentCharacter`: what a word (`true`, or an invalid symbol) is made of.
    fn is_content(c: u8) -> bool {
        !matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'}' | b']' | b'{' | b'[' | b'"' | b':' | b',' | b'/')
    }

    /// `createScanner(text, false)`. The value is kept for strings only (a property's name).
    struct Scanner<'a> {
        s: &'a [u8],
        pos: usize,
        offset: usize,
        token: Token,
        error: bool,
        value: Vec<u16>,
    }

    impl<'a> Scanner<'a> {
        fn new(s: &'a [u8]) -> Self {
            Scanner { s, pos: 0, offset: 0, token: Token::Unknown, error: false, value: Vec::new() }
        }

        fn at(&self, i: usize) -> Option<u8> {
            self.s.get(i).copied()
        }

        fn digit_at(&self, i: usize) -> bool {
            self.at(i).is_some_and(|c| c.is_ascii_digit())
        }

        fn length(&self) -> usize {
            self.pos - self.offset
        }

        fn scan(&mut self) -> Token {
            self.token = self.next();
            self.token
        }

        fn next(&mut self) -> Token {
            self.value.clear();
            self.error = false;
            self.offset = self.pos;
            let len = self.s.len();
            if self.pos >= len {
                self.offset = len;
                return Token::Eof;
            }
            let code = self.s[self.pos];
            if code == b' ' || code == b'\t' {
                while matches!(self.at(self.pos), Some(b' ' | b'\t')) {
                    self.pos += 1;
                }
                return Token::Trivia;
            }
            if is_break(code) {
                self.pos += 1;
                if code == b'\r' && self.at(self.pos) == Some(b'\n') {
                    self.pos += 1;
                }
                return Token::LineBreak;
            }
            let single = match code {
                b'{' => Some(Token::OpenBrace),
                b'}' => Some(Token::CloseBrace),
                b'[' => Some(Token::OpenBracket),
                b']' => Some(Token::CloseBracket),
                b':' => Some(Token::Colon),
                b',' => Some(Token::Comma),
                _ => None,
            };
            if let Some(token) = single {
                self.pos += 1;
                return token;
            }
            match code {
                b'"' => {
                    self.pos += 1;
                    self.string();
                    Token::Str
                }
                b'/' => {
                    if self.at(self.pos + 1) == Some(b'/') {
                        self.pos += 2;
                        while self.pos < len && !is_break(self.s[self.pos]) {
                            self.pos += 1;
                        }
                        return Token::LineComment;
                    }
                    if self.at(self.pos + 1) == Some(b'*') {
                        self.pos += 2;
                        let safe = len - 1;
                        let mut closed = false;
                        while self.pos < safe {
                            let c = self.s[self.pos];
                            if c == b'*' && self.s[self.pos + 1] == b'/' {
                                self.pos += 2;
                                closed = true;
                                break;
                            }
                            self.pos += 1;
                            if c == b'\r' && self.at(self.pos) == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        if !closed {
                            // One past the end, as the library has it.
                            self.pos += 1;
                            self.error = true;
                        }
                        return Token::BlockComment;
                    }
                    self.pos += 1;
                    Token::Unknown
                }
                b'-' => {
                    self.pos += 1;
                    if !self.digit_at(self.pos) {
                        return Token::Unknown;
                    }
                    self.number();
                    Token::Number
                }
                b'0'..=b'9' => {
                    self.number();
                    Token::Number
                }
                _ => {
                    while self.pos < len && is_content(self.s[self.pos]) {
                        self.pos += 1;
                    }
                    if self.offset == self.pos {
                        self.pos += 1;
                        return Token::Unknown;
                    }
                    match &self.s[self.offset..self.pos] {
                        b"true" => Token::True,
                        b"false" => Token::False,
                        b"null" => Token::Null,
                        _ => Token::Unknown,
                    }
                }
            }
        }

        /// `scanNumber`, from its first digit.
        fn number(&mut self) {
            self.pos += 1;
            if self.s[self.pos - 1] != b'0' {
                while self.digit_at(self.pos) {
                    self.pos += 1;
                }
            }
            if self.at(self.pos) == Some(b'.') {
                self.pos += 1;
                if !self.digit_at(self.pos) {
                    self.error = true;
                    return;
                }
                while self.digit_at(self.pos) {
                    self.pos += 1;
                }
            }
            if matches!(self.at(self.pos), Some(b'e' | b'E')) {
                self.pos += 1;
                if matches!(self.at(self.pos), Some(b'+' | b'-')) {
                    self.pos += 1;
                }
                if self.digit_at(self.pos) {
                    while self.digit_at(self.pos) {
                        self.pos += 1;
                    }
                } else {
                    self.error = true;
                }
            }
        }

        fn push(&mut self, start: usize, end: usize) {
            let text = std::str::from_utf8(&self.s[start..end]).unwrap_or("");
            self.value.extend(text.encode_utf16());
        }

        /// `scanString`, after the opening quote.
        fn string(&mut self) {
            let len = self.s.len();
            let mut start = self.pos;
            loop {
                if self.pos >= len {
                    self.push(start, self.pos);
                    self.error = true;
                    return;
                }
                let c = self.s[self.pos];
                if c == b'"' {
                    self.push(start, self.pos);
                    self.pos += 1;
                    return;
                }
                if c == b'\\' {
                    self.push(start, self.pos);
                    self.pos += 1;
                    if self.pos >= len {
                        self.error = true;
                        return;
                    }
                    let escape = self.s[self.pos];
                    self.pos += 1;
                    let unit = match escape {
                        b'"' => Some(u16::from(b'"')),
                        b'\\' => Some(u16::from(b'\\')),
                        b'/' => Some(u16::from(b'/')),
                        b'b' => Some(8),
                        b'f' => Some(12),
                        b'n' => Some(10),
                        b'r' => Some(13),
                        b't' => Some(9),
                        b'u' => {
                            let mut value: u32 = 0;
                            let mut digits = 0;
                            while digits < 4 {
                                let Some(d) = self.at(self.pos).and_then(|c| (c as char).to_digit(16)) else { break };
                                value = value * 16 + d;
                                self.pos += 1;
                                digits += 1;
                            }
                            if digits < 4 {
                                self.error = true;
                                None
                            } else {
                                Some(value as u16)
                            }
                        }
                        _ => {
                            self.error = true;
                            None
                        }
                    };
                    self.value.extend(unit);
                    start = self.pos;
                    continue;
                }
                if c <= 0x1f {
                    if is_break(c) {
                        self.push(start, self.pos);
                        self.error = true;
                        return;
                    }
                    self.error = true;
                }
                self.pos += 1;
            }
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Object,
        Array,
        Property,
        Str,
        Literal,
    }

    struct Node {
        kind: Kind,
        offset: i64,
        length: i64,
        parent: usize,
        children: Vec<usize>,
        /// A string's value (UTF-16, as JS compares it).
        value: Vec<u16>,
    }

    /// `parseTree(text, errors)` with the default options (no trailing commas). The errors themselves are not
    /// needed: only the tree the visitor builds around them.
    struct Tree<'a> {
        sc: Scanner<'a>,
        nodes: Vec<Node>,
        current: usize,
    }

    impl Tree<'_> {
        fn parse(text: &str) -> Tree<'_> {
            // The artificial root (an array) holds the document's value.
            let root = Node { kind: Kind::Array, offset: -1, length: -1, parent: 0, children: Vec::new(), value: Vec::new() };
            let mut tree = Tree { sc: Scanner::new(text.as_bytes()), nodes: vec![root], current: 0 };
            tree.scan_next();
            if tree.sc.token != Token::Eof {
                tree.value();
            }
            tree
        }

        /// The document's value.
        fn root(&self) -> Option<usize> {
            self.nodes[0].children.first().copied()
        }

        fn token_offset(&self) -> i64 {
            self.sc.offset as i64
        }

        fn token_end(&self) -> i64 {
            self.sc.pos as i64
        }

        fn add(&mut self, kind: Kind, offset: i64, length: i64) -> usize {
            let index = self.nodes.len();
            let value = if kind == Kind::Str { self.sc.value.clone() } else { Vec::new() };
            self.nodes.push(Node { kind, offset, length, parent: self.current, children: Vec::new(), value });
            self.nodes[self.current].children.push(index);
            index
        }

        fn ensure_property_complete(&mut self, end: i64) {
            if self.nodes[self.current].kind == Kind::Property {
                let node = &mut self.nodes[self.current];
                node.length = end - node.offset;
                self.current = node.parent;
            }
        }

        fn begin(&mut self, kind: Kind) {
            self.current = self.add(kind, self.token_offset(), -1);
        }

        fn end(&mut self, object: bool) {
            let end = self.token_end();
            if object {
                self.ensure_property_complete(end);
            }
            let node = &mut self.nodes[self.current];
            node.length = end - node.offset;
            self.current = node.parent;
            self.ensure_property_complete(end);
        }

        fn literal(&mut self, kind: Kind) {
            self.add(kind, self.token_offset(), self.token_end() - self.token_offset());
            self.ensure_property_complete(self.token_end());
        }

        fn property(&mut self) {
            let (offset, length) = (self.token_offset(), self.token_end() - self.token_offset());
            self.current = self.add(Kind::Property, offset, -1);
            self.add(Kind::Str, offset, length);
        }

        fn comma(&mut self) {
            if self.nodes[self.current].kind == Kind::Property {
                self.ensure_property_complete(self.token_offset());
            }
        }

        /// `scanNext`: the next token that is not trivia, a comment or an invalid symbol.
        fn scan_next(&mut self) -> Token {
            loop {
                match self.sc.scan() {
                    Token::LineComment | Token::BlockComment | Token::Unknown | Token::Trivia | Token::LineBreak => {}
                    token => return token,
                }
            }
        }

        /// `handleError`'s recovery: skip to (or past) one of the tokens.
        fn skip(&mut self, after: &[Token], until: &[Token]) {
            let mut token = self.sc.token;
            while token != Token::Eof {
                if after.contains(&token) {
                    self.scan_next();
                    break;
                } else if until.contains(&token) {
                    break;
                }
                token = self.scan_next();
            }
        }

        fn value(&mut self) -> bool {
            match self.sc.token {
                Token::OpenBracket => self.array(),
                Token::OpenBrace => self.object(),
                Token::Str => {
                    self.literal(Kind::Str);
                    self.scan_next();
                    true
                }
                Token::Number | Token::Null | Token::True | Token::False => {
                    self.literal(Kind::Literal);
                    self.scan_next();
                    true
                }
                _ => false,
            }
        }

        fn parse_property(&mut self) -> bool {
            if self.sc.token != Token::Str {
                self.skip(&[], &[Token::CloseBrace, Token::Comma]);
                return false;
            }
            self.property();
            self.scan_next();
            if self.sc.token == Token::Colon {
                self.scan_next();
                if !self.value() {
                    self.skip(&[], &[Token::CloseBrace, Token::Comma]);
                }
            } else {
                self.skip(&[], &[Token::CloseBrace, Token::Comma]);
            }
            true
        }

        fn object(&mut self) -> bool {
            self.begin(Kind::Object);
            self.scan_next();
            while self.sc.token != Token::CloseBrace && self.sc.token != Token::Eof {
                if self.sc.token == Token::Comma {
                    self.comma();
                    self.scan_next();
                }
                if !self.parse_property() {
                    self.skip(&[], &[Token::CloseBrace, Token::Comma]);
                }
            }
            self.end(true);
            if self.sc.token != Token::CloseBrace {
                self.skip(&[Token::CloseBrace], &[]);
            } else {
                self.scan_next();
            }
            true
        }

        fn array(&mut self) -> bool {
            self.begin(Kind::Array);
            self.scan_next();
            while self.sc.token != Token::CloseBracket && self.sc.token != Token::Eof {
                if self.sc.token == Token::Comma {
                    self.comma();
                    self.scan_next();
                }
                if !self.value() {
                    self.skip(&[], &[Token::CloseBracket, Token::Comma]);
                }
            }
            self.end(false);
            if self.sc.token != Token::CloseBracket {
                self.skip(&[Token::CloseBracket], &[]);
            } else {
                self.scan_next();
            }
            true
        }

        /// `findNodeAtLocation(object, [key])`: the value of the first complete property named `key`.
        fn find(&self, object: usize, key: &[u16]) -> Option<usize> {
            if self.nodes[object].kind != Kind::Object {
                return None;
            }
            self.nodes[object].children.iter().find_map(|&property| {
                let children = &self.nodes[property].children;
                (children.len() == 2 && self.nodes[children[0]].value == key).then(|| children[1])
            })
        }
    }

    /// One edit: replace `length` units at `offset` with `content`.
    pub(super) struct Edit {
        offset: i64,
        length: i64,
        content: String,
    }

    /// `text.substring(start, end)`: clamped to the text, the bounds swapped when reversed.
    fn substring(text: &str, start: i64, end: i64) -> &str {
        let clamp = |n: i64| n.clamp(0, text.len() as i64) as usize;
        let (a, b) = (clamp(start), clamp(end));
        let (a, b) = if a > b { (b, a) } else { (a, b) };
        text.get(a..b).unwrap_or("")
    }

    /// `applyEdit`.
    pub(super) fn apply_edit(text: &str, edit: &Edit) -> String {
        let tail = substring(text, edit.offset + edit.length, text.len() as i64);
        format!("{}{}{}", substring(text, 0, edit.offset), edit.content, tail)
    }

    /// `setProperty(text, [key], value, options)`: `None` where it throws, `Some(None)` for no edit.
    pub(super) fn set_property(text: &str, key: &str, value: Option<&Json>) -> Option<Option<Edit>> {
        let tree = Tree::parse(text);
        let key16: Vec<u16> = key.encode_utf16().collect();
        let encoded_key = crate::json::text(&Json::String(key.to_owned()));
        let Some(parent) = tree.root() else {
            // No value in the document: the property becomes one, wrapped in an object.
            let value = crate::json::text(value?);
            return Some(Some(with_formatting(text, Edit { offset: 0, length: 0, content: format!("{{{encoded_key}:{value}}}") })));
        };
        let node = |index: usize| &tree.nodes[index];
        if node(parent).kind != Kind::Object {
            // "Can not add index to parent of type ..."
            return None;
        }
        let children = &node(parent).children;
        let edit = match (tree.find(parent, &key16), value) {
            (Some(existing), None) => {
                let property = node(existing).parent;
                let index = children.iter().position(|&c| c == property)?;
                let mut end = node(property).offset + node(property).length;
                let begin = if index > 0 {
                    let previous = node(children[index - 1]);
                    previous.offset + previous.length
                } else {
                    if children.len() > 1 {
                        end = node(children[1]).offset;
                    }
                    node(parent).offset + 1
                };
                Edit { offset: begin, length: end - begin, content: String::new() }
            }
            (Some(existing), Some(value)) => {
                Edit { offset: node(existing).offset, length: node(existing).length, content: crate::json::text(value) }
            }
            (None, None) => return Some(None),
            (None, Some(value)) => {
                let property = format!("{encoded_key}: {}", crate::json::text(value));
                match children.last() {
                    Some(&previous) => {
                        let previous = node(previous);
                        Edit { offset: previous.offset + previous.length, length: 0, content: format!(",{property}") }
                    }
                    None => Edit { offset: node(parent).offset + 1, length: 0, content: property },
                }
            }
        };
        Some(Some(with_formatting(text, edit)))
    }

    /// `isEOL(text, offset)`; past the end `charAt` is `""`, which `"\r\n".indexOf` finds.
    fn is_eol(text: &[u8], offset: i64) -> bool {
        usize::try_from(offset).ok().and_then(|i| text.get(i)).is_none_or(|&c| is_break(c))
    }

    /// `withFormatting`: the edit applied, the lines it touched formatted, and both as one edit.
    fn with_formatting(text: &str, edit: Edit) -> Edit {
        let mut new_text = apply_edit(text, &edit);
        let mut begin = edit.offset;
        let mut end = edit.offset + edit.content.len() as i64;
        if edit.length == 0 || edit.content.is_empty() {
            let bytes = new_text.as_bytes();
            while begin > 0 && !is_eol(bytes, begin - 1) {
                begin -= 1;
            }
            while end < bytes.len() as i64 && !is_eol(bytes, end) {
                end += 1;
            }
        }
        let edits = format(&new_text, begin, end - begin);
        for edit in edits.iter().rev() {
            new_text = apply_edit(&new_text, edit);
            begin = begin.min(edit.offset);
            end = end.max(edit.offset + edit.length);
            end += edit.content.len() as i64 - edit.length;
        }
        let length = text.len() as i64 - (new_text.len() as i64 - end) - begin;
        Edit { offset: begin, length, content: substring(&new_text, begin, end).to_owned() }
    }

    /// `getEOL`: the document's first line break, else `\n`.
    fn eol(text: &[u8]) -> &'static str {
        for (i, &c) in text.iter().enumerate() {
            if c == b'\r' {
                return if text.get(i + 1) == Some(&b'\n') { "\r\n" } else { "\r" };
            }
            if c == b'\n' {
                return "\n";
            }
        }
        "\n"
    }

    /// `format(text, range, { tabSize: 2, insertSpaces: true, keepLines: false })`.
    fn format(document: &str, offset: i64, length: i64) -> Vec<Edit> {
        let doc = document.as_bytes();
        let (range_start, range_end) = (offset, offset + length);
        let mut start = range_start;
        while start > 0 && !is_eol(doc, start - 1) {
            start -= 1;
        }
        let mut stop = range_end;
        while stop < doc.len() as i64 && !is_eol(doc, stop) {
            stop += 1;
        }
        let text = substring(document, start, stop).as_bytes();
        // `computeIndentLevel`: the first line's leading spaces and tabs (a tab counts as tabSize).
        let leading: i64 = text.iter().map_while(|&c| match c {
            b' ' => Some(1),
            b'\t' => Some(2),
            _ => None,
        }).sum();
        let initial = leading / 2;
        let eol = eol(doc);
        let mut f = Formatter { sc: Scanner::new(text), base: start, line_breaks: 0, has_error: false, edits: Vec::new() };
        let mut indent: i64 = 0;
        let newline = |indent: i64| format!("{eol}{}", " ".repeat((2 * (initial + indent)).max(0) as usize));
        let add = |f: &mut Formatter, content: &str, from: i64, to: i64| {
            if !f.has_error && from < range_end && to > range_start && substring(document, from, to) != content {
                f.edits.push(Edit { offset: from, length: to - from, content: content.to_owned() });
            }
        };
        let mut first = f.scan_next();
        if first != Token::Eof {
            let token_start = f.token_start();
            add(&mut f, &" ".repeat(2 * initial as usize), start, token_start);
        }
        while first != Token::Eof {
            let mut first_end = f.token_end();
            let mut second = f.scan_next();
            let mut replace = String::new();
            let mut needs_line_break = false;
            while f.line_breaks == 0 && matches!(second, Token::LineComment | Token::BlockComment) {
                let comment_start = f.token_start();
                add(&mut f, " ", first_end, comment_start);
                first_end = f.token_end();
                needs_line_break = second == Token::LineComment;
                replace = if needs_line_break { newline(indent) } else { String::new() };
                second = f.scan_next();
            }
            let comment = matches!(second, Token::LineComment | Token::BlockComment);
            if second == Token::CloseBrace || second == Token::CloseBracket {
                let open = if second == Token::CloseBrace { Token::OpenBrace } else { Token::OpenBracket };
                if first != open {
                    indent -= 1;
                    replace = newline(indent);
                }
            } else {
                match first {
                    Token::OpenBracket | Token::OpenBrace => {
                        indent += 1;
                        replace = newline(indent);
                    }
                    Token::Comma | Token::LineComment => replace = newline(indent),
                    Token::BlockComment => {
                        if f.line_breaks > 0 {
                            replace = newline(indent);
                        } else if !needs_line_break {
                            replace = " ".into();
                        }
                    }
                    Token::Colon => {
                        if !needs_line_break {
                            replace = " ".into();
                        }
                    }
                    Token::Str => {
                        if second == Token::Colon && !needs_line_break {
                            replace = String::new();
                        }
                    }
                    Token::Null | Token::True | Token::False | Token::Number | Token::CloseBrace | Token::CloseBracket => {
                        if comment && !needs_line_break {
                            replace = " ".into();
                        } else if second != Token::Comma && second != Token::Eof {
                            f.has_error = true;
                        }
                    }
                    Token::Unknown => f.has_error = true,
                    _ => {}
                }
                if f.line_breaks > 0 && comment {
                    replace = newline(indent);
                }
            }
            if second == Token::Eof {
                replace = String::new();
            }
            let second_start = f.token_start();
            add(&mut f, &replace, first_end, second_start);
            first = second;
        }
        f.edits
    }

    struct Formatter<'a> {
        sc: Scanner<'a>,
        /// Where the formatted text starts in the document.
        base: i64,
        line_breaks: u32,
        has_error: bool,
        edits: Vec<Edit>,
    }

    impl Formatter<'_> {
        /// The formatter's `scanNext`: trivia and line breaks skipped (and counted), comments kept.
        fn scan_next(&mut self) -> Token {
            let mut token = self.sc.scan();
            self.line_breaks = 0;
            while matches!(token, Token::Trivia | Token::LineBreak) {
                if token == Token::LineBreak {
                    self.line_breaks = 1;
                }
                token = self.sc.scan();
            }
            self.has_error = token == Token::Unknown || self.sc.error;
            token
        }

        fn token_start(&self) -> i64 {
            self.sc.offset as i64 + self.base
        }

        fn token_end(&self) -> i64 {
            (self.sc.offset + self.sc.length()) as i64 + self.base
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(v: &Option<Json>) -> Option<String> {
        v.as_ref().map(crate::json::text)
    }

    #[test]
    fn accepts_comments_and_trailing_commas() {
        let doc = "// comments are allowed\n{\n  \"username\": \"from-jsonc\",\n  \"timezone\": \"Europe/Paris\", // trailing comment\n}\n";
        assert_eq!(text(&parse(doc)).as_deref(), Some(r#"{"username":"from-jsonc","timezone":"Europe/Paris"}"#));
        assert_eq!(text(&parse("{ \"a\": [1, 2,], /* x */ }")).as_deref(), Some(r#"{"a":[1,2]}"#));
        assert_eq!(text(&parse("{\"a\":1,\"a\":2,\"b\":3}")).as_deref(), Some(r#"{"a":2,"b":3}"#));
        assert_eq!(text(&parse("{\"b\":1,\"2\":2,\"1\":3}")).as_deref(), Some(r#"{"1":3,"2":2,"b":1}"#));
    }

    #[test]
    fn rejects_what_jsonc_parser_reports() {
        for doc in ["", "  ", "{ \"username\": \"broken\", ", "{,}", "[,]", "[1,,2]", "{\"a\" 1}", "01", "1.", "-", "tru", "{} x", "/* open", "{'a':1}", "\"a\nb\""] {
            assert!(parse(doc).is_none(), "{doc:?}");
        }
        assert!(parse("[1, 2, 3]").is_some());
        assert!(parse("\"just a string\"").is_some());
    }

    /// Recorded from jsonc-parser 3.3.1 (`applyEdits(text, modify(text, ["shell"], value, { formattingOptions: {
    /// tabSize: 2, insertSpaces: true } }))`).
    #[test]
    fn edits_root_properties() {
        let set = |text: &str, value: Option<&str>| set_root_property(text, "shell", value.map(|v| Json::String(v.into())).as_ref());
        assert_eq!(set("{}\n", Some("/bin/sh")).as_deref(), Some("{\n  \"shell\": \"/bin/sh\"\n}\n"));
        assert_eq!(set("{\n  \"shell\": \"/bin/sh\"\n}\n", Some("/bin/bash")).as_deref(), Some("{\n  \"shell\": \"/bin/bash\"\n}\n"));
        assert_eq!(set("{\n  \"shell\": \"/bin/sh\"\n}\n", None).as_deref(), Some("{\n}\n"));
        assert_eq!(set("{\"a\": 1}", Some("x")).as_deref(), Some("{\n  \"a\": 1,\n  \"shell\": \"x\"\n}"));
        assert_eq!(set("{}\n", None).as_deref(), Some("{}\n"));
        assert_eq!(set("[1]", Some("x")), None);
        assert_eq!(set("", Some("x")).as_deref(), Some("{\n  \"shell\": \"x\"\n}"));
        assert_eq!(set("", None), None);
        assert_eq!(set("garbage", Some("x")).as_deref(), Some("{\n  \"shell\": \"x\"\n}garbage"));
        assert_eq!(set("// c\n{\n\t\"a\": 1, // x\n}\n", Some("y")).as_deref(), Some("// c\n{\n  \"a\": 1,\n  \"shell\": \"y\", // x\n}\n"));
        assert_eq!(set("{\"shell\":1,\"shell\":2}", Some("z")).as_deref(), Some("{\"shell\":\"z\",\"shell\":2}"));
        assert_eq!(set("{\"a\":1,\"shell\":2}", None).as_deref(), Some("{\n  \"a\": 1\n}"));
    }

    /// The differential check against jsonc-parser (`harness/jsonc/run.sh`): `JSONC_CASES` names the cases and
    /// what the library made of them.
    #[test]
    #[ignore]
    fn recorded_edits() {
        use crate::json;
        let path = std::env::var("JSONC_CASES").expect("JSONC_CASES names the recorded cases");
        let cases = json::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
        let cases = json::array(&cases).unwrap();
        let mut failures = 0;
        for (index, case) in cases.iter().enumerate() {
            let text = json::get_str(case, "text").unwrap();
            let key = json::get_str(case, "key").unwrap();
            let value = json::get(case, "value");
            let expected = json::get(case, "result").and_then(json::as_str);
            let actual = set_root_property(text, key, value);
            if actual.as_deref() != expected {
                failures += 1;
                if failures <= 10 {
                    let value = value.map(json::text);
                    eprintln!("case {index}: {text:?} {key:?} {value:?}\n  expected {expected:?}\n  actual   {actual:?}");
                }
            }
        }
        eprintln!("{} cases, {failures} differences", cases.len());
        assert_eq!(failures, 0);
    }
}
