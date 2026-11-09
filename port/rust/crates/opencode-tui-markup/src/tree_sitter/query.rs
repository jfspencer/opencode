//! web-tree-sitter 0.25.10's `Query` (src/query.ts): the query is compiled by the tree-sitter C library (the
//! same sources web-tree-sitter's WASM is built from), but its predicates are parsed and evaluated as
//! web-tree-sitter's JS does — `#match?` with JavaScript regexes, unknown predicates ignored, `#set!` properties —
//! and captures are collected as `ts_query_captures_wasm` returns them, then filtered by the predicates.
//!
//! Trees are parsed as UTF-16 (as web-tree-sitter parses JS strings), so byte offsets are twice the UTF-16
//! indices [`QueryCapture`] reports.

use crate::js::{JsRegex, JsStr, JsString};
use crate::marked::Nullable;
use std::collections::HashMap;
use std::ffi::c_char;
use tree_sitter::ffi;

/// `QueryProperties`: `#set!` keys and values (`null` when no value was given).
pub type QueryProperties = HashMap<String, Nullable>;

#[derive(Clone, Debug)]
enum Step {
    Capture(String),
    Str(String),
}

/// A text predicate (`eq?`, `match?`, `any-of?` and their variants).
enum TextPredicate {
    EqCapture { a: String, b: String, positive: bool, match_all: bool },
    EqString { capture: String, value: JsString, positive: bool, match_all: bool },
    Match { capture: String, regex: JsRegex, positive: bool, match_all: bool },
    AnyOf { capture: String, values: Vec<JsString>, positive: bool },
}

/// A compiled query.
pub struct Query {
    raw: *mut ffi::TSQuery,
    pub capture_names: Vec<String>,
    text_predicates: Vec<Vec<TextPredicate>>,
    pub set_properties: Vec<Option<QueryProperties>>,
    pub asserted_properties: Vec<Option<QueryProperties>>,
    pub refuted_properties: Vec<Option<QueryProperties>>,
}

// The C query is immutable after construction and documented as shareable across threads.
unsafe impl Send for Query {}
unsafe impl Sync for Query {}

impl Drop for Query {
    fn drop(&mut self) {
        unsafe { ffi::ts_query_delete(self.raw) }
    }
}

/// A capture: its pattern, name and node.
#[derive(Clone, Copy, Debug)]
pub struct QueryCapture {
    pub pattern_index: usize,
    pub capture_index: usize,
    pub node: ffi::TSNode,
}

impl QueryCapture {
    /// `node.startIndex` (UTF-16 code units).
    pub fn start_index(&self) -> usize {
        unsafe { ffi::ts_node_start_byte(self.node) as usize / 2 }
    }

    pub fn end_index(&self) -> usize {
        unsafe { ffi::ts_node_end_byte(self.node) as usize / 2 }
    }
}

fn steps_str(steps: &[Step], i: usize) -> String {
    match &steps[i] {
        Step::Str(s) => s.clone(),
        Step::Capture(c) => c.clone(),
    }
}

/// Node text: the UTF-16 source between the node's start and end.
pub fn node_text<'a>(node: ffi::TSNode, source: &'a [u16]) -> &'a [u16] {
    let (start, end) = unsafe { (ffi::ts_node_start_byte(node) as usize / 2, ffi::ts_node_end_byte(node) as usize / 2) };
    &source[start.min(source.len())..end.min(source.len())]
}

impl Query {
    /// `new Query(language, source)`. Errors carry web-tree-sitter's messages.
    pub fn new(language: &tree_sitter::Language, source: &str) -> Result<Query, String> {
        let mut error_offset = 0u32;
        let mut error_type: ffi::TSQueryError = 0;
        let raw = unsafe {
            ffi::ts_query_new(
                language.into_raw_ref(),
                source.as_ptr() as *const c_char,
                source.len() as u32,
                &mut error_offset,
                &mut error_type,
            )
        };
        if raw.is_null() {
            return Err(Self::error_message(source, error_offset as usize, error_type));
        }
        let mut query = Query {
            raw,
            capture_names: Vec::new(),
            text_predicates: Vec::new(),
            set_properties: Vec::new(),
            asserted_properties: Vec::new(),
            refuted_properties: Vec::new(),
        };
        let read = |ptr: *const c_char, len: u32| -> String {
            let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
            String::from_utf8_lossy(bytes).into_owned()
        };
        unsafe {
            let capture_count = ffi::ts_query_capture_count(raw);
            for i in 0..capture_count {
                let mut len = 0;
                let ptr = ffi::ts_query_capture_name_for_id(raw, i, &mut len);
                query.capture_names.push(read(ptr, len));
            }
            let string_count = ffi::ts_query_string_count(raw);
            let mut strings = Vec::with_capacity(string_count as usize);
            for i in 0..string_count {
                let mut len = 0;
                let ptr = ffi::ts_query_string_value_for_id(raw, i, &mut len);
                strings.push(read(ptr, len));
            }
            let pattern_count = ffi::ts_query_pattern_count(raw);
            for i in 0..pattern_count as usize {
                query.text_predicates.push(Vec::new());
                query.set_properties.push(None);
                query.asserted_properties.push(None);
                query.refuted_properties.push(None);
                let mut step_count = 0u32;
                let steps_ptr = ffi::ts_query_predicates_for_pattern(raw, i as u32, &mut step_count);
                let raw_steps = if step_count == 0 { &[][..] } else { std::slice::from_raw_parts(steps_ptr, step_count as usize) };
                let mut steps: Vec<Step> = Vec::new();
                for step in raw_steps {
                    match step.type_ {
                        ffi::TSQueryPredicateStepTypeCapture => {
                            steps.push(Step::Capture(query.capture_names[step.value_id as usize].clone()))
                        }
                        ffi::TSQueryPredicateStepTypeString => steps.push(Step::Str(strings[step.value_id as usize].clone())),
                        _ => {
                            if !steps.is_empty() {
                                query.parse_predicate(i, &steps)?;
                                steps.clear();
                            }
                        }
                    }
                }
            }
        }
        Ok(query)
    }

    fn error_message(source: &str, error_byte: usize, kind: ffi::TSQueryError) -> String {
        // web-tree-sitter turns the byte offset into a UTF-16 index and quotes up to 100 units of the line.
        let prefix = String::from_utf8_lossy(&source.as_bytes()[..error_byte.min(source.len())]);
        let index = prefix.encode_utf16().count();
        let js = JsString::from(source);
        let suffix = js.js_slice(index as isize, Some(index as isize + 100)).split_str(&[b'\n' as u16])[0].to_string();
        let word: String = suffix.chars().skip_while(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-')).take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
        match kind {
            ffi::TSQueryErrorNodeType => format!("Bad node name '{word}'"),
            ffi::TSQueryErrorField => format!("Bad field name '{word}'"),
            ffi::TSQueryErrorCapture => format!("Bad capture name @{word}"),
            ffi::TSQueryErrorStructure => format!("Bad pattern structure at offset {index}: '{suffix}'..."),
            _ => format!("Bad syntax at offset {index}: '{suffix}'..."),
        }
    }

    /// `parsePattern` for one complete predicate: records it or fails as web-tree-sitter throws.
    fn parse_predicate(&mut self, index: usize, steps: &[Step]) -> Result<(), String> {
        let Step::Str(operator) = &steps[0] else { return Err("Predicates must begin with a literal value".into()) };
        let operator = operator.as_str();
        match operator {
            "any-not-eq?" | "not-eq?" | "any-eq?" | "eq?" => {
                if steps.len() != 3 {
                    return Err(format!(
                        "Wrong number of arguments to `#{operator}` predicate. Expected 2, got {}",
                        steps.len() - 1
                    ));
                }
                let Step::Capture(a) = &steps[1] else {
                    return Err(format!("First argument of `#{operator}` predicate must be a capture. Got \"{}\"", steps_str(steps, 1)));
                };
                let positive = operator == "eq?" || operator == "any-eq?";
                let match_all = !operator.starts_with("any-");
                self.text_predicates[index].push(match &steps[2] {
                    Step::Capture(b) => TextPredicate::EqCapture { a: a.clone(), b: b.clone(), positive, match_all },
                    Step::Str(v) => TextPredicate::EqString { capture: a.clone(), value: JsString::from(v.as_str()), positive, match_all },
                });
            }
            "any-not-match?" | "not-match?" | "any-match?" | "match?" => {
                if steps.len() != 3 {
                    return Err(format!(
                        "Wrong number of arguments to `#{operator}` predicate. Expected 2, got {}.",
                        steps.len() - 1
                    ));
                }
                let Step::Capture(capture) = &steps[1] else {
                    return Err(format!("First argument of `#{operator}` predicate must be a capture. Got \"{}\".", steps_str(steps, 1)));
                };
                let Step::Str(pattern) = &steps[2] else {
                    return Err(format!("Second argument of `#{operator}` predicate must be a string. Got @{}.", steps_str(steps, 2)));
                };
                let regex = JsRegex::new(pattern, "").map_err(|e| format!("Invalid regular expression: /{pattern}/: {e}"))?;
                let positive = operator == "match?" || operator == "any-match?";
                let match_all = !operator.starts_with("any-");
                self.text_predicates[index].push(TextPredicate::Match { capture: capture.clone(), regex, positive, match_all });
            }
            "not-any-of?" | "any-of?" => {
                if steps.len() < 2 {
                    return Err(format!(
                        "Wrong number of arguments to `#{operator}` predicate. Expected at least 1. Got {}.",
                        steps.len() - 1
                    ));
                }
                let Step::Capture(capture) = &steps[1] else {
                    return Err(format!("First argument of `#{operator}` predicate must be a capture. Got \"{}\".", steps_str(steps, 1)));
                };
                let mut values = Vec::new();
                for s in &steps[2..] {
                    let Step::Str(v) = s else { return Err(format!("Arguments to `#{operator}` predicate must be strings.\".")) };
                    values.push(JsString::from(v.as_str()));
                }
                self.text_predicates[index].push(TextPredicate::AnyOf { capture: capture.clone(), values, positive: operator == "any-of?" });
            }
            "is?" | "is-not?" => {
                if steps.len() < 2 || steps.len() > 3 {
                    return Err(format!(
                        "Wrong number of arguments to `#{operator}` predicate. Expected 1 or 2. Got {}.",
                        steps.len() - 1
                    ));
                }
                if !steps.iter().all(|s| matches!(s, Step::Str(_))) {
                    return Err(format!("Arguments to `#{operator}` predicate must be strings.\"."));
                }
                let target = if operator == "is?" { &mut self.asserted_properties } else { &mut self.refuted_properties };
                let props = target[index].get_or_insert_with(HashMap::new);
                props.insert(steps_str(steps, 1), steps.get(2).map_or(Nullable::Null, |s| Nullable::Str(JsString::from(steps_str(std::slice::from_ref(s), 0).as_str()))));
            }
            "set!" => {
                if steps.len() < 2 || steps.len() > 3 {
                    return Err(format!(
                        "Wrong number of arguments to `#set!` predicate. Expected 1 or 2. Got {}.",
                        steps.len() - 1
                    ));
                }
                if !steps.iter().all(|s| matches!(s, Step::Str(_))) {
                    return Err("Arguments to `#set!` predicate must be strings.\".".into());
                }
                let props = self.set_properties[index].get_or_insert_with(HashMap::new);
                props.insert(steps_str(steps, 1), steps.get(2).map_or(Nullable::Null, |s| Nullable::Str(JsString::from(steps_str(std::slice::from_ref(s), 0).as_str()))));
            }
            // Other predicates are recorded and never evaluated.
            _ => {}
        }
        Ok(())
    }

    fn predicates_hold(&self, pattern: usize, captures: &[ffi::TSQueryCapture], source: &[u16]) -> bool {
        let nodes = |name: &str| -> Vec<&[u16]> {
            captures
                .iter()
                .filter(|c| self.capture_names[c.index as usize] == name)
                .map(|c| node_text(c.node, source))
                .collect()
        };
        self.text_predicates[pattern].iter().all(|p| match p {
            TextPredicate::EqCapture { a, b, positive, match_all } => {
                let n1 = nodes(a);
                let n2 = nodes(b);
                let cmp = |x: &[u16], y: &[u16]| if *positive { x == y } else { x != y };
                if *match_all {
                    n1.iter().all(|x| n2.iter().any(|y| cmp(x, y)))
                } else {
                    n1.iter().any(|x| n2.iter().any(|y| cmp(x, y)))
                }
            }
            TextPredicate::EqString { capture, value, positive, match_all } => {
                let n = nodes(capture);
                let test = |x: &&[u16]| if *positive { *x == value.as_slice() } else { *x != value.as_slice() };
                if *match_all { n.iter().all(test) } else { n.iter().any(test) }
            }
            TextPredicate::Match { capture, regex, positive, match_all } => {
                let n = nodes(capture);
                if n.is_empty() {
                    return !*positive;
                }
                let test = |x: &&[u16]| if *positive { regex.test(x) } else { !regex.test(x) };
                if *match_all { n.iter().all(test) } else { n.iter().any(test) }
            }
            TextPredicate::AnyOf { capture, values, positive } => {
                let n = nodes(capture);
                if n.is_empty() {
                    return !*positive;
                }
                n.iter().all(|x| values.iter().any(|v| v.as_slice() == *x)) == *positive
            }
        })
    }

    /// `query.captures(node)` over the whole node, as `ts_query_captures_wasm` runs it: every capture of every
    /// match in order, kept when its pattern's text predicates hold for the match's captures at that point.
    pub fn captures(&self, node: ffi::TSNode, source: &[u16]) -> Vec<QueryCapture> {
        let mut out = Vec::new();
        unsafe {
            let cursor = ffi::ts_query_cursor_new();
            ffi::ts_query_cursor_set_match_limit(cursor, u32::MAX);
            ffi::ts_query_cursor_set_point_range(cursor, ffi::TSPoint { row: 0, column: 0 }, ffi::TSPoint { row: 0, column: 0 });
            ffi::ts_query_cursor_set_byte_range(cursor, 0, 0);
            ffi::ts_query_cursor_set_match_limit(cursor, u32::MAX);
            ffi::ts_query_cursor_set_max_start_depth(cursor, u32::MAX);
            ffi::ts_query_cursor_set_timeout_micros(cursor, 0);
            ffi::ts_query_cursor_exec(cursor, self.raw, node);
            let mut m = std::mem::zeroed::<ffi::TSQueryMatch>();
            let mut capture_index = 0u32;
            while ffi::ts_query_cursor_next_capture(cursor, &mut m, &mut capture_index) {
                let captures = if m.capture_count == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(m.captures, m.capture_count as usize)
                };
                let pattern = m.pattern_index as usize;
                if self.predicates_hold(pattern, captures, source) {
                    let c = &captures[capture_index as usize];
                    out.push(QueryCapture { pattern_index: pattern, capture_index: c.index as usize, node: c.node });
                }
            }
            ffi::ts_query_cursor_delete(cursor);
        }
        out
    }
}

/// Access to the raw language pointer without consuming the `Language`.
trait LanguageRaw {
    fn into_raw_ref(&self) -> *const ffi::TSLanguage;
}

impl LanguageRaw for tree_sitter::Language {
    fn into_raw_ref(&self) -> *const ffi::TSLanguage {
        self.clone().into_raw()
    }
}
