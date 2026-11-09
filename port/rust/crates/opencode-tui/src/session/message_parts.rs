//! `routes/session/message-parts.tsx`: assistant text and reasoning parts, and the inline tool row the tool parts,
//! notices and groups share.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::{RGBA, text_attributes};
use opencode_tui_core::elements::{box_, code, markdown, span, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::renderables::text::text_node_from_string;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, FlexDirection, MouseEventType, MouseHandler};
use opencode_tui_markup::js::JsString;
use opencode_tui_markup::markdown::{InternalBlockMode, MarkdownTableOptions, MarkdownTableStyle};
use opencode_tui_sync::json::JsonExt;

use super::render_context::{MarkdownMode, ThinkingMode, use_ctx};
use crate::border::split_border_chars;
use crate::ctx::tui;
use crate::spinner::{SpinnerProps, spinner};
use crate::syntax::{current_syntax, thinking_syntax};
use crate::util::{fade, rgba};

pub const INLINE_TOOL_ICON_WIDTH: f32 = 2.0;

const TOOL_DISPLAYS: [&str; 14] = [
    "shell", "glob", "read", "grep", "webfetch", "websearch", "write", "edit", "subagent", "execute", "patch",
    "todowrite", "question", "skill",
];

/// `canonicalToolName(name)` (util/tool-display.ts).
pub fn canonical_tool_name(name: &str) -> &str {
    match name {
        "bash" => "shell",
        "task" => "subagent",
        "apply_patch" => "patch",
        other => other,
    }
}

/// `toolDisplay(tool)`.
pub fn tool_display(tool: &str) -> &'static str {
    let normalized = canonical_tool_name(tool);
    TOOL_DISPLAYS.iter().find(|t| **t == normalized).copied().unwrap_or("generic")
}

/// `reasoningContent(part)`: OpenRouter encrypts some reasoning blocks; drop the placeholder.
pub fn reasoning_content(part: &Json) -> String {
    part.str("text").unwrap_or_default().replacen("[REDACTED]", "", 1).trim().to_string()
}

/// `reasoningSummary(text)` (context/thinking.ts): a leading bold title block as the header.
pub fn reasoning_summary(text: &str) -> (Option<String>, String) {
    let content = text.trim();
    let Some(rest) = content.strip_prefix("**") else { return (None, content.to_string()) };
    let Some(end) = rest.find("**") else { return (None, content.to_string()) };
    let title = &rest[..end];
    if title.is_empty() || title.contains('*') || title.contains('\n') {
        return (None, content.to_string());
    }
    let after = &rest[end + 2..];
    let separator = if after.is_empty() {
        0
    } else if after.starts_with("\r\n\r\n") {
        4
    } else if after.starts_with("\n\n") {
        2
    } else if after.starts_with("\r\n\n") || after.starts_with("\n\r\n") {
        3
    } else {
        return (None, content.to_string());
    };
    let matched = 2 + end + 2 + separator;
    (Some(title.trim().to_string()), content[matched..].trim_end().to_string())
}

/// `Locale.duration(ms)` (@opencode/util/duration).
pub fn duration(input: f64) -> String {
    if input < 1000.0 {
        return format!("{input}ms");
    }
    if input < 60000.0 {
        return format!("{:.1}s", input / 1000.0);
    }
    if input < 3600000.0 {
        return format!("{}m {}s", (input / 60000.0).floor(), ((input % 60000.0) / 1000.0).floor());
    }
    if input < 86400000.0 {
        return format!("{}h {}m", (input / 3600000.0).floor(), ((input % 3600000.0) / 60000.0).floor());
    }
    format!("{}d {}h", (input / 86400000.0).floor(), ((input % 86400000.0) / 3600000.0).floor())
}

fn time(value: &Json, key: &str) -> Option<f64> {
    value.get("time").and_then(|t| t.num(key))
}

/// A reasoning or text part's props: the part and its message, both read reactively.
#[derive(Clone)]
pub struct PartProps {
    pub part: Rc<dyn Fn() -> Rc<Json>>,
    pub message: Rc<dyn Fn() -> Rc<Json>>,
    pub last: bool,
}

/// `<ReasoningPart last part message>`.
pub fn reasoning_part(props: PartProps) -> View {
    let t = tui();
    let theme = t.theme;
    let ctx = use_ctx();
    let thinking = create_memo_equals(
        move || Rc::new(thinking_syntax(&current_syntax(), rgba(theme.with(|th| th.text.muted)))),
        None,
    );
    // Collapsed by default in hide mode: a single line throughout, so the layout never shifts. Click to open the
    // full markdown block, click to close.
    let (expanded, set_expanded) = create_signal(false);
    let part = props.part.clone();
    let message = props.message.clone();
    let content = {
        let part = part.clone();
        create_memo(move || reasoning_content(&part()))
    };
    let is_done = {
        let (part, message) = (part.clone(), message.clone());
        create_memo(move || time(&part(), "completed").is_some() || time(&message(), "completed").is_some())
    };
    let thinking_mode = ctx.thinking_mode.clone();
    let in_minimal = create_memo(move || thinking_mode() == ThinkingMode::Hide);
    let duration_ms = {
        let (part, message) = (part.clone(), message.clone());
        create_memo(move || {
            let (p, m) = (part(), message());
            let end = time(&p, "completed").or_else(|| time(&m, "completed"));
            let start = time(&p, "created").or_else(|| time(&m, "created")).unwrap_or(0.0);
            end.map_or(0.0, |end| (end - start).max(0.0))
        })
    };
    let summary = create_memo(move || reasoning_summary(&content.get()).0);
    let toggle: MouseHandler = Rc::new(move |_| {
        if !in_minimal.get_untracked() {
            return;
        }
        set_expanded.update(|e| *e = !*e);
    });
    let markdown_mode = ctx.markdown_mode.clone();
    show(
        move || !content.get().is_empty(),
        move || {
            let outer = box_();
            outer.set_padding(Edge::Left, Some(Value::Point(3.0)));
            outer.set_flex_direction(FlexDirection::Column);
            outer.set_flex_shrink(Some(0.0));
            let header_box = box_();
            insert_node(outer, header_box, None);
            bind(move || !in_minimal.get() || expanded.get(), move |open| {
                header_box.set_border(if open { BorderProp::Sides(vec![Side::Left]) } else { BorderProp::All(false) });
            });
            header_box.set_custom_border_chars(Some(split_border_chars()));
            bind_always(move || rgba(theme.with(|th| th.decrease(th.background.base, 1.0))), move |c| header_box.set_border_color(Some(c)));
            bind(move || if !in_minimal.get() || expanded.get() { 1.0 } else { 0.0 }, move |p| {
                header_box.set_padding(Edge::Left, Some(Value::Point(p)))
            });
            let clicker = box_();
            insert_node(header_box, clicker, None);
            clicker.set_on_mouse(Some(MouseEventType::Up), Some(toggle.clone()));
            insert(
                clicker,
                reasoning_header(ReasoningHeaderProps {
                    toggleable: Rc::new(move || in_minimal.get()),
                    open: Rc::new(move || !in_minimal.get() || expanded.get()),
                    done: Rc::new(move || is_done.get()),
                    title: Rc::new(move || if in_minimal.get() && !expanded.get() { summary.get() } else { None }),
                    duration: Rc::new(move || is_done.get().then(|| duration(duration_ms.get()))),
                }),
                At::Only,
            );
            let markdown_mode = markdown_mode.clone();
            insert(
                outer,
                show(
                    move || !in_minimal.get() || expanded.get(),
                    move || {
                        let wrap = box_();
                        wrap.set_margin(Edge::Top, Some(Value::Point(1.0)));
                        let inner = box_();
                        insert_node(wrap, inner, None);
                        inner.set_border(BorderProp::Sides(vec![Side::Left]));
                        inner.set_custom_border_chars(Some(split_border_chars()));
                        bind_always(move || rgba(theme.with(|th| th.decrease(th.background.base, 1.0))), move |c| inner.set_border_color(Some(c)));
                        bind(move || if in_minimal.get() { 3.0 } else { 1.0 }, move |p| inner.set_padding(Edge::Left, Some(Value::Point(p))));
                        let block = code();
                        insert_node(inner, block, None);
                        block.set_filetype(Some("markdown".into()));
                        block.set_draw_unstyled_text(false);
                        block.set_streaming(true);
                        bind_always(move || thinking.get(), move |s| block.set_syntax_style(s));
                        bind(move || content.get(), move |c| block.set_code_content(JsString::from(c.as_str())));
                        let markdown_mode = markdown_mode.clone();
                        bind(move || markdown_mode() == MarkdownMode::Rendered, move |c| block.set_conceal(c));
                        bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| block.set_fg(Some(c)));
                        View::Node(wrap)
                    },
                    None,
                ),
                At::End,
            );
            View::Node(outer)
        },
        None,
    )
}

struct ReasoningHeaderProps {
    toggleable: Rc<dyn Fn() -> bool>,
    open: Rc<dyn Fn() -> bool>,
    done: Rc<dyn Fn() -> bool>,
    title: Rc<dyn Fn() -> Option<String>>,
    duration: Rc<dyn Fn() -> Option<String>>,
}

/// `<ReasoningHeader>`.
fn reasoning_header(props: ReasoningHeaderProps) -> View {
    let theme = tui().theme;
    let open = props.open.clone();
    let fg: Rc<dyn Fn() -> Option<RGBA>> = Rc::new(move || {
        let warning = rgba(theme.with(|th| th.text.feedback.warning.base));
        Some(if open() { fade(warning, 0.6) } else { warning })
    });
    let done = props.done.clone();
    let fallback_props = (props.toggleable.clone(), props.open.clone(), props.title.clone(), props.duration.clone(), fg.clone());
    switch(
        vec![when(move || !done(), {
            let title = props.title.clone();
            let fg = fg.clone();
            move || {
                let row = box_();
                row.set_flex_direction(FlexDirection::Row);
                let title = title.clone();
                insert(
                    row,
                    spinner(SpinnerProps {
                        children: Some(Rc::new(move || {
                            View::from(match title() {
                                Some(title) => format!("Thinking: {title}"),
                                None => "Thinking".to_string(),
                            })
                        })),
                        color: Some(fg.clone()),
                        shimmer: None,
                    }),
                    At::Only,
                );
                View::Node(row)
            }
        })],
        Some(Rc::new(move || {
            let (toggleable, open, title, duration, fg) = fallback_props.clone();
            let el = text();
            bind_always(move || fg(), move |c| el.set_fg(c));
            el.set_wrap_mode(opencode_tui_core::renderables::text::WrapMode::None);
            let open_for = open.clone();
            insert(
                el,
                show(move || toggleable(), move || {
                    let s = span();
                    let open = open_for.clone();
                    insert(s, View::dynamic(move || View::from(if open() { "- " } else { "+ " })), At::Only);
                    View::Node(s)
                }, None),
                At::End,
            );
            let thought = span();
            insert_node(thought, text_node_from_string("Thought"), None);
            insert_node(el, thought, None);
            let (t1, d1) = (title.clone(), duration.clone());
            insert(
                el,
                show(move || t1().is_some() || d1().is_some(), || {
                    let s = span();
                    insert_node(s, text_node_from_string(": "), None);
                    View::Node(s)
                }, None),
                At::End,
            );
            let t2 = title.clone();
            insert(
                el,
                show_value(move || t2(), |title| {
                    let s = span();
                    insert(s, View::dynamic(move || View::from(title.get().unwrap_or_default())), At::Only);
                    View::Node(s)
                }, None),
                At::End,
            );
            let (t3, d3) = (title.clone(), duration.clone());
            insert(
                el,
                show_value(move || d3(), move |duration| {
                    let s = span();
                    let t3 = t3.clone();
                    insert(s, View::dynamic(move || View::from(if t3().is_some() { " · " } else { "" })), At::End);
                    insert(s, View::dynamic(move || View::from(duration.get().unwrap_or_default())), At::End);
                    View::Node(s)
                }, None),
                At::End,
            );
            View::Node(el)
        })),
    )
}

/// `<TextPart last part message>`.
pub fn text_part(props: PartProps) -> View {
    let ctx = use_ctx();
    let theme = tui().theme;
    let part = props.part.clone();
    let message = props.message.clone();
    let text_of = {
        let part = part.clone();
        create_memo(move || part().str("text").unwrap_or_default().trim().to_string())
    };
    show(
        move || !text_of.get().is_empty(),
        move || {
            let wrap = box_();
            wrap.set_padding(Edge::Left, Some(Value::Point(3.0)));
            wrap.set_flex_shrink(Some(0.0));
            // Configure custom nodes before parsing; apply content before streaming so completion keeps the final
            // tokens.
            let md = markdown();
            insert_node(wrap, md, None);
            md.set_markdown_internal_block_mode(InternalBlockMode::TopLevel);
            md.set_markdown_table_options(Some(MarkdownTableOptions {
                style: Some(MarkdownTableStyle::Grid),
                cell_padding_x: Some(1.0),
                ..MarkdownTableOptions::default()
            }));
            bind_always(current_syntax, move |s| md.set_markdown_syntax_style(s));
            bind(move || text_of.get(), move |c| md.set_markdown_content(&c));
            let message = message.clone();
            bind(move || time(&message(), "completed").is_none(), move |s| md.set_markdown_streaming(s));
            let markdown_mode = ctx.markdown_mode.clone();
            bind(move || markdown_mode() == MarkdownMode::Rendered, move |c| md.set_markdown_conceal(c));
            bind_always(move || rgba(theme.with(|th| th.markdown.text)), move |c| md.set_markdown_fg(Some(c)));
            bind_always(move || rgba(theme.with(|th| th.background.base)), move |c| md.set_markdown_bg(Some(c)));
            View::Node(wrap)
        },
        None,
    )
}

type Color = Rc<dyn Fn() -> Option<RGBA>>;

/// The props of `<InlineToolRow>`; reactive ones are closures.
#[derive(Clone)]
pub struct InlineToolRowProps {
    pub icon: Rc<dyn Fn() -> String>,
    pub icon_color: Option<Color>,
    pub color: Option<Color>,
    pub error_color: Option<Color>,
    pub failed: Rc<dyn Fn() -> bool>,
    pub denied: Rc<dyn Fn() -> bool>,
    pub error: Rc<dyn Fn() -> Option<String>>,
    pub error_expanded: Rc<dyn Fn() -> bool>,
    pub complete: Rc<dyn Fn() -> bool>,
    pub pending: Rc<dyn Fn() -> String>,
    pub failure: Option<Rc<dyn Fn() -> View>>,
    pub spinner: Rc<dyn Fn() -> bool>,
    pub status: Option<Rc<dyn Fn() -> View>>,
    pub children: Rc<dyn Fn() -> View>,
    pub on_mouse_over: Option<MouseHandler>,
    pub on_mouse_out: Option<MouseHandler>,
    pub on_mouse_up: Option<MouseHandler>,
}

impl Default for InlineToolRowProps {
    fn default() -> Self {
        InlineToolRowProps {
            icon: Rc::new(String::new),
            icon_color: None,
            color: None,
            error_color: None,
            failed: Rc::new(|| false),
            denied: Rc::new(|| false),
            error: Rc::new(|| None),
            error_expanded: Rc::new(|| false),
            complete: Rc::new(|| false),
            pending: Rc::new(String::new),
            failure: None,
            spinner: Rc::new(|| false),
            status: None,
            children: Rc::new(|| View::Empty),
            on_mouse_over: None,
            on_mouse_out: None,
            on_mouse_up: None,
        }
    }
}

fn color_of(color: &Option<Color>) -> Option<RGBA> {
    color.as_ref().and_then(|c| c())
}

fn strike(denied: bool) -> Option<u32> {
    denied.then_some(text_attributes::STRIKETHROUGH)
}

/// `<InlineToolRow>`.
pub fn inline_tool_row(props: InlineToolRowProps) -> View {
    let row = box_();
    row.set_padding(Edge::Left, Some(Value::Point(3.0)));
    row.set_on_mouse(Some(MouseEventType::Over), props.on_mouse_over.clone());
    row.set_on_mouse(Some(MouseEventType::Out), props.on_mouse_out.clone());
    row.set_on_mouse(Some(MouseEventType::Up), props.on_mouse_up.clone());
    let p = props.clone();
    let spinning = props.spinner.clone();
    let body = switch(
        vec![
            when(move || spinning(), {
                let p = p.clone();
                move || {
                    let color = p.color.clone();
                    match p.status.clone() {
                        None => spinner(SpinnerProps { children: Some(p.children.clone()), color, shimmer: None }),
                        Some(status) => {
                            let line = box_();
                            line.set_flex_direction(FlexDirection::Row);
                            line.set_gap(2, Some(Value::Point(1.0)));
                            insert(line, spinner(SpinnerProps { children: None, color: color.clone(), shimmer: None }), At::End);
                            insert(line, inline_tool_label(color, Rc::new(|| false), status, p.children.clone()), At::End);
                            View::Node(line)
                        }
                    }
                }
            }),
            when(|| true, {
                let p = p.clone();
                move || {
                    let p = p.clone();
                    let (complete, failed) = (p.complete.clone(), p.failed.clone());
                    let pending = p.pending.clone();
                    let color = p.color.clone();
                    show(
                        move || complete() || failed(),
                        move || {
                            let line = box_();
                            line.set_flex_direction(FlexDirection::Row);
                            let icon = text();
                            insert_node(line, icon, None);
                            icon.set_width(Some(Value::Point(INLINE_TOOL_ICON_WIDTH)));
                            let q = p.clone();
                            bind_always(
                                move || if (q.failed)() { color_of(&q.error_color) } else { color_of(&q.icon_color).or_else(|| color_of(&q.color)) },
                                move |c| icon.set_fg(c),
                            );
                            let q = p.clone();
                            bind(move || strike((q.denied)()), move |a| icon.set_attributes(a));
                            let q = p.clone();
                            insert(icon, View::dynamic(move || View::from((q.icon)())), At::Only);
                            let label: Rc<dyn Fn() -> View> = {
                                let q = p.clone();
                                Rc::new(move || {
                                    if (q.failed)() && !(q.complete)() {
                                        match &q.failure {
                                            Some(f) => f(),
                                            None => (q.children)(),
                                        }
                                    } else {
                                        (q.children)()
                                    }
                                })
                            };
                            match p.status.clone() {
                                None => {
                                    let body = text();
                                    insert_node(line, body, None);
                                    body.set_flex_grow(Some(1.0));
                                    let q = p.clone();
                                    bind_always(
                                        move || if (q.failed)() { color_of(&q.error_color) } else { color_of(&q.color) },
                                        move |c| body.set_fg(c),
                                    );
                                    let q = p.clone();
                                    bind(move || strike((q.denied)()), move |a| body.set_attributes(a));
                                    insert(body, View::dynamic(move || label()), At::Only);
                                }
                                Some(status) => {
                                    let q = p.clone();
                                    let color: Color = Rc::new(move || if (q.failed)() { color_of(&q.error_color) } else { color_of(&q.color) });
                                    insert(line, inline_tool_label(Some(color), p.denied.clone(), status, label), At::End);
                                }
                            }
                            View::Node(line)
                        },
                        Some(Rc::new(move || {
                            let pending = pending.clone();
                            spinner(SpinnerProps { children: Some(Rc::new(move || View::from(pending()))), color: color.clone(), shimmer: None })
                        })),
                    )
                }
            }),
        ],
        None,
    );
    insert(row, body, At::End);
    let p = props.clone();
    insert(
        row,
        show(
            move || (p.failed)() && (p.error_expanded)(),
            {
                let p = props.clone();
                move || {
                    let wrap = box_();
                    wrap.set_padding(Edge::Left, Some(Value::Point(INLINE_TOOL_ICON_WIDTH)));
                    let message = text();
                    insert_node(wrap, message, None);
                    let q = p.clone();
                    bind_always(move || color_of(&q.error_color), move |c| message.set_fg(c));
                    let q = p.clone();
                    insert(message, View::dynamic(move || View::from((q.error)().unwrap_or_default())), At::Only);
                    View::Node(wrap)
                }
            },
            None,
        ),
        At::End,
    );
    View::Node(row)
}

/// `<InlineToolLabel color denied status>{children}</InlineToolLabel>`.
fn inline_tool_label(color: Option<Color>, denied: Rc<dyn Fn() -> bool>, status: Rc<dyn Fn() -> View>, children: Rc<dyn Fn() -> View>) -> View {
    let wrap = box_();
    wrap.set_flex_direction(FlexDirection::Row);
    wrap.set_flex_wrap(1);
    wrap.set_gap(0, Some(Value::Point(1.0)));
    wrap.set_flex_grow(Some(1.0));
    let label = text();
    insert_node(wrap, label, None);
    label.set_max_width(Some(Value::Percent(100.0)));
    label.set_flex_shrink(Some(0.0));
    bind_always(move || color_of(&color), move |c| label.set_fg(c));
    bind(move || strike(denied()), move |a| label.set_attributes(a));
    insert(label, View::dynamic(move || children()), At::Only);
    insert(wrap, View::dynamic(move || status()), At::End);
    View::Node(wrap)
}

