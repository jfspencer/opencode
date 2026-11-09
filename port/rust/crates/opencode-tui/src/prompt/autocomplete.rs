//! The prompt's autocomplete (`component/prompt/autocomplete.tsx`): slash commands (the keymap's and the
//! server's), `@` mentions (skills, references, subagents, then files from the server's file search), and
//! directories for a command's argument (`/cd`), in a list above the prompt; and `prompt/directory-completion.ts`.
//!
//! TODO(port-tui): the editor context's mentions (`editor.onMention`), directory recents' destructive action
//! (ctrl+d) has no recents to act on yet.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::border::{BorderCharacters, border_chars_to_array};
use opencode_tui_core::elements::{box_, scrollbox, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::renderables::scrollbox::{
    MacOSScrollAccel, ScrollAcceleration, ScrollBarOptions, ScrollUnit,
};
use opencode_tui_core::renderables::text::WrapMode;
use crate::prompt::input::Textarea;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element, FlexDirection, MouseEventType};
use opencode_tui_sync::client::RequestOptions;
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::obj;

use crate::ctx::tui;
use crate::data::{array, get_str};
use crate::keymap::{self, Command, Enabled, Layer, create_layer};
use crate::prompt::attachment::path_to_file_url;
use crate::prompt::frecency::{Frecency, resolve_path};
use crate::prompt::history::{PartRef, PromptAgent, PromptFile, PromptInfo, PromptSkill, Range};
use crate::prompt::text::{
    LineRange, has_whitespace, measure, mention_trigger_index, parse_file_line_range, parse_slash_head, slash_trigger_index,
    strip_file_line_range, truncate, truncate_middle,
};
use crate::util::{rgba, string_width};

/// What the autocomplete shows (`visible`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visible {
    None,
    Reference,
    Command,
    Directory,
}

/// The kind of a mention part (its extmark style).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    File,
    Agent,
    Skill,
}

/// What the autocomplete needs of its prompt (the props TS passes).
pub trait PromptHost {
    /// `props.value` (tracked).
    fn value(&self) -> String;
    fn textarea(&self) -> Textarea;
    /// `setPrompt(produce(cb))`.
    fn update_prompt(&self, f: &mut dyn FnMut(&mut PromptInfo));
    fn set_extmark(&self, part: PartRef, id: u32);
    fn create_part_extmark(&self, start: i64, end: i64, kind: PartKind) -> u32;
    fn has_skill(&self, id: &str) -> bool;
    /// `location.current` (where files are searched).
    fn location(&self) -> Json;
    /// `directoryOptions(query)`.
    fn directory_options(&self, query: &str) -> Vec<AutocompleteOption>;
    /// `argumentAutocomplete(command) === "directory"`.
    fn argument_autocomplete(&self, command: &Command) -> bool;
}

/// `AutocompleteOption["destructive"]`.
#[derive(Clone)]
pub struct Destructive {
    pub id: String,
    pub confirm: String,
    pub run: Rc<dyn Fn()>,
}

/// `AutocompleteOption`.
#[derive(Clone, Default)]
pub struct AutocompleteOption {
    pub display: String,
    pub value: Option<String>,
    pub aliases: Vec<String>,
    pub description: Option<String>,
    pub is_directory: bool,
    pub on_select: Option<Rc<dyn Fn()>>,
    pub path: Option<String>,
    pub absolute: Option<String>,
    pub destructive: Option<Destructive>,
    pub kind: Option<&'static str>,
    pub queueable: bool,
}

impl PartialEq for AutocompleteOption {
    // The data fields (a row reads them; selection reads the current list).
    fn eq(&self, other: &Self) -> bool {
        self.display == other.display
            && self.value == other.value
            && self.description == other.description
            && self.is_directory == other.is_directory
            && self.path == other.path
            && self.kind == other.kind
            && self.destructive.as_ref().map(|d| (&d.id, &d.confirm)) == other.destructive.as_ref().map(|d| (&d.id, &d.confirm))
    }
}

/// `AutocompleteResults`.
#[derive(Clone, Default, PartialEq)]
struct Results {
    options: Vec<AutocompleteOption>,
    failed: bool,
    mode: Option<Visible>,
    query: String,
    resolved: bool,
}

struct AutoState {
    host: Rc<dyn PromptHost>,
    anchor: Element,
    input: Element,
    frecency: Frecency,
    index: Signal<i64>,
    selected: Signal<usize>,
    visible: Signal<Visible>,
    position_tick: Signal<u64>,
    dismissed_value: Signal<Option<String>>,
    confirming: Signal<Option<String>>,
    search: Signal<String>,
    files: Signal<Results>,
    files_loading: Signal<bool>,
    generation: Cell<u64>,
    options: RefCell<Option<Memo<Vec<AutocompleteOption>>>>,
    height: RefCell<Option<Memo<usize>>>,
    scroll: Cell<Option<Element>>,
    commands: Memo<keymap::Commands>,
}

/// `AutocompleteRef`.
#[derive(Clone)]
pub struct AutocompleteRef(Rc<AutoState>);

impl PartialEq for AutocompleteRef {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl AutocompleteRef {
    /// `visible` (untracked).
    pub fn visible(&self) -> Visible {
        self.0.visible.get_untracked()
    }
    /// `visible`, tracked.
    pub fn visible_tracked(&self) -> Visible {
        self.0.visible.get()
    }
    /// `completeQueueableCommand()`.
    pub fn complete_queueable_command(&self) -> bool {
        let s = &self.0;
        if s.visible.get_untracked() != Visible::Command
            || !s.options().get(s.selected.get_untracked()).is_some_and(|o| o.queueable)
        {
            return false;
        }
        s.select();
        true
    }
    /// `onInput(value)`.
    pub fn on_input(&self, value: &str) {
        self.0.on_input(value);
    }
}

pub struct AutocompleteProps {
    pub session_id: Option<String>,
    pub anchor: Element,
    pub input: Element,
    pub state: Rc<dyn PromptHost>,
    pub frecency: Frecency,
    pub on_ref: Rc<dyn Fn(AutocompleteRef)>,
}

/// `CustomSpeedScroll` (util/scroll.ts).
struct CustomSpeedScroll(f64);

impl ScrollAcceleration for CustomSpeedScroll {
    fn tick(&self, _now: f64) -> f64 {
        self.0
    }
    fn reset(&self) {}
}

/// `getScrollAcceleration(config)`.
fn scroll_acceleration(config: &crate::config::Resolved) -> Rc<dyn ScrollAcceleration> {
    if config.scroll.as_ref().and_then(|s| s.acceleration) == Some(true) {
        return Rc::new(MacOSScrollAccel::new(None, None, None));
    }
    if let Some(speed) = config.scroll.as_ref().and_then(|s| s.speed) {
        return Rc::new(CustomSpeedScroll(speed));
    }
    Rc::new(CustomSpeedScroll(3.0))
}

/// `moveSelection(selected, { count, delta, policy: "wrap" })`.
fn move_selection(selected: usize, count: usize, delta: i64) -> usize {
    if count == 0 {
        return 0;
    }
    let next = selected as i64 + delta;
    if next < 0 {
        return count - 1;
    }
    if next >= count as i64 {
        return 0;
    }
    next as usize
}

/// `revealSelectionOffset(offset, { count, limit, selected })`.
fn reveal_selection_offset(offset: f64, count: usize, limit: usize, selected: usize) -> f64 {
    let max = count.saturating_sub(limit) as f64;
    if (selected as f64) < offset {
        return max.min(selected as f64);
    }
    if selected as f64 >= offset + limit as f64 {
        return max.min((selected + 1 - limit) as f64);
    }
    0f64.max(max.min(offset))
}

/// `reconcileSelectionWindow(selected, { count, limit, offset })`.
fn reconcile_selection_window(selected: usize, count: usize, limit: usize, offset: f64) -> usize {
    if count == 0 {
        return 0;
    }
    let first = (offset.max(0.0) as usize).min(count - 1);
    let last = (count - 1).min(first + limit.max(1) - 1);
    selected.clamp(first, last)
}

impl AutoState {
    fn options(&self) -> Vec<AutocompleteOption> {
        self.options.borrow().as_ref().map(|m| m.get_untracked()).unwrap_or_default()
    }

    fn height(&self) -> usize {
        self.height.borrow().as_ref().map(|m| m.get_untracked()).unwrap_or(1)
    }

    /// The editor cursor at an offset (`input.cursorOffset = offset; input.logicalCursor`).
    fn logical_at(&self, ta: &Textarea, offset: u32) -> (u32, u32) {
        let current = ta.cursor_offset();
        ta.set_cursor_offset(offset);
        let at = ta.logical_cursor();
        ta.set_cursor_offset(current);
        (at.row, at.col)
    }

    /// `insertPart(text, part)`: the trigger and query replaced by `@text` and its extmark.
    fn insert_part(&self, text: &str, part: Part) {
        if let Part::Skill(skill) = &part
            && self.host.has_skill(&skill.id)
        {
            return;
        }
        let ta = self.host.textarea();
        let cursor = ta.cursor_offset();
        let needs_space = ta.get_text_range(cursor, cursor + 1) != " ";
        let append = format!("@{text}{}", if needs_space { " " } else { "" });
        let index = self.index.get_untracked().max(0) as u32;
        let start = self.logical_at(&ta, index);
        ta.set_cursor_offset(cursor);
        let end = ta.logical_cursor();
        ta.delete_range(start.0, start.1, end.row, end.col);
        ta.insert_text(&append);
        let virtual_text = format!("@{text}");
        let extmark_start = index as i64;
        let extmark_end = extmark_start + measure(&virtual_text, ta.width_method());
        let kind = match &part {
            Part::File(..) => PartKind::File,
            Part::Agent(_) => PartKind::Agent,
            Part::Skill(_) => PartKind::Skill,
        };
        let id = self.host.create_part_extmark(extmark_start, extmark_end, kind);
        let mention = Range { start: extmark_start, end: extmark_end, text: virtual_text };
        let host = self.host.clone();
        let frecency_path = match &part {
            Part::File(_, path) => path.clone(),
            _ => None,
        };
        let mut part = Some(part);
        self.host.update_prompt(&mut |draft| {
            let Some(part) = part.take() else { return };
            match part {
                Part::File(mut file, _) => {
                    let files = draft.files.get_or_insert_with(Vec::new);
                    if let Some(existing) = files.iter_mut().find(|f| f.uri == file.uri) {
                        if let Some(m) = existing.mention.as_mut() {
                            *m = mention.clone();
                        }
                        return;
                    }
                    if file.mention.is_some() {
                        file.mention = Some(mention.clone());
                    }
                    let index = files.len();
                    files.push(file);
                    host.set_extmark(PartRef::File(index), id);
                }
                Part::Skill(mut skill) => {
                    let skills = draft.skills.get_or_insert_with(Vec::new);
                    if skills.iter().any(|s| s.id == skill.id) {
                        return;
                    }
                    if skill.mention.is_some() {
                        skill.mention = Some(mention.clone());
                    }
                    let index = skills.len();
                    skills.push(skill);
                    host.set_extmark(PartRef::Skill(index), id);
                }
                Part::Agent(mut agent) => {
                    let agents = draft.agents.get_or_insert_with(Vec::new);
                    if agent.mention.is_some() {
                        agent.mention = Some(mention.clone());
                    }
                    let index = agents.len();
                    agents.push(agent);
                    host.set_extmark(PartRef::Agent(index), id);
                }
            }
        });
        if let Some(path) = frecency_path {
            self.frecency.update_frecency(&path);
        }
    }

    /// `insertDirectory(directory)`.
    fn insert_directory(&self, directory: &str) {
        let ta = self.host.textarea();
        let cursor = ta.cursor_offset();
        let start = self.logical_at(&ta, self.index.get_untracked().max(0) as u32);
        ta.set_cursor_offset(cursor);
        let end = ta.logical_cursor();
        ta.delete_range(start.0, start.1, end.row, end.col);
        ta.insert_text(directory);
    }

    /// `insertSlash(name)`.
    fn insert_slash(&self, name: &str) {
        let ta = self.host.textarea();
        let text = format!("/{name} ");
        let cursor = ta.logical_cursor();
        ta.delete_range(0, 0, cursor.row, cursor.col);
        ta.insert_text(&text);
        ta.set_cursor_offset(measure(&text, ta.width_method()) as u32);
    }

    fn move_by(&self, direction: i64) {
        if self.visible.get_untracked() == Visible::None {
            return;
        }
        let count = self.options().len();
        if count == 0 {
            return;
        }
        self.move_to(move_selection(self.selected.get_untracked(), count, direction));
    }

    fn move_to(&self, next: usize) {
        if next != self.selected.get_untracked() {
            self.confirming.set(None);
        }
        self.selected.set(next);
        let Some(scroll) = self.scroll.get().filter(|s| s.is_alive()) else { return };
        let count = self.options().len();
        let top = scroll.scroll_top();
        let offset = reveal_selection_offset(top, count, self.height().min(count), next);
        if offset == top {
            return;
        }
        scroll.scroll_by(offset - top, ScrollUnit::Absolute);
    }

    fn sync_selection_window(&self) {
        let Some(scroll) = self.scroll.get().filter(|s| s.is_alive()) else { return };
        let count = self.options().len();
        let selected = reconcile_selection_window(self.selected.get_untracked(), count, self.height().min(count), scroll.scroll_top());
        if selected == self.selected.get_untracked() {
            return;
        }
        self.confirming.set(None);
        self.selected.set(selected);
    }

    fn select(&self) {
        let options = self.options();
        let Some(selected) = options.get(self.selected.get_untracked()).cloned() else { return };
        if self.visible.get_untracked() != Visible::Directory {
            self.hide(true);
            if let Some(on_select) = &selected.on_select {
                on_select();
            }
            return;
        }
        if let Some(on_select) = &selected.on_select {
            on_select();
        }
        self.dismissed_value.set(Some(self.host.textarea().plain_text()));
        self.hide(true);
    }

    fn trigger_destructive(&self) -> bool {
        let options = self.options();
        let Some(action) = options.get(self.selected.get_untracked()).and_then(|o| o.destructive.clone()) else { return false };
        if self.confirming.get_untracked().as_deref() != Some(action.id.as_str()) {
            self.confirming.set(Some(action.id));
            return true;
        }
        (action.run)();
        let selected = self.selected.get_untracked().min(options.len().saturating_sub(2));
        self.selected.set(selected);
        self.confirming.set(None);
        true
    }

    fn expand_directory(&self) {
        let options = self.options();
        let Some(selected) = options.get(self.selected.get_untracked()) else { return };
        let ta = self.host.textarea();
        let cursor = ta.cursor_offset();
        let display = selected.value.clone().unwrap_or_else(|| selected.display.clone()).trim_end().to_string();
        let path = display.strip_prefix('@').unwrap_or(&display).to_string();
        if self.visible.get_untracked() == Visible::Directory {
            self.insert_directory(&if path.ends_with('/') { path.clone() } else { format!("{path}/") });
            self.selected.set(0);
            return;
        }
        let start = self.logical_at(&ta, self.index.get_untracked().max(0) as u32);
        ta.set_cursor_offset(cursor);
        let end = ta.logical_cursor();
        ta.delete_range(start.0, start.1, end.row, end.col);
        ta.insert_text(&format!("@{}", if path.ends_with('/') { path.clone() } else { format!("{path}/") }));
        self.selected.set(0);
    }

    fn show(&self, mode: Visible, index: Option<i64>) {
        let index = index.unwrap_or_else(|| self.host.textarea().cursor_offset() as i64);
        batch(|| {
            self.visible.set(mode);
            self.index.set(index);
        });
    }

    fn hide(&self, remove_token: bool) {
        if remove_token && self.visible.get_untracked() == Visible::Command {
            let ta = self.host.textarea();
            let cursor = ta.cursor_offset();
            let start = self.logical_at(&ta, self.index.get_untracked().max(0) as u32);
            ta.set_cursor_offset(cursor);
            let end = ta.logical_cursor();
            ta.delete_range(start.0, start.1, end.row, end.col);
            // The prompt store follows at once (onContentChange is deferred).
            let text = ta.plain_text();
            self.host.update_prompt(&mut |draft| draft.text = text.clone());
        }
        self.confirming.set(None);
        self.visible.set(Visible::None);
    }

    fn on_input(&self, value: &str) {
        if self.dismissed_value.get_untracked().as_deref() == Some(value) {
            return;
        }
        self.dismissed_value.set(None);
        let ta = self.host.textarea();
        let offset = ta.cursor_offset();
        let before = ta.get_text_range(0, offset);
        let width_method = ta.width_method();
        if let Some(index) = self.slash_argument_autocomplete(&before, width_method) {
            self.show(Visible::Directory, Some(index));
            return;
        }
        if self.visible.get_untracked() != Visible::None {
            let index = self.index.get_untracked();
            // Typed text before the trigger, or a space between the trigger and the cursor.
            if offset as i64 <= index || has_whitespace(&ta.get_text_range(index.max(0) as u32, offset)) {
                self.hide(false);
            }
            return;
        }
        // Reopen after a backspace deleted a space, for instance.
        if offset == 0 {
            return;
        }
        if slash_trigger_index(&before, width_method) == Some(0) {
            self.show(Visible::Command, None);
            self.index.set(0);
            return;
        }
        if let Some(index) = mention_trigger_index(&before, width_method) {
            self.show(Visible::Reference, None);
            self.index.set(index);
        }
    }

    /// `slashArgumentAutocomplete(beforeCursor, widthMethod, commands, argumentAutocomplete)`: the offset after
    /// `/name ` of a command whose argument completes as a directory.
    fn slash_argument_autocomplete(&self, before: &str, width_method: opencode_tui_text::WidthMethod) -> Option<i64> {
        let head = parse_slash_head(before, true)?;
        if head.end == before.len() {
            return None;
        }
        let commands = self.commands.get_untracked();
        let command = commands.iter().find(|c| {
            c.slash.as_ref().is_some_and(|s| s.arguments && (s.name == head.name || s.aliases.contains(&head.name)))
        })?;
        if !self.host.argument_autocomplete(command) {
            return None;
        }
        let sep = before[head.end..].chars().next().map_or(1, char::len_utf8);
        Some(measure(&before[..head.end + sep], width_method))
    }
}

/// A part `insertPart` inserts.
enum Part {
    File(PromptFile, Option<String>),
    Agent(PromptAgent),
    Skill(PromptSkill),
}

/// `createFilePart(item, filePath, lineRange)`: the mention's name and its file part.
fn create_file_part(item_path: &str, directory: bool, file_path: &str, range: Option<LineRange>) -> (String, PromptFile) {
    let range = range.filter(|_| !directory);
    let filename = match range {
        Some(range) => format!(
            "{item_path}#{}{}",
            range.start_line,
            range.end_line.map(|e| format!("-{e}")).unwrap_or_default()
        ),
        None => item_path.to_string(),
    };
    let mut uri = path_to_file_url(file_path);
    if directory && !uri.ends_with('/') && file_path.ends_with('/') {
        uri.push('/');
    }
    if let Some(range) = range {
        uri.push_str(&format!("?start={}", range.start_line));
        if let Some(end) = range.end_line {
            uri.push_str(&format!("&end={end}"));
        }
    }
    (
        filename.clone(),
        PromptFile { uri, name: Some(filename), description: None, path: None, mention: Some(Range::default()) },
    )
}

/// `directoryAutocompleteSearch(query, directory, home)`: where to list, the value prefix and the name query.
fn directory_search(query: &str, directory: &str, home: &str) -> (String, String, String) {
    fn split(query: &str, root: &str, prefix: &str) -> (String, String, String) {
        match query.rfind('/') {
            None => (root.to_string(), prefix.to_string(), query.to_string()),
            Some(separator) => {
                let parent = &query[..separator + 1];
                (resolve_path(root, parent), format!("{prefix}{parent}"), query[separator + 1..].to_string())
            }
        }
    }
    if query == "~" {
        return (home.to_string(), "~/".to_string(), String::new());
    }
    if let Some(rest) = query.strip_prefix("~/") {
        return split(rest, home, "~/");
    }
    let parents = !query.is_empty() && query.split('/').all(|p| p == "..");
    if parents {
        return (resolve_path(directory, query), format!("{query}/"), String::new());
    }
    if let Some(rest) = query.strip_prefix('/') {
        return split(rest, "/", "/");
    }
    split(query, directory, "")
}

/// `directoryAutocomplete(file, location, query, home)`: the exact value, then the matching subdirectories.
async fn directory_autocomplete(
    client: opencode_tui_sync::client::Client,
    directory: String,
    query: String,
    home: String,
) -> Result<Vec<(String, String)>, opencode_tui_sync::client::Error> {
    let (dir, prefix, name) = directory_search(&query, &directory, &home);
    let input = obj! { "location" => obj! { "directory" => directory.as_str() }, "path" => dir.as_str() };
    let result = client.call_named("file.list", &input, RequestOptions::default()).await?.json();
    let exact = (!query.is_empty() && !prefix.is_empty() && name.is_empty()).then(|| (query.clone(), dir.clone()));
    let base = result.get("location").and_then(|l| l.str("directory")).unwrap_or(&directory).to_string();
    let mut out: Vec<(String, String)> = exact.into_iter().collect();
    for item in array(&result) {
        if get_str(item, "type") != Some("directory") {
            continue;
        }
        let absolute = resolve_path(&base, get_str(item, "path").unwrap_or_default());
        let basename = absolute.rsplit('/').next().unwrap_or("").to_string();
        let value = basename.trim_start_matches('/');
        if name.is_empty() && value.starts_with('.') {
            continue;
        }
        if !value.to_lowercase().starts_with(&name.to_lowercase()) {
            continue;
        }
        out.push((format!("{prefix}{}", format!("{basename}/").trim_start_matches('/')), absolute));
    }
    Ok(out)
}

/// `<Autocomplete>`.
pub fn autocomplete(props: AutocompleteProps) -> View {
    let t = tui();
    let theme = t.theme;
    let dimensions = t.dimensions;
    let store = t.data.store.clone();
    let client = t.data.client.clone();
    let new_designs = t.config.new_designs();
    let home = t.paths.home.clone();
    let cwd = t.paths.cwd.clone();
    let state = Rc::new(AutoState {
        host: props.state.clone(),
        anchor: props.anchor,
        input: props.input,
        frecency: props.frecency.clone(),
        index: create_signal(0i64).0,
        selected: create_signal(0usize).0,
        visible: create_signal(Visible::None).0,
        position_tick: create_signal(0u64).0,
        dismissed_value: create_signal(None).0,
        confirming: create_signal(None).0,
        search: create_signal(String::new()).0,
        files: create_signal(Results::default()).0,
        files_loading: create_signal(false).0,
        generation: Cell::new(0),
        options: RefCell::new(None),
        height: RefCell::new(None),
        scroll: Cell::new(None),
        commands: keymap::use_commands(),
    });
    let s = state.clone();

    {
        let s = s.clone();
        create_effect(move || {
            if s.visible.get() == Visible::None {
                return;
            }
            let pop = keymap::mode_push("autocomplete", None);
            on_cleanup(pop);
        });
    }
    {
        let s = s.clone();
        create_effect(move || {
            if s.visible.get() == Visible::None {
                return;
            }
            let last = Rc::new(Cell::new((0i32, 0i32, 0f32)));
            let stopped = Rc::new(Cell::new(false));
            fn tick(s: Rc<AutoState>, last: Rc<Cell<(i32, i32, f32)>>, stopped: Rc<Cell<bool>>) {
                renderer::set_timeout(50, move || {
                    if stopped.get() || !s.anchor.is_alive() {
                        return;
                    }
                    let current = (s.anchor.x(), s.anchor.y(), s.anchor.width());
                    if current != last.get() {
                        last.set(current);
                        s.position_tick.set(s.position_tick.get_untracked() + 1);
                    }
                    tick(s, last, stopped);
                });
            }
            tick(s.clone(), last, stopped.clone());
            on_cleanup(move || stopped.set(true));
        });
    }
    let position = {
        let s = s.clone();
        create_memo(move || {
            if s.visible.get() == Visible::None {
                return (0i32, 0i32, 0f32);
            }
            dimensions.get();
            s.position_tick.get();
            let anchor = s.anchor;
            let parent = anchor.parent();
            let (px, py) = parent.map(|p| (p.x(), p.y())).unwrap_or((0, 0));
            (anchor.x() - px, anchor.y() - py, anchor.width())
        })
    };
    let filter = {
        let s = s.clone();
        create_memo(move || {
            let visible = s.visible.get();
            if visible == Visible::None {
                return None;
            }
            // Tracks the prompt's text; the cursor and the text are read as they are.
            s.host.value();
            let ta = s.host.textarea();
            let index = s.index.get();
            let from = if visible == Visible::Directory { index } else { index + 1 };
            Some(ta.get_text_range(from.max(0) as u32, ta.cursor_offset()))
        })
    };
    {
        let s = s.clone();
        create_effect(move || {
            let next = filter.get();
            s.search.set(next.unwrap_or_default());
        });
    }

    let references = {
        let store = store.clone();
        let s = s.clone();
        create_memo(move || {
            let location = s.host.location();
            store.location.reference.list(Some(&location)).map(|r| array(&r).to_vec()).unwrap_or_default()
        })
    };
    let reference_match = {
        let s = s.clone();
        create_memo(move || {
            if s.visible.get() != Visible::Reference {
                return None;
            }
            let (base, _) = parse_file_line_range(&s.search.get());
            let alias = base.split('/').next().unwrap_or("").to_string();
            references.with(|refs| {
                refs.iter()
                    .find(|item| !item.bool("hidden").unwrap_or(false) && item.str("name") == Some(alias.as_str()))
                    .cloned()
            })
        })
    };

    // The files resource (`createResource`): re-fetched when the query, the location or the mode changes; only the
    // latest fetch's result lands.
    {
        let s = s.clone();
        let client = client.clone();
        let home = home.clone();
        let cwd = cwd.clone();
        create_effect(move || {
            let query = s.search.get();
            let location = s.host.location();
            let visible = s.visible.get();
            let matched = untrack(|| reference_match.get().is_some());
            let generation = s.generation.get() + 1;
            s.generation.set(generation);
            if visible == Visible::None || visible == Visible::Command || matched {
                s.files.set(Results { options: vec![], failed: false, mode: Some(visible), query, resolved: true });
                s.files_loading.set(false);
                return;
            }
            s.files_loading.set(true);
            let width = (s.anchor.width() as i64 - 4).max(0);
            let (base, range) = parse_file_line_range(&query);
            let directory = location.str("directory").map(str::to_string);
            let s = s.clone();
            let client = client.clone();
            let home = home.clone();
            let cwd = cwd.clone();
            tokio::task::spawn_local(async move {
                let result = if visible == Visible::Directory {
                    directory_autocomplete(client, directory.unwrap_or(cwd), base, home).await.map(|items| {
                        items
                            .into_iter()
                            .map(|(value, absolute)| {
                                let insert_value = value.clone();
                                let host = s.clone();
                                AutocompleteOption {
                                    display: truncate_middle(&value, width),
                                    value: Some(value.clone()),
                                    is_directory: true,
                                    path: Some(value),
                                    absolute: Some(absolute),
                                    on_select: Some(Rc::new(move || host.insert_directory(&insert_value))),
                                    ..AutocompleteOption::default()
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                } else {
                    let mut input = obj! { "query" => base.as_str(), "limit" => 20.0 };
                    input.set("location", obj! { "directory" => directory.clone() });
                    match client.call_named("file.find", &input, RequestOptions::default()).await {
                        Ok(value) => {
                            let result = value.json();
                            let root = result.get("location").and_then(|l| l.str("directory")).unwrap_or("").to_string();
                            Ok(array(&result)
                                .iter()
                                .map(|item| {
                                    let path = get_str(item, "path").unwrap_or_default().to_string();
                                    let is_directory = get_str(item, "type") == Some("directory");
                                    let (filename, part) = create_file_part(&path, is_directory, &format!("{root}/{path}"), range);
                                    let host = s.clone();
                                    let name = filename.clone();
                                    let frecency = path.clone();
                                    AutocompleteOption {
                                        display: truncate_middle(&filename, width),
                                        value: Some(filename),
                                        is_directory,
                                        path: Some(path),
                                        on_select: Some(Rc::new(move || {
                                            host.insert_part(&name, Part::File(part.clone(), Some(frecency.clone())))
                                        })),
                                        ..AutocompleteOption::default()
                                    }
                                })
                                .collect::<Vec<_>>())
                        }
                        Err(error) => Err(error),
                    }
                };
                if s.generation.get() != generation {
                    return;
                }
                match result {
                    Ok(options) => s.files.set(Results { options, failed: false, mode: Some(visible), query, resolved: true }),
                    Err(_) => {
                        let previous = s.files.get_untracked();
                        if previous.mode == Some(visible) {
                            s.files.set(Results { failed: true, ..previous });
                        } else {
                            s.files.set(Results { options: vec![], failed: true, mode: Some(visible), query, resolved: false });
                        }
                    }
                }
                s.files_loading.set(false);
                renderer::request_render();
            });
        });
    }
    let visible_files = {
        let s = s.clone();
        create_memo(move || {
            let value = s.files.get();
            if value.mode == Some(s.visible.get()) {
                return value;
            }
            Results::default()
        })
    };

    let agents = {
        let store = store.clone();
        let s = s.clone();
        create_memo(move || {
            let location = s.host.location();
            let list = store.location.agent.list(Some(&location)).map(|r| array(&r).to_vec()).unwrap_or_default();
            list.iter()
                .filter(|agent| !agent.bool("hidden").unwrap_or(false) && agent.str("mode") != Some("primary"))
                .map(|agent| {
                    let id = agent.str("id").unwrap_or_default().to_string();
                    let host = s.clone();
                    let name = id.clone();
                    AutocompleteOption {
                        display: format!("@{id}"),
                        kind: Some("agent"),
                        on_select: Some(Rc::new(move || {
                            host.insert_part(&name, Part::Agent(PromptAgent { name: name.clone(), mention: Some(Range::default()) }))
                        })),
                        ..AutocompleteOption::default()
                    }
                })
                .collect::<Vec<_>>()
        })
    };
    let skill_options = {
        let store = store.clone();
        let s = s.clone();
        create_memo(move || {
            let location = s.host.location();
            let list = store.location.skill.list(Some(&location)).map(|r| array(&r).to_vec()).unwrap_or_default();
            list.iter()
                .map(|skill| {
                    let id = skill.str("id").unwrap_or_default().to_string();
                    let host = s.clone();
                    let name = id.clone();
                    AutocompleteOption {
                        display: format!("@{id}"),
                        description: skill.str("description").map(str::to_string),
                        kind: Some("skill"),
                        on_select: Some(Rc::new(move || {
                            host.insert_part(&name, Part::Skill(PromptSkill { id: name.clone(), mention: Some(Range::default()) }))
                        })),
                        ..AutocompleteOption::default()
                    }
                })
                .collect::<Vec<_>>()
        })
    };
    let reference_aliases = {
        let s = s.clone();
        create_memo(move || {
            references.with(|refs| {
                refs.iter()
                    .filter(|r| !r.bool("hidden").unwrap_or(false))
                    .map(|reference| {
                        let name = reference.str("name").unwrap_or_default().to_string();
                        let source = reference.get("source");
                        let detail = if source.and_then(|s| s.str("type")) == Some("git") {
                            source.and_then(|s| s.str("repository")).unwrap_or_default()
                        } else {
                            source.and_then(|s| s.str("path")).unwrap_or_default()
                        };
                        let host = s.clone();
                        let path = reference.str("path").unwrap_or_default().to_string();
                        let alias = name.clone();
                        AutocompleteOption {
                            display: format!("@{name}"),
                            kind: Some("reference"),
                            description: Some(format!(" {detail}")),
                            on_select: Some(Rc::new(move || {
                                let file = PromptFile {
                                    uri: path_to_file_url(&path),
                                    name: Some(alias.clone()),
                                    mention: Some(Range::default()),
                                    ..PromptFile::default()
                                };
                                host.insert_part(&alias, Part::File(file, Some(alias.clone())))
                            })),
                            ..AutocompleteOption::default()
                        }
                    })
                    .collect::<Vec<_>>()
            })
        })
    };
    let commands = {
        let store = store.clone();
        let s = s.clone();
        create_memo(move || {
            let mut results: Vec<AutocompleteOption> = Vec::new();
            for command in s.commands.get().iter() {
                let Some(slash) = command.slash.as_ref() else { continue };
                for name in std::iter::once(&slash.name).chain(slash.aliases.iter()) {
                    let on_select: Rc<dyn Fn()> = if slash.arguments {
                        let host = s.clone();
                        let name = name.clone();
                        Rc::new(move || host.insert_slash(&name))
                    } else {
                        let command = command.clone();
                        Rc::new(move || command.invoke(None))
                    };
                    results.push(AutocompleteOption {
                        display: format!("/{name}"),
                        description: command.description.clone().or_else(|| command.title.clone()),
                        on_select: Some(on_select),
                        ..AutocompleteOption::default()
                    });
                }
            }
            let location = s.host.location();
            for server in store.location.command.list(Some(&location)).map(|r| array(&r).to_vec()).unwrap_or_default() {
                let name = server.str("name").unwrap_or_default().to_string();
                let host = s.clone();
                let insert = name.clone();
                results.push(AutocompleteOption {
                    display: format!("/{name}"),
                    description: server.str("description").map(str::to_string),
                    queueable: true,
                    on_select: Some(Rc::new(move || host.insert_slash(&insert))),
                    ..AutocompleteOption::default()
                });
            }
            results.sort_by(|a, b| crate::prompt::text::locale_compare(&a.display, &b.display));
            let max = results.iter().map(|r| crate::prompt::text::js_len(&r.display)).max();
            let Some(max) = max.filter(|m| *m > 0) else { return results };
            for item in results.iter_mut() {
                let pad = (max + 2).saturating_sub(crate::prompt::text::js_len(&item.display));
                item.display.push_str(&" ".repeat(pad));
            }
            results
        })
    };
    let supplemental = {
        let s = s.clone();
        create_memo(move || {
            let results = visible_files.get();
            if s.visible.get() != Visible::Directory || !results.resolved {
                return vec![];
            }
            let width = (s.anchor.width() as i64 - 4).max(0);
            s.host
                .directory_options(&results.query)
                .into_iter()
                .map(|item| {
                    let value = item.value.clone();
                    let host = s.clone();
                    AutocompleteOption {
                        display: truncate_middle(&item.display, width),
                        on_select: item.on_select.clone().or_else(|| {
                            value.map(|v| Rc::new(move || host.insert_directory(&v)) as Rc<dyn Fn()>)
                        }),
                        ..item
                    }
                })
                .collect::<Vec<_>>()
        })
    };
    let options = {
        let s = s.clone();
        create_memo(move || {
            let file_search = visible_files.get();
            let matched = reference_match.get();
            let agents = agents.get();
            let aliases = reference_aliases.get();
            let commands = commands.get();
            let search = s.search.get();
            let visible = s.visible.get();
            if visible == Visible::Directory {
                let supplemental = supplemental.get();
                let paths: Vec<Option<String>> = supplemental.iter().map(|i| i.absolute.clone()).collect();
                return supplemental
                    .into_iter()
                    .chain(file_search.options.into_iter().filter(|i| !paths.contains(&i.absolute)))
                    .collect();
            }
            if visible == Visible::Reference
                && let Some(matched) = matched
            {
                let name = format!("@{}", matched.str("name").unwrap_or_default());
                return aliases.into_iter().filter(|item| item.display == name).collect();
            }
            // Files come from the server already ranked and filtered.
            let file_options = if visible == Visible::Reference { file_search.options } else { vec![] };
            let non_file: Vec<AutocompleteOption> = if visible == Visible::Reference {
                skill_options.get().into_iter().chain(aliases).chain(agents).collect()
            } else if s.index.get() == 0 {
                commands
            } else {
                vec![]
            };
            if search.is_empty() {
                return non_file.into_iter().chain(file_options).collect();
            }
            let needle = strip_file_line_range(&search);
            let frecency = s.frecency.clone();
            let prefix = match visible {
                Visible::Reference => "@",
                Visible::Command => "/",
                _ => "",
            };
            let display_key = |o: &AutocompleteOption| Some(strip_file_line_range(o.value.as_deref().unwrap_or(&o.display).trim_end()));
            let description_key = |o: &AutocompleteOption| o.description.clone();
            let aliases_key = |o: &AutocompleteOption| Some(o.aliases.join(" "));
            let keys: Vec<crate::fuzzysort::Key<'_, AutocompleteOption>> = if visible == Visible::Command {
                vec![&display_key, &description_key, &aliases_key]
            } else {
                vec![&display_key, &aliases_key]
            };
            let starts = format!("{prefix}{search}");
            let score_fn = |r: &crate::fuzzysort::KeysResult| {
                let mut score = r.score();
                if r.keys.first().is_some_and(|k| k.target.starts_with(&starts)) {
                    score *= 2.0;
                }
                let path = non_file[r.index].path.clone();
                let frecency = path.map(|p| frecency.get_frecency(&p)).unwrap_or(0.0);
                score * (1.0 + frecency)
            };
            let fuzzied = crate::fuzzysort::go_keys(
                &needle,
                &non_file,
                &keys,
                &crate::fuzzysort::Options {
                    threshold: if visible == Visible::Reference { 0.5 } else { 0.0 },
                    limit: Some(10),
                    score_fn: Some(&score_fn),
                },
            );
            let mut out: Vec<AutocompleteOption> = fuzzied.into_iter().map(|r| non_file[r.index].clone()).collect();
            out.extend(file_options);
            out.truncate(10);
            out
        })
    };
    *state.options.borrow_mut() = Some(options);
    {
        let s = s.clone();
        create_effect(move || {
            filter.get();
            s.selected.set(0);
            s.confirming.set(None);
        });
    }

    // The autocomplete's key layer.
    {
        let s = s.clone();
        let target: Rc<dyn Fn() -> Option<Element>> = {
            let input = s.input;
            Rc::new(move || Some(input))
        };
        create_layer(move || {
            let s1 = s.clone();
            let s2 = s.clone();
            let s3 = s.clone();
            let s4 = s.clone();
            let s5 = s.clone();
            let s6 = s.clone();
            let s7 = s.clone();
            let visible = s.visible;
            Layer {
                mode: Some("autocomplete"),
                target: Some(target.clone()),
                enabled: Some(Enabled::Fn(Rc::new(move || visible.get_untracked() != Visible::None))),
                bindings: vec!["prompt.queue"],
                commands: vec![
                    Command::new("prompt.autocomplete.prev", move || s1.move_by(-1)).title("Previous autocomplete item").group("Autocomplete"),
                    Command::new("prompt.autocomplete.next", move || s2.move_by(1)).title("Next autocomplete item").group("Autocomplete"),
                    Command::new("prompt.autocomplete.hide", move || s3.hide(false)).title("Hide autocomplete").group("Autocomplete"),
                    Command::new("prompt.clear", move || s4.hide(true)).title("Dismiss autocomplete").group("Autocomplete"),
                    Command::new("prompt.autocomplete.select", move || s5.select()).title("Select autocomplete item").group("Autocomplete"),
                    Command::new("prompt.autocomplete.complete", move || {
                        let options = s6.options();
                        if options.get(s6.selected.get_untracked()).is_some_and(|o| o.is_directory) {
                            s6.expand_directory();
                            return;
                        }
                        s6.select();
                    })
                    .title("Complete autocomplete item")
                    .group("Autocomplete"),
                    Command::with_input("prompt.autocomplete.destructive", move |_| s7.trigger_destructive())
                        .title("Confirm autocomplete action")
                        .group("Autocomplete")
                        .bind(crate::keymap::Bind::Key("ctrl+d".into())),
                ],
                ..Layer::default()
            }
        });
    }

    // `ref`.
    {
        let reference = AutocompleteRef(state.clone());
        let on_ref = props.on_ref.clone();
        on_mount(move || on_ref(reference));
    }

    let height = {
        let s = s.clone();
        create_memo(move || {
            let count = options.with(|o| o.len()).max(1);
            if s.visible.get() == Visible::None {
                return count.min(10);
            }
            s.position_tick.get();
            count.min(10).min((s.anchor.y().max(1)) as usize)
        })
    };
    *state.height.borrow_mut() = Some(height);
    let empty_message = {
        let s = s.clone();
        create_memo(move || {
            let search = visible_files.get();
            let loading = s.files_loading.get();
            match s.visible.get() {
                Visible::Command => "No matching commands",
                Visible::Directory if loading => "Searching…",
                Visible::Directory if search.failed => "Could not search directories. Keep typing to try again.",
                Visible::Directory => "No matching directories",
                _ if loading => "Searching…",
                _ if search.failed => "Could not search files. Keep typing to try again.",
                _ => "No matching files, agents, or references",
            }
            .to_string()
        })
    };
    let empty_error = {
        let s = s.clone();
        create_memo(move || s.visible.get() == Visible::Reference && !s.files_loading.get() && visible_files.with(|f| f.failed))
    };
    let row_background = move || if new_designs { Some(rgba(theme.with(|th| th.background.raised.high))) } else { None };

    let root = box_();
    {
        let s = s.clone();
        bind(move || s.visible.get() != Visible::None, move |v| root.set_visible(v));
    }
    root.set_position_type(true);
    bind(move || (position.get(), height.get()), move |((x, y, w), h)| {
        root.set_positions(Some(Some(Value::Point(x as f32))), Some(Some(Value::Point((y - h as i32) as f32))), None, None);
        root.set_width(Some(Value::Point(w)));
    });
    root.set_z_index(100);
    root.set_border(BorderProp::Sides(if new_designs { vec![Side::Right] } else { vec![Side::Left, Side::Right] }));
    root.set_custom_border_chars(Some(border_chars_to_array(&BorderCharacters { vertical: "┃", ..EMPTY })));
    bind_always(move || rgba(theme.with(|th| th.border.base)), move |c| root.set_border_color(Some(c)));

    let scroll = scrollbox();
    insert_node(root, scroll, None);
    state.scroll.set(Some(scroll));
    {
        let s = s.clone();
        scroll.on_scroll_change(true, Rc::new(move |_| s.sync_selection_window()));
    }
    if !new_designs {
        bind_always(move || rgba(theme.with(|th| th.background.raised.high)), move |c| scroll.set_background_color(Some(c)));
    }
    bind(move || height.get() as f32, move |h| scroll.set_height(Some(Value::Point(h))));
    scroll.set_scrollbar_options(Some(true), &ScrollBarOptions { visible: Some(Some(false)), ..ScrollBarOptions::default() });
    scroll.set_scroll_acceleration(scroll_acceleration(&t.config));

    let fallback = {
        Rc::new(move || {
            let el = box_();
            el.set_padding(Edge::Left, Some(Value::Point(1.0)));
            el.set_padding(Edge::Right, Some(Value::Point(1.0)));
            if new_designs {
                el.set_flex_grow(Some(1.0));
            }
            bind_always(row_background, move |c| el.set_background_color(c));
            let label = text();
            insert_node(el, label, None);
            bind_always(
                move || if empty_error.get() { rgba(theme.with(|th| th.text.feedback.error.base)) } else { rgba(theme.with(|th| th.text.muted)) },
                move |c| label.set_fg(Some(c)),
            );
            if new_designs {
                label.set_wrap_mode(WrapMode::None);
            }
            insert(
                label,
                View::dynamic(move || {
                    if new_designs {
                        View::from(truncate(&empty_message.get(), (position.get().2 as i64 - 4).max(1) as usize))
                    } else {
                        View::from(empty_message.get())
                    }
                }),
                At::Only,
            );
            rail(new_designs, None, View::Node(el))
        }) as Children
    };
    let rows = {
        let s = s.clone();
        index(
            move || options.get(),
            move |option, idx| {
                let s = s.clone();
                let confirming_action = {
                    let s = s.clone();
                    create_memo(move || {
                        let id = option.with(|o| o.destructive.as_ref().map(|d| d.id.clone()));
                        id.is_some() && id == s.confirming.get()
                    })
                };
                let label = move || option.with(|o| o.kind);
                let content_width = move || {
                    let text = label();
                    (position.get().2 as i64 - 4 - text.map(|t| string_width(t) as i64 + 2).unwrap_or(0)).max(1)
                };
                let selected = {
                    let s = s.clone();
                    move || s.selected.get() == idx
                };
                let row = box_();
                row.set_padding(Edge::Left, Some(Value::Point(1.0)));
                row.set_padding(Edge::Right, Some(Value::Point(1.0)));
                if new_designs {
                    row.set_flex_grow(Some(1.0));
                }
                {
                    let selected = selected.clone();
                    bind_always(
                        move || {
                            if confirming_action.get() {
                                return Some(rgba(theme.with(|th| th.background.action.destructive.focused)));
                            }
                            if selected() {
                                return Some(rgba(theme.with(|th| th.background.action.primary.focused)));
                            }
                            row_background()
                        },
                        move |c| row.set_background_color(c),
                    );
                }
                row.set_flex_direction(FlexDirection::Row);
                {
                    let s1 = s.clone();
                    let s2 = s.clone();
                    let s3 = s.clone();
                    row.set_on_mouse(Some(MouseEventType::Move), Some(Rc::new(move |_| s1.move_to(idx))));
                    row.set_on_mouse(Some(MouseEventType::Down), Some(Rc::new(move |_| s2.move_to(idx))));
                    row.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| s3.select())));
                }
                let display = text();
                insert_node(row, display, None);
                {
                    let selected = selected.clone();
                    bind_always(
                        move || {
                            if confirming_action.get() {
                                return rgba(theme.with(|th| th.text.action.destructive.focused));
                            }
                            if selected() {
                                return rgba(theme.with(|th| th.text.action.primary.focused));
                            }
                            rgba(theme.with(|th| th.text.base))
                        },
                        move |c| display.set_fg(Some(c)),
                    );
                }
                display.set_flex_shrink(Some(0.0));
                display.set_wrap_mode(WrapMode::None);
                insert(
                    display,
                    View::dynamic(move || {
                        let text = if confirming_action.get() {
                            option.with(|o| o.destructive.as_ref().map(|d| d.confirm.clone()).unwrap_or_default())
                        } else {
                            option.with(|o| o.display.clone())
                        };
                        View::from(truncate_middle(&text, content_width()))
                    }),
                    At::Only,
                );
                let description = {
                    let selected = selected.clone();
                    show_value(
                        move || if confirming_action.get() { None } else { option.with(|o| o.description.clone()) },
                        move |description| {
                            let el = text();
                            let selected = selected.clone();
                            bind_always(
                                move || if selected() { rgba(theme.with(|th| th.text.action.primary.focused)) } else { rgba(theme.with(|th| th.text.muted)) },
                                move |c| el.set_fg(Some(c)),
                            );
                            el.set_wrap_mode(WrapMode::None);
                            el.set_flex_shrink(Some(1.0));
                            el.set_min_width(Some(Value::Point(0.0)));
                            insert(
                                el,
                                View::dynamic(move || {
                                    let text = description.get().unwrap_or_default();
                                    let collapsed: Vec<&str> = text.split(crate::prompt::text::is_js_whitespace).filter(|p| !p.is_empty()).collect();
                                    let mut value = String::from(" ");
                                    // `" " + description.replace(/\s+/g, " ").trim()`.
                                    value.push_str(&collapsed.join(" "));
                                    View::from(value)
                                }),
                                At::Only,
                            );
                            View::Node(el)
                        },
                        None,
                    )
                };
                insert(row, description, At::End);
                let kind_selected = selected.clone();
                let kind = show_value(
                    move || if confirming_action.get() { None } else { label() },
                    move |kind| {
                        let selected = kind_selected.clone();
                        let spacer = box_();
                        spacer.set_flex_grow(Some(1.0));
                        spacer.set_min_width(Some(Value::Point(2.0)));
                        let el = text();
                        el.set_flex_shrink(Some(0.0));
                        bind_always(
                            move || if selected() { rgba(theme.with(|th| th.text.action.primary.focused)) } else { rgba(theme.with(|th| th.text.muted)) },
                            move |c| el.set_fg(Some(c)),
                        );
                        insert(el, View::dynamic(move || View::from(kind.get().unwrap_or_default())), At::Only);
                        View::list([View::Node(spacer), View::Node(el)])
                    },
                    None,
                );
                insert(row, kind, At::End);
                let rail_color = {
                    let selected = selected.clone();
                    Rc::new(move || {
                        if confirming_action.get() {
                            return Some(rgba(theme.with(|th| th.text.feedback.error.base)));
                        }
                        if selected() {
                            return Some(rgba(theme.with(|th| th.text.formfield.focused)));
                        }
                        None
                    }) as Rc<dyn Fn() -> Option<opencode_tui_core::color::RGBA>>
                };
                rail(new_designs, Some(rail_color), View::Node(row))
            },
            Some(fallback),
        )
    };
    insert(scroll, rows, At::Only);
    View::Node(root)
}

const EMPTY: BorderCharacters = BorderCharacters {
    top_left: "",
    top_right: "",
    bottom_left: "",
    bottom_right: "",
    horizontal: " ",
    vertical: "",
    top_t: "",
    bottom_t: "",
    left_t: "",
    right_t: "",
    cross: "",
};

/// `AutocompleteRail`: under new designs each row draws its own left rail (the selected row's is the selection bar).
fn rail(enabled: bool, color: Option<Rc<dyn Fn() -> Option<opencode_tui_core::color::RGBA>>>, children: View) -> View {
    if !enabled {
        return children;
    }
    let theme = tui().theme;
    let row = box_();
    row.set_flex_direction(FlexDirection::Row);
    let bar = text();
    insert_node(row, bar, None);
    bar.set_width(Some(Value::Point(1.0)));
    bar.set_flex_shrink(Some(0.0));
    bind_always(
        move || color.as_ref().and_then(|c| c()).unwrap_or_else(|| rgba(theme.with(|th| th.border.base))),
        move |c| bar.set_fg(Some(c)),
    );
    insert(bar, View::from("┃"), At::Only);
    insert(row, children, At::End);
    View::Node(row)
}
