//! html-markdown.ts `convertHTMLToMarkdown`, statement for statement.
//!
//! The TS frame objects that are compared by identity (`link`, `code`, `marker`, ...) live in arenas and are referred
//! to by index. The output array is a list of chunks over one byte buffer; a chunk is a plain string (possibly
//! with spaces prepended by `flushSpace`) or a `{ raw }` chunk that the final normalization leaves alone.
//! `outputBytes` is kept exactly as TS keeps it, drift included (it is not always the size of the output).

use crate::MAX_MARKDOWN_BYTES;
use crate::js::{self, JsString};
use crate::parser::{Attributes, Handler, Parser, Writes};

const MAX: i64 = MAX_MARKDOWN_BYTES as i64;
const CONTENT_BYTES: i64 = MAX - 64 * 1024;

fn is_omitted(name: &str) -> bool {
    matches!(name, "script" | "style" | "noscript" | "iframe" | "object" | "embed" | "meta" | "link" | "template")
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "details"
            | "dialog"
            | "div"
            | "dl"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "header"
            | "main"
            | "nav"
            | "p"
            | "section"
            | "summary"
    )
}

fn heading_level(name: &str) -> Option<usize> {
    let b = name.as_bytes();
    (b.len() == 2 && b[0] == b'h' && (b'1'..=b'6').contains(&b[1])).then(|| (b[1] - b'0') as usize)
}

fn inline_marker(name: &str) -> Option<&'static str> {
    match name {
        "strong" | "b" => Some("**"),
        "em" | "i" => Some("*"),
        "s" | "strike" | "del" => Some("~~"),
        _ => None,
    }
}

struct Link {
    href: String,
    title: Option<String>,
}

struct Code {
    inline: bool,
    text: JsString,
    language: Option<String>,
}

struct Marker {
    index: i64,
    block: u64,
    leading_space: bool,
    previous: Option<usize>,
}

struct List {
    ordered: bool,
    next: f64,
    previous: Option<usize>,
}

struct Item {
    indent: usize,
    previous: Option<usize>,
}

struct Table {
    start: usize,
    rows: Vec<Vec<JsString>>,
    row: Option<Vec<JsString>>,
    caption: Option<JsString>,
    fallback: bool,
    previous: Option<usize>,
}

struct Details {
    open: bool,
    summary: bool,
    previous: Option<usize>,
}

#[derive(Default)]
struct Frame {
    suppressed: bool,
    link: Option<usize>,
    suspended_link: Option<usize>,
    marker: Option<usize>,
    code: Option<usize>,
    link_code: Option<usize>,
    resumed_code: Option<usize>,
    list: Option<usize>,
    item: Option<usize>,
    table: Option<usize>,
    cell: Option<usize>,
    caption: Option<usize>,
    details: Option<usize>,
}

#[derive(Clone, Copy)]
struct Chunk {
    start: usize,
    end: usize,
    /// Spaces `flushSpace` put in front.
    lead: usize,
    raw: bool,
}

/// The `output` array.
#[derive(Default)]
struct Output {
    buf: Vec<u8>,
    chunks: Vec<Chunk>,
}

impl Output {
    fn len(&self) -> usize {
        self.chunks.len()
    }

    fn push(&mut self, value: &[u8]) {
        let start = self.buf.len();
        self.buf.extend_from_slice(value);
        self.chunks.push(Chunk { start, end: self.buf.len(), lead: 0, raw: false });
    }

    fn write_chunk(&self, chunk: Chunk, out: &mut JsString) {
        if chunk.lead > 0 {
            out.extend(std::iter::repeat_n(b' ', chunk.lead));
        }
        js::push(out, &self.buf[chunk.start..chunk.end]);
    }

    /// `output.splice(start)`, joined.
    fn take(&mut self, start: usize) -> JsString {
        let mut value = Vec::new();
        if start >= self.chunks.len() {
            return value;
        }
        for &chunk in &self.chunks[start..] {
            self.write_chunk(chunk, &mut value);
        }
        self.buf.truncate(self.chunks[start].start);
        self.chunks.truncate(start);
        value
    }
}

struct Markdown {
    output: Output,
    stack: Vec<Frame>,
    pending_space: bool,
    /// Spaces only (it is always an item's indent or empty).
    pending_indent: usize,
    /// The last UTF-16 unit appended, as its last byte (only compared with `" "` and `"\n"`); `None` is `""`.
    last: Option<u8>,
    quote_depth: i64,
    needs_quote_prefix: bool,
    block_count: u64,
    depth: i64,
    stopped: bool,
    output_bytes: i64,
    active_code: Option<usize>,
    active_link: Option<usize>,
    link_open: bool,
    active_marker: Option<usize>,
    active_list: Option<usize>,
    active_item: Option<usize>,
    active_table: Option<usize>,
    active_cell: bool,
    table_depth: i64,
    fallback_suppressed_depth: i64,
    fallback_omitted_depth: i64,
    active_details: Option<usize>,

    links: Vec<Link>,
    codes: Vec<Code>,
    markers: Vec<Marker>,
    lists: Vec<List>,
    items: Vec<Item>,
    tables: Vec<Table>,
    details: Vec<Details>,
}

fn spaces(n: usize) -> Vec<u8> {
    vec![b' '; n]
}

/// Replaces each run of the given ASCII bytes with `with`.
fn collapse(value: &[u8], set: &[u8], with: &[u8]) -> JsString {
    let mut out = Vec::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        if set.contains(&value[i]) {
            while i < value.len() && set.contains(&value[i]) {
                i += 1;
            }
            out.extend_from_slice(with);
        } else {
            out.push(value[i]);
            i += 1;
        }
    }
    out
}

/// `destination(value)`.
fn destination(value: &str) -> JsString {
    let mut escaped = Vec::with_capacity(value.len());
    for &b in value.as_bytes() {
        if matches!(b, b'\\' | b'(' | b')') {
            escaped.push(b'\\');
        }
        escaped.push(b);
    }
    collapse(&escaped, b"\t\n\r ", b"%20")
}

/// `title(value)`.
fn title(value: Option<&str>) -> JsString {
    let Some(value) = value.filter(|v| !v.is_empty()) else { return Vec::new() };
    let collapsed = collapse(value.as_bytes(), b"\t\n\r ", b" ");
    let mut out = b" \"".to_vec();
    for &b in js::trim(&collapsed) {
        if b == b'\\' || b == b'"' {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b'"');
    out
}

/// The escaping `text()` applies to a word.
fn escape(part: &[u8]) -> JsString {
    let mut out = Vec::with_capacity(part.len() + 2);
    if matches!(part[0], b'#' | b'+' | b'-') {
        out.push(b'\\');
    }
    let digits = part.iter().take_while(|b| b.is_ascii_digit()).count();
    let numbered = digits > 0 && part.get(digits) == Some(&b'.');
    for (i, &b) in part.iter().enumerate() {
        if matches!(b, b'\\' | b'`' | b'*' | b'_' | b'[' | b']' | b'<' | b'>' | b'|' | b'~') || (numbered && i == digits) {
            out.push(b'\\');
        }
        out.push(b);
    }
    out
}

/// `/^[ `]|[ `]$/.test(payload) && !/^ +$/.test(payload)`.
fn needs_padding(payload: &[u8]) -> bool {
    let edge = |b: u8| b == b' ' || b == b'`';
    (edge(payload[0]) || edge(payload[payload.len() - 1])) && !payload.iter().all(|&b| b == b' ')
}

/// `candidate.replace(/^/gm, quote)`: the quote at the start and after every line terminator.
fn quote_lines(candidate: &[u8], quote: &[u8]) -> JsString {
    let mut out = Vec::with_capacity(candidate.len() + quote.len() * 8);
    out.extend_from_slice(quote);
    let mut i = 0;
    while i < candidate.len() {
        let b = candidate[i];
        if b == b'\n' || b == b'\r' {
            out.push(b);
            out.extend_from_slice(quote);
            i += 1;
        } else if b == 0xe2 && candidate.get(i + 1) == Some(&0x80) && matches!(candidate.get(i + 2), Some(0xa8 | 0xa9)) {
            out.extend_from_slice(&candidate[i..i + 3]);
            out.extend_from_slice(quote);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    out
}

/// `/(?:language-|lang-)([^\s]+)/` on a class attribute: the first capture.
fn code_language(class: &str) -> Option<String> {
    let s = class.as_bytes();
    let mut i = 0;
    while i < s.len() {
        for prefix in [&b"language-"[..], b"lang-"] {
            if s[i..].starts_with(prefix) {
                let start = i + prefix.len();
                let mut end = start;
                while end < s.len() {
                    let (cp, len) = js::decode_at(s, end);
                    if js::is_js_whitespace(cp) {
                        break;
                    }
                    end += len;
                }
                if end > start {
                    return Some(class[start..end].to_owned());
                }
            }
        }
        i += js::decode_at(s, i).1;
    }
    None
}

/// The final pass over a run of plain chunks (`flushText`).
fn normalize_text(text: &[u8], out: &mut JsString) {
    // .replace(/[ \t]+\n/g, (space) => (space.startsWith("  ") ? "  \n" : "\n"))
    let mut spaced = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let b = text[i];
        if b == b' ' || b == b'\t' {
            let start = i;
            while i < text.len() && (text[i] == b' ' || text[i] == b'\t') {
                i += 1;
            }
            if i < text.len() && text[i] == b'\n' {
                spaced.extend_from_slice(if text[start..].starts_with(b"  ") { b"  \n" } else { b"\n" });
                i += 1;
            } else {
                spaced.extend_from_slice(&text[start..i]);
            }
        } else {
            spaced.push(b);
            i += 1;
        }
    }
    // .replace(/\n{3,}/g, "\n\n")
    let mut collapsed = Vec::with_capacity(spaced.len());
    let mut i = 0;
    while i < spaced.len() {
        if spaced[i] == b'\n' {
            let start = i;
            while i < spaced.len() && spaced[i] == b'\n' {
                i += 1;
            }
            let run = i - start;
            collapsed.extend(std::iter::repeat_n(b'\n', if run >= 3 { 2 } else { run }));
        } else {
            collapsed.push(spaced[i]);
            i += 1;
        }
    }
    let lines: Vec<&[u8]> = collapsed.split(|&b| b == b'\n').collect();
    let quote_len = |line: Option<&&[u8]>| -> usize {
        let Some(line) = line else { return 0 };
        let mut n = 0;
        while line[n..].starts_with(b"> ") {
            n += 2;
        }
        n
    };
    let mut joined = Vec::with_capacity(collapsed.len());
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            joined.push(b'\n');
        }
        if !line.is_empty() {
            joined.extend_from_slice(line);
            continue;
        }
        let before = if index > 0 { quote_len(lines.get(index - 1)) } else { 0 };
        let after = quote_len(lines.get(index + 1));
        if before == 0 || after == 0 || before == after {
            continue;
        }
        let quote = "> ".repeat(before.min(after) / 2);
        joined.extend_from_slice(js::trim_end(quote.as_bytes()));
    }
    js::push(out, &joined);
}

impl Markdown {
    fn new() -> Self {
        Markdown {
            output: Output::default(),
            stack: Vec::new(),
            pending_space: false,
            pending_indent: 0,
            last: None,
            quote_depth: 0,
            needs_quote_prefix: false,
            block_count: 0,
            depth: 0,
            stopped: false,
            output_bytes: 0,
            active_code: None,
            active_link: None,
            link_open: false,
            active_marker: None,
            active_list: None,
            active_item: None,
            active_table: None,
            active_cell: false,
            table_depth: 0,
            fallback_suppressed_depth: 0,
            fallback_omitted_depth: 0,
            active_details: None,
            links: Vec::new(),
            codes: Vec::new(),
            markers: Vec::new(),
            lists: Vec::new(),
            items: Vec::new(),
            tables: Vec::new(),
            details: Vec::new(),
        }
    }

    fn top(&mut self) -> &mut Frame {
        self.stack.last_mut().expect("a frame for the open tag")
    }

    fn append(&mut self, value: &[u8], content: bool) {
        let limit = if content { CONTENT_BYTES } else { MAX };
        if value.is_empty() || self.output_bytes >= limit {
            return;
        }
        let bytes = value.len() as i64;
        let remaining = limit - self.output_bytes;
        let next = if bytes <= remaining { value } else { js::slice_bytes(value, remaining) };
        self.output.push(next);
        self.output_bytes += next.len() as i64;
        if let Some(&b) = next.last() {
            self.last = Some(b);
        }
    }

    fn append_raw(&mut self, value: &[u8]) {
        let before = self.output.len();
        self.append(value, false);
        if self.output.len() > before {
            self.output.chunks.last_mut().expect("a chunk").raw = true;
        }
    }

    fn take(&mut self, start: usize) -> JsString {
        let value = self.output.take(start);
        self.output_bytes -= value.len() as i64;
        value
    }

    fn quote_prefix(&self) -> Vec<u8> {
        // TS throws a RangeError for a negative count; the depth never goes below zero where this is called.
        b"> ".repeat(self.quote_depth.clamp(0, 8) as usize)
    }

    fn prefix_quote(&mut self) {
        if !self.needs_quote_prefix || self.quote_depth == 0 || self.active_cell {
            return;
        }
        let prefix = self.quote_prefix();
        self.append(&prefix, false);
        self.needs_quote_prefix = false;
    }

    fn flush_space(&mut self) {
        if !self.pending_space {
            return;
        }
        if let Some(m) = self.active_marker {
            let index = self.markers[m].index;
            if self.output.len() as i64 == index + 1 && self.last != Some(b' ') && self.last != Some(b'\n') {
                // `output[-1]` is a property, not an element: setting it changes no output.
                if index >= 0 {
                    let chunk = &mut self.output.chunks[index as usize];
                    if !chunk.raw {
                        chunk.lead += 1;
                    }
                }
                self.output_bytes += 1;
                self.markers[m].leading_space = true;
                self.pending_space = false;
                return;
            }
        }
        if self.last.is_some() && self.last != Some(b'\n') && self.last != Some(b' ') {
            self.append(b" ", false);
        }
        self.pending_space = false;
    }

    fn inline(&mut self, value: &[u8], open: bool, content: bool) {
        if open {
            self.flush_space();
        }
        self.prefix_quote();
        if self.pending_indent > 0 {
            let indent = spaces(self.pending_indent);
            self.append(&indent, false);
            self.pending_indent = 0;
        }
        if self.active_link.is_some() && !self.link_open {
            self.append(b"[", false);
            self.link_open = true;
        }
        self.append(value, content);
    }

    fn block(&mut self) {
        if let Some(link) = self.active_link
            && self.link_open
        {
            self.close_link(link);
            self.link_open = false;
        }
        self.pending_space = false;
        self.append(b"\n\n", false);
        self.block_count += 1;
        self.needs_quote_prefix = self.quote_depth > 0;
        self.pending_indent = self.active_item.map_or(0, |i| self.items[i].indent);
    }

    fn suspend_link(&mut self) {
        let Some(link) = self.active_link else { return };
        self.top().suspended_link = Some(link);
        self.block();
        self.active_link = None;
        self.link_open = false;
    }

    fn text(&mut self, value: &[u8]) {
        if let Some(code) = self.active_code {
            js::push(&mut self.codes[code].text, value);
            return;
        }
        let is_space = |b: u8| matches!(b, b'\t' | b'\n' | 0x0c | b'\r' | b' ');
        let mut i = 0;
        while i < value.len() {
            let start = i;
            if is_space(value[i]) {
                while i < value.len() && is_space(value[i]) {
                    i += 1;
                }
                self.pending_space = true;
            } else {
                while i < value.len() && !is_space(value[i]) {
                    i += 1;
                }
                let escaped = escape(&value[start..i]);
                self.inline(&escaped, true, true);
            }
        }
    }

    fn close_link(&mut self, link: usize) {
        let link = &self.links[link];
        let mut value = b"](".to_vec();
        value.extend_from_slice(&destination(&link.href));
        value.extend_from_slice(&title(link.title.as_deref()));
        value.push(b')');
        self.append(&value, false);
    }

    fn finish_code(&mut self, code: usize) {
        let inline = self.codes[code].inline;
        if inline && self.codes[code].text.is_empty() {
            return;
        }
        let (mut backticks, mut tildes, mut current_backticks, mut current_tildes) = (0usize, 0usize, 0usize, 0usize);
        for &b in &self.codes[code].text {
            // Every byte of a non-ASCII character resets both runs, as the character itself does.
            current_backticks = if b == b'`' { current_backticks + 1 } else { 0 };
            current_tildes = if b == b'~' { current_tildes + 1 } else { 0 };
            backticks = backticks.max(current_backticks);
            tildes = tildes.max(current_tildes);
        }
        if inline {
            let fence = b"`".repeat((backticks + 1).max(1));
            self.flush_space();
            self.prefix_quote();
            let available = (CONTENT_BYTES - self.output_bytes - fence.len() as i64 * 2).max(0);
            let text = std::mem::take(&mut self.codes[code].text);
            let mut payload = js::slice_bytes(&text, available);
            while !payload.is_empty() {
                let padding: &[u8] = if needs_padding(payload) { b" " } else { b"" };
                let bytes = payload.len() as i64;
                if bytes + padding.len() as i64 * 2 <= available {
                    let mut value = fence.clone();
                    value.extend_from_slice(padding);
                    js::push(&mut value, payload);
                    value.extend_from_slice(padding);
                    value.extend_from_slice(&fence);
                    self.append_raw(&value);
                    break;
                }
                payload = js::slice_bytes(payload, bytes - 1);
            }
            self.codes[code].text = text;
            return;
        }
        if self.active_cell {
            let text = std::mem::take(&mut self.codes[code].text);
            self.text(&text);
            self.codes[code].text = text;
            return;
        }
        let marker = if backticks <= tildes { b'`' } else { b'~' };
        let length = 3.max(if marker == b'`' { backticks } else { tildes } + 1);
        let fence = vec![marker; length];
        self.block();
        let mut prefix = fence.clone();
        if let Some(language) = &self.codes[code].language {
            prefix.extend_from_slice(language.as_bytes());
        }
        prefix.push(b'\n');
        let quote = if self.quote_depth > 0 { self.quote_prefix() } else { Vec::new() };
        let text = std::mem::take(&mut self.codes[code].text);
        let mut payload: &[u8] = &text;
        loop {
            let mut candidate = prefix.clone();
            js::push(&mut candidate, payload);
            if !payload.ends_with(b"\n") {
                candidate.push(b'\n');
            }
            candidate.extend_from_slice(&fence);
            let value = if quote.is_empty() { candidate } else { quote_lines(&candidate, &quote) };
            let value_bytes = value.len() as i64;
            if self.output_bytes + value_bytes <= CONTENT_BYTES {
                self.append_raw(&value);
                self.block();
                break;
            }
            if payload.is_empty() {
                break;
            }
            let excess = value_bytes - (CONTENT_BYTES - self.output_bytes).max(0);
            payload = js::slice_bytes(payload, (payload.len() as i64 - excess).max(0));
        }
        self.codes[code].text = text;
    }

    fn new_code(&mut self, inline: bool) -> usize {
        self.codes.push(Code { inline, text: Vec::new(), language: None });
        self.codes.len() - 1
    }

    fn new_link(&mut self, attributes: &Attributes) -> usize {
        self.links.push(Link {
            href: attributes.get("href").unwrap_or("").to_owned(),
            title: attributes.get("title").map(str::to_owned),
        });
        self.links.len() - 1
    }

    fn open_tag(&mut self, name: &str, attributes: &Attributes) {
        self.depth += 1;
        if self.depth > 10_000 {
            if self.stack.last().is_some_and(|f| f.suppressed) {
                let visible_parent = self.stack.iter().rposition(|f| !f.suppressed).map_or(-1, |i| i as i64);
                self.fallback_suppressed_depth = visible_parent + 2;
            }
            self.active_code = None;
            self.stopped = true;
        }
        if self.stopped {
            if is_omitted(name) {
                self.fallback_omitted_depth += 1;
            } else {
                self.pending_space = true;
            }
            return;
        }
        let suppressed = self.stack.last().is_some_and(|f| f.suppressed) || is_omitted(name);
        let mut frame = Frame { suppressed, ..Frame::default() };
        let hidden = attributes.has("hidden")
            || attributes.get("aria-hidden").is_some_and(|v| js::to_lower_case(v) == "true")
            || name == "head";
        let details = self.active_details;
        if hidden || details.is_some_and(|d| !self.details[d].open && !self.details[d].summary && name != "summary") {
            frame.suppressed = true;
        }
        let suppressed = frame.suppressed;
        self.stack.push(frame);
        if suppressed {
            return;
        }

        if let Some(code) = self.active_code
            && !self.codes[code].inline
        {
            if name == "br" {
                self.codes[code].text.push(b'\n');
            }
            if name == "code"
                && let Some(class) = attributes.get("class").filter(|c| !c.is_empty())
            {
                self.codes[code].language = code_language(class);
            }
            return;
        }
        if name == "details" {
            self.details.push(Details { open: attributes.has("open"), summary: false, previous: self.active_details });
            let id = self.details.len() - 1;
            self.top().details = Some(id);
            self.active_details = Some(id);
            self.block();
            return;
        }
        if name == "summary" {
            if let Some(d) = details {
                self.details[d].summary = true;
            }
            self.block();
            return;
        }
        if name == "pre" {
            self.suspend_link();
            let code = self.new_code(false);
            self.top().code = Some(code);
            self.active_code = Some(code);
            return;
        }
        if name == "code" {
            if self.active_code.is_some_and(|c| self.codes[c].inline) {
                return;
            }
            let code = self.new_code(true);
            self.top().code = Some(code);
            self.active_code = Some(code);
            return;
        }
        if let Some(level) = heading_level(name) {
            self.block();
            let mut value = vec![b'#'; level];
            value.push(b' ');
            self.inline(&value, false, false);
            return;
        }
        if is_block(name) {
            self.suspend_link();
            if name == "p" && self.last == Some(b' ') {
                return;
            }
            self.block();
            return;
        }
        if name == "br" {
            self.pending_space = false;
            self.inline(b"  \n", false, false);
            self.needs_quote_prefix = self.quote_depth > 0;
            return;
        }
        if name == "hr" {
            self.block();
            self.inline(b"---", false, false);
            self.block();
            return;
        }
        if let Some(marker) = inline_marker(name) {
            self.inline(marker.as_bytes(), true, false);
            self.markers.push(Marker {
                index: self.output.len() as i64 - 1,
                block: self.block_count,
                leading_space: false,
                previous: self.active_marker,
            });
            let id = self.markers.len() - 1;
            self.top().marker = Some(id);
            self.active_marker = Some(id);
            return;
        }
        if name == "a" {
            if let (Some(link), Some(code)) = (self.active_link, self.active_code)
                && self.codes[code].inline
            {
                let parent =
                    self.stack.iter().rposition(|c| c.link == Some(link) && c.link_code == Some(code));
                if let Some(parent) = parent {
                    self.finish_code(code);
                    if self.link_open {
                        self.close_link(link);
                    }
                    self.active_code = self.stack[parent].resumed_code;
                    self.stack[parent].link = None;
                    self.stack[parent].link_code = None;
                    self.active_link = None;
                    self.link_open = false;
                }
            }
            if let Some(code) = self.active_code
                && self.codes[code].inline
            {
                self.top().resumed_code = Some(code);
                if !self.codes[code].text.is_empty() {
                    self.finish_code(code);
                }
                self.codes[code].text.clear();
                self.active_code = None;
                let link = self.new_link(attributes);
                self.top().link = Some(link);
                self.active_link = Some(link);
                self.link_open = true;
                self.inline(b"[", true, false);
                let link_code = self.new_code(true);
                self.top().link_code = Some(link_code);
                self.active_code = Some(link_code);
                return;
            }
            if let Some(link) = self.active_link {
                if self.link_open {
                    self.close_link(link);
                }
                if let Some(parent) = self.stack.iter().rposition(|c| c.link == Some(link)) {
                    self.stack[parent].link = None;
                }
                self.active_link = None;
                self.link_open = false;
            }
            let link = self.new_link(attributes);
            self.top().link = Some(link);
            self.active_link = Some(link);
            self.link_open = true;
            self.inline(b"[", true, false);
            return;
        }
        if name == "img" {
            let mut alt = Vec::new();
            for &b in attributes.get("alt").unwrap_or("").as_bytes() {
                if b == b'\\' || b == b']' {
                    alt.push(b'\\');
                }
                alt.push(b);
            }
            let mut close = b"](".to_vec();
            close.extend_from_slice(&destination(attributes.get("src").unwrap_or("")));
            close.extend_from_slice(&title(attributes.get("title")));
            close.push(b')');
            let open = b"![";
            let available = CONTENT_BYTES - self.output_bytes - (open.len() + close.len()) as i64;
            let mut value = open.to_vec();
            value.extend_from_slice(js::slice_bytes(&alt, available.max(0)));
            value.extend_from_slice(&close);
            self.inline(&value, true, false);
            return;
        }
        if name == "blockquote" {
            self.suspend_link();
            self.block();
            self.quote_depth += 1;
            self.needs_quote_prefix = true;
            return;
        }
        if name == "ul" || name == "ol" {
            self.suspend_link();
            let start = js::parse_int(attributes.get("start").unwrap_or("1"));
            self.lists.push(List {
                ordered: name == "ol",
                next: if start.is_nan() { 1.0 } else { start },
                previous: self.active_list,
            });
            let id = self.lists.len() - 1;
            self.top().list = Some(id);
            self.active_list = Some(id);
            self.block();
            return;
        }
        if name == "li" {
            self.block();
            let value = js::parse_int(attributes.get("value").unwrap_or(""));
            let ordered = self.active_list.filter(|&l| self.lists[l].ordered);
            if let Some(list) = ordered
                && !value.is_nan()
            {
                self.lists[list].next = value;
            }
            let marker = match ordered {
                Some(list) => {
                    let n = self.lists[list].next;
                    self.lists[list].next = n + 1.0;
                    format!("{}.", js::number_to_string(n))
                }
                None => "-".to_owned(),
            };
            let indent = self.active_item.map_or(0, |i| self.items[i].indent).min(24);
            let mut prefix = spaces(indent);
            prefix.extend_from_slice(marker.as_bytes());
            prefix.push(b' ');
            self.items.push(Item { indent: prefix.len(), previous: self.active_item });
            let id = self.items.len() - 1;
            self.top().item = Some(id);
            self.active_item = Some(id);
            self.pending_indent = 0;
            self.inline(&prefix, false, false);
            return;
        }
        if name == "table" {
            self.suspend_link();
            self.table_depth += 1;
            if self.table_depth == 1 {
                self.block();
                self.tables.push(Table {
                    start: self.output.len(),
                    rows: Vec::new(),
                    row: None,
                    caption: None,
                    fallback: false,
                    previous: self.active_table,
                });
                let id = self.tables.len() - 1;
                self.top().table = Some(id);
                self.active_table = Some(id);
            } else {
                self.pending_space = true;
            }
            return;
        }
        if name == "tr" {
            if self.table_depth != 1 {
                self.pending_space = true;
                return;
            }
            if let Some(t) = self.active_table {
                self.tables[t].row = Some(Vec::new());
            }
            return;
        }
        if name == "th" || name == "td" {
            if self.table_depth != 1 {
                self.pending_space = true;
                return;
            }
            let spans = attributes.get("colspan").is_some_and(|v| !v.is_empty())
                || attributes.get("rowspan").is_some_and(|v| !v.is_empty());
            if spans {
                // `activeTable!.fallback = true`: a table depth of one always has an active table here.
                if let Some(t) = self.active_table {
                    self.tables[t].fallback = true;
                }
            }
            let start = self.output.len();
            self.top().cell = Some(start);
            self.active_cell = true;
            return;
        }
        if name == "caption" {
            let start = self.output.len();
            self.top().caption = Some(start);
            return;
        }
        if name == "dt" {
            self.block();
            self.inline(b"**", false, false);
            return;
        }
        if name == "dd" {
            self.inline(b"\n: ", false, false);
        }
    }

    fn on_text(&mut self, value: &[u8]) {
        if self.stopped {
            if self.fallback_suppressed_depth == 0 && self.fallback_omitted_depth == 0 {
                self.text(value);
            }
            return;
        }
        if self.stack.last().is_some_and(|f| f.suppressed) {
            return;
        }
        self.text(value);
    }

    fn close_tag(&mut self, name: &str) {
        self.depth -= 1;
        if self.stopped {
            if self.fallback_omitted_depth > 0 && is_omitted(name) {
                self.fallback_omitted_depth -= 1;
            }
            if self.fallback_suppressed_depth > 0 && self.depth < self.fallback_suppressed_depth {
                self.fallback_suppressed_depth = 0;
            }
            return;
        }
        let Some(frame) = self.stack.pop() else { return };
        if frame.suppressed {
            return;
        }
        if let Some(link_code) = frame.link_code {
            let resumed = frame.resumed_code.expect("a link in code resumes that code");
            if self.active_link != frame.link || !self.link_open {
                let text = std::mem::take(&mut self.codes[link_code].text);
                js::push(&mut self.codes[resumed].text, &text);
                self.codes[link_code].text = text;
                self.active_code = Some(resumed);
                if self.active_link == frame.link {
                    self.active_link = None;
                }
                self.link_open = false;
                return;
            }
            self.active_code = None;
            self.finish_code(link_code);
            if let Some(link) = frame.link
                && self.link_open
            {
                self.close_link(link);
            }
            self.active_link = None;
            self.link_open = false;
            self.active_code = Some(resumed);
            return;
        }
        if self.active_code.is_some_and(|c| !self.codes[c].inline) && frame.code.is_none() {
            return;
        }
        if let Some(code) = frame.code {
            self.active_code = None;
            self.finish_code(code);
            if frame.suspended_link.is_some() {
                self.active_link = frame.suspended_link;
            }
            return;
        }
        if name == "summary" {
            if let Some(d) = self.active_details {
                self.details[d].summary = false;
            }
            return self.block();
        }
        if name == "details" {
            self.active_details = frame.details.and_then(|d| self.details[d].previous);
            return self.block();
        }
        if name == "dt" {
            self.inline(b"**", false, false);
            return;
        }
        if name == "dd" {
            return self.block();
        }
        if let Some(marker) = inline_marker(name) {
            let trailing_space = self.pending_space;
            self.pending_space = false;
            if let Some(m) = frame.marker {
                self.active_marker = self.markers[m].previous;
                let (index, block) = (self.markers[m].index, self.markers[m].block);
                if block != self.block_count || self.output.len() as i64 == index + 1 {
                    if index >= 0 && (index as usize) < self.output.len() {
                        let chunk = &mut self.output.chunks[index as usize];
                        *chunk = Chunk { start: chunk.start, end: chunk.start, lead: 0, raw: false };
                    }
                    self.pending_space = trailing_space || self.markers[m].leading_space;
                    return;
                }
            }
            self.inline(marker.as_bytes(), false, false);
            self.pending_space = trailing_space || frame.marker.is_some_and(|m| self.markers[m].leading_space);
            return;
        }
        if name == "a" {
            if let Some(link) = frame.link
                && (self.active_link == Some(link) || self.active_link.is_none())
            {
                self.active_link = Some(link);
                if self.link_open || self.last.is_some_and(|b| b != b'\n') {
                    self.close_link(link);
                }
                self.link_open = false;
                self.active_link = None;
            }
            return;
        }
        if heading_level(name).is_some() || is_block(name) {
            self.block();
            if frame.suspended_link.is_some() {
                self.active_link = frame.suspended_link;
            }
            return;
        }
        if name == "blockquote" {
            self.quote_depth -= 1;
            self.block();
            if frame.suspended_link.is_some() {
                self.active_link = frame.suspended_link;
            }
            return;
        }
        if name == "li" {
            self.active_item = frame.item.and_then(|i| self.items[i].previous);
            return self.block();
        }
        if name == "ul" || name == "ol" {
            self.active_list = frame.list.and_then(|l| self.lists[l].previous);
            self.block();
            if frame.suspended_link.is_some() {
                self.active_link = frame.suspended_link;
            }
            return;
        }
        if (name == "th" || name == "td") && self.table_depth == 1 {
            self.active_cell = false;
            if let Some(start) = frame.cell {
                let taken = self.take(start);
                let collapsed = collapse(&taken, b"\t\r\n ", b" ");
                let trimmed = js::trim(&collapsed);
                // .replace(/(?<!\\)\|/g, "\\|")
                let mut value = Vec::with_capacity(trimmed.len() + 4);
                for (i, &b) in trimmed.iter().enumerate() {
                    if b == b'|' && (i == 0 || trimmed[i - 1] != b'\\') {
                        value.push(b'\\');
                    }
                    value.push(b);
                }
                if let Some(t) = self.active_table
                    && let Some(row) = self.tables[t].row.as_mut()
                {
                    row.push(value);
                }
            }
            return;
        }
        if name == "tr" {
            if self.table_depth != 1 {
                return;
            }
            if let Some(t) = self.active_table {
                if let Some(row) = self.tables[t].row.take() {
                    self.tables[t].rows.push(row);
                }
                self.tables[t].row = None;
            }
            self.pending_space = true;
            return;
        }
        if name == "caption"
            && let Some(start) = frame.caption
            && let Some(t) = self.active_table
        {
            let taken = self.take(start);
            let collapsed = collapse(&taken, b"\t\r\n ", b" ");
            self.tables[t].caption = Some(js::trim(&collapsed).to_vec());
            return;
        }
        if name == "table" {
            self.table_depth -= 1;
            if self.table_depth == 0 {
                self.active_table = frame.table.and_then(|t| self.tables[t].previous);
                if let Some(t) = frame.table {
                    let start = self.tables[t].start;
                    let taken = self.take(start);
                    let collapsed = collapse(&taken, b"\t\r\n ", b" ");
                    let loose = js::trim(&collapsed).to_vec();
                    let table = &self.tables[t];
                    let width = table.rows.first().map_or(0, Vec::len);
                    let rectangular = width > 0 && table.rows.iter().all(|r| r.len() == width);
                    if !loose.is_empty() {
                        self.append(&loose, false);
                        self.block();
                    }
                    if let Some(caption) = self.tables[t].caption.take().filter(|c| !c.is_empty()) {
                        self.append(&caption, false);
                        self.block();
                    }
                    let rows = std::mem::take(&mut self.tables[t].rows);
                    if !self.tables[t].fallback && rectangular {
                        let mut prefix = if self.quote_depth > 0 { self.quote_prefix() } else { Vec::new() };
                        prefix.extend(spaces(self.pending_indent));
                        self.pending_indent = 0;
                        let mut head = prefix.clone();
                        head.extend_from_slice(b"| ");
                        join_row(&mut head, &rows[0]);
                        head.extend_from_slice(b" |\n");
                        head.extend_from_slice(&prefix);
                        head.push(b'|');
                        head.extend_from_slice(&b" --- |".repeat(width));
                        self.append(&head, false);
                        for row in &rows[1..] {
                            let mut line = b"\n".to_vec();
                            line.extend_from_slice(&prefix);
                            line.extend_from_slice(b"| ");
                            join_row(&mut line, row);
                            line.extend_from_slice(b" |");
                            self.append(&line, false);
                        }
                    } else {
                        for (index, row) in rows.iter().enumerate() {
                            if index > 0 {
                                self.block();
                            }
                            let mut line = Vec::new();
                            join_row(&mut line, row);
                            self.append(&line, false);
                        }
                    }
                }
                self.block();
                if frame.suspended_link.is_some() {
                    self.active_link = frame.suspended_link;
                }
                return;
            }
            self.pending_space = true;
        }
    }

    fn finish(self) -> String {
        let mut normalized = Vec::with_capacity(self.output.buf.len());
        let mut pending = Vec::new();
        for &chunk in &self.output.chunks {
            if chunk.raw {
                if !pending.is_empty() {
                    normalize_text(&pending, &mut normalized);
                    pending.clear();
                }
                js::push(&mut normalized, &self.output.buf[chunk.start..chunk.end]);
                continue;
            }
            self.output.write_chunk(chunk, &mut pending);
        }
        if !pending.is_empty() {
            normalize_text(&pending, &mut normalized);
        }
        let trimmed = js::trim(&normalized);
        js::into_string(js::slice_bytes(trimmed, MAX).to_vec())
    }
}

/// `row.join(" | ")`.
fn join_row(out: &mut JsString, row: &[JsString]) {
    for (i, cell) in row.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(b" | ");
        }
        js::push(out, cell);
    }
}

impl Handler for Markdown {
    fn open_tag(&mut self, name: &str, attributes: &Attributes) {
        Markdown::open_tag(self, name, attributes);
    }

    fn text(&mut self, text: &[u8]) {
        self.on_text(text);
    }

    fn close_tag(&mut self, name: &str) {
        Markdown::close_tag(self, name);
    }
}

pub(crate) fn convert(html: &str) -> String {
    Parser::new(html, Writes::Chunks(64 * 1024), Markdown::new()).run().finish()
}
