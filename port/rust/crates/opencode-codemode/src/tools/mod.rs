//! The tool bridge (tool-runtime.ts): the tool tree, discovery and `search`, resolution of a call's path, and one
//! call's admission, hooks, and the JSON round trip of its result.

pub mod schema;

use crate::interp::errors::{DiagnosticKind, Fail, ToolRuntimeError};
use crate::interp::{Ctx, Fut, R, boxed};
use crate::json::{Json, JsonObj};
use crate::jsstr::JsStr;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

/// What a tool's result is described and decoded as.
#[derive(Clone, Debug)]
pub enum ToolOutput {
    /// No output schema: the program receives `undefined` and the signature says `Promise<void>`.
    Void,
    /// A render-only JSON Schema: results pass through unchanged.
    Schema(Json),
    /// core's default `Schema.NullOr(Schema.String)`: a string or null, anything else is an invalid output.
    NullOrString,
}

/// A host-side input check (an Effect Schema decode on the TS side): the decoded input, or the error text that
/// `String(cause)` gives.
pub type DecodeInput = Rc<dyn Fn(Option<&Json>) -> Result<Option<Json>, String>>;

/// One tool the program can call, at its dotted path (`opencode.models`).
#[derive(Clone)]
pub struct ToolDef {
    pub path: String,
    pub description: String,
    /// A render-only JSON Schema for the input.
    pub input: Json,
    pub output: ToolOutput,
    /// When set, the input is decoded (and may be rejected) before the call is admitted.
    pub decode_input: Option<DecodeInput>,
    /// When set, overrides the signature rendered from the schemas.
    pub signature: Option<String>,
    /// A namespace description for the namespace at the tool's parent path, merged with others.
    pub namespace_description: Option<(String, String)>,
}

impl ToolDef {
    pub fn new(path: impl Into<String>, description: impl Into<String>, input: Json, output: ToolOutput) -> ToolDef {
        ToolDef {
            path: path.into(),
            description: description.into(),
            input,
            output,
            decode_input: None,
            signature: None,
            namespace_description: None,
        }
    }
}

/// How one tool call ended on the host side.
pub enum ToolOutcome {
    /// The result value (`None` is JS `undefined`).
    Ok(Option<Json>),
    /// A refusal or failure: the message the program sees (`ToolFailure`).
    Err(String),
    /// Interrupted by the host: settles uncatchably and interrupts the execution.
    Interrupted,
}

/// The host side of tool calls: run a call by its canonical path with its decoded input.
pub type ToolHandler = Rc<dyn Fn(String, Option<Json>) -> Fut<ToolOutcome>>;

/// Identifies one call within an execution, so `end` finds what `start` recorded (TS hands both hooks the same call
/// object). Tool and extension calls share one sequence, in start order from 0.
pub type CallId = u64;

/// Observers around every admitted call (TS `tool.before`/`tool.after`, `extension.before`/`extension.after`):
/// `start` sees the decoded input, `end` how the call ended. Every started call ends exactly once, with
/// `Interrupted` when the execution ends first. They observe only; they cannot deny a call.
#[derive(Clone, Default)]
pub struct Hooks {
    /// `(id, tool path, decoded input)`.
    pub tool_start: Option<Rc<dyn Fn(CallId, &str, Option<&Json>)>>,
    /// `(id, tool path, status)`.
    pub tool_end: Option<Rc<dyn Fn(CallId, &str, CallStatus)>>,
    /// `(id, extension, name, args)` for a call to an extension global or a global object's function.
    pub extension_start: Option<crate::extensions::ExtensionHook>,
    /// `(id, extension, name, status)`.
    pub extension_end: Option<crate::extensions::ExtensionEndHook>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallStatus {
    Success,
    Failure,
    Interrupted,
}

#[derive(Default)]
struct Node {
    tool: Option<Rc<ToolDef>>,
    namespace: Option<String>,
    children: BTreeMap<String, Node>,
    order: Vec<String>,
}

impl Node {
    fn child_mut(&mut self, segment: &str) -> &mut Node {
        if !self.children.contains_key(segment) {
            self.order.push(segment.to_string());
        }
        self.children.entry(segment.to_string()).or_default()
    }
    fn child(&self, segment: &str) -> Option<&Node> {
        self.children.get(segment)
    }
}

/// A visible tool with its rendered description.
pub struct ToolDescription {
    pub path: String,
    pub description: String,
    signature: OnceCell<String>,
    tool: Rc<ToolDef>,
}

impl ToolDescription {
    pub fn signature(&self) -> &str {
        self.signature.get_or_init(|| signature(&self.path, &self.tool))
    }
}

struct SearchEntry {
    description: ToolDescription,
    path_words: Vec<String>,
    search_text: String,
}

/// Tools indexed once, shared by every execution over the same inventory.
pub struct Prepared {
    root: Node,
    index: OnceCell<Vec<SearchEntry>>,
}

pub fn tool_expression(path: &str) -> String {
    let mut out = String::from("tools");
    for segment in path.split('.') {
        if schema::is_identifier_segment(segment) {
            out.push('.');
            out.push_str(segment);
        } else {
            out.push('[');
            out.push_str(&Json::str(segment).to_json_string());
            out.push(']');
        }
    }
    out
}

fn is_empty_input(tool: &ToolDef) -> bool {
    schema::to_typescript(&tool.input, false) == "{}"
}

fn output_typescript(tool: &ToolDef, pretty: bool) -> String {
    match &tool.output {
        ToolOutput::Void => "void".into(),
        ToolOutput::Schema(schema) => schema::to_typescript(schema, pretty),
        ToolOutput::NullOrString => "string | null".into(),
    }
}

fn signature(path: &str, tool: &ToolDef) -> String {
    if let Some(signature) = &tool.signature {
        return signature.clone();
    }
    let input = if is_empty_input(tool) { "()".to_string() } else { format!("({})", schema::to_typescript(&tool.input, true)) };
    format!("{}{input}: Promise<{}>", tool_expression(path), output_typescript(tool, true))
}

/// The exact callable signature of the built-in `search` function, for host-owned instructions.
pub const SEARCH_SIGNATURE: &str = "search({\n  query?: string,\n  namespace?: string,\n  /** @integer @exclusiveMinimum 0 */\n  limit?: number,\n  /** @integer @minimum 0 */\n  offset?: number,\n}): {\n  items: Array<{\n      path: string,\n      description: string,\n      signature: string,\n    }>,\n  /** @integer @minimum 0 */\n  remaining: number,\n  next: {\n      /** @integer @minimum 0 */\n      offset: number,\n    } | null,\n}";

fn tokenize(query: &str) -> Vec<String> {
    let chars: Vec<char> = query.chars().collect();
    let mut spaced = String::new();
    for (index, &ch) in chars.iter().enumerate() {
        spaced.push(ch);
        if (ch.is_ascii_lowercase() || ch.is_ascii_digit()) && chars.get(index + 1).is_some_and(|n| n.is_ascii_uppercase()) {
            spaced.push(' ');
        }
    }
    let lower = js_lower(&spaced);
    lower.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit())).filter(|t| !t.is_empty() && *t != "*").map(String::from).collect()
}

fn js_lower(text: &str) -> String {
    text.to_lowercase()
}

fn term_forms(term: &str) -> Vec<String> {
    let mut forms = vec![term.to_string()];
    let units = term.encode_utf16().count();
    if term.ends_with("es") && units > 3 {
        forms.push(term[..term.len() - 2].to_string());
    }
    if term.ends_with('s') && units > 2 {
        forms.push(term[..term.len() - 1].to_string());
    }
    forms
}

fn js_less(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn rank<'a>(entries: &[&'a SearchEntry], query: &str) -> Vec<&'a SearchEntry> {
    let terms: Vec<Vec<String>> = tokenize(query).iter().map(|t| term_forms(t)).collect();
    let mut scored: Vec<(&SearchEntry, i64)> = entries
        .iter()
        .map(|entry| {
            let path = js_lower(&entry.description.path);
            let description = js_lower(&entry.description.description);
            let score = terms.iter().fold(0i64, |total, forms| {
                total
                    + if forms.iter().any(|f| path == *f || path.ends_with(&format!(".{f}"))) { 20 } else { 0 }
                    + if forms.iter().any(|f| entry.path_words.contains(f)) { 12 } else { 0 }
                    + if forms.iter().any(|f| path.contains(f.as_str())) { 8 } else { 0 }
                    + if forms.iter().any(|f| description.contains(f.as_str())) { 4 } else { 0 }
                    + if forms.iter().any(|f| entry.search_text.contains(f.as_str())) { 2 } else { 0 }
            });
            (*entry, score)
        })
        .filter(|(_, score)| terms.is_empty() || *score > 0)
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| js_less(&a.0.description.path, &b.0.description.path)));
    scored.into_iter().map(|(entry, _)| entry).collect()
}

impl Prepared {
    pub fn new(tools: &[ToolDef]) -> Result<Prepared, String> {
        Prepared::with_namespaces(&[], tools)
    }

    /// `new`, with namespaces (`(path, description)`, core's `Namespace.make` registrations) placed in the tree before
    /// the tools, so a namespace without tools of its own is still listed (`Object.keys(tools)`), in that order.
    pub fn with_namespaces(namespaces: &[(String, String)], tools: &[ToolDef]) -> Result<Prepared, String> {
        let mut root = Node::default();
        for (namespace, description) in namespaces {
            let mut node = &mut root;
            for segment in namespace.split('.') {
                node = node.child_mut(segment);
            }
            node.namespace = Some(description.clone());
        }
        for tool in tools {
            let mut current = &mut root;
            for segment in tool.path.split('.') {
                if segment.is_empty() {
                    return Err(format!("Tool name '{}' contains an empty segment.", tool.path));
                }
                current = current.child_mut(segment);
            }
            current.tool = Some(Rc::new(tool.clone()));
            if let Some((namespace, description)) = &tool.namespace_description {
                let mut node = &mut root;
                for segment in namespace.split('.') {
                    node = node.child_mut(segment);
                }
                node.namespace = Some(description.clone());
            }
        }
        Ok(Prepared { root, index: OnceCell::new() })
    }

    fn lookup(&self, segments: &[String]) -> Option<&Node> {
        let mut node = &self.root;
        for segment in segments {
            node = node.child(segment)?;
        }
        Some(node)
    }

    fn index(&self) -> &Vec<SearchEntry> {
        self.index.get_or_init(|| {
            let mut visible: Vec<(String, Rc<ToolDef>, Vec<String>)> = Vec::new();
            fn flatten(node: &Node, path: &mut Vec<String>, namespaces: &[String], out: &mut Vec<(String, Rc<ToolDef>, Vec<String>)>) {
                let mut next = namespaces.to_vec();
                if let Some(description) = &node.namespace {
                    next.push(description.clone());
                }
                if let Some(tool) = &node.tool {
                    out.push((path.join("."), tool.clone(), next.clone()));
                }
                for name in &node.order {
                    path.push(name.clone());
                    flatten(&node.children[name], path, &next, out);
                    path.pop();
                }
            }
            flatten(&self.root, &mut Vec::new(), &[], &mut visible);
            visible.sort_by(|a, b| js_less(&a.0, &b.0));
            visible
                .into_iter()
                .map(|(path, tool, namespaces)| {
                    let mut text = vec![path.clone(), tool.description.clone()];
                    text.extend(namespaces);
                    for property in schema::input_properties(&tool.input) {
                        text.push(property.name);
                        if let Some(description) = property.description {
                            text.push(description);
                        }
                    }
                    SearchEntry {
                        path_words: tokenize(&path),
                        search_text: js_lower(&text.join("\n")),
                        description: ToolDescription {
                            path,
                            description: tool.description.clone(),
                            signature: OnceCell::new(),
                            tool,
                        },
                    }
                })
                .collect()
        })
    }

    /// Structured descriptions of every visible tool, in path order.
    pub fn catalog(&self) -> Vec<&ToolDescription> {
        self.index().iter().map(|entry| &entry.description).collect()
    }
}

fn canonical(path: &[JsStr]) -> Vec<String> {
    path.iter().flat_map(|segment| segment.to_rust_lossy().split('.').map(String::from).collect::<Vec<_>>()).collect()
}

fn runtime_error(kind: DiagnosticKind, message: String, suggestions: Vec<&str>) -> Fail {
    Fail::ToolRuntime(Rc::new(ToolRuntimeError {
        kind,
        message: JsStr::from(message),
        suggestions: suggestions.into_iter().map(JsStr::from).collect(),
    }))
}

/// The tool bridge of one execution. Arguments arrive and results leave as JSON.
pub struct ToolSession {
    prepared: Rc<Prepared>,
    max_calls: Option<u64>,
    handler: ToolHandler,
    hooks: Hooks,
    ids: Rc<Cell<CallId>>,
    pub calls: Rc<RefCell<Vec<String>>>,
}

/// Runs `after` when dropped before completion: the call was interrupted.
struct EndGuard {
    id: CallId,
    name: String,
    hooks: Hooks,
    done: bool,
}

impl Drop for EndGuard {
    fn drop(&mut self) {
        if !self.done
            && let Some(end) = &self.hooks.tool_end
        {
            end(self.id, &self.name, CallStatus::Interrupted);
        }
    }
}

impl ToolSession {
    /// `ids` numbers the execution's calls, shared with its extension globals.
    pub fn new(prepared: Rc<Prepared>, max_calls: Option<u64>, handler: ToolHandler, hooks: Hooks, ids: Rc<Cell<CallId>>) -> ToolSession {
        ToolSession { prepared, max_calls, handler, hooks, ids, calls: Default::default() }
    }

    fn next_id(&self) -> CallId {
        let id = self.ids.get();
        self.ids.set(id + 1);
        id
    }

    fn record_call(&self, name: &str) -> R<()> {
        let mut calls = self.calls.borrow_mut();
        if let Some(max) = self.max_calls
            && calls.len() as u64 >= max
        {
            return Err(runtime_error(
                DiagnosticKind::ToolCallLimitExceeded,
                format!("Execution exceeded its tool-call limit of {max}."),
                vec![],
            ));
        }
        calls.push(name.to_string());
        Ok(())
    }

    /// Names a namespace lists (`Object.keys(tools.ns)`).
    pub fn keys(&self, path: &[JsStr]) -> R<Vec<JsStr>> {
        let segments = canonical(path);
        match self.prepared.lookup(&segments) {
            None => Err(runtime_error(
                DiagnosticKind::UnknownTool,
                format!("Unknown tool namespace '{}'.", segments.join(".")),
                vec![],
            )),
            Some(node) => Ok(node.order.iter().map(|name| JsStr::from(name.as_str())).collect()),
        }
    }

    /// Whether `path` names a namespace without a tool of its own (the root is one).
    pub fn is_namespace(&self, path: &[JsStr]) -> bool {
        matches!(self.prepared.lookup(&canonical(path)), Some(node) if node.tool.is_none())
    }

    fn resolve(&self, path: &[JsStr]) -> R<Rc<ToolDef>> {
        let segments = canonical(path);
        let name = segments.join(".");
        let Some(node) = self.prepared.lookup(&segments) else {
            let ns = if segments.len() > 1 && self.prepared.root.child(&segments[0]).is_some() { Some(&segments[0]) } else { None };
            let index = self.prepared.index();
            let scoped: Vec<&SearchEntry> = match ns {
                Some(ns) => index.iter().filter(|e| e.description.path.starts_with(&format!("{ns}."))).collect(),
                None => index.iter().collect(),
            };
            let query = match ns {
                Some(_) => segments[1..].join(" "),
                None => name.clone(),
            };
            let message = match rank(&scoped, &query).first() {
                Some(closest) => {
                    format!("Unknown tool '{name}'. Did you mean {}?", tool_expression(&closest.description.path))
                }
                None => format!("Unknown tool '{name}'."),
            };
            return Err(runtime_error(DiagnosticKind::UnknownTool, message, vec!["Use search to find available tools."]));
        };
        node.tool.clone().ok_or_else(|| runtime_error(DiagnosticKind::UnknownTool, format!("Tool '{name}' is not callable."), vec![]))
    }

    /// One call: admission, hooks, the host call, and the JSON round trip of its result, as program values.
    pub fn execute(self: &Rc<Self>, ctx: &Ctx, path: Vec<JsStr>, args: Vec<Option<Json>>) -> Fut<R<crate::interp::value::Value>> {
        let session = self.clone();
        let ctx = ctx.clone();
        boxed(async move {
            let segments = canonical(&path);
            // Models often write `tools.search(...)` for the bare `search(...)`; honor it unless a tool owns that path.
            if segments.len() == 1 && segments[0] == "search" && session.prepared.lookup(&segments).is_none() {
                let result = session.search(args)?;
                return Ok(crate::interp::data::from_json_opt(&ctx, &Some(result)));
            }
            let tool = session.resolve(&path)?;
            let name = segments.join(".");
            let result = session.call(&name, &tool, args).await?;
            Ok(crate::interp::data::from_json_opt(&ctx, &result))
        })
    }

    async fn call(&self, name: &str, tool: &ToolDef, args: Vec<Option<Json>>) -> R<Option<Json>> {
        let normalized = if args.is_empty() { vec![Some(Json::Obj(JsonObj::new()))] } else { args };
        if normalized.len() != 1 {
            return Err(runtime_error(
                DiagnosticKind::InvalidToolInput,
                format!("Tool '{name}' expects at most one input object."),
                vec![],
            ));
        }
        let raw = normalized.into_iter().next().unwrap();
        let input = match &tool.decode_input {
            None => raw,
            Some(decode) => decode(raw.as_ref()).map_err(|cause| {
                runtime_error(
                    DiagnosticKind::InvalidToolInput,
                    format!("Invalid input for tool '{name}': {cause}"),
                    vec!["The signature may have changed. Use search to get the current signature."],
                )
            })?,
        };
        self.record_call(name)?;
        let id = self.next_id();
        if let Some(start) = &self.hooks.tool_start {
            start(id, name, input.as_ref());
        }
        let mut guard = EndGuard { id, name: name.to_string(), hooks: self.hooks.clone(), done: false };
        let outcome = (self.handler)(name.to_string(), input).await;
        guard.done = true;
        let end = |status| {
            if let Some(end) = &self.hooks.tool_end {
                end(id, name, status);
            }
        };
        let raw = match outcome {
            ToolOutcome::Interrupted => {
                end(CallStatus::Interrupted);
                return Err(Fail::Interrupt);
            }
            ToolOutcome::Err(message) => {
                end(CallStatus::Failure);
                return Err(Fail::Tool(JsStr::from(message)));
            }
            ToolOutcome::Ok(value) => value,
        };
        // The same round trip a tool result would take through text: exact JSON.stringify semantics.
        let decoded = match &tool.output {
            ToolOutput::Void => None,
            ToolOutput::Schema(_) => raw,
            ToolOutput::NullOrString => match &raw {
                Some(Json::Str(_)) | Some(Json::Null) => raw,
                _ => {
                    end(CallStatus::Failure);
                    return Err(runtime_error(
                        DiagnosticKind::InvalidToolOutput,
                        format!("Invalid output from tool '{name}': SchemaError(Expected string | null)"),
                        vec![],
                    ));
                }
            },
        };
        end(CallStatus::Success);
        Ok(decoded.map(|value| normalize(&value)))
    }

    /// The built-in search: synchronous, counts as an admitted call.
    pub fn search(&self, args: Vec<Option<Json>>) -> R<Json> {
        let normalized = if args.is_empty() { vec![Some(Json::Obj(JsonObj::new()))] } else { args };
        if normalized.len() != 1 {
            return Err(runtime_error(DiagnosticKind::InvalidToolInput, "Tool 'search' expects at most one input object.".into(), vec![]));
        }
        let input = normalized.into_iter().next().unwrap();
        let request = decode_search_input(input.as_ref())
            .map_err(|cause| runtime_error(DiagnosticKind::InvalidToolInput, format!("Invalid input for tool 'search': {cause}"), vec![]))?;
        self.record_call("search")?;
        let id = self.next_id();
        if let Some(start) = &self.hooks.tool_start {
            start(id, "search", input.as_ref());
        }
        let result = self.run_search(&request);
        if let Some(end) = &self.hooks.tool_end {
            end(id, "search", CallStatus::Success);
        }
        Ok(result)
    }

    fn run_search(&self, request: &SearchRequest) -> Json {
        let index = self.prepared.index();
        let query = request.query.clone().unwrap_or_default();
        let offset = request.offset.unwrap_or(0.0) as usize;
        let mut ns = request.namespace.clone();
        if let Some(namespace) = &ns
            && !index.iter().any(|entry| entry.description.path.starts_with("tools."))
        {
            if namespace == "tools" {
                ns = None;
            } else if let Some(rest) = namespace.strip_prefix("tools.") {
                ns = Some(rest.to_string());
            }
        }
        let scoped: Vec<&SearchEntry> = match &ns {
            None => index.iter().collect(),
            Some(ns) => index
                .iter()
                .filter(|e| e.description.path == *ns || e.description.path.starts_with(&format!("{ns}.")))
                .collect(),
        };
        let trimmed = crate::stdlib::string::is_js_whitespace;
        let trimmed: String = {
            let units: Vec<u16> = query.encode_utf16().collect();
            let start = units.iter().position(|u| !trimmed(*u)).unwrap_or(units.len());
            let end = units.iter().rposition(|u| !trimmed(*u)).map(|i| i + 1).unwrap_or(start);
            String::from_utf16_lossy(&units[start..end.max(start)])
        };
        let path_query = trimmed.strip_prefix("tools.").unwrap_or(&trimmed).to_string();
        let exact = if path_query.is_empty() {
            None
        } else {
            scoped
                .iter()
                .find(|entry| entry.description.path == path_query || tool_expression(&entry.description.path) == trimmed)
                .copied()
        };
        let ranked = match exact {
            Some(entry) => vec![entry],
            None => rank(&scoped, &query),
        };
        let limit = request.limit.unwrap_or(10.0) as usize;
        let items: Vec<Json> = ranked
            .iter()
            .skip(offset)
            .take(limit)
            .map(|entry| {
                let mut item = JsonObj::new();
                item.insert_str("path", Json::str(&tool_expression(&entry.description.path)));
                item.insert_str("description", Json::str(&entry.description.description));
                item.insert_str("signature", Json::str(entry.description.signature()));
                Json::Obj(item)
            })
            .collect();
        let remaining = ranked.len().saturating_sub(offset + items.len());
        let mut result = JsonObj::new();
        let count = items.len();
        result.insert_str("items", Json::Arr(items));
        result.insert_str("remaining", Json::Num(remaining as f64));
        result.insert_str(
            "next",
            if remaining > 0 {
                let mut next = JsonObj::new();
                next.insert_str("offset", Json::Num((offset + count) as f64));
                Json::Obj(next)
            } else {
                Json::Null
            },
        );
        Json::Obj(result)
    }

    /// `search(...)` as the program sees it: a promise-like copy of the result, after a blocking call.
    pub async fn search_value(&self, ctx: &Ctx, args: Vec<crate::interp::value::Value>) -> R<crate::interp::value::Value> {
        let mut json = Vec::with_capacity(args.len());
        for arg in args {
            json.push(crate::interp::data::to_boundary(ctx, arg).await?);
        }
        let result = self.search(json)?;
        let value = crate::interp::data::from_json(ctx, &result);
        Ok(crate::interp::promises::promise_like(ctx, value))
    }
}

/// Non-finite numbers become null, as the JSON round trip makes them.
fn normalize(value: &Json) -> Json {
    match value {
        Json::Num(n) if !n.is_finite() => Json::Null,
        Json::Arr(items) => Json::Arr(items.iter().map(normalize).collect()),
        Json::Obj(obj) => {
            let mut copy = JsonObj::new();
            for (key, item) in obj.iter() {
                copy.insert(key.clone(), normalize(item));
            }
            Json::Obj(copy)
        }
        other => other.clone(),
    }
}

struct SearchRequest {
    query: Option<String>,
    namespace: Option<String>,
    limit: Option<f64>,
    offset: Option<f64>,
}

/// Decoding the search input as its Effect Schema does (`Struct({ query?, namespace?, limit?: Int > 0, offset?: Int
/// >= 0 })`), with Effect's default formatter for the first issue.
fn decode_search_input(input: Option<&Json>) -> Result<SearchRequest, String> {
    let issue = |message: String| format!("SchemaError({message})");
    let Some(Json::Obj(obj)) = input else { return Err(issue("Expected object".into())) };
    let string = |key: &str| -> Result<Option<String>, String> {
        match obj.get(key) {
            None => Ok(None),
            Some(Json::Str(text)) => Ok(Some(text.to_rust_lossy())),
            Some(_) => Err(issue(format!("Expected string\n  at [\"{key}\"]"))),
        }
    };
    let integer = |key: &str, exclusive: bool| -> Result<Option<f64>, String> {
        match obj.get(key) {
            None => Ok(None),
            Some(Json::Num(n)) if n.is_finite() => {
                if n.fract() != 0.0 || n.abs() > 9007199254740991.0 {
                    return Err(issue(format!("Expected an integer\n  at [\"{key}\"]")));
                }
                if exclusive && *n <= 0.0 {
                    return Err(issue(format!("Expected a value greater than 0\n  at [\"{key}\"]")));
                }
                if !exclusive && *n < 0.0 {
                    return Err(issue(format!("Expected a value greater than or equal to 0\n  at [\"{key}\"]")));
                }
                Ok(Some(*n))
            }
            Some(_) => Err(issue(format!("Expected number\n  at [\"{key}\"]"))),
        }
    };
    Ok(SearchRequest {
        query: string("query")?,
        namespace: string("namespace")?,
        limit: integer("limit", true)?,
        offset: integer("offset", false)?,
    })
}
