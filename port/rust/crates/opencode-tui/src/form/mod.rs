//! The form prompt (`routes/session/form.tsx`): a form's fields one tab at a time (options to pick or toggle, a
//! custom answer, typed answers in a textarea, external links to acknowledge), the review tab, submission and
//! dismissal, with drafts kept across remounts; and [`util`] (`util/form.ts`).
//!
//! The store's `answers`, `custom`, `externalReady`, `selected`, `editing`, `error` and `tab` are one signal each.
//!
//! TODO(port-tui): the dialogs slice's dialog stack (`dialog.stack.length === 0` holds here) and the clipboard
//! (`prompt.paste` and `c` copy do nothing yet).
pub mod util;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::{RGBA, text_attributes};
use opencode_tui_core::elements::{box_, scrollbox, span, text, textarea};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::renderables::scrollbox::{ScrollBarOptions, ScrollUnit};
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element, FlexDirection, Justify, MouseEventType};
use opencode_tui_sync::json::JsonExt;

use self::util::*;
use crate::ctx::tui;
use crate::keymap::{Command, Layer, create_layer, intercept_key, mode_current, mode_push, parsed_key};
use crate::permission::split_border_chars;
use crate::util::rgba;

pub const FORM_MODE: &str = "form";

/// The textarea methods the form uses.
mod ta {
    use opencode_tui_core::renderables::textarea::TextareaAction;
    use opencode_tui_core::tree::Element;

    pub fn plain_text(el: Element) -> String {
        el.textarea_text()
    }
    pub fn set_text(el: Element, text: &str) {
        el.set_textarea_text(text);
    }
    /// `gotoLineEnd()`.
    pub fn goto_line_end(el: Element) {
        el.textarea_action(TextareaAction::LineEnd);
    }
    /// `insertText(text)` at the cursor.
    pub fn insert_text(el: Element, text: &str) {
        el.textarea_insert_text(text);
    }
    /// `scrollY + visualCursor.visualRow`.
    pub fn cursor_row(el: Element) -> u32 {
        el.editor_view().map(|view| view.get_viewport().y + view.get_visual_cursor().visual_row).unwrap_or(0)
    }
}

/// `truncate(label, max)` (UTF-16 lengths).
fn truncate(label: &str, max: usize) -> String {
    let units: Vec<u16> = label.encode_utf16().collect();
    if units.len() <= max {
        return label.to_string();
    }
    let kept = String::from_utf16_lossy(&units[..max - 1]);
    format!("{}…", kept.trim_end_matches(opencode_tui_sync::js::is_space))
}

#[derive(Clone, Default, PartialEq)]
struct FormDraft {
    tab: usize,
    answers: HashMap<String, Json>,
    custom: HashMap<String, String>,
    external_ready: HashMap<String, bool>,
    selected: usize,
    editing: bool,
    error: String,
}

thread_local! {
    /// In-progress answers per form across remounts (the session route unmounts on tab switch); a draft is
    /// consumed on take.
    static DRAFTS: RefCell<HashMap<String, FormDraft>> = RefCell::new(HashMap::new());
}

#[derive(Clone, Copy)]
struct Store {
    tab: Signal<usize>,
    answers: Signal<HashMap<String, Json>>,
    custom: Signal<HashMap<String, String>>,
    external_ready: Signal<HashMap<String, bool>>,
    selected: Signal<usize>,
    editing: Signal<bool>,
    error: Signal<String>,
}

impl Store {
    fn answer_of(&self, key: &str) -> Option<Json> {
        self.answers.with(|a| a.get(key).cloned())
    }
    fn custom_of(&self, key: &str) -> Option<String> {
        self.custom.with(|c| c.get(key).cloned())
    }
    fn set_custom(&self, key: &str, value: &str) {
        let key = key.to_string();
        let value = value.to_string();
        self.custom.update(|c| {
            c.insert(key, value);
        });
    }
    fn snapshot(&self) -> FormDraft {
        FormDraft {
            tab: self.tab.get_untracked(),
            answers: self.answers.get_untracked(),
            custom: self.custom.get_untracked(),
            external_ready: self.external_ready.get_untracked(),
            selected: self.selected.get_untracked(),
            editing: self.editing.get_untracked(),
            error: self.error.get_untracked(),
        }
    }
}

fn fields_of(form: &Json) -> Vec<Json> {
    form.arr("fields").into_iter().flatten().cloned().collect()
}

/// `{key} <span muted>{hint}</span>`.
fn hint_text(el: Element, key: impl Fn() -> String + 'static, hint: impl Fn() -> String + 'static) {
    let theme = tui().theme;
    bind_always(move || rgba(theme.with(|t| t.text.base)), move |c| el.set_fg(Some(c)));
    insert(el, View::dynamic(move || View::from(key())), At::End);
    insert_node(el, opencode_tui_core::renderables::text::text_node_from_string(" "), None);
    let muted = span();
    insert_node(el, muted, None);
    bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| muted.set_span_style(Some(c), None, 0, 0));
    insert(muted, View::dynamic(move || View::from(hint())), At::Only);
}

fn colored_text(value: impl Fn() -> String + 'static, fg: impl Fn() -> RGBA + 'static) -> Element {
    let el = text();
    bind_always(fg, move |c| el.set_fg(Some(c)));
    insert(el, View::dynamic(move || View::from(value())), At::Only);
    el
}

/// `<FormPrompt form>`.
pub fn form_prompt(form: Rc<Json>) -> View {
    let t = tui();
    let theme = t.theme;
    let dimensions = t.dimensions;
    let store_data = t.data.store.clone();
    let enabled = crate::interactivity::use_interactivity();
    let active = Rc::new(move || enabled() && mode_current() == FORM_MODE);
    let all_fields = Rc::new(fields_of(&form));
    let configured_fields: Vec<Json> =
        all_fields.iter().filter(|f| is_answer_field(f) && f.get("hidden") != Some(&Json::Bool(true))).cloned().collect();
    let (initial_answers, initial_custom) = initial_values(&all_fields);
    let form_id = form.str("id").unwrap_or("").to_string();
    let draft = DRAFTS.with(|d| d.borrow_mut().remove(&form_id));

    let (tab_hover, set_tab_hover) = create_signal(None::<Option<usize>>);
    let (review_content_height, set_review_content_height) = create_signal(0.0f32);
    let draft = draft.unwrap_or_else(|| FormDraft {
        tab: 0,
        answers: initial_answers,
        custom: initial_custom,
        external_ready: HashMap::new(),
        selected: selected(configured_fields.first(), configured_fields.first().and_then(|f| f.get("default"))),
        editing: false,
        error: String::new(),
    });
    let store = Store {
        tab: create_signal(draft.tab).0,
        answers: create_signal_with(draft.answers, None).0,
        custom: create_signal_with(draft.custom, None).0,
        external_ready: create_signal_with(draft.external_ready, None).0,
        selected: create_signal(draft.selected).0,
        editing: create_signal(draft.editing).0,
        error: create_signal(draft.error).0,
    };

    let textarea_ref: Rc<RefCell<Option<Element>>> = Rc::default();
    let (input_target, set_input_target) = create_signal(None::<Element>);
    let review: Rc<RefCell<Option<Element>>> = Rc::default();

    let message = {
        let form = form.clone();
        create_memo(move || form.get("metadata").and_then(|m| m.str("message")).map(str::to_string))
    };
    let active_fields = {
        let all_fields = all_fields.clone();
        create_memo(move || {
            let mut answers: HashMap<String, Option<Json>> = HashMap::new();
            all_fields
                .iter()
                .filter(|field| {
                    if field_type(field) == "external" {
                        return true;
                    }
                    let active = field.arr("when").into_iter().flatten().all(|when| {
                        let Some(Some(value)) = answers.get(when.str("key").unwrap_or("")) else { return false };
                        let expected = when.get("value");
                        let hit = match value {
                            Json::Array(items) => items.iter().any(|item| Some(item) == expected),
                            other => Some(other) == expected,
                        };
                        if when.str("op") == Some("eq") { hit } else { !hit }
                    });
                    if active {
                        answers.insert(field_key(field).to_string(), store.answer_of(field_key(field)));
                    }
                    active
                })
                .cloned()
                .collect::<Vec<Json>>()
        })
    };
    // Hidden fields are not asked: they answer with their default.
    let fields = create_memo(move || {
        active_fields.with(|list| {
            list.iter()
                .filter(|f| field_type(f) == "external" || f.get("hidden") != Some(&Json::Bool(true)))
                .cloned()
                .collect::<Vec<Json>>()
        })
    });
    let single = create_memo(move || {
        fields.with(|list| {
            if list.len() != 1 {
                return false;
            }
            let field = &list[0];
            match field_type(field) {
                "external" => false,
                "boolean" => true,
                "string" => field.get("options").is_some(),
                _ => false,
            }
        })
    });
    let tabs = create_memo(move || if single.get() { 1 } else { fields.with(Vec::len) + 1 });
    let tabbed = create_memo(move || {
        let width = fields.with(|list| {
            list.iter().fold("Submit".len() + 3, |sum, item| sum + opencode_tui_sync::js::length(&truncate(&form_label(item), 24)) + 3)
        });
        (width as i64) <= dimensions.get().0 as i64 - 8
    });
    let completed = move |item: &Json| {
        let Some(value) = store.answer_of(field_key(item)) else { return false };
        if field_type(item) == "external" {
            return value == Json::Bool(true);
        }
        validate_value(item, Some(&value)).is_none()
    };
    let answered = create_memo(move || fields.with(|list| list.iter().filter(|f| completed(f)).count()));
    let field = create_memo(move || {
        let tab = store.tab.get();
        fields.with(|list| list.get(tab).cloned())
    });
    let answer_field = create_memo(move || field.get().filter(is_answer_field));
    let external_field = create_memo(move || field.get().filter(|f| field_type(f) == "external"));
    let confirm = create_memo(move || !single.get() && store.tab.get() >= fields.with(Vec::len));
    let review_max_height = create_memo(move || 3.max(dimensions.get().1 as i64 - 14));
    let configured_rows = create_memo(move || rows(answer_field.get().as_ref()));
    let rows_memo = create_memo(move || {
        let Some(current) = answer_field.get() else { return Vec::new() };
        let configured = configured_rows.get();
        let value = store.answer_of(field_key(&current));
        let Some(Json::Array(values)) = value.filter(|_| field_type(&current) == "multiselect") else { return configured };
        let custom = store.custom_of(field_key(&current));
        let mut out = configured.clone();
        for item in values {
            if configured.iter().any(|row| row.value == item) {
                continue;
            }
            if matches!((&item, &custom), (Json::String(s), Some(c)) if s == c) {
                continue;
            }
            let label = opencode_tui_sync::js::string(&item);
            out.push(FormRow { value: item, label, description: None });
        }
        out
    });
    let is_textual = create_memo(move || !confirm.get() && textual(answer_field.get().as_ref()));
    let is_custom = create_memo(move || custom(answer_field.get().as_ref()));
    let multi = create_memo(move || answer_field.with(|f| f.as_ref().is_some_and(|f| field_type(f) == "multiselect")));
    let placeholder = create_memo(move || {
        let current = answer_field.get();
        if let Some(current) = current.as_ref().filter(|f| field_type(f) == "string") {
            if let Some(p) = current.str("placeholder").filter(|p| !p.is_empty()) {
                return p.to_string();
            }
            match current.str("format") {
                Some("email") => return "name@example.com".into(),
                Some("uri") => return "https://example.com".into(),
                Some("date") => return "YYYY-MM-DD".into(),
                Some("date-time") => return "YYYY-MM-DDTHH:MM:SSZ".into(),
                _ => {}
            }
        }
        if let Some(current) = current.as_ref().filter(|f| matches!(field_type(f), "number" | "integer")) {
            let n = |key| current.num(key).map(opencode_tui_sync::js::number);
            match (n("minimum"), n("maximum")) {
                (Some(min), Some(max)) => return format!("{min}-{max}"),
                (Some(min), None) => return format!("at least {min}"),
                (None, Some(max)) => return format!("at most {max}"),
                _ => {}
            }
        }
        "Type your answer".to_string()
    });
    let other = create_memo(move || is_custom.get() && store.selected.get() == rows_memo.with(Vec::len));
    let input = create_memo(move || {
        let key = answer_field.with(|f| f.as_ref().map(|f| field_key(f).to_string()).unwrap_or_default());
        store.custom_of(&key).unwrap_or_default()
    });
    let custom_picked = create_memo(move || {
        let value = input.get();
        if value.is_empty() {
            return false;
        }
        let key = answer_field.with(|f| f.as_ref().map(|f| field_key(f).to_string()).unwrap_or_default());
        match store.answer_of(&key) {
            Some(Json::Array(items)) => items.iter().any(|i| i.str_value() == Some(value.as_str())),
            Some(answer) => answer == Json::String(value),
            None => false,
        }
    });
    let custom_checked = create_memo(move || custom_picked.get() || (multi.get() && other.get() && store.editing.get()));
    let action_label = create_memo(move || {
        if confirm.get() {
            return "submit";
        }
        if let Some(external) = external_field.get() {
            if store.answer_of(field_key(&external)) == Some(Json::Bool(true)) {
                return "continue";
            }
            return if store.external_ready.with(|r| r.get(field_key(&external)).copied().unwrap_or(false)) {
                "I finished"
            } else {
                "open link"
            };
        }
        if multi.get() {
            if other.get() && store.editing.get() {
                return "done";
            }
            if other.get() && input.get().is_empty() {
                return "edit";
            }
            return "toggle";
        }
        if single.get() {
            return "submit";
        }
        "confirm"
    });

    {
        // A reply or cancel removes the form from data before this unmount runs, so a form still listed here is
        // only hidden by navigation and worth restoring.
        let form = form.clone();
        let store_data = store_data.clone();
        let textarea_ref = textarea_ref.clone();
        on_cleanup(move || {
            let session = form.str("sessionID").unwrap_or("");
            let pending = store_data
                .session
                .form
                .list(session, form.get("location"))
                .is_some_and(|list| list.iter().any(|item| item.str("id") == form.str("id")));
            if !pending {
                return;
            }
            // Textual answers live in the editor until committed, so capture them here.
            let current = answer_field.get_untracked();
            let mut snapshot = store.snapshot();
            if let (Some(current), Some(area)) = (current, *textarea_ref.borrow()) {
                if area.is_alive() {
                    snapshot.custom.insert(field_key(&current).to_string(), ta::plain_text(area));
                }
            }
            DRAFTS.with(|d| d.borrow_mut().insert(form.str("id").unwrap_or("").to_string(), snapshot));
        });
    }

    // Refs publish after initialization so burst typing stays with the interceptor until the editor is ready.
    {
        let active = active.clone();
        create_effect(move || {
            let Some(target) = input_target.get() else { return };
            if !target.is_alive() {
                return;
            }
            if !active() {
                target.blur();
                target.set_focusable(false);
                return;
            }
            target.set_focusable(true);
            target.focus();
        });
    }

    // Actions.
    let answer: Rc<dyn Fn(&str, Option<Json>)> = Rc::new(move |key: &str, value: Option<Json>| {
        let field = fields.with_untracked(|list| list.iter().find(|item| field_key(item) == key).cloned());
        let empty_optional_multiselect = matches!(&value, Some(Json::Array(items)) if items.is_empty())
            && field.as_ref().is_some_and(|f| field_type(f) == "multiselect" && f.get("required") != Some(&Json::Bool(true)));
        let key = key.to_string();
        store.answers.update(|answers| match value.filter(|_| !empty_optional_multiselect) {
            Some(value) => {
                answers.insert(key, value);
            }
            None => {
                answers.remove(&key);
            }
        });
        store.error.set(String::new());
    });
    let show_error = move |message: String| store.error.set(message);
    let reply: Rc<dyn Fn(Json)> = {
        let form = form.clone();
        let store_data = store_data.clone();
        Rc::new(move |answer: Json| {
            let input = opencode_tui_sync::obj! {
                "sessionID" => form.str("sessionID").unwrap_or(""),
                "formID" => form.str("id").unwrap_or(""),
                "answer" => answer,
            };
            let promise = store_data.session.form.reply(input, form.get("location"));
            tokio::task::spawn_local(async move {
                if let Err(error) = promise.wait().await {
                    show_error(error.message());
                }
            });
        })
    };
    let cancel: Rc<dyn Fn()> = {
        let form = form.clone();
        let store_data = store_data.clone();
        Rc::new(move || {
            let input = opencode_tui_sync::obj! {
                "sessionID" => form.str("sessionID").unwrap_or(""),
                "formID" => form.str("id").unwrap_or(""),
            };
            let promise = store_data.session.form.cancel(input, form.get("location"));
            tokio::task::spawn_local(async move {
                if let Err(error) = promise.wait().await {
                    show_error(error.message());
                }
            });
        })
    };
    let reply_single: Rc<dyn Fn(&Json, Json)> = {
        let reply = reply.clone();
        Rc::new(move |field: &Json, value: Json| {
            let mut answer = opencode_protocol::wire::Object::default();
            answer.insert(field_key(field).to_string(), value);
            reply(Json::Object(answer));
        })
    };
    let select_tab: Rc<dyn Fn(usize)> = Rc::new(move |index: usize| {
        let next = fields.with_untracked(|list| list.get(index).cloned());
        batch(|| {
            store.tab.set(index);
            store.selected.set(match next.as_ref().filter(|n| is_answer_field(n)) {
                Some(next) => selected(Some(next), store.answer_of(field_key(next)).as_ref()),
                None => 0,
            });
            store.editing.set(false);
            store.error.set(String::new());
        });
    });
    let pick: Rc<dyn Fn(Json, Option<String>)> = {
        let answer = answer.clone();
        let reply_single = reply_single.clone();
        let select_tab = select_tab.clone();
        Rc::new(move |value: Json, custom_value: Option<String>| {
            let Some(current) = answer_field.get_untracked() else { return };
            if let Some(invalid) = validate_value(&current, Some(&value)) {
                store.error.set(invalid);
                return;
            }
            answer(field_key(&current), Some(value.clone()));
            if let Some(custom_value) = custom_value {
                store.set_custom(field_key(&current), &custom_value);
            }
            if single.get_untracked() {
                reply_single(&current, value);
                return;
            }
            select_tab(store.tab.get_untracked() + 1);
        })
    };
    let toggle: Rc<dyn Fn(&str)> = {
        let answer = answer.clone();
        Rc::new(move |value: &str| {
            let Some(current) = answer_field.get_untracked() else { return };
            let next = toggle_multiselect(store.answer_of(field_key(&current)).as_ref(), value);
            answer(field_key(&current), Some(next));
        })
    };
    let update_custom: Rc<dyn Fn(&Json, String)> = {
        let answer = answer.clone();
        Rc::new(move |current: &Json, value: String| {
            let key = field_key(current);
            let previous = store.custom_of(key);
            if previous.as_deref() == Some(value.as_str()) {
                return;
            }
            store.set_custom(key, &value);
            let next = if field_type(current) == "multiselect" {
                Some(set_multiselect_custom(store.answer_of(key).as_ref(), previous.as_deref(), &value))
            } else {
                (!value.is_empty()).then(|| Json::String(value.clone()))
            };
            answer(key, next);
        })
    };
    let select_option: Rc<dyn Fn()> = {
        let toggle = toggle.clone();
        let pick = pick.clone();
        Rc::new(move || {
            if other.get_untracked() {
                if !multi.get_untracked() {
                    store.editing.set(true);
                    return;
                }
                let value = input.get_untracked();
                if !value.is_empty() && custom_picked.get_untracked() {
                    toggle(&value);
                    return;
                }
                store.editing.set(true);
                return;
            }
            let Some(row) = rows_memo.with_untracked(|r| r.get(store.selected.get_untracked()).cloned()) else { return };
            if multi.get_untracked() {
                toggle(&opencode_tui_sync::js::string(&row.value));
                return;
            }
            pick(row.value, None);
        })
    };
    let paste_custom: Rc<dyn Fn(&str) -> bool> = {
        let update_custom = update_custom.clone();
        Rc::new(move |value: &str| {
            let Some(current) = answer_field.get_untracked() else { return false };
            if is_textual.get_untracked() || !is_custom.get_untracked() || confirm.get_untracked() {
                return false;
            }
            store.selected.set(rows_memo.with_untracked(Vec::len));
            update_custom(&current, input.get_untracked() + value);
            store.editing.set(true);
            true
        })
    };
    {
        let active = active.clone();
        let paste_custom = paste_custom.clone();
        let textarea_ref = textarea_ref.clone();
        let token = renderer::on_paste(move |event| {
            if !active() {
                return;
            }
            let decoded = opencode_tui_input::paste::decode_paste_bytes(&event.bytes);
            let value = opencode_tui_input::paste::strip_ansi_sequences(&decoded).replace("\r\n", "\n").replace('\r', "\n");
            let area = *textarea_ref.borrow();
            if store.editing.get_untracked() && area.is_some_and(|a| a.is_alive() && renderer::current_focused_renderable() == Some(a)) {
                ta::insert_text(area.unwrap(), &value);
                event.default_prevented = true;
                return;
            }
            if !paste_custom(&value) {
                return;
            }
            event.default_prevented = true;
        });
        on_cleanup(move || renderer::off_paste(token));
    }
    let commit_input: Rc<dyn Fn(&str) -> bool> = {
        let answer = answer.clone();
        Rc::new(move |text: &str| {
            let Some(current) = answer_field.get_untracked() else { return false };
            let key = field_key(&current).to_string();
            let textual = is_textual.get_untracked();
            let multi = multi.get_untracked();
            if text.is_empty() {
                let value = if !textual && multi {
                    Some(set_multiselect_custom(store.answer_of(&key).as_ref(), store.custom_of(&key).as_deref(), ""))
                } else {
                    None
                };
                if textual || !multi {
                    if let Some(invalid) = validate_value(&current, value.as_ref()) {
                        store.error.set(invalid);
                        return false;
                    }
                }
                answer(&key, value);
                store.set_custom(&key, "");
                store.editing.set(false);
                return true;
            }
            if textual && matches!(field_type(&current), "number" | "integer") {
                let value = Json::Number(js_number(text));
                if let Some(invalid) = validate_value(&current, Some(&value)) {
                    store.error.set(invalid);
                    return false;
                }
                answer(&key, Some(value));
            }
            if textual && field_type(&current) == "string" {
                let value = Json::String(text.to_string());
                if let Some(invalid) = validate_value(&current, Some(&value)) {
                    store.error.set(invalid);
                    return false;
                }
                answer(&key, Some(value));
            }
            if !textual && multi {
                answer(&key, Some(set_multiselect_custom(store.answer_of(&key).as_ref(), store.custom_of(&key).as_deref(), text)));
            }
            if !textual && !multi {
                let value = Json::String(text.to_string());
                if let Some(invalid) = validate_value(&current, Some(&value)) {
                    store.error.set(invalid);
                    return false;
                }
                answer(&key, Some(value));
            }
            let configured = field_type(&current) == "string"
                && current.arr("options").into_iter().flatten().any(|o| o.str("value") == Some(text));
            store.set_custom(&key, if configured { "" } else { text });
            store.editing.set(false);
            true
        })
    };
    let submit_input: Rc<dyn Fn(&str, i64)> = {
        let commit_input = commit_input.clone();
        let select_tab = select_tab.clone();
        Rc::new(move |text: &str, direction: i64| {
            let tabs = tabs.get_untracked() as i64;
            let next = ((store.tab.get_untracked() as i64 + direction + tabs) % tabs) as usize;
            if !commit_input(text) {
                if direction == -1 {
                    select_tab(next);
                }
                return;
            }
            if !single.get_untracked() {
                select_tab(next);
            }
        })
    };
    let select_tab_from_mouse: Rc<dyn Fn(Option<Json>)> = {
        let commit_input = commit_input.clone();
        let select_tab = select_tab.clone();
        let textarea_ref = textarea_ref.clone();
        Rc::new(move |target: Option<Json>| {
            let target_index = fields.with_untracked(|list| {
                let length = list.len();
                match &target {
                    Some(target) => list.iter().position(|f| f == target).unwrap_or(length),
                    None => length,
                }
            });
            if !is_textual.get_untracked() && !store.editing.get_untracked() {
                select_tab(target_index);
                return;
            }
            let text = textarea_ref.borrow().map(|a| opencode_tui_sync::js::trim(&ta::plain_text(a)).to_string()).unwrap_or_default();
            if !commit_input(&text) {
                if target_index < store.tab.get_untracked() {
                    select_tab(target_index);
                }
                return;
            }
            select_tab(target_index);
        })
    };
    let open_external: Rc<dyn Fn()> = Rc::new(move || {
        let Some(current) = external_field.get_untracked() else { return };
        store.error.set(String::new());
        let key = field_key(&current).to_string();
        match open_url(current.str("url").unwrap_or("")) {
            Ok(()) => store.external_ready.update(|r| {
                r.insert(key, true);
            }),
            Err(()) => store.error.set("Could not open the browser. Copy the URL and continue manually.".into()),
        }
    });
    let copy_external: Rc<dyn Fn()> = Rc::new(move || {
        // TODO(port-tui): the clipboard (`clipboard.write(url)`, then ready and a toast).
    });
    let acknowledge_external: Rc<dyn Fn()> = {
        let answer = answer.clone();
        let select_tab = select_tab.clone();
        let open_external = open_external.clone();
        Rc::new(move || {
            let Some(current) = external_field.get_untracked() else { return };
            let key = field_key(&current);
            if store.answer_of(key) == Some(Json::Bool(true)) {
                select_tab(store.tab.get_untracked() + 1);
                return;
            }
            if !store.external_ready.with_untracked(|r| r.get(key).copied().unwrap_or(false)) {
                open_external();
                return;
            }
            answer(key, Some(Json::Bool(true)));
            select_tab(store.tab.get_untracked() + 1);
        })
    };
    let submit: Rc<dyn Fn()> = {
        let reply = reply.clone();
        Rc::new(move || {
            let list = fields.get_untracked();
            if let Some(unacknowledged) =
                list.iter().find(|f| field_type(f) == "external" && store.answer_of(field_key(f)) != Some(Json::Bool(true)))
            {
                store.error.set(format!("External action must be acknowledged: {}", form_label(unacknowledged)));
                return;
            }
            if let Some(invalid) =
                list.iter().filter(|f| is_answer_field(f)).find(|f| validate_value(f, store.answer_of(field_key(f)).as_ref()).is_some())
            {
                store.error.set(validate_value(invalid, store.answer_of(field_key(invalid)).as_ref()).unwrap_or_else(|| "Invalid answer".into()));
                return;
            }
            let mut answer = opencode_protocol::wire::Object::default();
            for field in active_fields.get_untracked() {
                if let Some(value) = store.answer_of(field_key(&field)) {
                    answer.insert(field_key(&field).to_string(), value);
                }
            }
            reply(Json::Object(answer));
        })
    };

    // Typing on the custom row starts its answer.
    {
        let active = active.clone();
        let update_custom = update_custom.clone();
        let textarea_ref = textarea_ref.clone();
        let dispose = intercept_key(0, move |event| {
            if !active() {
                return false;
            }
            let area = *textarea_ref.borrow();
            let focused_editor = area.is_some_and(|a| a.is_alive() && renderer::current_focused_renderable() == Some(a));
            if is_textual.get_untracked() || !other.get_untracked() || (store.editing.get_untracked() && focused_editor) {
                return false;
            }
            let Some(key) = parsed_key(event) else { return false };
            if key.ctrl || key.meta || key.option || key.super_ || key.hyper {
                return false;
            }
            let mut chars = key.sequence.chars();
            let printable = match (chars.next(), chars.next()) {
                (Some(c), None) => !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}') && !is_format_or_unassigned(c),
                _ => false,
            };
            if (!store.editing.get_untracked() && key.sequence == " ") || !printable {
                return false;
            }
            let Some(current) = answer_field.get_untracked() else { return false };
            update_custom(&current, input.get_untracked() + &key.sequence);
            if !store.editing.get_untracked() {
                store.editing.set(true);
            }
            true
        });
        on_cleanup(move || dispose());
    }

    on_mount(move || {
        let pop = mode_push(FORM_MODE, Some(Rc::new(|| true)));
        on_cleanup(move || pop());
    });

    create_layer(move || Layer {
        mode: Some(FORM_MODE),
        enabled: Some((!confirm.get() && answer_field.with(Option::is_some)).into()),
        commands: vec![Command::with_input("prompt.paste", |_| {
            // TODO(port-tui): the clipboard (`pasteClipboard()`).
            true
        })
        .title("Paste from clipboard")
        .group("Form")],
        ..Layer::default()
    });

    {
        let cancel = cancel.clone();
        let submit_input = submit_input.clone();
        let commit_input = commit_input.clone();
        let reply_single = reply_single.clone();
        let select_tab = select_tab.clone();
        let textarea_ref = textarea_ref.clone();
        create_layer(move || {
            let text_now = {
                let textarea_ref = textarea_ref.clone();
                move || textarea_ref.borrow().map(ta::plain_text).unwrap_or_default()
            };
            let trimmed = {
                let text_now = text_now.clone();
                move || opencode_tui_sync::js::trim(&text_now()).to_string()
            };
            let clear = {
                let cancel = cancel.clone();
                let textarea_ref = textarea_ref.clone();
                let text_now = text_now.clone();
                move || {
                    if text_now().is_empty() {
                        if is_textual.get_untracked() {
                            cancel();
                            return;
                        }
                        store.editing.set(false);
                        return;
                    }
                    if let Some(area) = *textarea_ref.borrow() {
                        ta::set_text(area, "");
                    }
                }
            };
            let escape = {
                let cancel = cancel.clone();
                move || {
                    if is_textual.get_untracked() {
                        cancel();
                        return true;
                    }
                    store.editing.set(false);
                    true
                }
            };
            let next = {
                let submit_input = submit_input.clone();
                let trimmed = trimmed.clone();
                move || {
                    submit_input(&trimmed(), 1);
                    true
                }
            };
            let previous = {
                let submit_input = submit_input.clone();
                let trimmed = trimmed.clone();
                move || {
                    submit_input(&trimmed(), -1);
                    true
                }
            };
            let up = {
                let textarea_ref = textarea_ref.clone();
                move || {
                    let Some(area) = *textarea_ref.borrow() else { return false };
                    if is_textual.get_untracked() || !area.is_alive() || store.selected.get_untracked() == 0 {
                        return false;
                    }
                    if ta::cursor_row(area) > 0 {
                        return false;
                    }
                    store.editing.set(false);
                    store.selected.set(store.selected.get_untracked() - 1);
                    true
                }
            };
            let submit_edit = {
                let submit_input = submit_input.clone();
                let commit_input = commit_input.clone();
                let reply_single = reply_single.clone();
                let select_tab = select_tab.clone();
                let trimmed = trimmed.clone();
                move || {
                    let text = trimmed();
                    let Some(current) = answer_field.get_untracked() else { return true };
                    if is_textual.get_untracked() {
                        submit_input(&text, 1);
                        return true;
                    }
                    let was_multi = multi.get_untracked();
                    if !commit_input(&text) || was_multi || text.is_empty() {
                        return true;
                    }
                    if single.get_untracked() {
                        reply_single(&current, Json::String(text));
                        return true;
                    }
                    select_tab(store.tab.get_untracked() + 1);
                    true
                }
            };
            Layer {
                mode: Some(FORM_MODE),
                priority: Some(1),
                enabled: Some(((store.editing.get() || is_textual.get()) && !confirm.get()).into()),
                commands: vec![
                    Command::new("prompt.clear", clear).title("Clear answer edit").group("Form"),
                    Command::inline("escape", escape)
                        .title(if is_textual.get() { "Dismiss form" } else { "Close answer edit" })
                        .group("Form"),
                    Command::inline("tab", next).title("Next field").group("Form"),
                    Command::inline("shift+tab", previous).title("Previous field").group("Form"),
                    Command::inline("up", up).title("Leave answer edit").group("Form"),
                    Command::inline("return", submit_edit).title("Submit answer edit").group("Form"),
                ],
                ..Layer::default()
            }
        });
    }

    {
        let cancel = cancel.clone();
        let select_tab = select_tab.clone();
        let select_option = select_option.clone();
        let acknowledge_external = acknowledge_external.clone();
        let copy_external = copy_external.clone();
        let submit = submit.clone();
        let review = review.clone();
        create_layer(move || {
            let total = rows_memo.with(Vec::len) + if is_custom.get() { 1 } else { 0 };
            let max = total.min(9);
            let external = external_field.get();
            let tab_step = {
                let select_tab = select_tab.clone();
                move |delta: i64| {
                    let select_tab = select_tab.clone();
                    move || {
                        let tabs = tabs.get_untracked() as i64;
                        select_tab(((store.tab.get_untracked() as i64 + delta + tabs) % tabs) as usize);
                        true
                    }
                }
            };
            let dismiss = {
                let cancel = cancel.clone();
                move || {
                    cancel();
                    true
                }
            };
            let mut commands = vec![
                {
                    let cancel = cancel.clone();
                    Command::new("app.exit", move || cancel()).title("Dismiss form").group("Form")
                },
                Command::inline("left", tab_step(-1)).title("Previous field").group("Form"),
                Command::inline("h", tab_step(-1)).title("Previous field").group("Form"),
                Command::inline("right", tab_step(1)).title("Next field").group("Form"),
                Command::inline("l", tab_step(1)).title("Next field").group("Form"),
                Command::inline("tab", tab_step(1)).title("Next field").group("Form"),
                Command::inline("shift+tab", tab_step(-1)).title("Previous field").group("Form"),
            ];
            if let Some(external) = external {
                let acknowledged = store.answer_of(field_key(&external)) == Some(Json::Bool(true));
                let ready = store.external_ready.with(|r| r.get(field_key(&external)).copied().unwrap_or(false));
                let acknowledge = acknowledge_external.clone();
                let copy = copy_external.clone();
                commands.push(
                    Command::inline("return", move || {
                        acknowledge();
                        true
                    })
                    .title(if acknowledged {
                        "Continue"
                    } else if ready {
                        "Confirm completion"
                    } else {
                        "Open link"
                    })
                    .group("Form"),
                );
                commands.push(
                    Command::inline("c", move || {
                        copy();
                        true
                    })
                    .title("Copy link")
                    .group("Form"),
                );
                commands.push(Command::inline("escape", dismiss.clone()).title("Dismiss form").group("Form"));
            } else if confirm.get() {
                let submit = submit.clone();
                commands.push(
                    Command::inline("return", move || {
                        submit();
                        true
                    })
                    .title("Submit form")
                    .group("Form"),
                );
                commands.push(Command::inline("escape", dismiss.clone()).title("Dismiss form").group("Form"));
                for (key, delta) in [("up", -1.0), ("k", -1.0), ("down", 1.0), ("j", 1.0)] {
                    let review = review.clone();
                    commands.push(
                        Command::inline(key, move || {
                            if let Some(review) = *review.borrow() {
                                review.scroll_by(delta, ScrollUnit::Absolute);
                            }
                            true
                        })
                        .title("Scroll review")
                        .group("Form"),
                    );
                }
            } else {
                for index in 0..max {
                    let select_option = select_option.clone();
                    commands.push(
                        Command::inline(&(index + 1).to_string(), move || {
                            store.selected.set(index);
                            select_option();
                            true
                        })
                        .title(format!("Select answer {}", index + 1))
                        .group("Form"),
                    );
                }
                for (key, delta, title) in
                    [("up", -1i64, "Previous answer"), ("k", -1, "Previous answer"), ("down", 1, "Next answer"), ("j", 1, "Next answer")]
                {
                    commands.push(
                        Command::inline(key, move || {
                            let total = total as i64;
                            if total > 0 {
                                store.selected.set(((store.selected.get_untracked() as i64 + delta + total) % total) as usize);
                            }
                            true
                        })
                        .title(title)
                        .group("Form"),
                    );
                }
                {
                    let select_option = select_option.clone();
                    commands.push(
                        Command::inline("return", move || {
                            select_option();
                            true
                        })
                        .title("Select answer")
                        .group("Form"),
                    );
                }
                if multi.get() {
                    let select_option = select_option.clone();
                    commands.push(
                        Command::inline("space", move || {
                            select_option();
                            true
                        })
                        .title("Toggle answer")
                        .group("Form"),
                    );
                }
                commands.push(Command::inline("escape", dismiss.clone()).title("Dismiss form").group("Form"));
            }
            Layer {
                mode: Some(FORM_MODE),
                enabled: Some((!store.editing.get() && !is_textual.get()).into()),
                commands,
                ..Layer::default()
            }
        });
    }

    // The view.
    let root = box_();
    bind_always(move || rgba(theme.with(|t| t.background.raised.base)), move |c| root.set_background_color(Some(c)));
    root.set_border(BorderProp::Sides(vec![Side::Left]));
    bind_always(move || rgba(theme.with(|t| t.background.action.primary.focused)), move |c| root.set_border_color(Some(c)));
    root.set_custom_border_chars(Some(split_border_chars()));

    let main = box_();
    insert_node(root, main, None);
    main.set_gap(2, Some(Value::Point(1.0)));
    main.set_padding(Edge::Left, Some(Value::Point(1.0)));
    main.set_padding(Edge::Right, Some(Value::Point(3.0)));
    main.set_padding(Edge::Top, Some(Value::Point(1.0)));
    main.set_padding(Edge::Bottom, Some(Value::Point(1.0)));

    let title_box = box_();
    insert_node(main, title_box, None);
    title_box.set_padding(Edge::Left, Some(Value::Point(1.0)));
    {
        let title = form.str("title").unwrap_or("").to_string();
        insert_node(title_box, colored_text(move || title.clone(), move || rgba(theme.with(|t| t.text.muted))), None);
    }
    insert(
        main,
        show(
            move || message.with(Option::is_some),
            move || {
                let el = box_();
                el.set_padding(Edge::Left, Some(Value::Point(1.0)));
                insert_node(el, colored_text(move || message.get().unwrap_or_default(), move || rgba(theme.with(|t| t.text.base))), None);
                View::Node(el)
            },
            None,
        ),
        At::End,
    );
    insert(
        main,
        show(
            move || !single.get() && !tabbed.get(),
            move || {
                let row = box_();
                row.set_flex_direction(FlexDirection::Row);
                row.set_gap(2, Some(Value::Point(3.0)));
                row.set_padding(Edge::Left, Some(Value::Point(1.0)));
                insert_node(
                    row,
                    colored_text(
                        move || {
                            if confirm.get() {
                                return "Review".to_string();
                            }
                            let count = fields.with(Vec::len);
                            format!("Field {} of {count}", store.tab.get().min(count.saturating_sub(1)) + 1)
                        },
                        move || rgba(theme.with(|t| t.text.muted)),
                    ),
                    None,
                );
                insert(
                    row,
                    show(
                        move || fields.with(|f| !f.is_empty()),
                        move || {
                            View::Node(colored_text(
                                move || format!("· {}/{} completed", answered.get(), fields.with(Vec::len)),
                                move || rgba(theme.with(|t| t.text.muted)),
                            ))
                        },
                        None,
                    ),
                    At::End,
                );
                View::Node(row)
            },
            None,
        ),
        At::End,
    );
    {
        let select_tab_from_mouse = select_tab_from_mouse.clone();
        insert(
            main,
            show(
                move || !single.get() && tabbed.get(),
                move || {
                    let row = box_();
                    row.set_flex_direction(FlexDirection::Row);
                    row.set_padding(Edge::Left, Some(Value::Point(1.0)));
                    let select_tab_from_mouse = select_tab_from_mouse.clone();
                    let select_confirm = select_tab_from_mouse.clone();
                    insert(
                        row,
                        for_each(
                            move || fields.get(),
                            |field: &Json| opencode_tui_sync::client::stringify(field),
                            move |item, index| {
                                let el = box_();
                                el.set_padding(Edge::Right, Some(Value::Point(2.0)));
                                bind_always(move || rgba(theme.with(|t| t.background.raised.base)), move |c| el.set_background_color(Some(c)));
                                el.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| set_tab_hover.set(Some(Some(index.get_untracked()))))));
                                el.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| set_tab_hover.set(None))));
                                {
                                    let select_tab_from_mouse = select_tab_from_mouse.clone();
                                    el.set_on_mouse(
                                        Some(MouseEventType::Up),
                                        Some(Rc::new(move |_| select_tab_from_mouse(Some(item.get_untracked())))),
                                    );
                                }
                                let is_tab = move || index.get() == store.tab.get();
                                let label = colored_text(
                                    move || truncate(&form_label(&item.get()), 24),
                                    move || {
                                        theme.with(|t| {
                                            if is_tab() {
                                                rgba(t.text.base)
                                            } else if tab_hover.get() == Some(Some(index.get())) {
                                                rgba(t.text.formfield.focused)
                                            } else {
                                                rgba(t.text.muted)
                                            }
                                        })
                                    },
                                );
                                bind(is_tab, move |tab| label.set_attributes(tab.then_some(text_attributes::BOLD)));
                                insert_node(el, label, None);
                                View::Node(el)
                            },
                            None,
                        ),
                        At::End,
                    );
                    let submit_tab = box_();
                    insert_node(row, submit_tab, None);
                    bind_always(move || rgba(theme.with(|t| t.background.raised.base)), move |c| submit_tab.set_background_color(Some(c)));
                    submit_tab.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| set_tab_hover.set(Some(None)))));
                    submit_tab.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| set_tab_hover.set(None))));
                    submit_tab.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| select_confirm(None))));
                    let label = colored_text(
                        || "Submit".into(),
                        move || {
                            theme.with(|t| {
                                if confirm.get() {
                                    rgba(t.text.base)
                                } else if tab_hover.get() == Some(None) {
                                    rgba(t.text.formfield.focused)
                                } else {
                                    rgba(t.text.muted)
                                }
                            })
                        },
                    );
                    bind(move || confirm.get(), move |c| label.set_attributes(c.then_some(text_attributes::BOLD)));
                    insert_node(submit_tab, label, None);
                    View::Node(row)
                },
                None,
            ),
            At::End,
        );
    }

    {
        let open_external = open_external.clone();
        insert(
            main,
            show_value(
                move || external_field.get().filter(|_| !confirm.get()),
                move |external| {
                    let el = box_();
                    el.set_padding(Edge::Left, Some(Value::Point(1.0)));
                    el.set_gap(2, Some(Value::Point(1.0)));
                    let get = move |key: &'static str| move || external.with(|e| e.as_ref().and_then(|e| e.str(key)).map(str::to_string));
                    insert(
                        el,
                        show(
                            move || get("title")().is_some(),
                            move || View::Node(colored_text(move || get("title")().unwrap_or_default(), move || rgba(theme.with(|t| t.text.base)))),
                            None,
                        ),
                        At::End,
                    );
                    insert(
                        el,
                        show(
                            move || get("description")().is_some(),
                            move || View::Node(colored_text(move || get("description")().unwrap_or_default(), move || rgba(theme.with(|t| t.text.muted)))),
                            None,
                        ),
                        At::End,
                    );
                    let url = colored_text(move || get("url")().unwrap_or_default(), move || rgba(theme.with(|t| t.text.action.primary.base)));
                    {
                        let open_external = open_external.clone();
                        url.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| open_external())));
                    }
                    insert(el, View::Node(url), At::End);
                    let key = move || get("key")().unwrap_or_default();
                    let status = colored_text(
                        move || {
                            if store.answer_of(&key()) == Some(Json::Bool(true)) {
                                "✓ Acknowledged".to_string()
                            } else if store.external_ready.with(|r| r.get(&key()).copied().unwrap_or(false)) {
                                "Complete the external action, then press enter to confirm.".to_string()
                            } else {
                                "Open or copy the URL, complete the external action, then confirm.".to_string()
                            }
                        },
                        move || {
                            theme.with(|t| {
                                if store.answer_of(&key()) == Some(Json::Bool(true)) {
                                    rgba(t.text.feedback.success.base)
                                } else {
                                    rgba(t.text.muted)
                                }
                            })
                        },
                    );
                    insert(el, View::Node(status), At::End);
                    View::Node(el)
                },
                None,
            ),
            At::End,
        );
    }

    {
        let select_option = select_option.clone();
        let update_custom = update_custom.clone();
        let textarea_ref = textarea_ref.clone();
        insert(
            main,
            show(
                move || !confirm.get() && answer_field.with(Option::is_some),
                move || {
                    let el = box_();
                    el.set_padding(Edge::Left, Some(Value::Point(1.0)));
                    el.set_gap(2, Some(Value::Point(1.0)));
                    let question = box_();
                    insert_node(el, question, None);
                    insert_node(
                        question,
                        colored_text(
                            move || {
                                answer_field.with(|f| {
                                    f.as_ref().map(|f| f.str("description").map(str::to_string).unwrap_or_else(|| form_label(f))).unwrap_or_default()
                                })
                            },
                            move || rgba(theme.with(|t| t.text.base)),
                        ),
                        None,
                    );
                    // The typed answer, keyed by the field.
                    {
                        let textarea_ref = textarea_ref.clone();
                        insert(
                            el,
                            show_keyed(
                                move || if is_textual.get() { answer_field.with(|f| f.as_ref().map(|f| field_key(f).to_string())) } else { None },
                                move |_key| {
                                    let wrapper = box_();
                                    wrapper.set_padding(Edge::Left, Some(Value::Point(1.0)));
                                    let area = textarea();
                                    insert_node(wrapper, area, None);
                                    *textarea_ref.borrow_mut() = Some(area);
                                    let current = answer_field.get_untracked();
                                    let initial = {
                                        let value = input.get_untracked();
                                        if !value.is_empty() {
                                            value
                                        } else {
                                            current
                                                .as_ref()
                                                .map(|f| display_value(f, store.answer_of(field_key(f)).as_ref(), "(none)"))
                                                .unwrap_or_default()
                                        }
                                    };
                                    ta::set_text(area, &initial);
                                    bind(move || placeholder.get(), move |p| area.set_placeholder(&p));
                                    bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| area.set_placeholder_color(c));
                                    area.set_min_height(Some(Value::Point(1.0)));
                                    area.set_max_height(Some(Value::Point(6.0)));
                                    bind_always(move || rgba(theme.with(|t| t.text.base)), move |c| {
                                        area.set_text_color(c);
                                        area.set_focused_text_color(c);
                                        area.set_cursor_color_prop(c);
                                    });
                                    renderer::next_tick(move || {
                                        if !area.is_alive() {
                                            return;
                                        }
                                        ta::goto_line_end(area);
                                        set_input_target.set(Some(area));
                                    });
                                    View::Node(wrapper)
                                },
                                None,
                            ),
                            At::End,
                        );
                    }
                    // The options.
                    let select_option = select_option.clone();
                    let update_custom = update_custom.clone();
                    let textarea_ref = textarea_ref.clone();
                    insert(
                        el,
                        show(
                            move || !is_textual.get(),
                            move || {
                                let list = box_();
                                let select_option_rows = select_option.clone();
                                insert(
                                    list,
                                    for_each(
                                        move || rows_memo.get(),
                                        |row: &FormRow| opencode_tui_sync::client::stringify(&row.value),
                                        move |row, i| {
                                            let active = move || i.get() == store.selected.get();
                                            let picked = move || {
                                                let key = answer_field.with(|f| f.as_ref().map(|f| field_key(f).to_string()).unwrap_or_default());
                                                let value = row.with(|r| r.value.clone());
                                                match store.answer_of(&key) {
                                                    Some(Json::Array(items)) => {
                                                        let text = opencode_tui_sync::js::string(&value);
                                                        items.iter().any(|item| item.str_value() == Some(text.as_str()))
                                                    }
                                                    Some(answer) => answer == value,
                                                    None => false,
                                                }
                                            };
                                            let item = box_();
                                            item.set_on_mouse(Some(MouseEventType::Move), Some(Rc::new(move |_| store.selected.set(i.get_untracked()))));
                                            item.set_on_mouse(Some(MouseEventType::Down), Some(Rc::new(move |_| store.selected.set(i.get_untracked()))));
                                            {
                                                let select_option = select_option_rows.clone();
                                                item.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| select_option())));
                                            }
                                            let line = box_();
                                            insert_node(item, line, None);
                                            line.set_flex_direction(FlexDirection::Row);
                                            let number = box_();
                                            insert_node(line, number, None);
                                            bind_always(move || rgba(theme.with(|t| t.background.raised.base)), move |c| number.set_background_color(Some(c)));
                                            number.set_padding(Edge::Right, Some(Value::Point(1.0)));
                                            insert_node(
                                                number,
                                                colored_text(
                                                    move || format!("{}.", i.get() + 1),
                                                    move || theme.with(|t| if active() { rgba(t.text.formfield.focused) } else { rgba(t.text.muted) }),
                                                ),
                                                None,
                                            );
                                            let label_box = box_();
                                            insert_node(line, label_box, None);
                                            bind_always(move || rgba(theme.with(|t| t.background.raised.base)), move |c| label_box.set_background_color(Some(c)));
                                            label_box.set_flex_direction(FlexDirection::Row);
                                            insert(
                                                label_box,
                                                show(
                                                    move || multi.get(),
                                                    move || {
                                                        let check = colored_text(
                                                            move || format!("[{}]", if picked() { "✓" } else { " " }),
                                                            move || {
                                                                theme.with(|t| {
                                                                    if active() {
                                                                        rgba(t.text.formfield.focused)
                                                                    } else if picked() {
                                                                        rgba(t.text.formfield.selected)
                                                                    } else {
                                                                        rgba(t.text.muted)
                                                                    }
                                                                })
                                                            },
                                                        );
                                                        check.set_width(Some(Value::Point(4.0)));
                                                        check.set_flex_shrink(Some(0.0));
                                                        View::Node(check)
                                                    },
                                                    None,
                                                ),
                                                At::End,
                                            );
                                            insert_node(
                                                label_box,
                                                colored_text(
                                                    move || row.with(|r| r.label.clone()),
                                                    move || theme.with(|t| if active() { rgba(t.text.formfield.focused) } else { rgba(t.text.formfield.base) }),
                                                ),
                                                None,
                                            );
                                            insert(
                                                line,
                                                show(
                                                    move || !multi.get(),
                                                    move || {
                                                        View::Node(colored_text(
                                                            move || if picked() { " ✓".into() } else { String::new() },
                                                            move || rgba(theme.with(|t| t.text.formfield.selected)),
                                                        ))
                                                    },
                                                    None,
                                                ),
                                                At::End,
                                            );
                                            insert(
                                                item,
                                                show(
                                                    move || row.with(|r| r.description.is_some()),
                                                    move || {
                                                        let pad = box_();
                                                        bind(move || if multi.get() { 7.0 } else { 3.0 }, move |p| pad.set_padding(Edge::Left, Some(Value::Point(p))));
                                                        insert_node(
                                                            pad,
                                                            colored_text(
                                                                move || row.with(|r| r.description.clone().unwrap_or_default()),
                                                                move || rgba(theme.with(|t| t.text.muted)),
                                                            ),
                                                            None,
                                                        );
                                                        View::Node(pad)
                                                    },
                                                    None,
                                                ),
                                                At::End,
                                            );
                                            View::Node(item)
                                        },
                                        None,
                                    ),
                                    At::End,
                                );
                                // The custom answer row.
                                let select_option = select_option.clone();
                                let update_custom = update_custom.clone();
                                let textarea_ref = textarea_ref.clone();
                                insert(
                                    list,
                                    show(
                                        move || is_custom.get(),
                                        move || custom_row(CustomRow {
                                            select_option: select_option.clone(),
                                            update_custom: update_custom.clone(),
                                            textarea_ref: textarea_ref.clone(),
                                            rows: rows_memo,
                                            other,
                                            multi,
                                            custom_checked,
                                            custom_picked,
                                            input,
                                            answer_field,
                                            store,
                                            set_input_target,
                                        }),
                                        None,
                                    ),
                                    At::End,
                                );
                                View::Node(list)
                            },
                            None,
                        ),
                        At::End,
                    );
                    View::Node(el)
                },
                None,
            ),
            At::End,
        );
    }

    {
        let review = review.clone();
        insert(
            main,
            show(
                move || confirm.get(),
                move || {
                    let scroll = scrollbox();
                    bind(move || review_max_height.get(), move |h| scroll.set_max_height(Some(Value::Point(h as f32))));
                    scroll.with_scroll_content(|content| content.set_value(opencode_tui_core::layout::VALUE_MIN_HEIGHT, 0, Value::Point(0.0)));
                    scroll.set_scrollbar_options(None, &ScrollBarOptions { visible: Some(Some(false)), ..Default::default() });
                    *review.borrow_mut() = Some(scroll);
                    let content = box_();
                    insert_node(scroll, content, None);
                    content.set_on_size_change(Some(Rc::new(move || set_review_content_height.set(content.height()))));
                    insert(
                        content,
                        for_each(
                            move || fields.get(),
                            |field: &Json| opencode_tui_sync::client::stringify(field),
                            move |item, _| review_row(item.get_untracked(), store),
                            None,
                        ),
                        At::Only,
                    );
                    View::Node(scroll)
                },
                None,
            ),
            At::End,
        );
    }

    // The footer.
    let footer = box_();
    insert_node(root, footer, None);
    footer.set_flex_direction(FlexDirection::Row);
    footer.set_flex_shrink(Some(0.0));
    footer.set_gap(2, Some(Value::Point(1.0)));
    footer.set_padding(Edge::Left, Some(Value::Point(2.0)));
    footer.set_padding(Edge::Right, Some(Value::Point(3.0)));
    footer.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
    footer.set_justify_content(Justify::SpaceBetween);
    let hints = box_();
    insert_node(footer, hints, None);
    hints.set_flex_direction(FlexDirection::Row);
    hints.set_gap(2, Some(Value::Point(2.0)));
    insert(
        hints,
        show(
            move || !single.get(),
            move || {
                let el = text();
                hint_text(el, || "⇆".into(), || "tab".into());
                View::Node(el)
            },
            None,
        ),
        At::End,
    );
    insert(
        hints,
        show(
            move || !confirm.get() && !is_textual.get() && external_field.with(Option::is_none) && !store.editing.get(),
            move || {
                let el = text();
                hint_text(el, || "↑↓".into(), || "select".into());
                View::Node(el)
            },
            None,
        ),
        At::End,
    );
    insert(
        hints,
        show(
            move || confirm.get() && (review_content_height.get() as i64) > review_max_height.get(),
            move || {
                let el = text();
                hint_text(el, || "↑↓".into(), || "scroll".into());
                View::Node(el)
            },
            None,
        ),
        At::End,
    );
    {
        let enter = text();
        hint_text(enter, || "enter".into(), move || action_label.get().to_string());
        let submit = submit.clone();
        let acknowledge_external = acknowledge_external.clone();
        enter.set_on_mouse(
            Some(MouseEventType::Up),
            Some(Rc::new(move |_| {
                if confirm.get_untracked() {
                    submit();
                }
                if external_field.with_untracked(Option::is_some) {
                    acknowledge_external();
                }
            })),
        );
        insert(hints, View::Node(enter), At::End);
    }
    {
        let copy_external = copy_external.clone();
        insert(
            hints,
            show(
                move || external_field.with(Option::is_some),
                move || {
                    let el = text();
                    hint_text(el, || "c".into(), || "copy".into());
                    let copy_external = copy_external.clone();
                    el.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| copy_external())));
                    View::Node(el)
                },
                None,
            ),
            At::End,
        );
    }
    {
        let esc = text();
        hint_text(esc, || "esc".into(), move || if store.editing.get() && !is_textual.get() { "close".into() } else { "dismiss".into() });
        let cancel = cancel.clone();
        esc.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| cancel())));
        insert(hints, View::Node(esc), At::End);
    }
    insert(
        footer,
        show(
            move || store.error.with(|e| !e.is_empty()),
            move || View::Node(colored_text(move || store.error.get(), move || rgba(theme.with(|t| t.text.feedback.error.base)))),
            None,
        ),
        At::End,
    );
    View::Node(root)
}

struct CustomRow {
    select_option: Rc<dyn Fn()>,
    update_custom: Rc<dyn Fn(&Json, String)>,
    textarea_ref: Rc<RefCell<Option<Element>>>,
    rows: Memo<Vec<FormRow>>,
    other: Memo<bool>,
    multi: Memo<bool>,
    custom_checked: Memo<bool>,
    custom_picked: Memo<bool>,
    input: Memo<String>,
    answer_field: Memo<Option<Json>>,
    store: Store,
    set_input_target: Signal<Option<Element>>,
}

/// The "Type your own answer" row: its label, or its textarea while editing.
fn custom_row(props: CustomRow) -> View {
    let theme = tui().theme;
    let CustomRow { select_option, update_custom, textarea_ref, rows, other, multi, custom_checked, custom_picked, input, answer_field, store, set_input_target } =
        props;
    let el = box_();
    el.set_on_mouse(Some(MouseEventType::Move), Some(Rc::new(move |_| store.selected.set(rows.with_untracked(Vec::len)))));
    el.set_on_mouse(Some(MouseEventType::Down), Some(Rc::new(move |_| store.selected.set(rows.with_untracked(Vec::len)))));
    el.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| select_option())));
    let line = box_();
    insert_node(el, line, None);
    line.set_flex_direction(FlexDirection::Row);
    let number = box_();
    insert_node(line, number, None);
    bind_always(move || rgba(theme.with(|t| t.background.raised.base)), move |c| number.set_background_color(Some(c)));
    number.set_padding(Edge::Right, Some(Value::Point(1.0)));
    insert_node(
        number,
        colored_text(
            move || format!("{}.", rows.with(Vec::len) + 1),
            move || theme.with(|t| if other.get() { rgba(t.text.formfield.focused) } else { rgba(t.text.muted) }),
        ),
        None,
    );
    let body = box_();
    insert_node(line, body, None);
    body.set_flex_direction(FlexDirection::Row);
    body.set_flex_grow(Some(1.0));
    bind_always(move || rgba(theme.with(|t| t.background.raised.base)), move |c| body.set_background_color(Some(c)));
    insert(
        body,
        show(
            move || multi.get(),
            move || {
                let check = colored_text(
                    move || format!("[{}]", if custom_checked.get() { "✓" } else { " " }),
                    move || {
                        theme.with(|t| {
                            if other.get() {
                                rgba(t.text.formfield.focused)
                            } else if custom_checked.get() {
                                rgba(t.text.formfield.selected)
                            } else {
                                rgba(t.text.muted)
                            }
                        })
                    },
                );
                check.set_width(Some(Value::Point(4.0)));
                check.set_flex_shrink(Some(0.0));
                View::Node(check)
            },
            None,
        ),
        At::End,
    );
    insert(
        body,
        show(
            move || store.editing.get(),
            move || {
                let area = textarea();
                area.set_flex_grow(Some(1.0));
                *textarea_ref.borrow_mut() = Some(area);
                ta::set_text(area, &input.get_untracked());
                area.set_placeholder("Type your own answer");
                bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| area.set_placeholder_color(c));
                area.set_min_height(Some(Value::Point(1.0)));
                area.set_max_height(Some(Value::Point(6.0)));
                bind_always(move || rgba(theme.with(|t| t.text.formfield.focused)), move |c| {
                    area.set_text_color(c);
                    area.set_focused_text_color(c);
                    area.set_cursor_color_prop(c);
                });
                {
                    let update_custom = update_custom.clone();
                    area.set_on_content_change(Some(Rc::new(move || {
                        let Some(current) = answer_field.get_untracked() else { return };
                        if !area.is_alive() {
                            return;
                        }
                        update_custom(&current, ta::plain_text(area));
                    })));
                }
                renderer::next_tick(move || {
                    if !area.is_alive() {
                        return;
                    }
                    ta::set_text(area, &input.get_untracked());
                    ta::goto_line_end(area);
                    set_input_target.set(Some(area));
                });
                View::Node(area)
            },
            Some(Rc::new(move || {
                View::list([
                    View::Node(colored_text(
                        move || {
                            let value = input.get();
                            if value.is_empty() { "Type your own answer".into() } else { value }
                        },
                        move || theme.with(|t| if other.get() { rgba(t.text.formfield.focused) } else { rgba(t.text.formfield.base) }),
                    )),
                    show(
                        move || !multi.get() && custom_picked.get(),
                        move || View::Node(colored_text(|| "✓".into(), move || rgba(theme.with(|t| t.text.formfield.selected)))),
                        None,
                    ),
                ])
            })),
        ),
        At::End,
    );
    View::Node(el)
}

/// One review line: the field's label and its answer (or why it has none).
fn review_row(item: Json, store: Store) -> View {
    let theme = tui().theme;
    let el = box_();
    el.set_padding(Edge::Left, Some(Value::Point(1.0)));
    let line = text();
    insert_node(el, line, None);
    let name = span();
    insert_node(line, name, None);
    bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| name.set_span_style(Some(c), None, 0, 0));
    insert_node(name, opencode_tui_core::renderables::text::text_node_from_string(&format!("{}:", truncate(&form_label(&item), 40))), None);
    insert_node(line, opencode_tui_core::renderables::text::text_node_from_string(" "), None);
    let value = span();
    insert_node(line, value, None);
    let key = field_key(&item).to_string();
    if field_type(&item) == "external" {
        let acknowledged = {
            let key = key.clone();
            move || store.answer_of(&key) == Some(Json::Bool(true))
        };
        {
            let acknowledged = acknowledged.clone();
            bind_always(
                move || theme.with(|t| if acknowledged() { rgba(t.text.feedback.success.base) } else { rgba(t.text.feedback.error.base) }),
                move |c| value.set_span_style(Some(c), None, 0, 0),
            );
        }
        insert(
            value,
            View::dynamic(move || View::from(if acknowledged() { "Acknowledged" } else { "(acknowledgement required)" })),
            At::Only,
        );
        return View::Node(el);
    }
    let item = Rc::new(item);
    let answered = {
        let key = key.clone();
        move || store.answer_of(&key).is_some()
    };
    let missing = {
        let answered = answered.clone();
        let item = item.clone();
        move || !answered() && item.get("required") == Some(&Json::Bool(true))
    };
    let invalid = {
        let key = key.clone();
        let item = item.clone();
        move || validate_value(&item, store.answer_of(&key).as_ref())
    };
    {
        let (answered, missing, invalid) = (answered.clone(), missing.clone(), invalid.clone());
        bind_always(
            move || {
                theme.with(|t| {
                    if invalid().is_some() || missing() {
                        rgba(t.text.feedback.error.base)
                    } else if answered() {
                        rgba(t.text.base)
                    } else {
                        rgba(t.text.muted)
                    }
                })
            },
            move |c| value.set_span_style(Some(c), None, 0, 0),
        );
    }
    insert(
        value,
        View::dynamic(move || {
            View::from(invalid().unwrap_or_else(|| {
                if answered() {
                    display_value(&item, store.answer_of(&key).as_ref(), "(none)")
                } else if missing() {
                    "(required)".into()
                } else {
                    "(not answered)".into()
                }
            }))
        }),
        At::Only,
    );
    View::Node(el)
}

/// `/^[^\p{C}\p{Zl}\p{Zp}]$/u` excludes format (Cf), unassigned (Cn), private-use (Co) and surrogate (Cs)
/// characters besides controls.
fn is_format_or_unassigned(c: char) -> bool {
    matches!(c as u32, 0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB | 0xE000..=0xF8FF | 0xF0000..=0x10FFFF)
}

/// `openUrl(input)` (`@opencode/util/open`): only http and https links open, with the platform's opener.
fn open_url(input: &str) -> Result<(), ()> {
    let lower = input.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(());
    }
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::process::Command::new(opener)
        .arg(input)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| ())
}

trait StrValue {
    fn str_value(&self) -> Option<&str>;
}

impl StrValue for Json {
    fn str_value(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }
}
