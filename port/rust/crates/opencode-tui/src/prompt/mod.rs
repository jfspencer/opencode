//! The prompt (`component/prompt/index.tsx`): the composer's textarea with its parts (file, agent and skill
//! mentions, pasted text and attachments as extmarks), shell mode, history, stash, paste, the commands and key
//! layers it registers, submission (creating the session from home, the shell, server commands, prompts), its
//! metadata row, the bottom edge, and the footer row (`prompt.footer.status`: the location or the running
//! session's spinner; the built-in `prompt.footer` plugin).
//!
//! The submodules port `component/prompt/*` ([`autocomplete`], [`metadata`], draft stash and local attachments in
//! [`stash`] and [`attachment`]), `prompt/*` ([`history`], [`stash`], [`frecency`], [`text`], [`attachment`]) and
//! `feature-plugins/prompt/*` ([`footer`]: footer, goal, btw).
//!
//! TODO(port-tui): stand-ins until their owners land: dialogs (`dialog.replace`/`clear` and the stack: the skill,
//! stash, image preview, integration and workspace dialogs are not opened), the editor context (IDE selections),
//! `openEditor`, the clipboard (`prompt.paste` without a bracketed paste), the worktree move (`usePromptMove`: no
//! pending destination), usage limits, directory recents (`useStorage`), `tui.prompt.append` events, the terminal environment (`variables` of a local server), image
//! previews, cursor styles, and a live config (the paste summary toggle).
pub mod attachment;
pub mod autocomplete;
pub mod composer;
pub mod draft_stash;
pub mod footer;
pub mod frecency;
pub mod history;
pub mod input;
pub mod metadata;
pub mod persistence;
pub mod restore;
pub mod stash;
pub mod text;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::border::{BorderCharacters, border_chars_to_array};
use opencode_tui_core::color::RGBA;
use opencode_tui_core::elements::{box_, text, textarea};
use opencode_tui_core::input::PasteEvent;
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::renderables::textarea::EditorTraits;
use crate::prompt::input::{Textarea, strip_ansi_sequences};
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element, FlexDirection, Justify, MouseEventType};
use opencode_tui_sync::client::RequestOptions;
use opencode_tui_sync::data::{CreateInput, PromptInput};
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::obj;
use opencode_tui_sync::runtime::Deferred;
use opencode_tui_text::{ExtmarkOptions, StyleSpec, SyntaxStyle, WidthMethod};

use crate::ctx::{Route, Tui, tui};
use crate::data::{array, get_str};
use crate::keymap::{self, Bind, Command, Enabled, Layer, create_layer};
use crate::local::{Local, local};
use crate::prompt::autocomplete::{AutocompleteProps, AutocompleteRef, Visible};
use crate::prompt::history::{
    History, Mode, PartRef, PastedText, PromptFile, PromptInfo, Range, agent_json, empty_prompt, file_json, skill_json,
};
use crate::prompt::draft_stash::{DraftEntry, register_composer, save_draft, take_draft};
use crate::prompt::stash::Stash;
use crate::prompt::text::{TrackedText, measure, parse_slash_head};
use crate::toast::{ToastInput, Toasts, Variant};
use crate::util::{abbreviate_home, rgba, take_width, tint, truncate_file_path};

const DRAFT_RETENTION_MIN_CHARS: usize = 20;

/// The home route's and the session route's placeholders (`placeholders`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Placeholders {
    pub normal: Vec<String>,
    pub shell: Vec<String>,
}

/// `PromptProps`.
pub struct PromptProps {
    pub session_id: Option<String>,
    pub visible: Option<Rc<dyn Fn() -> bool>>,
    pub disabled: Rc<dyn Fn() -> bool>,
    pub muted: Option<Rc<dyn Fn() -> bool>>,
    /// `onSubmit`: a non-empty prompt was taken.
    pub on_submit: Option<Rc<dyn Fn()>>,
    /// `onEmptySubmit`: enter on an empty prompt; true when it did something.
    pub on_empty_submit: Option<Rc<dyn Fn() -> bool>>,
    /// `ref`: the handle, and `None` on cleanup.
    pub on_ref: Rc<dyn Fn(Option<PromptRef>)>,
    pub placeholders: Placeholders,
}

/// `PromptRef`: what the routes and the app call.
#[derive(Clone)]
pub struct PromptRef(Rc<State>);

impl PartialEq for PromptRef {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl PromptRef {
    pub fn focused(&self) -> bool {
        !self.0.disabled() && self.0.input.focused()
    }
    pub fn current(&self) -> PromptInfo {
        self.0.prompt.get_untracked()
    }
    /// `current` read reactively.
    pub fn current_tracked(&self) -> PromptInfo {
        self.0.prompt.get()
    }
    pub fn mode(&self) -> Mode {
        self.0.mode.get_untracked()
    }
    pub fn set_mode(&self, mode: Mode) {
        self.0.mode.set(mode);
    }
    pub fn set(&self, prompt: &PromptInfo) {
        let s = &self.0;
        s.ta.set_text(&prompt.text);
        s.prompt.set(prompt.clone());
        s.restore_extmarks_from_prompt(prompt);
        s.ta.goto_buffer_end();
    }
    pub fn reset(&self) {
        self.0.reset_composer();
    }
    pub fn blur(&self) {
        self.0.input.blur();
    }
    pub fn focus(&self) {
        if self.0.disabled() {
            return;
        }
        self.0.input.focus();
    }
    pub fn submit(&self) {
        let state = self.0.clone();
        tokio::task::spawn_local(async move {
            submit(state, Delivery::Steer).await;
        });
    }
    /// The textarea.
    pub fn input(&self) -> Element {
        self.0.input
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Delivery {
    Steer,
    Queue,
}

impl Delivery {
    fn as_str(self) -> &'static str {
        match self {
            Delivery::Steer => "steer",
            Delivery::Queue => "queue",
        }
    }
}

/// The extmark styles of the prompt's parts (`syntax().getStyleId("extmark.*")`).
#[derive(Clone, Copy, Default)]
struct Styles {
    file: Option<u32>,
    agent: Option<u32>,
    skill: Option<u32>,
    paste: Option<u32>,
}

type Task = Box<dyn FnOnce(Rc<dyn Fn() -> bool>) -> Pin<Box<dyn Future<Output = ()>>>>;

/// The prompt's state (the store, its handles and what its handlers need, taken at creation).
struct State {
    t: Tui,
    local: Local,
    toast: Toasts,
    session_id: Option<String>,
    disabled_prop: Rc<dyn Fn() -> bool>,
    muted_prop: Option<Rc<dyn Fn() -> bool>>,
    on_submit: Option<Rc<dyn Fn()>>,
    on_empty_submit: Option<Rc<dyn Fn() -> bool>>,
    enabled: crate::interactivity::Interactive,
    leader: Rc<dyn Fn() -> bool>,
    input: Element,
    ta: Textarea,
    anchor: Element,
    prompt: Signal<PromptInfo>,
    mode: Signal<Mode>,
    extmark_to_part: RefCell<HashMap<u32, PartRef>>,
    interrupt: Signal<u32>,
    placeholder: Signal<usize>,
    part_type: Cell<u32>,
    styles: Styles,
    auto: Signal<Option<AutocompleteRef>>,
    disposed: Cell<bool>,
    submitting: Cell<bool>,
    paste_queue: RefCell<VecDeque<Task>>,
    pasting: Cell<bool>,
    history: History,
    stash: Stash,
    commands: Memo<keymap::Commands>,
    pending_directory: Signal<Option<String>>,
    cursor_version: Signal<u64>,
    placeholders: Placeholders,
}

thread_local! {
    static HISTORY: RefCell<Option<History>> = const { RefCell::new(None) };
    static STASH: RefCell<Option<Stash>> = const { RefCell::new(None) };
    static FRECENCY: RefCell<Option<frecency::Frecency>> = const { RefCell::new(None) };
    /// `args.takeNewSessionID()`: the first new session created from home takes `--session`'s new ID.
    static NEW_SESSION_ID: RefCell<Option<Option<String>>> = const { RefCell::new(None) };
}

/// The app-wide prompt contexts (`PromptHistoryProvider`, `PromptStashProvider`, `FrecencyProvider`), created with
/// the first prompt.
fn contexts(t: &Tui) -> (History, Stash, frecency::Frecency) {
    let history = HISTORY.with(|h| h.borrow_mut().get_or_insert_with(|| History::new(&t.paths.state)).clone());
    let stash = STASH.with(|s| s.borrow_mut().get_or_insert_with(|| Stash::new(&t.paths.state)).clone());
    let frecency =
        FRECENCY.with(|f| f.borrow_mut().get_or_insert_with(|| frecency::Frecency::new(&t.paths.state, &t.paths.cwd)).clone());
    (history, stash, frecency)
}

pub fn frecency_context() -> Option<frecency::Frecency> {
    FRECENCY.with(|f| f.borrow().clone())
}

fn take_new_session_id(t: &Tui) -> Option<String> {
    NEW_SESSION_ID.with(|pending| pending.borrow_mut().get_or_insert_with(|| t.args.new_session_id.clone()).take())
}

fn restore_new_session_id(t: &Tui, id: String) {
    NEW_SESSION_ID.with(|pending| {
        let mut pending = pending.borrow_mut();
        let slot = pending.get_or_insert_with(|| t.args.new_session_id.clone());
        if slot.is_none() {
            *slot = Some(id);
        }
    });
}

/// `randomIndex(count)`.
fn random_index(count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let mut bytes = [0u8; 4];
    let _ = getrandom::fill(&mut bytes);
    (u32::from_le_bytes(bytes) as usize) % count
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

fn border_chars(f: impl FnOnce(&mut BorderCharacters)) -> [u32; 11] {
    let mut chars = EMPTY;
    f(&mut chars);
    border_chars_to_array(&chars)
}

/// `SyntaxStyle.fromTheme(generateSyntax(theme))` (the theme context's `currentSyntax`).
pub fn syntax_style(theme: &crate::theme::ResolvedTheme) -> SyntaxStyle {
    let style = SyntaxStyle::create();
    for rule in crate::theme::syntax::generate_syntax(theme) {
        let spec = StyleSpec {
            fg: rule.style.foreground.map(|c| rgba(c).0),
            bg: rule.style.background.map(|c| rgba(c).0),
            bold: Some(rule.style.bold),
            italic: Some(rule.style.italic),
            underline: Some(rule.style.underline),
            dim: None,
        };
        for scope in &rule.scope {
            style.register_style(scope, spec);
        }
    }
    style
}

/// `errorMessage(error)` of a client error.
pub fn error_message(error: &opencode_tui_sync::client::Error) -> String {
    error.message()
}

/// `argumentSlash(input, commands)`: a command whose slash takes arguments, and its input.
fn argument_slash(input: &str, commands: &keymap::Commands) -> Option<(Rc<Command>, String)> {
    let head = parse_slash_head(input, true)?;
    let command = commands.iter().find(|command| {
        command.slash.as_ref().is_some_and(|slash| {
            slash.arguments && (slash.name == head.name || slash.aliases.iter().any(|alias| *alias == head.name))
        })
    })?;
    Some((command.clone(), head.arguments))
}

impl State {
    fn disabled(&self) -> bool {
        (self.disabled_prop)() || !(self.enabled)()
    }

    fn muted(&self) -> bool {
        (self.leader)() || self.muted_prop.as_ref().is_some_and(|m| m())
    }

    fn width_method(&self) -> WidthMethod {
        self.ta.width_method()
    }

    fn measure(&self, text: &str) -> i64 {
        measure(text, self.width_method())
    }

    /// `currentLocation.ref ?? data.location.default()`.
    fn location(&self) -> Json {
        self.t.data.location.r#ref().unwrap_or_else(|| self.t.data.store.location.default())
    }

    fn location_directory(&self) -> String {
        self.location().str("directory").unwrap_or_default().to_string()
    }

    fn status(&self) -> &'static str {
        self.t.data.store.session.status(self.session_id.as_deref().unwrap_or("")).as_str()
    }

    fn reset_composer(&self) {
        self.ta.extmarks().clear();
        self.prompt.set(empty_prompt());
        self.extmark_to_part.borrow_mut().clear();
        self.ta.clear();
    }

    /// `clearPrompt()`: long or rich prompts go to the history first.
    fn clear_prompt(&self) {
        let prompt = self.prompt.get_untracked();
        if prompt.text.trim().chars().count() >= DRAFT_RETENTION_MIN_CHARS
            || !prompt.pasted.is_empty()
            || prompt.files.as_ref().is_some_and(|f| !f.is_empty())
            || prompt.agents.as_ref().is_some_and(|a| !a.is_empty())
        {
            self.history.append(&PromptInfo { mode: Some(self.mode.get_untracked()), ..prompt });
        }
        self.reset_composer();
    }

    fn create_extmark(&self, start: i64, end: i64, style: Option<u32>) -> u32 {
        self.ta
            .extmarks()
            .create(ExtmarkOptions {
                start,
                end,
                virtual_: true,
                style_id: style,
                type_id: Some(self.part_type.get()),
                ..ExtmarkOptions::default()
            })
            .unwrap_or(0)
    }

    /// `restoreExtmarksFromPrompt(prompt)`.
    fn restore_extmarks_from_prompt(&self, prompt: &PromptInfo) {
        self.ta.extmarks().clear();
        self.extmark_to_part.borrow_mut().clear();
        let mut parts: Vec<(Option<Range>, PartRef, Option<u32>)> = Vec::new();
        for (index, part) in prompt.files.iter().flatten().enumerate() {
            parts.push((part.mention.clone(), PartRef::File(index), self.styles.file));
        }
        for (index, part) in prompt.agents.iter().flatten().enumerate() {
            parts.push((part.mention.clone(), PartRef::Agent(index), self.styles.agent));
        }
        for (index, part) in prompt.skills.iter().flatten().enumerate() {
            parts.push((part.mention.clone(), PartRef::Skill(index), self.styles.skill));
        }
        for (index, part) in prompt.pasted.iter().enumerate() {
            parts.push((Some(part.source.clone()), PartRef::Pasted(index), self.styles.paste));
        }
        for (mention, part, style) in parts {
            let Some(mention) = mention.filter(|m| !m.text.is_empty()) else { continue };
            let id = self.create_extmark(mention.start, mention.end, style);
            self.extmark_to_part.borrow_mut().insert(id, part);
        }
    }

    /// `syncExtmarksWithPromptParts()`: the parts follow their extmarks; parts whose extmark is gone drop out.
    fn sync_extmarks_with_prompt_parts(&self) {
        let all = self.ta.extmarks().get_all_for_type_id(self.part_type.get());
        let map = self.extmark_to_part.borrow().clone();
        let mut new_map: HashMap<u32, PartRef> = HashMap::new();
        let mut file_extmarks: Vec<(u32, PromptFile)> = Vec::new();
        let mut files: Vec<PromptFile> = Vec::new();
        let mut agents = Vec::new();
        let mut skills = Vec::new();
        let mut pasted: Vec<PastedText> = Vec::new();
        let current = self.prompt.get_untracked();
        let mut draft = current.clone();
        for extmark in &all {
            let Some(part) = map.get(&extmark.id) else { continue };
            match *part {
                PartRef::File(index) => {
                    let Some(file) = draft.files.as_mut().and_then(|f| f.get_mut(index)) else { continue };
                    let Some(mention) = file.mention.as_mut() else { continue };
                    mention.start = extmark.start;
                    mention.end = extmark.end;
                    files.push(file.clone());
                    file_extmarks.push((extmark.id, file.clone()));
                }
                PartRef::Agent(index) => {
                    let Some(agent) = draft.agents.as_mut().and_then(|a| a.get_mut(index)) else { continue };
                    let Some(mention) = agent.mention.as_mut() else { continue };
                    mention.start = extmark.start;
                    mention.end = extmark.end;
                    new_map.insert(extmark.id, PartRef::Agent(agents.len()));
                    agents.push(agent.clone());
                }
                PartRef::Skill(index) => {
                    let Some(skill) = draft.skills.as_mut().and_then(|s| s.get_mut(index)) else { continue };
                    let Some(mention) = skill.mention.as_mut() else { continue };
                    mention.start = extmark.start;
                    mention.end = extmark.end;
                    new_map.insert(extmark.id, PartRef::Skill(skills.len()));
                    skills.push(skill.clone());
                }
                PartRef::Pasted(index) => {
                    let Some(part) = draft.pasted.get_mut(index) else { continue };
                    part.source.start = extmark.start;
                    part.source.end = extmark.end;
                    new_map.insert(extmark.id, PartRef::Pasted(pasted.len()));
                    pasted.push(part.clone());
                }
            }
        }
        // The draft's files carry the moved mentions (TS mutates the parts in place).
        if let Some(draft_files) = draft.files.as_mut() {
            for file in draft_files.iter_mut() {
                if let Some(moved) = files.iter().find(|f| f.uri == file.uri && f.name == file.name && f.mention.as_ref().map(|m| &m.text) == file.mention.as_ref().map(|m| &m.text)) {
                    file.mention = moved.mention.clone();
                }
            }
        }
        let next_files = attachment::preserve_mentionless_prompt_attachments(draft.files.as_deref(), files);
        for (id, file) in file_extmarks {
            if let Some(index) = next_files.iter().position(|f| *f == file) {
                new_map.insert(id, PartRef::File(index));
            }
        }
        *self.extmark_to_part.borrow_mut() = new_map;
        draft.files = Some(next_files);
        if draft.files.as_ref().is_some_and(|f| f.is_empty()) && current.files.is_none() {
            draft.files = None;
        }
        draft.agents = Some(agents);
        draft.skills = Some(skills);
        draft.pasted = pasted;
        self.prompt.set(draft);
    }

    /// `pasteText(text, virtualText)`: a placeholder standing for the pasted text.
    fn paste_text(&self, text: &str, virtual_text: &str) {
        let start = self.ta.cursor_offset() as i64;
        let end = start + self.measure(virtual_text);
        self.ta.insert_text(&format!("{virtual_text} "));
        let id = self.create_extmark(start, end, self.styles.paste);
        self.prompt.update(|draft| {
            let index = draft.pasted.len();
            draft.pasted.push(PastedText {
                text: text.to_string(),
                source: Range { start, end, text: virtual_text.to_string() },
            });
            self.extmark_to_part.borrow_mut().insert(id, PartRef::Pasted(index));
        });
    }

    /// `expandPastedText(extmarkId)`: the placeholder replaced by its text.
    fn expand_pasted_text(&self, id: u32) -> bool {
        let extmarks = self.ta.extmarks();
        let Some(extmark) = extmarks.get(id) else { return false };
        let Some(PartRef::Pasted(index)) = self.extmark_to_part.borrow().get(&id).copied() else { return false };
        let Some(part) = self.prompt.with_untracked(|p| p.pasted.get(index).cloned()) else { return false };
        let _ = extmarks.delete(id);
        self.ta.set_selection(extmark.start as u32, extmark.end as u32);
        self.ta.insert_text(&part.text);
        true
    }

    /// `pasteAttachment(file)`.
    fn paste_attachment(&self, filename: Option<String>, uri: String, path: Option<String>) {
        let start = self.ta.cursor_offset() as i64;
        let virtual_text = self
            .prompt
            .with_untracked(|p| attachment::prompt_attachment_label(p.files.as_deref(), &uri, filename.as_deref(), None));
        let end = start + text::js_len(&virtual_text) as i64;
        self.ta.insert_text(&format!("{virtual_text} "));
        let id = self.create_extmark(start, end, self.styles.paste);
        let part = PromptFile { uri, name: filename, description: None, path, mention: Some(Range { start, end, text: virtual_text }) };
        self.prompt.update(|draft| {
            let files = draft.files.get_or_insert_with(Vec::new);
            let index = files.len();
            files.push(part);
            self.extmark_to_part.borrow_mut().insert(id, PartRef::File(index));
        });
    }

    /// `enqueuePaste(run)`: pastes run one at a time; each sees whether the prompt changed under it.
    fn enqueue_paste(self: &Rc<Self>, task: Task) {
        self.paste_queue.borrow_mut().push_back(task);
        if self.pasting.replace(true) {
            return;
        }
        let state = self.clone();
        tokio::task::spawn_local(async move {
            loop {
                let next = state.paste_queue.borrow_mut().pop_front();
                let Some(task) = next else { break };
                if state.disposed.get() || state.input.is_destroyed() || state.disabled() {
                    continue;
                }
                let before = (state.mode.get_untracked(), state.ta.plain_text());
                let watcher = state.clone();
                let changed: Rc<dyn Fn() -> bool> = Rc::new(move || {
                    watcher.disposed.get()
                        || watcher.input.is_destroyed()
                        || watcher.disabled()
                        || watcher.mode.get_untracked() != before.0
                        || watcher.ta.plain_text() != before.1
                });
                task(changed).await;
            }
            state.pasting.set(false);
        });
    }

    /// `pasteInputText(text, changed)`.
    async fn paste_input_text(self: Rc<Self>, text: String, changed: Rc<dyn Fn() -> bool>) {
        let normalized = strip_ansi_sequences(&text).replace("\r\n", "\n").replace('\r', "\n");
        let pasted = text::trim(&normalized).to_string();
        let cwd = self.t.paths.cwd.clone();
        let lookup = pasted.clone();
        let attachments =
            tokio::task::spawn_blocking(move || attachment::resolve_pasted_attachments(&lookup, &cwd)).await.ok().flatten();
        if changed() {
            return;
        }
        if let Some(attachments) = attachments {
            for item in attachments {
                match item {
                    attachment::Pasted::Text { content, filename } => {
                        let name = if filename.is_empty() { "image".to_string() } else { filename };
                        self.paste_text(&content, &format!("[SVG: {name}]"));
                    }
                    attachment::Pasted::File { uri, filename, path } => self.paste_attachment(Some(filename), uri, Some(path)),
                }
            }
            return;
        }
        let line_count = pasted.matches('\n').count() + 1;
        let full = self.t.config_context.data_untracked().prompt.as_ref().and_then(|p| p.paste.as_deref()) == Some("full");
        if (line_count >= 3 || text::js_len(&pasted) > 150) && !full {
            let cursor = self.ta.cursor_offset() as i64;
            let existing = self.ta.extmarks().get_all_for_type_id(self.part_type.get()).into_iter().find(|extmark| {
                let part = self.extmark_to_part.borrow().get(&extmark.id).copied();
                (extmark.end == cursor || extmark.end + 1 == cursor)
                    && matches!(part, Some(PartRef::Pasted(index)) if self.prompt.with_untracked(|p| p.pasted.get(index).is_some_and(|x| x.text == pasted)))
            });
            if let Some(extmark) = existing
                && self.expand_pasted_text(extmark.id)
            {
                return;
            }
            self.paste_text(&pasted, &format!("[Pasted ~{line_count} lines]"));
            return;
        }
        self.ta.insert_text(&normalized);
        let ta = self.ta.clone();
        renderer::set_timeout(0, move || {
            if ta.el.is_destroyed() {
                return;
            }
            ta.invalidate_intrinsic_size();
            renderer::request_render();
        });
    }

    fn prompt_model_warning(&self) {
        self.toast.show(ToastInput::new(Variant::Warning, "Connect an integration to send prompts").duration(3000));
        // TODO(port-tui): the dialogs slice: `dialog.replace(<DialogIntegration />)` when no integration is connected.
    }

    /// `restoreEntry()`: the snapshot back in the composer unless something new was typed.
    fn restore_entry(&self, entry: &PromptInfo) {
        if self.disposed.get() || self.input.is_destroyed() || !self.ta.plain_text().is_empty() {
            return;
        }
        self.ta.set_text(&entry.text);
        self.prompt.set(entry.clone());
        self.mode.set(entry.mode.unwrap_or(Mode::Normal));
        self.restore_extmarks_from_prompt(entry);
        self.ta.set_cursor_offset(self.measure(&entry.text) as u32);
    }

    fn fail(&self, title: &str, message: String, entry: &PromptInfo) {
        self.toast.show(ToastInput::new(Variant::Error, message).title(title));
        self.restore_entry(entry);
    }
}

/// `promptFilesForServer(files, localServer)`: a local server reads pasted files from their paths.
fn prompt_files_for_server(files: Option<&[PromptFile]>, local_server: bool) -> Option<Json> {
    let files = files?;
    Some(Json::Array(
        files
            .iter()
            .map(|file| {
                let mut json = file_json(&PromptFile { path: None, ..file.clone() });
                if let (Some(path), true) = (file.path.as_ref(), local_server) {
                    json.set("source", obj! { "type" => "uri", "uri" => attachment::path_to_file_url(path) });
                }
                json
            })
            .collect(),
    ))
}

/// `submit(delivery)`: overlapping invocations (a double enter) are dropped.
async fn submit(state: Rc<State>, delivery: Delivery) -> bool {
    if state.disabled() || state.submitting.get() {
        return false;
    }
    state.submitting.set(true);
    let handled = submit_inner(state.clone(), delivery).await;
    state.submitting.set(false);
    handled
}

async fn submit_inner(s: Rc<State>, delivery: Delivery) -> bool {
    // IME: the last composed character may not have reached the store yet.
    if !s.input.is_destroyed() && s.ta.plain_text() != s.prompt.with_untracked(|p| p.text.clone()) {
        let text = s.ta.plain_text();
        s.prompt.update(|p| p.text = text);
        s.sync_extmarks_with_prompt_parts();
    }
    // TODO(port-tui): `move.creating()` (the worktree move).
    if s.auto.get_untracked().is_some_and(|a| a.visible() != Visible::None) {
        return false;
    }
    let prompt = s.prompt.get_untracked();
    let trimmed = text::trim(&prompt.text).to_string();
    let mode = s.mode.get_untracked();
    if trimmed.is_empty() && (s.session_id.is_none() || mode == Mode::Shell || delivery == Delivery::Queue) {
        return delivery == Delivery::Steer && s.on_empty_submit.as_ref().is_some_and(|f| f());
    }
    let exit_word = matches!(trimmed.as_str(), "exit" | "quit" | ":q");
    let ranges: Vec<TrackedText> = s
        .ta
        .extmarks()
        .get_all_for_type_id(s.part_type.get())
        .into_iter()
        .filter_map(|extmark| {
            let Some(PartRef::Pasted(index)) = s.extmark_to_part.borrow().get(&extmark.id).copied() else { return None };
            let part = prompt.pasted.get(index)?;
            Some(TrackedText { start: extmark.start, end: extmark.end, text: part.text.clone() })
        })
        .collect();
    let input_text = text::expand_tracked_pasted_text(&prompt.text, &ranges, s.width_method());
    let commands = s.commands.get_untracked();
    let slash = argument_slash(&input_text, &commands);
    if delivery == Delivery::Queue && (mode == Mode::Shell || exit_word || slash.is_some()) {
        s.toast.show(ToastInput::new(Variant::Warning, "This prompt cannot be queued"));
        return false;
    }
    if exit_word {
        (s.t.exit)();
        return true;
    }
    if let Some((command, input)) = slash {
        s.clear_prompt();
        command.invoke(Some(input));
        return true;
    }
    let slash_head = parse_slash_head(&input_text, true);
    let location = s.location();
    let is_command = slash_head.as_ref().is_some_and(|head| {
        s.t.data
            .store
            .location
            .command
            .list(Some(&location))
            .is_some_and(|list| array(&list).iter().any(|command| get_str(command, "name") == Some(head.name.as_str())))
    });
    // TODO(port-tui): the editor context (pending IDE selections).
    let Some(agent) = s.local.agent_current() else { return false };
    let Some(selection) = s.local.model_selection() else {
        s.prompt_model_warning();
        return false;
    };
    let (provider_id, model_id, variant) = (selection.provider_id.clone(), selection.model_id.clone(), selection.variant.clone());
    let uses_model = s.session_id.is_none() || mode != Mode::Shell;
    if uses_model && !s.local.available(Some(&selection.model())) {
        s.toast.show(
            ToastInput::new(Variant::Warning, format!("{provider_id}/{model_id} is not available in this session's location"))
                .title("Model unavailable"),
        );
        return false;
    }

    // Snapshot the composer and clear it before the first await: text typed while a request is in flight lands in
    // the empty composer, and the history records exactly what was submitted.
    let entry = PromptInfo { mode: Some(mode), ..prompt.clone() };
    if !trimmed.is_empty() {
        s.reset_composer();
        if let Some(on_submit) = &s.on_submit {
            on_submit();
        }
    }

    let store = s.t.data.store.clone();
    let client = s.t.data.client.clone();
    let mut session_id = s.session_id.clone();
    let mut session = session_id.as_deref().and_then(|id| store.session.get(id));
    // New-session sends wait for the creation (and the environment setup).
    let mut gate: Option<Deferred<Result<(), opencode_tui_sync::client::Error>>> = None;
    let mut created_id: Option<String> = None;
    if session_id.is_none() {
        // TODO(port-tui): `move.getDirectory()` (a pending worktree destination).
        let new_id = take_new_session_id(&s.t);
        let model = obj! { "providerID" => provider_id.as_str(), "id" => model_id.as_str(), "variant" => variant.clone() };
        let created = store.session.create(CreateInput {
            id: new_id.clone(),
            location: Some(location.clone()),
            agent: Some(agent.id.clone()),
            model: Some(model),
            ..CreateInput::default()
        });
        if let Some(id) = new_id {
            let request = created.request.clone();
            let t = s.t.clone();
            tokio::task::spawn_local(async move {
                if request.wait().await.is_err() {
                    restore_new_session_id(&t, id);
                }
            });
        }
        session = store.session.get(&created.id);
        let request = created.request.clone();
        // TODO(port-tui): `session.environment` with the terminal's variables for a local server.
        gate = Some(Deferred::spawn(async move { request.wait().await.map(|_| ()) }));
        created_id = Some(created.id.clone());
        session_id = Some(created.id);
    }
    let target = session_id.clone().unwrap_or_default();

    let prepare_agent = {
        let store = store.clone();
        let client = client.clone();
        let target = target.clone();
        let agent_id = agent.id.clone();
        let known = session.clone();
        move || {
            let store = store.clone();
            let client = client.clone();
            let target = target.clone();
            let agent_id = agent_id.clone();
            let known = known.clone();
            async move {
                let mut current = known;
                if current.is_none() {
                    let _ = store.session.sync(&target, false).wait().await;
                    current = store.session.get(&target);
                }
                if current.as_ref().and_then(|c| c.str("agent").map(str::to_string)) != Some(agent_id.clone()) {
                    let input = obj! { "sessionID" => target.as_str(), "agent" => agent_id.as_str() };
                    client.call_named("session.switchAgent", &input, RequestOptions::default()).await.map(|_| ())?;
                }
                Ok::<(), opencode_tui_sync::client::Error>(())
            }
        }
    };
    let model_json = obj! { "providerID" => provider_id.as_str(), "id" => model_id.as_str(), "variant" => variant.clone() };
    let commit_model = {
        let client = client.clone();
        let target = target.clone();
        let model = model_json.clone();
        let local = s.local.clone();
        let (provider_id, model_id, variant, agent_id) = (provider_id.clone(), model_id.clone(), variant.clone(), agent.id.clone());
        move || {
            let cancel = local.track_session_commit(&target, &provider_id, &model_id, variant.as_deref(), &agent_id);
            let call = client.call_named(
                "session.switchModel",
                &obj! { "sessionID" => target.as_str(), "model" => model.clone() },
                RequestOptions::default(),
            );
            async move {
                call.await.map(|_| ()).map_err(|error| {
                    cancel();
                    opencode_tui_sync::client::Error::malformed(format!("Failed to switch model: {}", error.message()))
                })
            }
        }
    };

    if trimmed.is_empty() {
        // Blank enter in an existing session commits the composer's selection, then hands off to the route.
        let result = async {
            prepare_agent().await?;
            commit_model().await?;
            Ok::<(), opencode_tui_sync::client::Error>(())
        }
        .await;
        match result {
            Ok(()) => {
                if let Some(f) = &s.on_empty_submit {
                    f();
                }
            }
            Err(error) => s.fail("Failed to prepare session", error_message(&error), &entry),
        }
        return true;
    }
    s.history.append(&entry);
    let recover = {
        let s = s.clone();
        let entry = entry.clone();
        let created = created_id.clone();
        Rc::new(move |error: opencode_tui_sync::client::Error| {
            let Some(created) = created.clone() else {
                return;
            };
            let title = if s.t.data.store.session.get(&created).is_some() { "Failed to set up session" } else { "Creating a session failed" };
            s.toast.show(ToastInput::new(Variant::Error, error_message(&error)).title(title));
            let draft = take_draft(Some(&created)).unwrap_or(DraftEntry { prompt: entry.clone(), cursor: u32::MAX });
            save_draft(None, draft);
            if s.t.route.with_untracked(|r| matches!(r, Route::Session { session_id } if *session_id == created)) {
                s.t.route.set(Route::Home);
            }
        })
    };
    if mode == Mode::Shell {
        let client = client.clone();
        let input = obj! { "sessionID" => target.as_str(), "command" => input_text.as_str() };
        let gate = gate.clone();
        let recover = recover.clone();
        let s2 = s.clone();
        let entry2 = entry.clone();
        let new_session = created_id.is_some();
        tokio::task::spawn_local(async move {
            let result = async {
                if let Some(gate) = gate {
                    gate.wait().await?;
                }
                client.call_named("session.shell", &input, RequestOptions::default()).await.map(|_| ())
            }
            .await;
            if let Err(error) = result {
                if new_session {
                    recover(error);
                } else {
                    s2.fail("Failed to run shell command", error_message(&error), &entry2);
                }
            }
        });
        s.mode.set(Mode::Normal);
    } else if let (Some(head), true) = (slash_head.as_ref(), is_command) {
        let client = client.clone();
        let mut input = obj! {
            "sessionID" => target.as_str(),
            "name" => head.name.as_str(),
            "text" => head.arguments.as_str(),
        };
        if let Some(files) = prompt_files_for_server(entry.files.as_deref(), false) {
            input.set("files", files);
        }
        if let Some(agents) = entry.agents.as_ref() {
            input.set("agents", Json::Array(agents.iter().map(agent_json).collect()));
        }
        if let Some(skills) = entry.skills.as_ref().filter(|s| !s.is_empty()) {
            input.set("skills", Json::Array(skills.iter().map(skill_json).collect()));
        }
        input.set("delivery", Json::String(delivery.as_str().to_string()));
        let gate = gate.clone();
        let recover = recover.clone();
        let s2 = s.clone();
        let entry2 = entry.clone();
        let new_session = created_id.is_some();
        let prepare_agent = prepare_agent.clone();
        let commit_model = commit_model.clone();
        tokio::task::spawn_local(async move {
            let result = async {
                if let Some(gate) = gate {
                    gate.wait().await?;
                }
                // Commands inherit the composer selection; command-specific overrides stay the server's.
                prepare_agent().await?;
                commit_model().await?;
                client.call_named("session.command", &input, RequestOptions::default()).await.map(|_| ())
            }
            .await;
            if let Err(error) = result {
                if new_session {
                    recover(error);
                } else {
                    s2.fail("Failed to run command", error_message(&error), &entry2);
                }
            }
        });
    } else {
        // TODO(port-tui): `move.startSubmit()`.
        if let Err(error) = prepare_agent().await {
            s.fail("Failed to prepare session", error_message(&error), &entry);
            return true;
        }
        // A revert settles before optimistic admission.
        if session.as_ref().is_some_and(|session| session.get("revert").is_some_and(|r| !matches!(r, Json::Null))) {
            let call = client.call_named("session.revert.commit", &obj! { "sessionID" => target.as_str() }, RequestOptions::default());
            if let Err(error) = call.await {
                s.fail("Failed to commit revert", error_message(&error), &entry);
                return false;
            }
        }
        let mut request = obj! { "sessionID" => target.as_str(), "text" => input_text.as_str() };
        if let Some(files) = prompt_files_for_server(entry.files.as_deref(), false) {
            request.set("files", files);
        }
        if let Some(agents) = entry.agents.as_ref() {
            request.set("agents", Json::Array(agents.iter().map(agent_json).collect()));
        }
        if let Some(skills) = entry.skills.as_ref().filter(|s| !s.is_empty()) {
            request.set("skills", Json::Array(skills.iter().map(skill_json).collect()));
        }
        request.set("delivery", Json::String(delivery.as_str().to_string()));
        // Commit the captured selection after earlier admissions; the server makes an unchanged one a no-op.
        let prepare: Rc<dyn Fn() -> Deferred<Result<(), opencode_tui_sync::client::Error>>> = {
            let commit_model = commit_model.clone();
            Rc::new(move || Deferred::spawn(commit_model()))
        };
        let sent = store.session.prompt(PromptInput { request, gate: gate.clone(), prepare: Some(prepare) });
        let recover = recover.clone();
        let s2 = s.clone();
        let entry2 = entry.clone();
        let new_session = created_id.is_some();
        tokio::task::spawn_local(async move {
            if let Err(error) = sent.wait().await {
                if new_session {
                    recover(error);
                } else {
                    s2.fail("Failed to send prompt", error_message(&error), &entry2);
                }
            }
        });
    }

    // Optimistic admission put the message in the store, so the session view renders it on arrival.
    if s.session_id.is_none() {
        // Text typed while the session was being created lives in this (home) prompt, which unmounts on
        // navigation: stash it under the new session.
        if !s.disposed.get() && !s.input.is_destroyed() && !s.prompt.with_untracked(|p| p.text.is_empty()) {
            save_draft(Some(&target), DraftEntry { prompt: s.prompt.get_untracked(), cursor: s.ta.cursor_offset() });
            s.reset_composer();
        }
        s.t.route.set(Route::Session { session_id: target });
    }
    true
}

/// `<Prompt>`.
pub fn prompt(props: PromptProps) -> View {
    let t = tui();
    let local = local();
    let dimensions = t.dimensions;
    let theme = t.theme;
    let (history, stash, frecency) = contexts(&t);
    let enabled = crate::interactivity::use_interactivity();
    let leader: Rc<dyn Fn() -> bool> = Rc::new(keymap::use_leader_active());
    let commands = keymap::use_commands();
    let shortcuts = keymap::use_shortcuts();
    let animations = t.config.animations.unwrap_or(true);
    let placeholders = props.placeholders.clone();

    let anchor = box_();
    let input = textarea();
    let ta = Textarea::of(input).expect("a textarea");
    let syntax = theme.with_untracked(syntax_style);
    let styles = Styles {
        file: syntax.get_style_id("extmark.file"),
        agent: syntax.get_style_id("extmark.agent"),
        skill: syntax.get_style_id("extmark.skill"),
        paste: syntax.get_style_id("extmark.paste"),
    };
    let state = Rc::new(State {
        t: t.clone(),
        local: local.clone(),
        toast: t.toast.clone(),
        session_id: props.session_id.clone(),
        disabled_prop: props.disabled.clone(),
        muted_prop: props.muted.clone(),
        on_submit: props.on_submit.clone(),
        on_empty_submit: props.on_empty_submit.clone(),
        enabled: enabled.clone(),
        leader: leader.clone(),
        input,
        ta: ta.clone(),
        anchor,
        prompt: create_signal(empty_prompt()).0,
        mode: create_signal(Mode::Normal).0,
        extmark_to_part: RefCell::new(HashMap::new()),
        interrupt: create_signal(0u32).0,
        placeholder: create_signal(random_index(placeholders.normal.len())).0,
        part_type: Cell::new(0),
        styles,
        auto: create_signal_with(None, None).0,
        disposed: Cell::new(false),
        submitting: Cell::new(false),
        paste_queue: RefCell::new(VecDeque::new()),
        pasting: Cell::new(false),
        history,
        stash,
        commands,
        pending_directory: create_signal(None).0,
        cursor_version: create_signal(0u64).0,
        placeholders,
    });
    let s = state.clone();
    let disabled = {
        let s = s.clone();
        move || s.disabled()
    };

    // /cd (`session.cd`).
    {
        let s = s.clone();
        let disabled = disabled.clone();
        create_layer(move || {
            let s = s.clone();
            Layer {
                mode: Some("global"),
                enabled: Some(Enabled::Bool(!disabled())),
                commands: vec![
                    Command::with_input("session.cd", move |input| {
                        change_directory(s.clone(), input);
                        true
                    })
                    .title("Change working directory")
                    .slash_arguments("cd"),
                ],
                ..Layer::default()
            }
        });
    }

    // `tui.prompt.append`: text a client sends to this location's prompt. (TS never unsubscribes; the handler of
    // an unmounted prompt does nothing, so unsubscribing on cleanup changes nothing visible.)
    {
        let s = s.clone();
        let off = t.data.event.on("tui.prompt.append", move |event, metadata| {
            if metadata.directory.as_deref() != s.location().str("directory") {
                return;
            }
            if s.input.is_destroyed() {
                return;
            }
            s.ta.insert_text(event.data().str("text").unwrap_or_default());
            // setTimeout is a workaround and needs to be addressed properly
            let ta = s.ta.clone();
            renderer::set_timeout(0, move || {
                if ta.el.is_destroyed() {
                    return;
                }
                ta.invalidate_intrinsic_size();
                ta.goto_buffer_end();
                renderer::request_render();
            });
        });
        on_cleanup(move || off.call());
    }

    {
        let ta = ta.clone();
        let disabled = disabled.clone();
        create_effect(move || {
            let color = if disabled() { theme.with(|th| th.background.raised.base) } else { theme.with(|th| th.text.base) };
            if !ta.el.is_destroyed() {
                ta.el.set_cursor_color_prop(rgba(color));
            }
        });
    }

    // The placeholder is drawn again for a new session.
    {
        let s = s.clone();
        let session = props.session_id.clone();
        create_effect(on(
            move || session.clone(),
            move |_, _| {
                s.placeholder.set(random_index(s.placeholders.normal.len()));
            },
            true,
        ));
    }

    // promptCommands.
    {
        let s = s.clone();
        let disabled = disabled.clone();
        create_layer(move || {
            let prompt_command = |id: &str, title: &str, group: &str, run: Rc<dyn Fn(Option<String>) -> bool>| Command {
                id: Some(id.to_string()),
                title: Some(title.to_string()),
                group: Some(group.to_string()),
                bind: Bind::Off,
                palette: true,
                run,
                ..Command::default()
            };
            let clear = {
                let s = s.clone();
                Rc::new(move |_: Option<String>| {
                    s.clear_prompt();
                    true
                })
            };
            let submit_command = {
                let s = s.clone();
                Rc::new(move |_: Option<String>| {
                    if !s.input.focused() {
                        return true;
                    }
                    let state = s.clone();
                    tokio::task::spawn_local(async move {
                        submit(state, Delivery::Steer).await;
                    });
                    true
                })
            };
            let queue = {
                let s = s.clone();
                Rc::new(move |_: Option<String>| {
                    if !s.input.focused() {
                        return true;
                    }
                    if let Some(auto) = s.auto.get_untracked()
                        && auto.visible() != Visible::None
                        && !auto.complete_queueable_command()
                    {
                        return true;
                    }
                    let state = s.clone();
                    tokio::task::spawn_local(async move {
                        submit(state, Delivery::Queue).await;
                    });
                    true
                })
            };
            let paste = {
                let s = s.clone();
                Rc::new(move |_: Option<String>| {
                    // TODO(port-tui): the clipboard (`clipboard.read()`): images become attachments, text is pasted.
                    let _ = &s;
                    true
                })
            };
            let images_enabled = s.prompt.with(|p| {
                attachment::deduplicate_prompt_images(p.files.as_deref().unwrap_or(&[])).iter().any(|f| f.uri.starts_with("data:image/"))
            });
            let running = s.status() == "running";
            let interrupt = {
                let s = s.clone();
                Rc::new(move |_: Option<String>| {
                    if s.auto.get_untracked().is_some_and(|a| a.visible() != Visible::None) || !s.input.focused() {
                        return true;
                    }
                    // TODO: this should be its own command
                    if s.mode.get_untracked() == Mode::Shell {
                        s.mode.set(Mode::Normal);
                        return true;
                    }
                    let Some(session) = s.session_id.clone() else { return true };
                    s.interrupt.set(s.interrupt.get_untracked() + 1);
                    let reset = s.interrupt;
                    renderer::set_timeout(5000, move || {
                        if reset.is_alive() {
                            reset.set(0);
                        }
                    });
                    if s.interrupt.get_untracked() >= 2 {
                        let call = s.t.data.client.call_named(
                            "session.interrupt",
                            &obj! { "sessionID" => session.as_str(), "resume" => true },
                            RequestOptions::default(),
                        );
                        tokio::task::spawn_local(async move {
                            let _ = call.await;
                        });
                        s.interrupt.set(0);
                    }
                    true
                })
            };
            let background = {
                let s = s.clone();
                Rc::new(move |_: Option<String>| {
                    if s.auto.get_untracked().is_some_and(|a| a.visible() != Visible::None) || !s.input.focused() {
                        return true;
                    }
                    let Some(session) = s.session_id.clone() else { return true };
                    let call = s.t.data.client.call_named(
                        "session.background",
                        &obj! { "sessionID" => session.as_str() },
                        RequestOptions::default(),
                    );
                    tokio::task::spawn_local(async move {
                        let _ = call.await;
                    });
                    true
                })
            };
            let noop = || Rc::new(|_: Option<String>| true) as Rc<dyn Fn(Option<String>) -> bool>;
            Layer {
                mode: Some("global"),
                enabled: Some(Enabled::Bool(!disabled())),
                commands: vec![
                    prompt_command("prompt.clear", "Clear prompt", "Prompt", clear),
                    prompt_command("prompt.submit", "Submit prompt", "Prompt", submit_command),
                    prompt_command("prompt.queue", "Queue prompt", "Prompt", queue),
                    // TODO(port-tui): the editor context.
                    Command { enabled: Some(Enabled::Bool(false)), ..prompt_command("prompt.editor_context.clear", "Remove editor context", "Prompt", noop()) },
                    prompt_command("prompt.paste", "Paste", "Prompt", paste),
                    // TODO(port-tui): the dialogs slice: DialogImagePreview.
                    Command { enabled: Some(Enabled::Bool(images_enabled)), ..prompt_command("prompt.images.view", "View image attachments", "Prompt", noop()) },
                    Command { enabled: Some(Enabled::Bool(running)), ..prompt_command("session.interrupt", "Interrupt session", "Session", interrupt) },
                    Command { enabled: Some(Enabled::Bool(running)), ..prompt_command("session.background", "Background blocking tools", "Session", background) },
                    // TODO(port-tui): `openEditor` (the external editor).
                    prompt_command("prompt.editor", "Open editor", "Session", noop()).slash("editor", &[]),
                    // TODO(port-tui): the dialogs slice: DialogSkill.
                    prompt_command("prompt.skills", "Skills", "Prompt", noop()).slash("skills", &[]),
                    // TODO(port-tui): the dialogs slice: DialogWorkspaces (`move.open()`).
                    prompt_command("session.move", "Manage workspaces", "Session", noop()).description("Manage workspaces").slash("worktrees", &[]),
                ],
                ..Layer::default()
            }
        });
    }
    {
        let disabled = disabled.clone();
        create_layer(move || Layer { priority: Some(1), enabled: Some(Enabled::Bool(!disabled())), bindings: vec!["prompt.queue"], ..Layer::default() });
    }
    {
        let disabled = disabled.clone();
        create_layer(move || Layer {
            enabled: Some(Enabled::Bool(!disabled())),
            bindings: vec![
                "prompt.submit",
                "prompt.editor",
                "prompt.editor_context.clear",
                "prompt.images.view",
                "prompt.stash",
                "prompt.stash.pop",
                "prompt.stash.list",
                "prompt.skills",
                "session.interrupt",
                "session.background",
                "session.move",
            ],
            ..Layer::default()
        });
    }

    let reference = PromptRef(state.clone());

    // The textarea's tab's draft, kept across remounts (captured once: the session route is keyed by session).
    let stash_session = props.session_id.clone();
    {
        let reference = reference.clone();
        let s = s.clone();
        let stash_session = stash_session.clone();
        on_mount(move || {
            let unregister = register_composer(stash_session.clone(), reference.clone());
            on_cleanup(unregister);
            let saved = take_draft(stash_session.as_deref());
            if !s.prompt.with_untracked(|p| p.text.is_empty()) {
                return;
            }
            if let Some(saved) = saved.filter(|d| !d.prompt.text.is_empty()) {
                s.ta.set_text(&saved.prompt.text);
                s.prompt.set(saved.prompt.clone());
                s.restore_extmarks_from_prompt(&saved.prompt);
                s.ta.set_cursor_offset(saved.cursor.min(s.measure(&saved.prompt.text) as u32));
            }
        });
    }
    {
        let s = s.clone();
        let on_ref = props.on_ref.clone();
        let current = t.prompt.clone();
        on_cleanup(move || {
            s.disposed.set(true);
            if !s.prompt.with_untracked(|p| p.text.is_empty()) {
                save_draft(stash_session.as_deref(), DraftEntry { prompt: s.prompt.get_untracked(), cursor: s.ta.cursor_offset() });
            }
            if *current.borrow() == Some(s.input) {
                *current.borrow_mut() = None;
            }
            on_ref(None);
        });
    }

    // Focus follows visibility, the disabled state and open dialogs.
    {
        let s = s.clone();
        let visible = props.visible.clone();
        let disabled = disabled.clone();
        create_effect(move || {
            if s.input.is_destroyed() {
                return;
            }
            // TODO(port-tui): the dialogs slice: `dialog.stack.length > 0`.
            if visible.as_ref().is_some_and(|v| !v()) || disabled() {
                if s.input.focused() {
                    s.input.blur();
                }
                s.input.set_focusable(false);
                return;
            }
            s.input.set_focusable(true);
            if !s.input.focused() {
                s.input.focus();
            }
        });
    }
    {
        let s = s.clone();
        create_effect(move || {
            if s.input.is_destroyed() {
                return;
            }
            let shell = s.mode.get() == Mode::Shell;
            let visible = s.auto.get().is_some_and(|a| a.visible_tracked() != Visible::None);
            let (capture, status) = text::compute_prompt_traits(shell, visible);
            let traits = s.ta.traits();
            s.ta.set_traits(EditorTraits { capture, status, ..traits });
        });
    }

    // Stash commands.
    {
        let s = s.clone();
        let disabled = disabled.clone();
        create_layer(move || {
            let has_text = s.prompt.with(|p| !p.text.is_empty());
            let stashed = !s.stash.is_empty();
            let stash_push = {
                let s = s.clone();
                Rc::new(move |_: Option<String>| {
                    let prompt = s.prompt.get_untracked();
                    if prompt.text.is_empty() {
                        return true;
                    }
                    s.stash.push(&prompt);
                    s.reset_composer();
                    true
                }) as Rc<dyn Fn(Option<String>) -> bool>
            };
            let stash_pop = {
                let s = s.clone();
                Rc::new(move |_: Option<String>| {
                    if let Some(entry) = s.stash.pop() {
                        s.ta.set_text(&entry.prompt.text);
                        s.prompt.set(entry.prompt.clone());
                        s.restore_extmarks_from_prompt(&entry.prompt);
                        s.ta.goto_buffer_end();
                    }
                    true
                }) as Rc<dyn Fn(Option<String>) -> bool>
            };
            let command = |id: &str, title: &str, enabled: bool, run: Rc<dyn Fn(Option<String>) -> bool>| Command {
                id: Some(id.to_string()),
                title: Some(title.to_string()),
                group: Some("Prompt".to_string()),
                bind: Bind::Off,
                palette: true,
                enabled: Some(Enabled::Bool(enabled)),
                run,
                ..Command::default()
            };
            Layer {
                mode: Some("global"),
                enabled: Some(Enabled::Bool(!disabled())),
                commands: vec![
                    command("prompt.stash", "Stash prompt", has_text, stash_push),
                    command("prompt.stash.pop", "Stash pop", stashed, stash_pop),
                    // TODO(port-tui): the dialogs slice: DialogStash.
                    command("prompt.stash.list", "Stash list", stashed, Rc::new(|_| true)),
                ],
                ..Layer::default()
            }
        });
    }

    let input_target: Rc<dyn Fn() -> Option<Element>> = Rc::new(move || Some(input));
    {
        let disabled = disabled.clone();
        let target = input_target.clone();
        create_layer(move || Layer {
            target: Some(target.clone()),
            enabled: Some(Enabled::Bool(!disabled())),
            bindings: vec!["prompt.paste"],
            ..Layer::default()
        });
    }
    {
        let disabled = disabled.clone();
        let target = input_target.clone();
        let s = s.clone();
        create_layer(move || Layer {
            target: Some(target.clone()),
            enabled: Some(Enabled::Bool(!disabled() && s.prompt.with(|p| !p.text.is_empty()))),
            bindings: vec!["prompt.clear"],
            ..Layer::default()
        });
    }
    {
        let disabled = disabled.clone();
        let target = input_target.clone();
        let s = s.clone();
        create_layer(move || {
            let auto_visible = s.auto.get().is_some_and(|a| a.visible_tracked() != Visible::None);
            let run = {
                let s = s.clone();
                move || {
                    s.placeholder.set(random_index(s.placeholders.shell.len()));
                    s.mode.set(Mode::Shell);
                    true
                }
            };
            let ta = s.ta.clone();
            Layer {
                target: Some(target.clone()),
                enabled: Some(Enabled::Bool(!disabled() && s.mode.get() == Mode::Normal && !auto_visible)),
                commands: vec![
                    // Read the cursor when the key arrives: cursor events lag behind keys typed in one burst.
                    Command::inline("!", run).title("Shell mode").group("Prompt").enabled_fn(move || ta.visual_cursor().offset == 0),
                ],
                ..Layer::default()
            }
        });
    }
    {
        let disabled = disabled.clone();
        let target = input_target.clone();
        let s = s.clone();
        create_layer(move || {
            let normal = {
                let mode = s.mode;
                move || {
                    mode.set(Mode::Normal);
                    true
                }
            };
            let prompt = s.prompt;
            Layer {
                priority: Some(1),
                target: Some(target.clone()),
                enabled: Some(Enabled::Bool(!disabled() && s.mode.get() == Mode::Shell)),
                commands: vec![
                    Command::inline("escape", normal.clone()).title("Exit shell mode").group("Prompt"),
                    Command::inline("ctrl+c", normal)
                        .title("Exit shell mode")
                        .group("Prompt")
                        .enabled_fn(move || prompt.with_untracked(|p| p.text.is_empty())),
                ],
                ..Layer::default()
            }
        });
    }
    {
        let disabled = disabled.clone();
        let target = input_target.clone();
        let s = s.clone();
        create_layer(move || {
            let mode = s.mode;
            let ta = s.ta.clone();
            Layer {
                target: Some(target.clone()),
                enabled: Some(Enabled::Bool(!disabled() && s.mode.get() == Mode::Shell)),
                commands: vec![
                    Command::inline("backspace", move || {
                        mode.set(Mode::Normal);
                        true
                    })
                    .title("Exit shell mode")
                    .group("Prompt")
                    .enabled_fn(move || ta.visual_cursor().offset == 0),
                ],
                ..Layer::default()
            }
        });
    }
    // History.
    for previous in [true, false] {
        let disabled = disabled.clone();
        let target = input_target.clone();
        let s = s.clone();
        create_layer(move || {
            s.cursor_version.get();
            let auto_visible = s.auto.get().is_some_and(|a| a.visible_tracked() != Visible::None);
            let s2 = s.clone();
            let run = move |_: Option<String>| history_move(&s2, previous);
            Layer {
                priority: Some(1),
                target: Some(target.clone()),
                enabled: Some(Enabled::Bool(!disabled() && !auto_visible)),
                commands: vec![
                    Command::with_input(if previous { "prompt.history.previous" } else { "prompt.history.next" }, run)
                        .title(if previous { "Previous prompt history" } else { "Next prompt history" })
                        .group("Prompt"),
                ],
                ..Layer::default()
            }
        });
    }

    // The prompt's display while destination catalogs load (`promptDisplay`).
    let display = {
        let local = local.clone();
        create_memo_with(move |previous: Option<Option<crate::local::Display>>| {
            if !local.ready() {
                return previous.flatten();
            }
            Some(local.display())
        })
    };
    let display_agent = move || display.with(|d| d.as_ref().and_then(|d| d.agent.clone()));
    let agent_color = {
        let local = local.clone();
        create_memo_equals(move || display.with(|d| d.as_ref().and_then(|d| d.agent.as_ref()).is_some()).then(|| local.agent_color()), None)
    };
    let highlight = {
        let s = s.clone();
        create_memo_equals(
            move || {
                if s.muted() {
                    return rgba(theme.with(|th| th.border.base));
                }
                if s.mode.get() == Mode::Shell {
                    return rgba(theme.with(|th| th.text.action.primary.selected));
                }
                agent_color.get().unwrap_or_else(|| rgba(theme.with(|th| th.border.base)))
            },
            None,
        )
    };
    let agent_label = {
        let s = s.clone();
        create_memo(move || if s.mode.get() == Mode::Shell { Some("Shell".to_string()) } else { display_agent() })
    };
    let metadata_animations = move || animations;
    let agent_alpha = metadata::create_fade_in(move || agent_label.with(|a| a.is_some()), metadata_animations);
    let model_alpha = {
        let s = s.clone();
        metadata::create_fade_in(move || display_agent().is_some() && s.mode.get() == Mode::Normal, metadata_animations)
    };
    let variant_alpha = {
        let s = s.clone();
        metadata::create_fade_in(
            move || {
                display_agent().is_some()
                    && s.mode.get() == Mode::Normal
                    && display.with(|d| d.as_ref().is_some_and(|d| d.variant.is_some()))
            },
            metadata_animations,
        )
    };
    let border_highlight =
        create_memo_equals(move || tint(rgba(theme.with(|th| th.border.base)), highlight.get(), agent_alpha.get()), None);

    let placeholder_text = {
        let s = s.clone();
        create_memo(move || {
            let value = if s.mode.get() == Mode::Shell {
                if s.placeholders.shell.is_empty() {
                    return None;
                }
                format!("Run a command… \"{}\"", s.placeholders.shell[s.placeholder.get() % s.placeholders.shell.len()])
            } else {
                if s.placeholders.normal.is_empty() {
                    return None;
                }
                format!("Ask anything… \"{}\"", s.placeholders.normal[s.placeholder.get() % s.placeholders.normal.len()])
            };
            let (w, _) = dimensions.get();
            let width = if w < 44 { w as i64 - 5 } else { (w as i64 - 4).min(75) - 5 };
            Some(take_width(&value, width.max(1)).trim_end().to_string())
        })
    };
    let footer_location = {
        let s = s.clone();
        create_memo(move || {
            let Some(session) = s.session_id.as_deref() else {
                // No session yet: where the next session will be created.
                return Some(s.location());
            };
            if s.status() != "idle" {
                return None;
            }
            s.t.data.store.session.get(session).and_then(|info| info.get("location").cloned())
        })
    };
    let location_label = {
        let s = s.clone();
        create_memo(move || {
            let pending = s.pending_directory.get();
            let directory = match pending {
                Some(pending) => pending,
                None => footer_location.get()?.str("directory")?.to_string(),
            };
            let label = abbreviate_home(&directory, &s.t.paths.home);
            let location = obj! { "directory" => directory.as_str() };
            let branch = s
                .t
                .data
                .store
                .location
                .vcs
                .list(Some(&location))
                .and_then(|vcs| vcs.get("branch").and_then(|b| b.str("current")).map(str::to_string));
            Some(match branch {
                Some(branch) => format!("{label}:{branch}"),
                None => label,
            })
        })
    };
    let (location_width, set_location_width) = create_signal(dimensions.get_untracked().0 as i64);
    let _ = set_location_width;
    let location_display = create_memo(move || location_label.get().map(|label| truncate_file_path(&label, location_width.get())));
    let location_hovered = create_signal(false).0;
    let max_height = create_memo(move || (dimensions.get().1 / 3).max(6) as f32);
    let prompt_bg = create_memo_equals(move || theme.with(|th| rgba(th.decrease(th.background.raised.base, 1.0))), None);

    // <box ref={anchor} visible width="100%">
    if let Some(visible) = props.visible.clone() {
        bind(move || visible(), move |v| anchor.set_visible(v));
    }
    anchor.set_width(Some(Value::Percent(100.0)));
    let outer = box_();
    let inner = box_();
    insert_node(anchor, outer, None);
    insert_node(outer, inner, None);
    outer.set_width(Some(Value::Percent(100.0)));
    outer.set_border(BorderProp::Sides(vec![Side::Left]));
    outer.set_custom_border_chars(Some(border_chars(|c| {
        c.vertical = "┃";
        c.bottom_left = "╹";
    })));
    bind_always(move || border_highlight.get(), move |c| outer.set_border_color(Some(c)));
    bind(move || if dimensions.get().0 < 44 { 1.0 } else { 2.0 }, move |p| {
        inner.set_padding(Edge::Left, Some(Value::Point(p)));
        inner.set_padding(Edge::Right, Some(Value::Point(p)));
    });
    inner.set_padding(Edge::Top, Some(Value::Point(1.0)));
    inner.set_flex_shrink(Some(0.0));
    bind_always(move || prompt_bg.get(), move |c| inner.set_background_color(Some(c)));
    inner.set_flex_grow(Some(1.0));
    inner.set_width(Some(Value::Percent(100.0)));
    // TODO(port-tui): image previews (`prompt.image_preview`; the Image renderable).

    // <textarea>
    insert_node(inner, input, None);
    input.set_width(Some(Value::Percent(100.0)));
    bind(move || placeholder_text.get(), move |p| input.set_placeholder(p.as_deref().unwrap_or("")));
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| input.set_placeholder_color(c));
    {
        let s = s.clone();
        bind_always(
            move || if s.muted() { rgba(theme.with(|th| th.text.muted)) } else { rgba(theme.with(|th| th.text.base)) },
            move |c| {
                input.set_text_color(c);
                input.set_focused_text_color(c);
            },
        );
    }
    input.set_min_height(Some(Value::Point(1.0)));
    bind(move || max_height.get(), move |h| input.set_max_height(Some(Value::Point(h))));
    {
        let s = s.clone();
        input.set_on_content_change(Some(Rc::new(move || {
            let value = s.ta.plain_text();
            s.prompt.update(|p| p.text = value.clone());
            if let Some(auto) = s.auto.get_untracked() {
                auto.on_input(&value);
            }
            s.sync_extmarks_with_prompt_parts();
            s.cursor_version.set(s.cursor_version.get_untracked() + 1);
        })));
    }
    {
        let version = s.cursor_version;
        input.set_on_cursor_change(Some(Rc::new(move || version.set(version.get_untracked() + 1))));
    }
    {
        let s = s.clone();
        input.set_on_key_down(Some(Rc::new(move |event| {
            if s.disabled() {
                event.prevent_default();
            }
        })));
    }
    {
        let s = s.clone();
        input.set_on_submit(Some(Rc::new(move || {
            if s.disabled() {
                return;
            }
            // IME: double-deferred so the last composed character is in the text.
            let state = s.clone();
            renderer::set_timeout(0, move || {
                renderer::set_timeout(0, move || {
                    tokio::task::spawn_local(async move {
                        submit(state, Delivery::Steer).await;
                    });
                });
            });
        })));
    }
    {
        let s = s.clone();
        input.set_on_paste(Some(Rc::new(move |event: &mut PasteEvent| {
            if s.disabled() {
                event.prevent_default();
                return;
            }
            let normalized = String::from_utf8_lossy(&event.bytes).replace("\r\n", "\n").replace('\r', "\n");
            if event.bytes.is_empty() {
                keymap::dispatch("prompt.paste", None);
                return;
            }
            event.prevent_default();
            let state = s.clone();
            s.enqueue_paste(Box::new(move |changed| Box::pin(state.paste_input_text(normalized, changed))));
        })));
    }
    // `ref`: the clipboard copies placeholders as their text.
    {
        let s2 = s.clone();
        let _ = &s2;
        let extmarks = ta.extmarks();
        if s.part_type.get() == 0 {
            s.part_type.set(extmarks.register_type("prompt-part").unwrap_or(0));
        }
        // TODO(port-tui): `getClipboardText` (copying expands placeholders) needs the renderer's selection copy.
    }
    {
        let syntax_memo = create_memo_equals(move || Rc::new(theme.with(syntax_style)), None);
        let ta = ta.clone();
        create_render_effect(move || {
            let style = syntax_memo.get();
            ta.set_syntax_style(Some(&style));
        });
    }
    input.set_focused_background_color(RGBA::from_values(0.0, 0.0, 0.0, 0.0));
    {
        let s = s.clone();
        input.set_on_mouse(
            Some(MouseEventType::Down),
            Some(Rc::new(move |event| {
                if s.disabled() {
                    event.prevent_default();
                    return;
                }
                if event.button != 0 {
                    return;
                }
                s.input.focus();
                let extmark = s.ta.extmarks().get_at_offset(s.ta.cursor_offset() as i64).into_iter().find(|item| {
                    matches!(s.extmark_to_part.borrow().get(&item.id), Some(PartRef::Pasted(_)))
                });
                if extmark.is_some_and(|e| s.expand_pasted_text(e.id)) {
                    event.prevent_default();
                    event.stop_propagation();
                }
            })),
        );
    }
    (props.on_ref)(Some(reference.clone()));
    *t.prompt.borrow_mut() = Some(input);
    {
        let ta = ta.clone();
        let disabled = disabled.clone();
        renderer::set_timeout(0, move || {
            if ta.el.is_destroyed() {
                return;
            }
            let color = if disabled() { theme.with_untracked(|th| th.background.raised.base) } else { theme.with_untracked(|th| th.text.base) };
            ta.el.set_cursor_color_prop(rgba(color));
        });
    }

    // The metadata row.
    let meta = box_();
    insert_node(inner, meta, None);
    meta.set_flex_direction(FlexDirection::Row);
    meta.set_flex_shrink(Some(0.0));
    meta.set_padding(Edge::Top, Some(Value::Point(1.0)));
    meta.set_gap(2, Some(Value::Point(1.0)));
    meta.set_justify_content(Justify::SpaceBetween);
    {
        let s = s.clone();
        let s_muted = s.clone();
        let props = metadata::MetadataProps {
            shell: Rc::new(move || s.mode.get() == Mode::Shell),
            agent: Rc::new(move || agent_label.get()),
            // `local.permission.mode === "autoaccept"` (context/permission.tsx).
            auto: {
                let auto = t.args.auto;
                let config = t.config_context.clone();
                Rc::new(move || auto || config.data().session.permissions == "autoaccept")
            },
            model: Rc::new(move || display.with(|d| d.as_ref().map(|d| d.model.clone()).unwrap_or_default())),
            provider: Rc::new(move || display.with(|d| d.as_ref().map(|d| d.provider.clone()).unwrap_or_default())),
            variant: Rc::new(move || display.with(|d| d.as_ref().and_then(|d| d.variant.clone()))),
            // TODO(port-tui): usage limits (OpenCode Console models only).
            limit: Rc::new(|| None),
            muted: Rc::new(move || s_muted.muted()),
            highlight: Rc::new(move || highlight.get()),
            agent_alpha: Rc::new(move || agent_alpha.get()),
            model_alpha: Rc::new(move || model_alpha.get()),
            variant_alpha: Rc::new(move || variant_alpha.get()),
        };
        insert(meta, component(move || metadata::metadata_row(props)), At::Only);
    }

    // The bottom edge.
    let edge = box_();
    let edge_inner = box_();
    insert_node(anchor, edge, None);
    insert_node(edge, edge_inner, None);
    edge.set_height(Some(Value::Point(1.0)));
    edge.set_border(BorderProp::Sides(vec![Side::Left]));
    bind_always(move || border_highlight.get(), move |c| edge.set_border_color(Some(c)));
    bind(move || prompt_bg.with(|bg| bg.a() != 0.0), move |filled| {
        edge.set_custom_border_chars(Some(border_chars(|c| c.vertical = if filled { "╹" } else { " " })));
        edge_inner.set_custom_border_chars(Some(border_chars(|c| c.horizontal = if filled { "▀" } else { " " })));
    });
    edge_inner.set_height(Some(Value::Point(1.0)));
    edge_inner.set_border(BorderProp::Sides(vec![Side::Bottom]));
    bind_always(move || prompt_bg.get(), move |c| edge_inner.set_border_color(Some(c)));

    // The footer row: `prompt.footer.status` (the running spinner, or the location), `prompt.footer.file`, and the
    // footer plugin appended to `prompt.footer`.
    let footer = box_();
    insert_node(anchor, footer, None);
    footer.set_width(Some(Value::Percent(100.0)));
    footer.set_flex_direction(FlexDirection::Row);
    footer.set_justify_content(Justify::SpaceBetween);
    footer.set_gap(2, Some(Value::Point(2.0)));
    let status = box_();
    insert_node(footer, status, None);
    status.set_flex_grow(Some(1.0));
    status.set_flex_shrink(Some(1.0));
    status.set_min_width(Some(Value::Point(0.0)));
    status.set_on_size_change(Some(Rc::new(move || {
        let width = status.width() as i64;
        renderer::next_tick(move || set_location_width.set(width));
    })));
    let running = {
        let s = s.clone();
        move || s.status() == "running"
    };
    let location_view = move || {
        show_value(
            move || location_display.get(),
            move |location| {
                let el = text();
                el.set_id("prompt.footer.location");
                bind_always(
                    move || if location_hovered.get() { rgba(theme.with(|th| th.text.base)) } else { rgba(theme.with(|th| th.text.muted)) },
                    move |c| el.set_fg(Some(c)),
                );
                el.set_wrap_mode(WrapMode::None);
                el.set_truncate(true);
                el.set_flex_grow(Some(1.0));
                el.set_flex_shrink(Some(1.0));
                el.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| location_hovered.set(true))));
                el.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| location_hovered.set(false))));
                // TODO(port-tui): `useWorkingDirectoryActions` (a click opens the workspaces dialog).
                insert(el, View::dynamic(move || View::from(location.get().unwrap_or_default())), At::Only);
                View::Node(el)
            },
            Some(Rc::new(|| View::Node(text()))),
        )
    };
    let running_view = {
        let s = s.clone();
        move || footer::running_status(s.interrupt, animations, agent_color)
    };
    insert(
        status,
        switch(vec![when(running, running_view), when(|| true, location_view)], None),
        At::Only,
    );
    let footer_props = footer::FooterProps {
        session_id: props.session_id.clone(),
        shell: {
            let mode = s.mode;
            Rc::new(move || mode.get() == Mode::Shell)
        },
        show_details: {
            let interrupt = s.interrupt;
            Rc::new(move || interrupt.get() == 0 || dimensions.get().0 >= 80)
        },
        shortcuts,
    };
    insert(footer, component(move || footer::prompt_footer(footer_props)), At::End);

    // <Autocomplete>
    let auto_view = {
        let s = s.clone();
        let frecency = frecency.clone();
        component(move || {
            autocomplete::autocomplete(AutocompleteProps {
                session_id: s.session_id.clone(),
                anchor: s.anchor,
                input: s.input,
                state: s.clone() as Rc<dyn autocomplete::PromptHost>,
                frecency: frecency.clone(),
                on_ref: {
                    let auto = s.auto;
                    Rc::new(move |r| auto.set(Some(r)))
                },
            })
        })
    };
    View::list([View::Node(anchor), auto_view])
}

/// The history commands' `run` (`prompt.history.previous` / `.next`).
fn history_move(s: &Rc<State>, previous: bool) -> bool {
    let ta = &s.ta;
    if previous {
        if ta.cursor_offset() != 0 {
            if ta.scroll_y() + ta.visual_cursor().visual_row == 0 {
                ta.set_cursor_offset(0);
                return true;
            }
            ta.move_cursor_up();
            return true;
        }
    } else {
        let end = s.measure(&ta.plain_text()) as u32;
        if ta.cursor_offset() != end {
            if ta.scroll_y() + ta.visual_cursor().visual_row == ta.total_virtual_line_count().saturating_sub(1) {
                ta.set_cursor_offset(end);
                return true;
            }
            ta.move_cursor_down();
            return true;
        }
    }
    let Some(item) = s.history.move_(if previous { -1 } else { 1 }, &ta.plain_text()) else { return false };
    ta.set_text(&item.text);
    s.prompt.set(item.clone());
    s.mode.set(item.mode.unwrap_or(Mode::Normal));
    s.restore_extmarks_from_prompt(&item);
    ta.set_cursor_offset(if previous { 0 } else { s.measure(&item.text) as u32 });
    true
}

/// `session.cd`'s `run(input)`.
fn change_directory(s: Rc<State>, input: Option<String>) {
    let Some(value) = input.map(|i| text::trim(&i).to_string()).filter(|i| !i.is_empty()) else {
        s.toast.show(ToastInput::new(Variant::Error, "Directory is required"));
        return;
    };
    let home = s.t.paths.home.clone();
    let expanded = if value == "~" {
        home.clone()
    } else if let Some(rest) = value.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        value.clone()
    };
    let session = s.session_id.as_deref().and_then(|id| s.t.data.store.session.get(id));
    let base = session
        .as_ref()
        .and_then(|session| session.get("location").and_then(|l| l.str("directory")).map(str::to_string))
        .unwrap_or_else(|| s.location_directory());
    let directory = frecency::resolve_path(&base, &expanded);
    let client = s.t.data.client.clone();
    match s.session_id.clone() {
        None => {
            s.pending_directory.set(Some(directory.clone()));
            let call = client.call_named("location.get", &obj! { "location" => obj! { "directory" => directory.as_str() } }, RequestOptions::default());
            tokio::task::spawn_local(async move {
                match call.await {
                    Err(error) => {
                        s.toast.show(ToastInput::new(Variant::Error, error_message(&error)).title("Failed to change directory"));
                        s.pending_directory.set(None);
                    }
                    Ok(location) => {
                        // TODO(port-tui): `directoryRecents.touch` (the storage context).
                        s.t.data.location.set(Some(location.json()));
                        s.pending_directory.set(None);
                    }
                }
            });
        }
        Some(session_id) => {
            let call = client.call_named(
                "session.move",
                &obj! { "sessionID" => session_id.as_str(), "directory" => value.as_str() },
                RequestOptions::default(),
            );
            tokio::task::spawn_local(async move {
                if let Err(error) = call.await {
                    s.toast.show(ToastInput::new(Variant::Error, error_message(&error)).title("Failed to change directory"));
                }
            });
        }
    }
}

// -- what autocomplete needs of the prompt -----------------------------------------------------------------------

impl autocomplete::PromptHost for State {
    fn value(&self) -> String {
        self.prompt.with(|p| p.text.clone())
    }
    fn textarea(&self) -> Textarea {
        self.ta.clone()
    }
    fn update_prompt(&self, f: &mut dyn FnMut(&mut PromptInfo)) {
        self.prompt.update(|p| f(p));
    }
    fn set_extmark(&self, part: PartRef, id: u32) {
        self.extmark_to_part.borrow_mut().insert(id, part);
    }
    fn create_part_extmark(&self, start: i64, end: i64, kind: autocomplete::PartKind) -> u32 {
        let style = match kind {
            autocomplete::PartKind::File => self.styles.file,
            autocomplete::PartKind::Agent => self.styles.agent,
            autocomplete::PartKind::Skill => self.styles.skill,
        };
        self.create_extmark(start, end, style)
    }
    fn has_skill(&self, id: &str) -> bool {
        self.prompt.with_untracked(|p| p.skills.as_ref().is_some_and(|s| s.iter().any(|skill| skill.id == id)))
    }
    fn location(&self) -> Json {
        State::location(self)
    }
    fn directory_options(&self, query: &str) -> Vec<autocomplete::AutocompleteOption> {
        if !query.is_empty() {
            return vec![];
        }
        // TODO(port-tui): directory recents (`useDirectoryRecents`, the storage context).
        vec![]
    }
    fn argument_autocomplete(&self, command: &Command) -> bool {
        command.id.as_deref() == Some("session.cd")
    }
}
