//! The prompt's value (`PromptInfo` of `prompt/history.tsx`) and its history (`usePromptHistory`): the last 50
//! submitted prompts in `<state>/prompt-history.jsonl`, browsed with up and down.
use std::cell::RefCell;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::reactive::on_mount;
use opencode_tui_text::WidthMethod;

use crate::data::{get, get_str};
use crate::prompt::persistence;
use crate::prompt::text::measure;

/// A mention's range in the prompt (editor offsets) and its text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Range {
    pub start: i64,
    pub end: i64,
    pub text: String,
}

/// `PromptFile`: an attachment (`uri`), with the local file a pasted attachment was read from (`path`, kept in the
/// TUI).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PromptFile {
    pub uri: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub path: Option<String>,
    pub mention: Option<Range>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PromptAgent {
    pub name: String,
    pub mention: Option<Range>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PromptSkill {
    pub id: String,
    pub mention: Option<Range>,
}

/// `PastedText`: a placeholder (`source`) and the text it stands for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PastedText {
    pub text: String,
    pub source: Range,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Shell,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Shell => "shell",
        }
    }
}

/// `PromptInfo`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PromptInfo {
    pub text: String,
    pub files: Option<Vec<PromptFile>>,
    pub agents: Option<Vec<PromptAgent>>,
    pub skills: Option<Vec<PromptSkill>>,
    pub pasted: Vec<PastedText>,
    pub mode: Option<Mode>,
}

/// `PromptPartRef`: what an extmark of the prompt stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartRef {
    File(usize),
    Agent(usize),
    Skill(usize),
    Pasted(usize),
}

/// `emptyPrompt()`.
pub fn empty_prompt() -> PromptInfo {
    PromptInfo { text: String::new(), files: Some(vec![]), agents: Some(vec![]), skills: Some(vec![]), pasted: vec![], mode: None }
}

/// The JSON of a prompt, as JSON.stringify writes the TS object.
pub fn range_json(range: &Range) -> Json {
    crate::data::object(vec![
        ("start", Json::Number(range.start as f64)),
        ("end", Json::Number(range.end as f64)),
        ("text", Json::String(range.text.clone())),
    ])
}

fn optional(entries: Vec<(&str, Option<Json>)>) -> Json {
    crate::data::object(entries.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))).collect())
}

pub fn file_json(file: &PromptFile) -> Json {
    optional(vec![
        ("uri", Some(Json::String(file.uri.clone()))),
        ("name", file.name.clone().map(Json::String)),
        ("description", file.description.clone().map(Json::String)),
        ("path", file.path.clone().map(Json::String)),
        ("mention", file.mention.as_ref().map(range_json)),
    ])
}

pub fn agent_json(agent: &PromptAgent) -> Json {
    optional(vec![("name", Some(Json::String(agent.name.clone()))), ("mention", agent.mention.as_ref().map(range_json))])
}

pub fn skill_json(skill: &PromptSkill) -> Json {
    optional(vec![("id", Some(Json::String(skill.id.clone()))), ("mention", skill.mention.as_ref().map(range_json))])
}

pub fn prompt_json(prompt: &PromptInfo) -> Json {
    optional(vec![
        ("text", Some(Json::String(prompt.text.clone()))),
        ("files", prompt.files.as_ref().map(|f| Json::Array(f.iter().map(file_json).collect()))),
        ("agents", prompt.agents.as_ref().map(|a| Json::Array(a.iter().map(agent_json).collect()))),
        ("skills", prompt.skills.as_ref().map(|s| Json::Array(s.iter().map(skill_json).collect()))),
        (
            "pasted",
            Some(Json::Array(
                prompt
                    .pasted
                    .iter()
                    .map(|p| crate::data::object(vec![("text", Json::String(p.text.clone())), ("source", range_json(&p.source))]))
                    .collect(),
            )),
        ),
        ("mode", prompt.mode.map(|m| Json::String(m.as_str().to_string()))),
    ])
}

fn range_of(value: Option<&Json>) -> Option<Range> {
    let value = value?;
    let num = |key: &str| match get(value, key) {
        Some(Json::Number(n)) => *n as i64,
        _ => 0,
    };
    Some(Range { start: num("start"), end: num("end"), text: get_str(value, "text").unwrap_or_default().to_string() })
}

fn list<T>(value: Option<&Json>, f: impl Fn(&Json) -> T) -> Option<Vec<T>> {
    match value {
        Some(Json::Array(items)) => Some(items.iter().map(f).collect()),
        _ => None,
    }
}

/// `parsePromptInfo(value)`: a prompt needs its `text` and `pasted`.
pub fn parse_prompt_info(value: &Json) -> Option<PromptInfo> {
    let text = get_str(value, "text")?.to_string();
    let Some(Json::Array(pasted)) = get(value, "pasted") else { return None };
    let opt = |key: &str| get_str(value, key).map(str::to_string);
    let _ = opt;
    Some(PromptInfo {
        text,
        files: list(get(value, "files"), |f| PromptFile {
            uri: get_str(f, "uri").unwrap_or_default().to_string(),
            name: get_str(f, "name").map(str::to_string),
            description: get_str(f, "description").map(str::to_string),
            path: get_str(f, "path").map(str::to_string),
            mention: range_of(get(f, "mention")),
        }),
        agents: list(get(value, "agents"), |a| PromptAgent {
            name: get_str(a, "name").unwrap_or_default().to_string(),
            mention: range_of(get(a, "mention")),
        }),
        skills: list(get(value, "skills"), |s| PromptSkill {
            id: get_str(s, "id").unwrap_or_default().to_string(),
            mention: range_of(get(s, "mention")),
        }),
        pasted: pasted
            .iter()
            .map(|p| PastedText { text: get_str(p, "text").unwrap_or_default().to_string(), source: range_of(get(p, "source")).unwrap_or_default() })
            .collect(),
        mode: match get_str(value, "mode") {
            Some("shell") => Some(Mode::Shell),
            Some("normal") => Some(Mode::Normal),
            _ => None,
        },
    })
}

/// `appendPrompt(prompt, following, widthMethod)`: part ranges shift by the measured offset of the joined text.
pub fn append_prompt(prompt: &PromptInfo, following: &PromptInfo, width_method: WidthMethod) -> PromptInfo {
    let text = if prompt.text.is_empty() { String::new() } else { format!("{}\n\n", prompt.text) };
    let offset = measure(&text, width_method);
    let shift = |range: &Range, by: i64| Range { start: range.start + by, end: range.end + by, text: range.text.clone() };
    let shift_opt = |range: &Option<Range>, by: i64| range.as_ref().map(|r| shift(r, by));
    PromptInfo {
        text: text + &following.text,
        files: Some(
            prompt.files.iter().flatten().map(|f| PromptFile { mention: shift_opt(&f.mention, 0), ..f.clone() })
                .chain(following.files.iter().flatten().map(|f| PromptFile { mention: shift_opt(&f.mention, offset), ..f.clone() }))
                .collect(),
        ),
        agents: Some(
            prompt.agents.iter().flatten().map(|a| PromptAgent { mention: shift_opt(&a.mention, 0), ..a.clone() })
                .chain(following.agents.iter().flatten().map(|a| PromptAgent { mention: shift_opt(&a.mention, offset), ..a.clone() }))
                .collect(),
        ),
        skills: Some(
            prompt.skills.iter().flatten().map(|s| PromptSkill { mention: shift_opt(&s.mention, 0), ..s.clone() })
                .chain(following.skills.iter().flatten().map(|s| PromptSkill { mention: shift_opt(&s.mention, offset), ..s.clone() }))
                .collect(),
        ),
        pasted: prompt
            .pasted
            .iter()
            .map(|p| PastedText { source: shift(&p.source, 0), ..p.clone() })
            .chain(following.pasted.iter().map(|p| PastedText { source: shift(&p.source, offset), ..p.clone() }))
            .collect(),
        mode: None,
    }
}

pub const MAX_HISTORY_ENTRIES: usize = 50;

/// The lines of a JSONL file as JSON values (unparseable lines dropped).
pub fn parse_lines(text: &str) -> Vec<Json> {
    text.split('\n').filter(|l| !l.is_empty()).filter_map(|line| opencode_protocol::wire::json::parse(line).ok()).collect()
}

/// `parsePromptHistory(text)`.
pub fn parse_prompt_history(text: &str) -> Vec<PromptInfo> {
    let lines: Vec<PromptInfo> = parse_lines(text).iter().filter_map(parse_prompt_info).collect();
    let skip = lines.len().saturating_sub(MAX_HISTORY_ENTRIES);
    lines.into_iter().skip(skip).collect()
}

fn stringify(value: &Json) -> String {
    opencode_tui_sync::client::stringify(value)
}

/// `JSON.stringify` of every entry, one per line.
fn lines_of(entries: &[PromptInfo]) -> String {
    entries.iter().map(|e| stringify(&prompt_json(e))).collect::<Vec<_>>().join("\n") + "\n"
}

struct HistoryState {
    index: i64,
    history: Vec<PromptInfo>,
}

/// `usePromptHistory()`.
#[derive(Clone)]
pub struct History {
    path: String,
    state: Rc<RefCell<HistoryState>>,
}

/// `array.at(index)`.
fn at<T>(items: &[T], index: i64) -> Option<&T> {
    let index = if index < 0 { items.len() as i64 + index } else { index };
    if index < 0 {
        return None;
    }
    items.get(index as usize)
}

impl History {
    /// `PromptHistoryProvider`: loads the file on mount and rewrites the retained entries (healing corruption and
    /// enforcing the limit).
    pub fn new(state_dir: &str) -> History {
        let path = format!("{state_dir}/prompt-history.jsonl");
        let history = History { path: path.clone(), state: Rc::new(RefCell::new(HistoryState { index: 0, history: vec![] })) };
        let state = history.state.clone();
        on_mount(move || {
            tokio::task::spawn_local(async move {
                let text = persistence::read_text(&path).await.unwrap_or_default();
                let lines = parse_prompt_history(&text);
                let rewrite = (!lines.is_empty()).then(|| lines_of(&lines));
                state.borrow_mut().history = lines;
                if let Some(content) = rewrite {
                    persistence::write_text(&path, content);
                }
            });
        });
        history
    }

    /// `move(direction, input)`: the entry up (-1) or down (1) from the current one, `None` when the input is an
    /// edited entry or the end is reached; moving back past the newest entry gives an empty prompt.
    pub fn move_(&self, direction: i64, input: &str) -> Option<PromptInfo> {
        let mut s = self.state.borrow_mut();
        if s.history.is_empty() {
            return None;
        }
        let current = at(&s.history, s.index)?;
        if current.text != input && !input.is_empty() {
            return None;
        }
        let next = s.index + direction;
        if next.abs() > s.history.len() as i64 || next > 0 {
            return None;
        }
        s.index = next;
        if next == 0 {
            return Some(empty_prompt());
        }
        at(&s.history, next).cloned()
    }

    /// `append(item)`.
    pub fn append(&self, item: &PromptInfo) {
        let entry = item.clone();
        let content = {
            let mut s = self.state.borrow_mut();
            if s.history.last().is_some_and(|last| stringify(&prompt_json(last)) == stringify(&prompt_json(&entry))) {
                s.index = 0;
                return;
            }
            s.history.push(entry.clone());
            let trimmed = s.history.len() > MAX_HISTORY_ENTRIES;
            if trimmed {
                let skip = s.history.len() - MAX_HISTORY_ENTRIES;
                s.history.drain(..skip);
            }
            s.index = 0;
            trimmed.then(|| lines_of(&s.history))
        };
        match content {
            Some(content) => persistence::write_text(&self.path, content),
            None => persistence::append_text(&self.path, stringify(&prompt_json(&entry)) + "\n"),
        }
    }
}
