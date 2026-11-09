//! The prompt (`component/prompt/index.tsx`), its metadata row (`component/prompt/metadata.tsx`) and the built-in
//! prompt footer plugin (`feature-plugins/prompt/footer.tsx`, appended to the `prompt.footer` slot).
//!
//! TODO(port-tui): the prompt feature slice. This is the home screen's prompt: the bordered box, the textarea with
//! its placeholder, the agent/model/variant row, the bottom edge, the location label and the shortcut hints, laid
//! out as TS lays them out. Autocomplete, attachments, extmarks, history, stash, shell mode, image previews and
//! submission into a session are the feature slice's (submission is wired to [`PromptRef::submit`]).
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use opencode_tui_core::border::{BorderCharacters, border_chars_to_array};
use opencode_tui_core::color::{RGBA, text_attributes};
use opencode_tui_core::elements::{box_, span, text, textarea};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element, FlexDirection, Justify};

use crate::ctx::tui;
use crate::local::{Local, local};
use crate::util::{abbreviate_home, fade, rgba, string_width, take_width, truncate_file_path, truncate_width};

const PLACEHOLDERS: [&str; 3] = ["Fix a TODO in the codebase", "What is the tech stack of this project?", "Fix broken tests"];

/// The prompt's handle (`PromptRef`): what Home and the app call.
#[derive(Clone)]
pub struct PromptRef {
    pub input: Element,
    pub submit: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
}

impl PromptRef {
    pub fn text(&self) -> String {
        self.input.textarea_text()
    }
    pub fn set(&self, text: &str) {
        self.input.set_textarea_text(text);
    }
    pub fn clear(&self) {
        self.input.set_textarea_text("");
    }
    pub fn submit(&self) {
        let submit = self.submit.borrow().clone();
        if let Some(submit) = submit {
            submit();
        }
    }
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

/// `randomIndex(length)`.
fn random_index(length: usize) -> usize {
    let mut bytes = [0u8; 4];
    let _ = getrandom::fill(&mut bytes);
    (u32::from_le_bytes(bytes) as usize) % length.max(1)
}

pub struct PromptProps {
    pub session_id: Option<String>,
    pub visible: Option<Rc<dyn Fn() -> bool>>,
    pub disabled: Rc<dyn Fn() -> bool>,
    pub on_ref: Rc<dyn Fn(PromptRef)>,
    pub on_submit: Rc<dyn Fn(String)>,
}

/// `<Prompt>`.
pub fn prompt(props: PromptProps) -> View {
    let t = tui();
    let local = local();
    let dimensions = t.dimensions;
    let theme = t.theme;
    let placeholder = create_signal(random_index(PLACEHOLDERS.len())).0;
    let (location_width, set_location_width) = create_signal(dimensions.get_untracked().0 as i64);
    let (text_value, set_text_value) = create_signal(String::new());

    let placeholder_text = create_memo(move || {
        let value = format!("Ask anything… \"{}\"", PLACEHOLDERS[placeholder.get() % PLACEHOLDERS.len()]);
        let w = dimensions.get().0 as i64;
        let width = if w < 44 { w - 5 } else { (w - 4).min(75) - 5 };
        take_width(&value, width.max(1)).trim_end().to_string()
    });
    let prompt_bg = create_memo_equals(move || theme.with(|th| rgba(th.decrease(th.background.raised.base, 1.0))), None);
    let highlight = {
        let local = local.clone();
        create_memo_equals(move || local.agent_color(), None)
    };
    let border_highlight = {
        let local = local.clone();
        // tint(border, highlight, agentMetaAlpha): the agent label fades in (at once with animations off).
        create_memo_equals(
            move || {
                let base = rgba(theme.with(|th| th.border.base));
                crate::util::tint(base, highlight.get(), if local.agent_label().is_some() { 1.0 } else { 0.0 })
            },
            None,
        )
    };

    let anchor = box_();
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

    // <textarea>
    let input = textarea();
    insert_node(inner, input, None);
    input.set_width(Some(Value::Percent(100.0)));
    bind(move || placeholder_text.get(), move |p| input.set_placeholder(&p));
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| input.set_placeholder_color(c));
    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| {
        input.set_text_color(c);
        input.set_focused_text_color(c);
    });
    input.set_min_height(Some(Value::Point(1.0)));
    bind(move || (dimensions.get().1 / 3).max(6) as f32, move |h| input.set_max_height(Some(Value::Point(h))));
    input.set_focused_background_color(RGBA::from_values(0.0, 0.0, 0.0, 0.0));
    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| input.set_cursor_color_prop(c));
    input.set_on_content_change(Some(Rc::new(move || set_text_value.set(input.textarea_text()))));
    let submit: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    {
        let on_submit = props.on_submit.clone();
        let disabled = props.disabled.clone();
        let submit_fn: Rc<dyn Fn()> = Rc::new(move || {
            if disabled() {
                return;
            }
            let value = input.textarea_text();
            if value.trim().is_empty() {
                return;
            }
            on_submit(value);
        });
        *submit.borrow_mut() = Some(submit_fn.clone());
        input.set_on_submit(Some(Rc::new(move || {
            // IME: double-deferred so the last composed character is in the text.
            let submit_fn = submit_fn.clone();
            opencode_tui_core::renderer::set_timeout(0, move || {
                opencode_tui_core::renderer::set_timeout(0, move || submit_fn());
            });
        })));
    }
    let reference = PromptRef { input, submit };
    (props.on_ref)(reference.clone());
    *t.prompt.borrow_mut() = Some(input);
    input.focus();

    // The metadata row.
    let meta = box_();
    insert_node(inner, meta, None);
    meta.set_flex_direction(FlexDirection::Row);
    meta.set_flex_shrink(Some(0.0));
    meta.set_padding(Edge::Top, Some(Value::Point(1.0)));
    meta.set_gap(2, Some(Value::Point(1.0)));
    meta.set_justify_content(Justify::SpaceBetween);
    insert(meta, metadata_row(local.clone()), At::Only);

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

    // The footer row: the location (prompt.footer.status), then the footer plugin.
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
        opencode_tui_core::renderer::next_tick(move || set_location_width.set(width));
    })));
    let paths = t.paths.clone();
    let data = t.data.clone();
    let location_label = create_memo(move || {
        let directory = data.directory.get()?;
        Some(abbreviate_home(&directory, &paths.home))
    });
    let display = create_memo(move || location_label.get().map(|label| truncate_file_path(&label, location_width.get())));
    insert(
        status,
        show_value(
            move || display.get(),
            move |location| {
                let el = text();
                el.set_id("prompt.footer.location");
                bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| el.set_fg(Some(c)));
                el.set_wrap_mode(WrapMode::None);
                el.set_truncate(true);
                el.set_flex_grow(Some(1.0));
                el.set_flex_shrink(Some(1.0));
                insert(el, View::dynamic(move || View::from(location.get().unwrap_or_default())), At::Only);
                View::Node(el)
            },
            Some(Rc::new(|| View::Node(text()))),
        ),
        At::Only,
    );
    let session = props.session_id.clone();
    insert(footer, prompt_footer(session, text_value), At::End);
    let _ = (string_width, truncate_width, text_attributes::DIM);
    View::Node(anchor)
}

/// `promptMetadataLayout`.
struct MetaLayout {
    agent: Option<String>,
    model: String,
    provider: Option<String>,
    variant: Option<String>,
}

fn meta_text(l: &MetaLayout) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(agent) = &l.agent {
        parts.push(agent.clone());
    }
    if !l.model.is_empty() {
        if l.agent.is_some() {
            parts.push("·".into());
        }
        parts.push(l.model.clone());
    }
    if let Some(provider) = l.provider.as_ref().filter(|p| !p.is_empty()) {
        parts.push(provider.clone());
    }
    if let Some(variant) = &l.variant {
        parts.push("·".into());
        parts.push(variant.clone());
    }
    parts.join(" ")
}

fn metadata_layout(width: i64, terminal_width: u32, agent: &str, model: &str, provider: &str, variant: Option<String>) -> MetaLayout {
    let agent = if terminal_width < 44 || agent.is_empty() { None } else { Some(agent.to_string()) };
    let provider = if terminal_width < 44 { String::new() } else { provider.to_string() };
    let last = provider.rsplit(" / ").next().unwrap_or(&provider).to_string();
    let candidates = [
        MetaLayout { agent: agent.clone(), model: model.into(), provider: Some(provider.clone()), variant: variant.clone() },
        MetaLayout { agent: agent.clone(), model: model.into(), provider: Some(last), variant: variant.clone() },
        MetaLayout { agent: agent.clone(), model: model.into(), provider: None, variant: variant.clone() },
    ];
    for candidate in candidates {
        if string_width(&meta_text(&candidate)) as i64 <= width {
            return candidate;
        }
    }
    let prefix = agent.as_ref().map(|a| format!("{a} · ")).unwrap_or_default();
    let suffix = variant.as_ref().map(|v| format!(" · {v}")).unwrap_or_default();
    let truncated = truncate_width(model, (width - string_width(&prefix) as i64 - string_width(&suffix) as i64).max(9));
    let truncated = match truncated.strip_suffix('…') {
        Some(rest) => format!("{}…", rest.trim_end()),
        None => truncated,
    };
    MetaLayout { agent, model: truncated, provider: None, variant }
}

/// `<PromptMetadataRow>` (normal mode).
fn metadata_row(local: Local) -> View {
    let t = tui();
    let theme = t.theme;
    let dimensions = t.dimensions;
    let row = box_();
    row.set_flex_direction(FlexDirection::Row);
    row.set_gap(2, Some(Value::Point(1.0)));
    row.set_flex_grow(Some(1.0));
    row.set_flex_shrink(Some(1.0));
    row.set_min_width(Some(Value::Point(0.0)));
    let layout = {
        let local = local.clone();
        create_memo_equals(
            move || {
                let (w, _) = dimensions.get();
                let width = w as i64 - if w < 44 { 9 } else { 13 };
                let display = local.display();
                Rc::new(metadata_layout(
                    width.max(0),
                    w,
                    &display.agent.unwrap_or_default(),
                    &display.model,
                    &display.provider,
                    display.variant,
                ))
            },
            None,
        )
    };
    let has_agent = {
        let local = local.clone();
        move || local.agent_label().is_some()
    };
    let highlight = {
        let local = local.clone();
        move || local.agent_color()
    };
    let content = show(
        move || has_agent() && layout.with(|l| l.agent.is_some() || !l.model.is_empty()),
        move || {
            let agent_text = show_value(
                move || layout.with(|l| l.agent.clone()),
                {
                    let highlight = highlight.clone();
                    move |agent| {
                        let el = text();
                        let highlight = highlight.clone();
                        bind_always(move || highlight(), move |c| el.set_fg(Some(c)));
                        insert(el, View::dynamic(move || View::from(agent.get().unwrap_or_default())), At::Only);
                        View::Node(el)
                    }
                },
                None,
            );
            let model_box = show(
                move || layout.with(|l| !l.model.is_empty()),
                move || {
                    let group = box_();
                    group.set_flex_direction(FlexDirection::Row);
                    group.set_gap(2, Some(Value::Point(1.0)));
                    group.set_flex_grow(Some(1.0));
                    group.set_flex_shrink(Some(1.0));
                    group.set_min_width(Some(Value::Point(0.0)));
                    let dot = show(
                        move || layout.with(|l| l.agent.is_some()),
                        move || {
                            let el = text();
                            bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| el.set_fg(Some(c)));
                            insert(el, View::from("·"), At::Only);
                            View::Node(el)
                        },
                        None,
                    );
                    insert(group, dot, At::End);
                    let model = text();
                    insert_node(group, model, None);
                    model.set_flex_shrink(Some(1.0));
                    model.set_min_width(Some(Value::Point(0.0)));
                    model.set_wrap_mode(WrapMode::None);
                    model.set_truncate(true);
                    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| model.set_fg(Some(c)));
                    insert(model, View::dynamic(move || View::from(layout.with(|l| l.model.clone()))), At::Only);
                    let provider = show_value(
                        move || layout.with(|l| l.provider.clone().filter(|p| !p.is_empty())),
                        move |provider| {
                            let el = text();
                            el.set_flex_shrink(Some(0.0));
                            bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| el.set_fg(Some(c)));
                            insert(el, View::dynamic(move || View::from(provider.get().unwrap_or_default())), At::Only);
                            View::Node(el)
                        },
                        None,
                    );
                    insert(group, provider, At::End);
                    let variant = show_value(
                        move || layout.with(|l| l.variant.clone()),
                        move |variant| {
                            let dot = text();
                            bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| dot.set_fg(Some(c)));
                            insert(dot, View::from("·"), At::Only);
                            let value = text();
                            bind_always(
                                move || rgba(theme.with(|th| th.text.feedback.warning.base)),
                                move |c| value.set_fg(Some(c)),
                            );
                            value.set_attributes(Some(text_attributes::BOLD));
                            insert(value, View::dynamic(move || View::from(variant.get().unwrap_or_default())), At::Only);
                            View::list([View::Node(dot), View::Node(value)])
                        },
                        None,
                    );
                    insert(group, variant, At::End);
                    View::Node(group)
                },
                None,
            );
            View::list([agent_text, model_box])
        },
        Some(Rc::new(|| {
            let el = box_();
            el.set_height(Some(Value::Point(1.0)));
            View::Node(el)
        })),
    );
    insert(row, content, At::Only);
    let _ = fade;
    View::Node(row)
}

/// `PromptFooter` (normal mode, no session): the shortcut hints.
fn prompt_footer(session_id: Option<String>, _text: Signal<String>) -> View {
    let t = tui();
    let theme = t.theme;
    let dimensions = t.dimensions;
    let shortcuts = Rc::new(crate::keymap::shortcuts());
    let agents = shortcuts.get("agent.cycle").cloned().unwrap_or_default();
    let commands = shortcuts.get("command.palette.show").cloned().unwrap_or_default();
    let _ = session_id;
    let visible = move || dimensions.get().0 >= 44;
    let hint = move |key: String, label: &'static str| {
        let el = text();
        bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| el.set_fg(Some(c)));
        el.set_flex_shrink(Some(0.0));
        let key_node = opencode_tui_core::solid::View::from(format!("{key} "));
        insert(el, key_node, At::End);
        let s = span();
        insert_node(el, s, None);
        bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| s.set_span_style(Some(c), None, 0, 0));
        insert(s, View::from(label), At::Only);
        el
    };
    let agents_hint = show(
        visible,
        {
            let hint = hint.clone();
            move || View::Node(hint(agents.clone(), "agents"))
        },
        None,
    );
    let commands_hint = show(
        visible,
        move || {
            let el = hint(commands.clone(), "commands");
            el.set_wrap_mode(WrapMode::None);
            View::Node(el)
        },
        None,
    );
    let _ = Cell::new(0);
    View::list([agents_hint, commands_hint])
}
