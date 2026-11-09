//! `lib/tree-sitter`: the parser registry and one-shot highlighting OpenTUI's `TreeSitterClient` runs in its
//! worker (`parser.worker.ts`), on the Rust `tree-sitter` crate with the bundled C grammars.
//!
//! Divergences from the TS API:
//! - There is no worker or download: [`TreeSitterClient::highlight_once`] runs synchronously (call it from a task
//!   or `spawn_blocking` to keep OpenTUI's "unhighlighted first" ordering), and grammars and queries are
//!   bundled, so a filetype TS would download is available at once. Query or language failures behave as in TS:
//!   the filetype has no parser.
//! - Parser options name a bundled [`Grammar`] and carry query texts instead of WASM and query URLs.
//! - The buffer API (`createBuffer`, edits) is not ported: the TUI's Code renderables only use `highlightOnce`.

mod grammars;
pub mod query;

pub use grammars::{default_parsers, language, tui_parsers, GRAMMARS};

use crate::highlight::{HighlightMeta, SimpleHighlight};
use crate::js::{JsRegex, JsStr, JsString, Replacement};
use crate::marked::Nullable;
use query::{node_text, Query, QueryCapture};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex};
use tree_sitter::ffi;

/// A bundled grammar, named by its C symbol suffix (`tree_sitter_<name>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Grammar(pub &'static str);

/// `InjectionMapping`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InjectionMapping {
    pub node_types: Vec<(String, String)>,
    pub info_string_map: Vec<(String, String)>,
}

fn map_get<'a>(map: &'a [(String, String)], key: &str) -> Option<&'a str> {
    map.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// `FiletypeParserOptions`, with query texts in place of query URLs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FiletypeParserOptions {
    pub filetype: String,
    pub aliases: Vec<String>,
    pub highlights: Vec<&'static str>,
    pub injections: Vec<&'static str>,
    pub grammar: Grammar,
    pub injection_mapping: Option<InjectionMapping>,
}

static DEFAULT_PARSER_OVERRIDES: LazyLock<Mutex<Vec<FiletypeParserOptions>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// `addDefaultParsers(parsers)`: parsers every client registers after OpenTUI's defaults, replacing a default of
/// the same filetype.
pub fn add_default_parsers(parsers: Vec<FiletypeParserOptions>) {
    let mut overrides = DEFAULT_PARSER_OVERRIDES.lock().unwrap();
    for parser in parsers {
        overrides.retain(|p| p.filetype != parser.filetype);
        overrides.push(parser);
    }
}

static LUA_MATCH: LazyLock<JsRegex> = LazyLock::new(|| {
    JsRegex::compile(
        r#"#(not-)?(any-)?lua-match\?(\s+@[\w.-]+\s+)"(\^?(?:(?:[\w/!:#.]|\[\^?(?:[\w*-]|%d)+\])[*+]?)*\$?)""#,
        "g",
    )
});

/// `translateLuaMatch(query)`: rewrites the `#lua-match?` predicates web-tree-sitter would ignore into `#match?`
/// when their Lua pattern is one the bundled queries use.
pub fn translate_lua_match(query: &str) -> String {
    let source = JsString::from(query);
    let mut f = |m: &crate::js::JsMatch, s: &[u16]| -> JsString {
        let predicate = m.get_or_empty(s, 0);
        let not = m.get_or_empty(s, 1);
        let any = m.get_or_empty(s, 2);
        let capture = m.get_or_empty(s, 3);
        let pattern = m.get_or_empty(s, 4).to_string();
        let translated = pattern.replace("%d", "0-9").replace('.', "[^]");
        if JsRegex::new(&translated, "").is_err() {
            return predicate;
        }
        JsString::from(format!("#{any}{not}match?{capture}\"{translated}\""))
    };
    LUA_MATCH.replace(&source, Replacement::Func(&mut f)).to_string()
}

/// `{ highlights, warning, error }` of `highlightOnce`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HighlightResult {
    pub highlights: Option<Vec<SimpleHighlight>>,
    pub warning: Option<String>,
    pub error: Option<String>,
}

struct Queries {
    highlights: Query,
    injections: Option<Query>,
}

/// `FiletypeParser`: a filetype's language and compiled queries.
struct FiletypeParser {
    queries: Queries,
    language: tree_sitter::Language,
    injection_mapping: Option<InjectionMapping>,
}

/// `ReusableParserState`.
struct ReusableParser {
    parser: tree_sitter::Parser,
    filetype_parser: Arc<FiletypeParser>,
}

/// The worker's parser state (`ParserWorker`).
#[derive(Default)]
struct Worker {
    options: HashMap<String, FiletypeParserOptions>,
    aliases: HashMap<String, String>,
    filetype_parsers: HashMap<String, Option<Arc<FiletypeParser>>>,
    reusable: HashMap<String, ReusableParser>,
}

/// A captured node with the offset an injection applies (`offsetCapture`).
struct Match<'q> {
    query: &'q Query,
    capture: QueryCapture,
    offset: usize,
    /// From an injected tree (`_injectedQuery` set).
    injected: bool,
}

impl Match<'_> {
    fn start(&self) -> usize {
        self.capture.start_index() + self.offset
    }

    fn end(&self) -> usize {
        self.capture.end_index() + self.offset
    }

    fn name(&self) -> &str {
        &self.query.capture_names[self.capture.capture_index]
    }
}

impl Worker {
    /// `addFiletypeParser(filetypeParser)`.
    fn add_filetype_parser(&mut self, parser: FiletypeParserOptions) {
        if let Some(previous) = self.options.get(&parser.filetype) {
            for alias in previous.aliases.clone() {
                if self.aliases.get(&alias) == Some(&parser.filetype) {
                    self.aliases.remove(&alias);
                }
            }
        }
        let mut aliases: Vec<String> = Vec::new();
        for alias in &parser.aliases {
            if alias != &parser.filetype && !aliases.contains(alias) {
                aliases.push(alias.clone());
            }
        }
        self.aliases.remove(&parser.filetype);
        let filetype = parser.filetype.clone();
        for alias in &aliases {
            self.aliases.insert(alias.clone(), filetype.clone());
        }
        self.options.insert(filetype.clone(), FiletypeParserOptions { aliases, ..parser });
        self.filetype_parsers.remove(&filetype);
        self.reusable.remove(&filetype);
    }

    fn resolve_canonical_filetype(&self, filetype: &str) -> String {
        if self.options.contains_key(filetype) {
            return filetype.to_owned();
        }
        self.aliases.get(filetype).cloned().unwrap_or_else(|| filetype.to_owned())
    }

    /// `createQueries(filetypeParser, language)`: queries are joined as `fetchHighlightQueries` joins them.
    fn create_queries(options: &FiletypeParserOptions, language: &tree_sitter::Language) -> Option<Queries> {
        let join = |sources: &[&str]| -> String {
            let parts: Vec<&str> = sources.iter().copied().filter(|q| !JsString::from(*q).js_trim().is_empty()).collect();
            translate_lua_match(&parts.join("\n"))
        };
        let highlights = join(&options.highlights);
        if highlights.is_empty() {
            return None;
        }
        let highlights = Query::new(language, &highlights).ok()?;
        let mut injections = None;
        if !options.injections.is_empty() {
            let source = join(&options.injections);
            if !source.is_empty() {
                injections = Some(Query::new(language, &source).ok()?);
            }
        }
        Some(Queries { highlights, injections })
    }

    /// `resolveFiletypeParser(filetype)`.
    fn resolve_filetype_parser(&mut self, filetype: &str) -> Option<Arc<FiletypeParser>> {
        let canonical = self.resolve_canonical_filetype(filetype);
        if let Some(Some(parser)) = self.filetype_parsers.get(&canonical) {
            return Some(parser.clone());
        }
        let options = self.options.get(&canonical)?.clone();
        let language = language(options.grammar);
        let queries = Self::create_queries(&options, &language)?;
        let parser = Arc::new(FiletypeParser { queries, language, injection_mapping: options.injection_mapping.clone() });
        self.filetype_parsers.insert(canonical, Some(parser.clone()));
        Some(parser)
    }

    /// `getReusableParser(filetype)`: the canonical filetype's parser, created once.
    fn reusable_parser(&mut self, filetype: &str) -> Option<&mut ReusableParser> {
        let canonical = self.resolve_canonical_filetype(filetype);
        if !self.reusable.contains_key(&canonical) {
            let filetype_parser = self.resolve_filetype_parser(&canonical)?;
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&filetype_parser.language).ok()?;
            self.reusable.insert(canonical.clone(), ReusableParser { parser, filetype_parser });
        }
        self.reusable.get_mut(&canonical)
    }

    fn parse(parser: &mut tree_sitter::Parser, content: &[u16]) -> Option<tree_sitter::Tree> {
        parser.parse_utf16_le(content, None)
    }

    /// `handleOneShotHighlight(content, filetype)`.
    fn highlight_once(&mut self, content: &JsString, filetype: &str) -> HighlightResult {
        let Some(state) = self.reusable_parser(filetype) else {
            return HighlightResult { warning: Some(format!("No parser available for filetype {filetype}")), ..Default::default() };
        };
        // Markdown Parser BUG workaround: a trailing fence only closes when a newline follows it.
        let parse_content = if filetype == "markdown" && content.js_ends_with(&[96, 96, 96]) {
            content.concat(&[10])
        } else {
            content.clone()
        };
        let Some(tree) = Self::parse(&mut state.parser, &parse_content) else {
            return HighlightResult { error: Some("Failed to parse content".into()), ..Default::default() };
        };
        let filetype_parser = state.filetype_parser.clone();
        let root = tree.root_node().into_raw();
        let mut matches: Vec<Match<'_>> = filetype_parser
            .queries
            .highlights
            .captures(root, &parse_content)
            .into_iter()
            .map(|capture| Match { query: &filetype_parser.queries.highlights, capture, offset: 0, injected: false })
            .collect();
        let mut injected_trees: Vec<(Arc<FiletypeParser>, tree_sitter::Tree, JsString, usize)> = Vec::new();
        let mut injection_ranges: Vec<(String, Vec<(usize, usize)>)> = Vec::new();
        if let Some(injections) = &filetype_parser.queries.injections {
            self.process_injections(
                injections,
                root,
                &parse_content,
                content,
                filetype_parser.injection_mapping.as_ref(),
                &mut injected_trees,
                &mut injection_ranges,
            );
        }
        let injected: Vec<Vec<QueryCapture>> = injected_trees
            .iter()
            .map(|(fp, tree, text, _)| fp.queries.highlights.captures(tree.root_node().into_raw(), text))
            .collect();
        for (i, captures) in injected.iter().enumerate() {
            let (fp, _, _, offset) = &injected_trees[i];
            for capture in captures {
                matches.push(Match { query: &fp.queries.highlights, capture: *capture, offset: *offset, injected: true });
            }
        }
        let highlights = get_simple_highlights(&matches, &injection_ranges, &filetype_parser.queries.highlights);
        HighlightResult { highlights: Some(highlights), ..Default::default() }
    }

    /// `processInjections(parserState)`: parses each injected node with its language's reusable parser.
    fn process_injections(
        &mut self,
        injections: &Query,
        root: ffi::TSNode,
        parse_source: &JsString,
        content: &JsString,
        mapping: Option<&InjectionMapping>,
        trees: &mut Vec<(Arc<FiletypeParser>, tree_sitter::Tree, JsString, usize)>,
        ranges: &mut Vec<(String, Vec<(usize, usize)>)>,
    ) {
        // Node text for predicates comes from the parsed content; injection text from `content` (without the
        // fence workaround newline), as in TS.
        let captures = injections.captures(root, parse_source);
        let mut groups: Vec<(String, Vec<ffi::TSNode>)> = Vec::new();
        for capture in &captures {
            let name = &injections.capture_names[capture.capture_index];
            if !(name == "injection.content" || name.contains("injection")) {
                continue;
            }
            let node_type = unsafe { std::ffi::CStr::from_ptr(ffi::ts_node_type(capture.node)) }.to_string_lossy().into_owned();
            let mut target: Option<String> = None;
            if let Some(t) = mapping.and_then(|m| map_get(&m.node_types, &node_type)).filter(|t| !t.is_empty()) {
                target = Some(t.to_owned());
            } else if node_type == "code_fence_content" {
                let parent = unsafe { ffi::ts_node_parent(capture.node) };
                if !unsafe { ffi::ts_node_is_null(parent) } {
                    if let Some(info) = child_of_type(parent, "info_string") {
                        if let Some(lang) = child_of_type(info, "language") {
                            let name = JsString(content.js_substring(start_index(lang) as isize, Some(end_index(lang) as isize)).0)
                                .to_string();
                            target = Some(match mapping.and_then(|m| map_get(&m.info_string_map, &name)).filter(|t| !t.is_empty()) {
                                Some(mapped) => mapped.to_owned(),
                                None => name,
                            });
                        }
                    }
                }
            }
            if let Some(target) = target.filter(|t| !t.is_empty()) {
                match groups.iter_mut().find(|(l, _)| *l == target) {
                    Some((_, nodes)) => nodes.push(capture.node),
                    None => groups.push((target, vec![capture.node])),
                }
            }
        }
        for (language_name, nodes) in groups {
            let Some(state) = self.reusable_parser(&language_name) else { continue };
            let entry = match ranges.iter().position(|(l, _)| *l == language_name) {
                Some(i) => i,
                None => {
                    ranges.push((language_name.clone(), Vec::new()));
                    ranges.len() - 1
                }
            };
            for node in nodes {
                let (start, end) = (start_index(node), end_index(node));
                ranges[entry].1.push((start, end));
                let text = content.js_substring(start as isize, Some(end as isize));
                if let Some(tree) = Self::parse(&mut state.parser, &text) {
                    trees.push((state.filetype_parser.clone(), tree, text, start));
                }
            }
        }
    }
}

fn start_index(node: ffi::TSNode) -> usize {
    unsafe { ffi::ts_node_start_byte(node) as usize / 2 }
}

fn end_index(node: ffi::TSNode) -> usize {
    unsafe { ffi::ts_node_end_byte(node) as usize / 2 }
}

/// `node.children.find((child) => child.type === type)`.
fn child_of_type(node: ffi::TSNode, kind: &str) -> Option<ffi::TSNode> {
    let count = unsafe { ffi::ts_node_child_count(node) };
    for i in 0..count {
        let child = unsafe { ffi::ts_node_child(node, i) };
        let t = unsafe { std::ffi::CStr::from_ptr(ffi::ts_node_type(child)) };
        if t.to_bytes() == kind.as_bytes() {
            return Some(child);
        }
    }
    None
}

/// `getSimpleHighlights(matches, injectionRanges)`.
fn get_simple_highlights(matches: &[Match<'_>], ranges: &[(String, Vec<(usize, usize)>)], main_query: &Query) -> Vec<SimpleHighlight> {
    let flat: Vec<(usize, usize, Rc<str>)> = ranges
        .iter()
        .flat_map(|(lang, rs)| {
            let lang: Rc<str> = Rc::from(lang.as_str());
            rs.iter().map(move |&(s, e)| (s, e, lang.clone())).collect::<Vec<_>>()
        })
        .collect();
    let mut names: HashMap<(*const Query, usize), Rc<str>> = HashMap::new();
    let mut out = Vec::with_capacity(matches.len());
    for m in matches {
        let (start, end) = (m.start(), m.end());
        let mut is_injection = false;
        let mut injection_lang = None;
        let mut contains_injection = false;
        for (rs, re, lang) in &flat {
            if start >= *rs && end <= *re {
                is_injection = true;
                injection_lang = Some(lang.clone());
                break;
            } else if start <= *rs && end >= *re {
                contains_injection = true;
                break;
            }
        }
        // An injected capture reads its pattern's properties from its own query; a main one from the match.
        // `patternProperties?.conceal ?? match.setProperties?.conceal`: an injected capture has only the first,
        // and `??` turns its null into undefined.
        let _ = main_query;
        let props = m.query.set_properties.get(m.capture.pattern_index).and_then(|p| p.as_ref());
        let read = |key: &str| -> Nullable {
            match props.and_then(|p| p.get(key)).cloned().unwrap_or(Nullable::Undefined) {
                Nullable::Null if m.injected => Nullable::Undefined,
                v => v,
            }
        };
        let conceal = read("conceal");
        let conceal_lines = read("conceal_lines");
        let meta = HighlightMeta {
            is_injection: (is_injection && injection_lang.is_some()).then_some(true),
            injection_lang: if is_injection { injection_lang } else { None },
            contains_injection: contains_injection.then_some(true),
            conceal,
            conceal_lines,
        };
        let key = (m.query as *const Query, m.capture.capture_index);
        let group = names.entry(key).or_insert_with(|| Rc::from(m.name())).clone();
        out.push(SimpleHighlight { start, end, group, meta: if meta.is_empty() { None } else { Some(meta) } });
    }
    out.sort_by_key(|h| h.start);
    out
}

/// `TreeSitterClient`: registered parsers and their state, shared so highlighting can run off the UI task.
#[derive(Clone)]
pub struct TreeSitterClient {
    worker: Arc<Mutex<Worker>>,
}

// Trees and parsers stay inside the worker; queries are immutable C objects.
unsafe impl Send for Worker {}

impl Default for TreeSitterClient {
    fn default() -> Self {
        TreeSitterClient::new()
    }
}

impl TreeSitterClient {
    /// A client with OpenTUI's default parsers and those added with [`add_default_parsers`]
    /// (`registerDefaultParsers` at initialisation).
    pub fn new() -> TreeSitterClient {
        let mut worker = Worker::default();
        let overrides = DEFAULT_PARSER_OVERRIDES.lock().unwrap().clone();
        for parser in default_parsers() {
            if !overrides.iter().any(|o| o.filetype == parser.filetype) {
                worker.add_filetype_parser(parser);
            }
        }
        for parser in overrides {
            worker.add_filetype_parser(parser);
        }
        TreeSitterClient { worker: Arc::new(Mutex::new(worker)) }
    }

    /// `addFiletypeParser(filetypeParser)`.
    pub fn add_filetype_parser(&self, parser: FiletypeParserOptions) {
        self.worker.lock().unwrap().add_filetype_parser(parser);
    }

    /// `preloadParser(filetype)`.
    pub fn preload_parser(&self, filetype: &str) -> bool {
        self.worker.lock().unwrap().resolve_filetype_parser(filetype).is_some()
    }

    /// `highlightOnce(content, filetype)`.
    pub fn highlight_once(&self, content: &JsString, filetype: &str) -> HighlightResult {
        self.worker.lock().unwrap().highlight_once(content, filetype)
    }
}

/// The text of a node, for callers walking trees.
pub fn text_of<'a>(node: ffi::TSNode, source: &'a [u16]) -> &'a [u16] {
    node_text(node, source)
}
