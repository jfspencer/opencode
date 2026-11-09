//! htmlparser2 8.0.2's `Tokenizer` and `Parser` (`lib/esm/Tokenizer.js`, `lib/esm/Parser.js`), with the options the
//! webfetch converters pass (none: HTML mode, entities decoded, tag and attribute names lower-cased, `/>` and CDATA
//! not recognized outside foreign content), reduced to the events they listen to: `onopentag`, `ontext` and
//! `onclosetag`.
//!
//! The tokenizer runs on the UTF-8 bytes of the input instead of UTF-16 code units. Every transition the tokenizer
//! makes on a non-ASCII unit is the same for all such units (and the entity trie has no non-ASCII branch), so a byte
//! walk visits the same states; the arithmetic on positions only ever spans ASCII characters. What a byte walk cannot
//! express directly is a position between the two halves of a surrogate pair, which the TS converter creates when
//! one of its 64 Ki-unit chunks ends inside an astral character: the tokenizer then emits the text up to the high
//! surrogate at the end of the chunk. Positions are therefore kept doubled (`2 * byte`, plus one for "after the high
//! surrogate of the character at `byte`"), and slices that start or end there carry the lone halves.
//!
//! `Parser#getSlice` keeps the chunks written so far and drops them lazily; when a section start is -1 (htmlparser2
//! reaches end of input after `</name ` or `<name/` with no `>`), its `slice(-1 - offset)` wraps around to the end of
//! the oldest chunk still held. That bookkeeping is reproduced (`buf`), so the wrapped text is the same.

use crate::entities::{HTML_DECODE_TREE, VALUE_LENGTH, determine_branch, replace_code_point};
use crate::js::{self, JsString};
use std::borrow::Cow;

/// The attributes of an open tag, in source order, the first of each name kept (a plain object in TS, where
/// `__proto__` cannot be set to a string).
#[derive(Default, Debug)]
pub struct Attributes {
    list: Vec<(String, String)>,
}

impl Attributes {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.list.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    pub fn has(&self, name: &str) -> bool {
        self.list.iter().any(|(n, _)| n == name)
    }

    fn insert(&mut self, name: &str, value: String) {
        if name != "__proto__" && !self.has(name) {
            self.list.push((name.to_owned(), value));
        }
    }
}

/// The callbacks a converter defines.
pub(crate) trait Handler {
    fn open_tag(&mut self, name: &str, attributes: &Attributes);
    /// A text event, as a JS string in generalized UTF-8 (it may start with a lone low surrogate or end with a lone
    /// high one).
    fn text(&mut self, text: &[u8]);
    fn close_tag(&mut self, name: &str);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Text,
    BeforeTagName,
    InTagName,
    InSelfClosingTag,
    BeforeClosingTagName,
    InClosingTagName,
    AfterClosingTagName,
    BeforeAttributeName,
    InAttributeName,
    AfterAttributeName,
    BeforeAttributeValue,
    InAttributeValueDq,
    InAttributeValueSq,
    InAttributeValueNq,
    BeforeDeclaration,
    InDeclaration,
    InProcessingInstruction,
    BeforeComment,
    CdataSequence,
    InSpecialComment,
    InCommentLike,
    BeforeSpecialS,
    SpecialStartSequence,
    InSpecialTag,
    BeforeEntity,
    BeforeNumericEntity,
    InNamedEntity,
    InNumericEntity,
    InHexEntity,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sequence {
    Cdata,
    CdataEnd,
    CommentEnd,
    ScriptEnd,
    StyleEnd,
    TitleEnd,
}

impl Sequence {
    fn bytes(self) -> &'static [u8] {
        match self {
            Sequence::Cdata => b"CDATA[",
            Sequence::CdataEnd => b"]]>",
            Sequence::CommentEnd => b"-->",
            Sequence::ScriptEnd => b"</script",
            Sequence::StyleEnd => b"</style",
            Sequence::TitleEnd => b"</title",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    NoValue,
    Unquoted,
    Single,
    Double,
}

fn is_whitespace(c: u8) -> bool {
    matches!(c, b' ' | b'\n' | b'\t' | 0x0c | b'\r')
}

fn is_end_of_tag_section(c: u8) -> bool {
    c == b'/' || c == b'>' || is_whitespace(c)
}

fn is_number(c: u8) -> bool {
    c.is_ascii_digit()
}

fn is_hex_digit(c: u8) -> bool {
    matches!(c, b'A'..=b'F' | b'a'..=b'f')
}

fn implies_close(name: &str) -> Option<&'static [&'static str]> {
    const FORM: &[&str] = &["input", "option", "optgroup", "select", "button", "datalist", "textarea"];
    const P: &[&str] = &["p"];
    const TABLE_SECTION: &[&str] = &["thead", "tbody"];
    const DDT: &[&str] = &["dd", "dt"];
    const RTP: &[&str] = &["rt", "rp"];
    Some(match name {
        "tr" => &["tr", "th", "td"],
        "th" => &["th"],
        "td" => &["thead", "th", "td"],
        "body" => &["head", "link", "script"],
        "li" => &["li"],
        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => P,
        "select" | "input" | "output" | "button" | "datalist" | "textarea" => FORM,
        "option" => &["option"],
        "optgroup" => &["optgroup", "option"],
        "dd" | "dt" => DDT,
        "address" | "article" | "aside" | "blockquote" | "details" | "div" | "dl" | "fieldset" | "figcaption"
        | "figure" | "footer" | "form" | "header" | "hr" | "main" | "nav" | "ol" | "pre" | "section" | "table"
        | "ul" => P,
        "rt" | "rp" => RTP,
        "tbody" | "tfoot" => TABLE_SECTION,
        _ => return None,
    })
}

fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "basefont"
            | "br"
            | "col"
            | "command"
            | "embed"
            | "frame"
            | "hr"
            | "img"
            | "input"
            | "isindex"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_foreign_context_element(name: &str) -> bool {
    matches!(name, "math" | "svg")
}

fn is_html_integration_element(name: &str) -> bool {
    matches!(name, "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml" | "foreignobject" | "desc" | "title")
}

/// The section start TS writes as -1.
const NO_SECTION: i64 = -2;

/// How the input is written to the parser.
pub(crate) enum Writes {
    /// One `parser.write(html)` (webfetch.ts `extractTextFromHTML`).
    Whole,
    /// `parser.write(html.slice(i, i + units))` for every `i` in steps of `units` UTF-16 code units
    /// (html-markdown.ts).
    Chunks(usize),
}

pub(crate) struct Parser<'a, H: Handler> {
    input: &'a [u8],
    pub(crate) handler: H,
    /// Doubled positions where the written chunks start, then the end of the input.
    bounds: Vec<i64>,
    /// Byte ranges the tokenizer walks for each chunk (a split astral character belongs to the earlier one).
    ranges: Vec<(usize, usize)>,
    /// The chunk `Parser.buffers[0]` holds.
    buf: usize,

    state: State,
    base_state: State,
    section_start: i64,
    index: usize,
    end: usize,
    is_special: bool,
    current_sequence: Sequence,
    sequence_index: usize,
    trie_index: i32,
    trie_current: u16,
    /// The trie index of a named entity's value, or a numeric entity's code point (saturated past U+10FFFF).
    entity_result: u32,
    entity_excess: usize,

    tagname: String,
    attribname: String,
    attribvalue: JsString,
    attribs: Option<Attributes>,
    stack: Vec<Box<str>>,
    foreign_context: Vec<bool>,
}

impl<'a, H: Handler> Parser<'a, H> {
    pub(crate) fn new(input: &'a str, writes: Writes, handler: H) -> Self {
        let input = input.as_bytes();
        let mut bounds = vec![0i64];
        let mut ranges = Vec::new();
        match writes {
            Writes::Whole => ranges.push((0, input.len())),
            Writes::Chunks(units) => {
                // Walk the UTF-16 length; a chunk boundary inside an astral character splits it.
                let mut start = 0usize;
                let mut count = 0usize;
                let mut next = units;
                let mut i = 0usize;
                while i < input.len() {
                    let len = utf8_len(input[i]);
                    let u16_len = if len == 4 { 2 } else { 1 };
                    if count == next {
                        ranges.push((start, i));
                        bounds.push(2 * i as i64);
                        start = i;
                        next += units;
                    } else if count + u16_len > next {
                        ranges.push((start, i + len));
                        bounds.push(2 * i as i64 + 1);
                        start = i + len;
                        next += units;
                    }
                    count += u16_len;
                    i += len;
                }
                // The last chunk; it is empty when it holds only the low half of a split character.
                if count > 0 {
                    ranges.push((start, input.len()));
                }
            }
        }
        bounds.push(2 * input.len() as i64);
        Parser {
            input,
            handler,
            bounds,
            ranges,
            buf: 0,
            state: State::Text,
            base_state: State::Text,
            section_start: 0,
            index: 0,
            end: 0,
            is_special: false,
            current_sequence: Sequence::Cdata,
            sequence_index: 0,
            trie_index: 0,
            trie_current: 0,
            entity_result: 0,
            entity_excess: 0,
            tagname: String::new(),
            attribname: String::new(),
            attribvalue: Vec::new(),
            attribs: None,
            stack: Vec::new(),
            foreign_context: Vec::new(),
        }
    }

    /// Writes every chunk, then `parser.end()`.
    pub(crate) fn run(mut self) -> H {
        for chunk in 0..self.ranges.len() {
            let (start, end) = self.ranges[chunk];
            debug_assert_eq!(self.index, start);
            self.index = start;
            self.end = end;
            self.parse();
            self.cleanup(self.bounds[chunk + 1]);
        }
        self.finish();
        self.handler
    }

    // ---- Parser#getSlice

    fn get_slice(&mut self, start: i64, end: i64) -> Cow<'a, [u8]> {
        let last = self.bounds.len() - 2;
        while self.buf < last && start >= self.bounds[self.buf + 1] {
            self.buf += 1;
        }
        let begin = if start < 0 { self.wrapped_start() } else { start };
        let slice = self.slice(begin, end);
        while self.buf < last && end > self.bounds[self.buf + 1] {
            self.buf += 1;
        }
        slice
    }

    /// Where `buffers[0].slice(-1 - bufferOffset)` starts: the last UTF-16 unit of the first chunk, or (for any later
    /// chunk, whose offset is past its length) the chunk's start.
    fn wrapped_start(&self) -> i64 {
        if self.buf > 0 {
            return self.bounds[self.buf];
        }
        let b = self.bounds[1];
        if b & 1 == 1 {
            // The chunk ends with a high surrogate, which the next chunk's low surrogate completes.
            return b - 1;
        }
        let byte = (b / 2) as usize;
        if byte == 0 {
            return 0;
        }
        let mut s = byte - 1;
        while s > 0 && self.input[s] & 0xc0 == 0x80 {
            s -= 1;
        }
        if byte - s == 4 { 2 * s as i64 + 1 } else { 2 * s as i64 }
    }

    fn slice(&self, start: i64, end: i64) -> Cow<'a, [u8]> {
        if end <= start {
            return Cow::Borrowed(&[]);
        }
        let (sb, sh) = ((start >> 1) as usize, start & 1 == 1);
        let (eb, eh) = ((end >> 1) as usize, end & 1 == 1);
        if !sh && !eh {
            return Cow::Borrowed(&self.input[sb..eb]);
        }
        let mut out = Vec::with_capacity(eb - sb + 6);
        let from = if sh {
            let (_, low) = surrogates(self.input, sb);
            js::push_code_point_raw(&mut out, low);
            sb + 4
        } else {
            sb
        };
        out.extend_from_slice(&self.input[from..eb]);
        if eh {
            let (high, _) = surrogates(self.input, eb);
            js::push_code_point_raw(&mut out, high);
        }
        Cow::Owned(out)
    }

    // ---- Parser callbacks (from the tokenizer)

    fn cb_text(&mut self, start: i64, end: i64) {
        let data = self.get_slice(start, end);
        self.handler.text(&data);
    }

    fn cb_text_entity(&mut self, cp: u32) {
        let mut data = Vec::with_capacity(4);
        js::push_code_point_raw(&mut data, cp);
        self.handler.text(&data);
    }

    fn cb_open_tag_name(&mut self, start: usize, end: usize) {
        let name = self.get_slice(2 * start as i64, 2 * end as i64);
        let name = js::to_lower_case(&js::into_string(name.into_owned()));
        self.emit_open_tag(name);
    }

    fn emit_open_tag(&mut self, name: String) {
        if let Some(closes) = implies_close(&name) {
            while let Some(top) = self.stack.last() {
                if !closes.contains(&&**top) {
                    break;
                }
                let element = self.stack.pop().expect("non-empty stack");
                self.handler.close_tag(&element);
            }
        }
        if !is_void_element(&name) {
            self.stack.push(name.as_str().into());
            if is_foreign_context_element(&name) {
                self.foreign_context.push(true);
            } else if is_html_integration_element(&name) {
                self.foreign_context.push(false);
            }
        }
        self.tagname = name;
        self.attribs = Some(Attributes::default());
    }

    fn end_open_tag(&mut self) {
        if let Some(attribs) = self.attribs.take() {
            self.handler.open_tag(&self.tagname, &attribs);
        }
        if is_void_element(&self.tagname) {
            self.handler.close_tag(&self.tagname);
        }
        self.tagname.clear();
    }

    fn cb_open_tag_end(&mut self) {
        self.end_open_tag();
    }

    fn cb_close_tag(&mut self, start: usize, end: usize) {
        let name = self.get_slice(2 * start as i64, 2 * end as i64);
        let name = js::to_lower_case(&js::into_string(name.into_owned()));
        if is_foreign_context_element(&name) || is_html_integration_element(&name) {
            self.foreign_context.pop();
        }
        if !is_void_element(&name) {
            if let Some(pos) = self.stack.iter().rposition(|n| **n == *name) {
                let mut count = self.stack.len() - pos;
                while count > 0 {
                    count -= 1;
                    let element = self.stack.pop().expect("non-empty stack");
                    self.handler.close_tag(&element);
                }
            } else if name == "p" {
                self.emit_open_tag("p".into());
                self.close_current_tag();
            }
        } else if name == "br" {
            self.handler.open_tag("br", &Attributes::default());
            self.handler.close_tag("br");
        }
    }

    fn cb_self_closing_tag(&mut self) {
        if self.foreign_context.last() == Some(&true) {
            self.close_current_tag();
        } else {
            self.cb_open_tag_end();
        }
    }

    fn close_current_tag(&mut self) {
        let name = self.tagname.clone();
        self.end_open_tag();
        if self.stack.last().is_some_and(|top| **top == *name) {
            self.handler.close_tag(&name);
            self.stack.pop();
        }
    }

    fn cb_attrib_name(&mut self, start: usize, end: usize) {
        let name = self.get_slice(2 * start as i64, 2 * end as i64);
        self.attribname = js::to_lower_case(&js::into_string(name.into_owned()));
    }

    fn cb_attrib_data(&mut self, start: i64, end: i64) {
        let data = self.get_slice(start, end);
        js::push(&mut self.attribvalue, &data);
    }

    fn cb_attrib_entity(&mut self, cp: u32) {
        js::push_code_point(&mut self.attribvalue, cp);
    }

    fn cb_attrib_end(&mut self, _quote: Quote) {
        let value = std::mem::take(&mut self.attribvalue);
        if let Some(attribs) = self.attribs.as_mut() {
            if attribs.has(&self.attribname) || self.attribname == "__proto__" {
                return;
            }
            attribs.insert(&self.attribname, js::into_string(value));
        }
    }

    /// `ondeclaration`, `onprocessinginstruction`, `oncdata`: no listener, but the slice they take still moves the
    /// parser's chunk bookkeeping.
    fn cb_unheard_slice(&mut self, start: i64, end: i64) {
        self.get_slice(start, end);
    }

    // ---- Tokenizer

    fn parse(&mut self) {
        while self.index < self.end {
            let c = self.input[self.index];
            match self.state {
                State::Text => self.state_text(c),
                State::SpecialStartSequence => self.state_special_start_sequence(c),
                State::InSpecialTag => self.state_in_special_tag(c),
                State::CdataSequence => self.state_cdata_sequence(c),
                State::InAttributeValueDq => self.handle_in_attribute_value(c, b'"'),
                State::InAttributeName => self.state_in_attribute_name(c),
                State::InCommentLike => self.state_in_comment_like(c),
                State::InSpecialComment => self.state_in_special_comment(c),
                State::BeforeAttributeName => self.state_before_attribute_name(c),
                State::InTagName => self.state_in_tag_name(c),
                State::InClosingTagName => self.state_in_closing_tag_name(c),
                State::BeforeTagName => self.state_before_tag_name(c),
                State::AfterAttributeName => self.state_after_attribute_name(c),
                State::InAttributeValueSq => self.handle_in_attribute_value(c, b'\''),
                State::BeforeAttributeValue => self.state_before_attribute_value(c),
                State::BeforeClosingTagName => self.state_before_closing_tag_name(c),
                State::AfterClosingTagName => self.state_after_closing_tag_name(c),
                State::BeforeSpecialS => self.state_before_special_s(c),
                State::InAttributeValueNq => self.state_in_attribute_value_no_quotes(c),
                State::InSelfClosingTag => self.state_in_self_closing_tag(c),
                State::InDeclaration => self.state_in_declaration(c),
                State::BeforeDeclaration => self.state_before_declaration(c),
                State::BeforeComment => self.state_before_comment(c),
                State::InProcessingInstruction => self.state_in_processing_instruction(c),
                State::InNamedEntity => self.state_in_named_entity(c),
                State::BeforeEntity => self.state_before_entity(c),
                State::InHexEntity => self.state_in_hex_entity(c),
                State::InNumericEntity => self.state_in_numeric_entity(c),
                State::BeforeNumericEntity => self.state_before_numeric_entity(c),
            }
            self.index += 1;
        }
    }

    fn at(&self) -> i64 {
        2 * self.index as i64
    }

    fn state_text(&mut self, c: u8) {
        if c == b'<' {
            if self.at() > self.section_start {
                self.cb_text(self.section_start, self.at());
            }
            self.state = State::BeforeTagName;
            self.section_start = self.at();
        } else if c == b'&' {
            self.state = State::BeforeEntity;
        }
    }

    fn state_special_start_sequence(&mut self, c: u8) {
        let seq = self.current_sequence.bytes();
        let is_end = self.sequence_index == seq.len();
        let is_match = if is_end { is_end_of_tag_section(c) } else { (c | 0x20) == seq[self.sequence_index] };
        if !is_match {
            self.is_special = false;
        } else if !is_end {
            self.sequence_index += 1;
            return;
        }
        self.sequence_index = 0;
        self.state = State::InTagName;
        self.state_in_tag_name(c);
    }

    fn state_in_special_tag(&mut self, c: u8) {
        let seq = self.current_sequence.bytes();
        if self.sequence_index == seq.len() {
            if c == b'>' || is_whitespace(c) {
                let end_of_text = self.index - seq.len();
                if self.section_start < 2 * end_of_text as i64 {
                    self.cb_text(self.section_start, 2 * end_of_text as i64);
                }
                self.is_special = false;
                self.section_start = 2 * (end_of_text + 2) as i64;
                self.state_in_closing_tag_name(c);
                return;
            }
            self.sequence_index = 0;
        }
        if (c | 0x20) == seq[self.sequence_index] {
            self.sequence_index += 1;
        } else if self.sequence_index == 0 {
            if self.current_sequence == Sequence::TitleEnd {
                if c == b'&' {
                    self.state = State::BeforeEntity;
                }
            } else if self.fast_forward_to(b'<') {
                self.sequence_index = 1;
            }
        } else {
            self.sequence_index = (c == b'<') as usize;
        }
    }

    fn state_cdata_sequence(&mut self, c: u8) {
        let seq = Sequence::Cdata.bytes();
        if c == seq[self.sequence_index] {
            self.sequence_index += 1;
            if self.sequence_index == seq.len() {
                self.state = State::InCommentLike;
                self.current_sequence = Sequence::CdataEnd;
                self.sequence_index = 0;
                self.section_start = self.at() + 2;
            }
        } else {
            self.sequence_index = 0;
            self.state = State::InDeclaration;
            self.state_in_declaration(c);
        }
    }

    fn fast_forward_to(&mut self, c: u8) -> bool {
        let from = self.index + 1;
        if from < self.end
            && let Some(offset) = self.input[from..self.end].iter().position(|&b| b == c)
        {
            self.index = from + offset;
            return true;
        }
        self.index = self.end - 1;
        false
    }

    fn state_in_comment_like(&mut self, c: u8) {
        let seq = self.current_sequence.bytes();
        if c == seq[self.sequence_index] {
            self.sequence_index += 1;
            if self.sequence_index == seq.len() {
                if self.current_sequence == Sequence::CdataEnd {
                    self.cb_unheard_slice(self.section_start, self.at() - 4);
                }
                self.sequence_index = 0;
                self.section_start = self.at() + 2;
                self.state = State::Text;
            }
        } else if self.sequence_index == 0 {
            if self.fast_forward_to(seq[0]) {
                self.sequence_index = 1;
            }
        } else if c != seq[self.sequence_index - 1] {
            self.sequence_index = 0;
        }
    }

    fn start_special(&mut self, sequence: Sequence, offset: usize) {
        self.is_special = true;
        self.current_sequence = sequence;
        self.sequence_index = offset;
        self.state = State::SpecialStartSequence;
    }

    fn state_before_tag_name(&mut self, c: u8) {
        if c == b'!' {
            self.state = State::BeforeDeclaration;
            self.section_start = self.at() + 2;
        } else if c == b'?' {
            self.state = State::InProcessingInstruction;
            self.section_start = self.at() + 2;
        } else if c.is_ascii_alphabetic() {
            let lower = c | 0x20;
            self.section_start = self.at();
            if lower == b't' {
                self.start_special(Sequence::TitleEnd, 3);
            } else {
                self.state = if lower == b's' { State::BeforeSpecialS } else { State::InTagName };
            }
        } else if c == b'/' {
            self.state = State::BeforeClosingTagName;
        } else {
            self.state = State::Text;
            self.state_text(c);
        }
    }

    fn state_in_tag_name(&mut self, c: u8) {
        if is_end_of_tag_section(c) {
            self.cb_open_tag_name((self.section_start / 2) as usize, self.index);
            self.section_start = NO_SECTION;
            self.state = State::BeforeAttributeName;
            self.state_before_attribute_name(c);
        }
    }

    fn state_before_closing_tag_name(&mut self, c: u8) {
        if is_whitespace(c) {
        } else if c == b'>' {
            self.state = State::Text;
        } else {
            self.state = if c.is_ascii_alphabetic() { State::InClosingTagName } else { State::InSpecialComment };
            self.section_start = self.at();
        }
    }

    fn state_in_closing_tag_name(&mut self, c: u8) {
        if c == b'>' || is_whitespace(c) {
            self.cb_close_tag((self.section_start / 2) as usize, self.index);
            self.section_start = NO_SECTION;
            self.state = State::AfterClosingTagName;
            self.state_after_closing_tag_name(c);
        }
    }

    fn state_after_closing_tag_name(&mut self, c: u8) {
        if c == b'>' || self.fast_forward_to(b'>') {
            self.state = State::Text;
            self.base_state = State::Text;
            self.section_start = self.at() + 2;
        }
    }

    fn state_before_attribute_name(&mut self, c: u8) {
        if c == b'>' {
            self.cb_open_tag_end();
            if self.is_special {
                self.state = State::InSpecialTag;
                self.sequence_index = 0;
            } else {
                self.state = State::Text;
            }
            self.base_state = self.state;
            self.section_start = self.at() + 2;
        } else if c == b'/' {
            self.state = State::InSelfClosingTag;
        } else if !is_whitespace(c) {
            self.state = State::InAttributeName;
            self.section_start = self.at();
        }
    }

    fn state_in_self_closing_tag(&mut self, c: u8) {
        if c == b'>' {
            self.cb_self_closing_tag();
            self.state = State::Text;
            self.base_state = State::Text;
            self.section_start = self.at() + 2;
            self.is_special = false;
        } else if !is_whitespace(c) {
            self.state = State::BeforeAttributeName;
            self.state_before_attribute_name(c);
        }
    }

    fn state_in_attribute_name(&mut self, c: u8) {
        if c == b'=' || is_end_of_tag_section(c) {
            self.cb_attrib_name((self.section_start / 2) as usize, self.index);
            self.section_start = NO_SECTION;
            self.state = State::AfterAttributeName;
            self.state_after_attribute_name(c);
        }
    }

    fn state_after_attribute_name(&mut self, c: u8) {
        if c == b'=' {
            self.state = State::BeforeAttributeValue;
        } else if c == b'/' || c == b'>' {
            self.cb_attrib_end(Quote::NoValue);
            self.state = State::BeforeAttributeName;
            self.state_before_attribute_name(c);
        } else if !is_whitespace(c) {
            self.cb_attrib_end(Quote::NoValue);
            self.state = State::InAttributeName;
            self.section_start = self.at();
        }
    }

    fn state_before_attribute_value(&mut self, c: u8) {
        if c == b'"' {
            self.state = State::InAttributeValueDq;
            self.section_start = self.at() + 2;
        } else if c == b'\'' {
            self.state = State::InAttributeValueSq;
            self.section_start = self.at() + 2;
        } else if !is_whitespace(c) {
            self.section_start = self.at();
            self.state = State::InAttributeValueNq;
            self.state_in_attribute_value_no_quotes(c);
        }
    }

    fn handle_in_attribute_value(&mut self, c: u8, quote: u8) {
        if c == quote {
            self.cb_attrib_data(self.section_start, self.at());
            self.section_start = NO_SECTION;
            self.cb_attrib_end(if quote == b'"' { Quote::Double } else { Quote::Single });
            self.state = State::BeforeAttributeName;
        } else if c == b'&' {
            self.base_state = self.state;
            self.state = State::BeforeEntity;
        }
    }

    fn state_in_attribute_value_no_quotes(&mut self, c: u8) {
        if is_whitespace(c) || c == b'>' {
            self.cb_attrib_data(self.section_start, self.at());
            self.section_start = NO_SECTION;
            self.cb_attrib_end(Quote::Unquoted);
            self.state = State::BeforeAttributeName;
            self.state_before_attribute_name(c);
        } else if c == b'&' {
            self.base_state = self.state;
            self.state = State::BeforeEntity;
        }
    }

    fn state_before_declaration(&mut self, c: u8) {
        if c == b'[' {
            self.state = State::CdataSequence;
            self.sequence_index = 0;
        } else {
            self.state = if c == b'-' { State::BeforeComment } else { State::InDeclaration };
        }
    }

    fn state_in_declaration(&mut self, c: u8) {
        if c == b'>' || self.fast_forward_to(b'>') {
            self.cb_unheard_slice(self.section_start, self.at());
            self.state = State::Text;
            self.section_start = self.at() + 2;
        }
    }

    fn state_in_processing_instruction(&mut self, c: u8) {
        if c == b'>' || self.fast_forward_to(b'>') {
            self.cb_unheard_slice(self.section_start, self.at());
            self.state = State::Text;
            self.section_start = self.at() + 2;
        }
    }

    fn state_before_comment(&mut self, c: u8) {
        if c == b'-' {
            self.state = State::InCommentLike;
            self.current_sequence = Sequence::CommentEnd;
            self.sequence_index = 2;
            self.section_start = self.at() + 2;
        } else {
            self.state = State::InDeclaration;
        }
    }

    fn state_in_special_comment(&mut self, c: u8) {
        if c == b'>' || self.fast_forward_to(b'>') {
            // `oncomment`: no listener, and the parser takes no slice without one.
            self.state = State::Text;
            self.section_start = self.at() + 2;
        }
    }

    fn state_before_special_s(&mut self, c: u8) {
        let lower = c | 0x20;
        if lower == b'c' {
            self.start_special(Sequence::ScriptEnd, 4);
        } else if lower == b't' {
            self.start_special(Sequence::StyleEnd, 4);
        } else {
            self.state = State::InTagName;
            self.state_in_tag_name(c);
        }
    }

    fn state_before_entity(&mut self, c: u8) {
        self.entity_excess = 1;
        self.entity_result = 0;
        if c == b'#' {
            self.state = State::BeforeNumericEntity;
        } else if c == b'&' {
        } else {
            self.trie_index = 0;
            self.trie_current = HTML_DECODE_TREE[0];
            self.state = State::InNamedEntity;
            self.state_in_named_entity(c);
        }
    }

    fn state_in_named_entity(&mut self, c: u8) {
        self.entity_excess += 1;
        self.trie_index = determine_branch(&HTML_DECODE_TREE, self.trie_current, self.trie_index + 1, c as u32);
        if self.trie_index < 0 {
            self.emit_named_entity();
            self.index -= 1;
            return;
        }
        self.trie_current = HTML_DECODE_TREE[self.trie_index as usize];
        let masked = self.trie_current & VALUE_LENGTH;
        if masked != 0 {
            let value_length = (masked >> 14) as i32 - 1;
            if !self.allow_legacy_entity() && c != b';' {
                self.trie_index += value_length;
            } else {
                let entity_start = self.index + 1 - self.entity_excess;
                if 2 * entity_start as i64 > self.section_start {
                    self.emit_partial(self.section_start, 2 * entity_start as i64);
                }
                self.entity_result = self.trie_index as u32;
                self.trie_index += value_length;
                self.entity_excess = 0;
                self.section_start = self.at() + 2;
                if value_length == 0 {
                    self.emit_named_entity();
                }
            }
        }
    }

    fn emit_named_entity(&mut self) {
        self.state = self.base_state;
        if self.entity_result == 0 {
            return;
        }
        let r = self.entity_result as usize;
        let value_length = (HTML_DECODE_TREE[r] & VALUE_LENGTH) >> 14;
        match value_length {
            1 => self.emit_code_point((HTML_DECODE_TREE[r] & !VALUE_LENGTH) as u32),
            2 => self.emit_code_point(HTML_DECODE_TREE[r + 1] as u32),
            3 => {
                self.emit_code_point(HTML_DECODE_TREE[r + 1] as u32);
                self.emit_code_point(HTML_DECODE_TREE[r + 2] as u32);
            }
            _ => {}
        }
    }

    fn state_before_numeric_entity(&mut self, c: u8) {
        if (c | 0x20) == b'x' {
            self.entity_excess += 1;
            self.state = State::InHexEntity;
        } else {
            self.state = State::InNumericEntity;
            self.state_in_numeric_entity(c);
        }
    }

    fn emit_numeric_entity(&mut self, strict: bool) {
        let entity_start = self.index - self.entity_excess - 1;
        let number_start = entity_start + 2 + (self.state == State::InHexEntity) as usize;
        if number_start != self.index {
            if 2 * entity_start as i64 > self.section_start {
                self.emit_partial(self.section_start, 2 * entity_start as i64);
            }
            self.section_start = 2 * (self.index + strict as usize) as i64;
            self.emit_code_point(replace_code_point(self.entity_result));
        }
        self.state = self.base_state;
    }

    fn add_digit(&mut self, base: u32, digit: u32) {
        // Any value past U+10FFFF stays past it and becomes U+FFFD.
        self.entity_result = (self.entity_result * base + digit).min(0x11_0000);
        self.entity_excess += 1;
    }

    fn state_in_numeric_entity(&mut self, c: u8) {
        if c == b';' {
            self.emit_numeric_entity(true);
        } else if is_number(c) {
            self.add_digit(10, (c - b'0') as u32);
        } else {
            if self.allow_legacy_entity() {
                self.emit_numeric_entity(false);
            } else {
                self.state = self.base_state;
            }
            self.index -= 1;
        }
    }

    fn state_in_hex_entity(&mut self, c: u8) {
        if c == b';' {
            self.emit_numeric_entity(true);
        } else if is_number(c) {
            self.add_digit(16, (c - b'0') as u32);
        } else if is_hex_digit(c) {
            self.add_digit(16, ((c | 0x20) - b'a' + 10) as u32);
        } else {
            if self.allow_legacy_entity() {
                self.emit_numeric_entity(false);
            } else {
                self.state = self.base_state;
            }
            self.index -= 1;
        }
    }

    fn allow_legacy_entity(&self) -> bool {
        self.base_state == State::Text || self.base_state == State::InSpecialTag
    }

    /// Emits what has been read at the end of a written chunk; `boundary` is where the chunk ends.
    fn cleanup(&mut self, boundary: i64) {
        if self.section_start != boundary {
            if self.state == State::Text || (self.state == State::InSpecialTag && self.sequence_index == 0) {
                self.cb_text(self.section_start, boundary);
                self.section_start = boundary;
            } else if matches!(
                self.state,
                State::InAttributeValueDq | State::InAttributeValueSq | State::InAttributeValueNq
            ) {
                self.cb_attrib_data(self.section_start, boundary);
                self.section_start = boundary;
            }
        }
    }

    fn finish(&mut self) {
        if self.state == State::InNamedEntity {
            self.emit_named_entity();
        }
        if self.section_start < self.at() {
            self.handle_trailing_data();
        }
        for index in (0..self.stack.len()).rev() {
            let name = self.stack[index].clone();
            self.handler.close_tag(&name);
        }
    }

    fn handle_trailing_data(&mut self) {
        let end_index = 2 * self.input.len() as i64;
        if self.state == State::InCommentLike {
            if self.current_sequence == Sequence::CdataEnd {
                self.cb_unheard_slice(self.section_start, end_index);
            }
        } else if matches!(self.state, State::InNumericEntity | State::InHexEntity) && self.allow_legacy_entity() {
            self.emit_numeric_entity(false);
        } else if matches!(
            self.state,
            State::InTagName
                | State::BeforeAttributeName
                | State::BeforeAttributeValue
                | State::AfterAttributeName
                | State::InAttributeName
                | State::InAttributeValueSq
                | State::InAttributeValueDq
                | State::InAttributeValueNq
                | State::InClosingTagName
        ) {
        } else {
            self.cb_text(self.section_start, end_index);
        }
    }

    fn emit_partial(&mut self, start: i64, end: i64) {
        if self.base_state != State::Text && self.base_state != State::InSpecialTag {
            self.cb_attrib_data(start, end);
        } else {
            self.cb_text(start, end);
        }
    }

    fn emit_code_point(&mut self, cp: u32) {
        if self.base_state != State::Text && self.base_state != State::InSpecialTag {
            self.cb_attrib_entity(cp);
        } else {
            self.cb_text_entity(cp);
        }
    }
}

fn utf8_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// The UTF-16 halves of the astral character at byte `i`.
fn surrogates(input: &[u8], i: usize) -> (u32, u32) {
    let (cp, len) = js::decode_at(input, i);
    debug_assert_eq!(len, 4);
    let v = cp - 0x10000;
    (0xd800 + (v >> 10), 0xdc00 + (v & 0x3ff))
}
