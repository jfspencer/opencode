//! TODO(port-tui): stand-ins for what the session slice owns, with the names it agreed on, until they land:
//! `crate::session::render_context::{use_ctx, RenderCtx}` (render-context.tsx), `crate::spinner::{spinner,
//! SpinnerProps}` (component/spinner.tsx) and `crate::session::message_parts::{inline_tool_row,
//! InlineToolRowProps, tool_display, INLINE_TOOL_ICON_WIDTH}` (message-parts.tsx). Each is a port of the TS it
//! stands for, minus what the tool views do not read; the spinner always draws the animations-off form.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::{RGBA, text_attributes};
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, FlexDirection, MouseEventType};

use crate::config::Resolved;
use crate::ctx::{Route, tui};
use crate::util::rgba;

pub const INLINE_TOOL_ICON_WIDTH: f32 = 2.0;

/// `ctx.diffWrapMode()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffWrapMode {
    Word,
    None,
}

/// What the tool views read of the render context.
#[derive(Clone)]
pub struct RenderCtx {
    /// Content width: terminal width minus vertical tabs, sidebar and padding.
    pub width: Rc<dyn Fn() -> i32>,
    pub terminal_width: Rc<dyn Fn() -> u32>,
    pub terminal_height: Rc<dyn Fn() -> u32>,
    pub session_id: String,
    pub diff_wrap_mode: Rc<dyn Fn() -> DiffWrapMode>,
    pub models: Rc<dyn Fn() -> Rc<Vec<Rc<Json>>>>,
    pub config: Rc<dyn Fn() -> Rc<Resolved>>,
}

/// `use()` of render-context.tsx (a stand-in derived from the TUI's contexts).
pub fn use_ctx() -> RenderCtx {
    let t = tui();
    let dimensions = t.dimensions;
    let session_id = t.route.with_untracked(|r| match r {
        Route::Session { session_id } => session_id.clone(),
        _ => String::new(),
    });
    let config = t.config_context.clone();
    let wrap_config = config.clone();
    let store = t.data.store.clone();
    RenderCtx {
        width: Rc::new(move || dimensions.get().0 as i32 - 4),
        terminal_width: Rc::new(move || dimensions.get().0),
        terminal_height: Rc::new(move || dimensions.get().1),
        session_id,
        diff_wrap_mode: Rc::new(move || match wrap_config.data().diffs.as_ref().and_then(|d| d.wrap.clone()).as_deref() {
            Some("none") => DiffWrapMode::None,
            _ => DiffWrapMode::Word,
        }),
        models: Rc::new(move || {
            let list = store.location.model.list(None);
            Rc::new(match list.as_deref() {
                Some(Json::Array(items)) => items.iter().map(|i| Rc::new(i.clone())).collect(),
                _ => Vec::new(),
            })
        }),
        config: Rc::new(move || config.data()),
    }
}

/// `SpinnerProps`.
pub struct SpinnerProps {
    pub children: Option<Rc<dyn Fn() -> View>>,
    pub color: Option<Rc<dyn Fn() -> RGBA>>,
}

/// `<Spinner color>{children}</Spinner>` with animations off: `⋯ {children}`.
pub fn spinner(props: SpinnerProps) -> View {
    let theme = tui().theme;
    let el = text();
    let color = props.color.clone();
    bind_always(
        move || match &color {
            Some(color) => color(),
            None => rgba(theme.with(|t| t.text.muted)),
        },
        move |c| el.set_fg(Some(c)),
    );
    match props.children {
        Some(children) => {
            insert_node(el, opencode_tui_core::renderables::text::text_node_from_string("⋯ "), None);
            insert(el, View::dynamic(move || children()), At::End);
        }
        None => insert_node(el, opencode_tui_core::renderables::text::text_node_from_string("⋯"), None),
    }
    View::Node(el)
}

type Prop<T> = Rc<dyn Fn() -> T>;

/// `InlineToolRow`'s props.
#[derive(Clone)]
pub struct InlineToolRowProps {
    pub icon: Prop<String>,
    pub icon_color: Option<Prop<Option<RGBA>>>,
    pub color: Prop<Option<RGBA>>,
    pub error_color: Prop<Option<RGBA>>,
    pub failed: Prop<bool>,
    pub denied: Prop<bool>,
    pub error: Prop<Option<String>>,
    pub error_expanded: Prop<bool>,
    pub complete: Prop<bool>,
    pub pending: Prop<String>,
    pub failure: Option<Prop<Option<String>>>,
    pub spinner: Prop<bool>,
    pub status: Option<Rc<dyn Fn() -> View>>,
    pub children: Rc<dyn Fn() -> View>,
    pub on_mouse_over: Option<Rc<dyn Fn()>>,
    pub on_mouse_out: Option<Rc<dyn Fn()>>,
    pub on_mouse_up: Option<Rc<dyn Fn()>>,
}

fn colored(el: opencode_tui_core::tree::Element, color: Prop<Option<RGBA>>) {
    bind_always(move || color(), move |c| el.set_fg(c));
}

/// `InlineToolLabel`.
fn inline_tool_label(color: Prop<Option<RGBA>>, denied: Prop<bool>, status: Rc<dyn Fn() -> View>, children: Rc<dyn Fn() -> View>) -> View {
    let row = box_();
    row.set_flex_direction(FlexDirection::Row);
    row.set_flex_wrap(1);
    row.set_gap(0, Some(Value::Point(1.0)));
    row.set_flex_grow(Some(1.0));
    let label = text();
    insert_node(row, label, None);
    label.set_max_width(Some(Value::Percent(100.0)));
    label.set_flex_shrink(Some(0.0));
    colored(label, color);
    bind(move || denied(), move |d| label.set_attributes(d.then_some(text_attributes::STRIKETHROUGH)));
    insert(label, View::dynamic(move || children()), At::Only);
    insert(row, View::dynamic(move || status()), At::End);
    View::Node(row)
}

/// `<InlineToolRow>`.
pub fn inline_tool_row(props: InlineToolRowProps) -> View {
    let root = box_();
    root.set_padding(Edge::Left, Some(Value::Point(3.0)));
    if let Some(f) = props.on_mouse_over.clone() {
        root.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| f())));
    }
    if let Some(f) = props.on_mouse_out.clone() {
        root.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| f())));
    }
    if let Some(f) = props.on_mouse_up.clone() {
        root.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| f())));
    }
    let p = props.clone();
    let spinning = {
        let p = p.clone();
        move || {
            let p = p.clone();
            match p.status.clone() {
                None => spinner(SpinnerProps {
                    children: Some(p.children.clone()),
                    color: Some(Rc::new({
                        let color = p.color.clone();
                        let theme = tui().theme;
                        move || color().unwrap_or_else(|| rgba(theme.with(|t| t.text.muted)))
                    })),
                }),
                Some(status) => {
                    let row = box_();
                    row.set_flex_direction(FlexDirection::Row);
                    row.set_gap(2, Some(Value::Point(1.0)));
                    let theme = tui().theme;
                    let color = p.color.clone();
                    insert(
                        row,
                        spinner(SpinnerProps {
                            children: None,
                            color: Some(Rc::new(move || color().unwrap_or_else(|| rgba(theme.with(|t| t.text.muted))))),
                        }),
                        At::End,
                    );
                    insert(row, inline_tool_label(p.color.clone(), Rc::new(|| false), status, p.children.clone()), At::End);
                    View::Node(row)
                }
            }
        }
    };
    let settled = {
        let p = p.clone();
        move || {
            let p = p.clone();
            let pending = {
                let p = p.clone();
                Rc::new(move || {
                    let theme = tui().theme;
                    let color = p.color.clone();
                    let pending = p.pending.clone();
                    spinner(SpinnerProps {
                        children: Some(Rc::new(move || View::from(pending()))),
                        color: Some(Rc::new(move || color().unwrap_or_else(|| rgba(theme.with(|t| t.text.muted))))),
                    })
                }) as Rc<dyn Fn() -> View>
            };
            let complete = p.complete.clone();
            let failed = p.failed.clone();
            show(
                move || complete() || failed(),
                move || {
                    let p = p.clone();
                    let row = box_();
                    row.set_flex_direction(FlexDirection::Row);
                    let icon = text();
                    insert_node(row, icon, None);
                    icon.set_width(Some(Value::Point(INLINE_TOOL_ICON_WIDTH)));
                    {
                        let (failed, error_color, icon_color, color) =
                            (p.failed.clone(), p.error_color.clone(), p.icon_color.clone(), p.color.clone());
                        colored(
                            icon,
                            Rc::new(move || {
                                if failed() {
                                    return error_color();
                                }
                                icon_color.as_ref().and_then(|c| c()).or_else(|| color())
                            }),
                        );
                    }
                    {
                        let denied = p.denied.clone();
                        bind(move || denied(), move |d| icon.set_attributes(d.then_some(text_attributes::STRIKETHROUGH)));
                    }
                    {
                        let icon_text = p.icon.clone();
                        insert(icon, View::dynamic(move || View::from(icon_text())), At::Only);
                    }
                    let label_color: Prop<Option<RGBA>> = {
                        let (failed, error_color, color) = (p.failed.clone(), p.error_color.clone(), p.color.clone());
                        Rc::new(move || if failed() { error_color() } else { color() })
                    };
                    let content: Rc<dyn Fn() -> View> = {
                        let (failed, complete, failure, children) =
                            (p.failed.clone(), p.complete.clone(), p.failure.clone(), p.children.clone());
                        Rc::new(move || {
                            if failed() && !complete() {
                                if let Some(text) = failure.as_ref().and_then(|f| f()) {
                                    return View::from(text);
                                }
                            }
                            children()
                        })
                    };
                    match p.status.clone() {
                        None => {
                            let label = text();
                            insert_node(row, label, None);
                            label.set_flex_grow(Some(1.0));
                            colored(label, label_color);
                            let denied = p.denied.clone();
                            bind(move || denied(), move |d| label.set_attributes(d.then_some(text_attributes::STRIKETHROUGH)));
                            insert(label, View::dynamic(move || content()), At::Only);
                        }
                        Some(status) => {
                            insert(row, inline_tool_label(label_color, p.denied.clone(), status, content), At::End);
                        }
                    }
                    View::Node(row)
                },
                Some(pending),
            )
        }
    };
    let spinner_on = props.spinner.clone();
    insert(root, switch(vec![when(move || spinner_on(), spinning), when(|| true, settled)], None), At::End);
    let (failed, expanded, error, error_color) =
        (props.failed.clone(), props.error_expanded.clone(), props.error.clone(), props.error_color.clone());
    insert(
        root,
        show(
            move || failed() && expanded(),
            move || {
                let pad = box_();
                pad.set_padding(Edge::Left, Some(Value::Point(INLINE_TOOL_ICON_WIDTH)));
                let label = text();
                insert_node(pad, label, None);
                colored(label, error_color.clone());
                let error = error.clone();
                insert(label, View::dynamic(move || View::from(error().unwrap_or_default())), At::Only);
                View::Node(pad)
            },
            None,
        ),
        At::End,
    );
    View::Node(root)
}

const TOOL_DISPLAYS: [&str; 14] =
    ["shell", "glob", "read", "grep", "webfetch", "websearch", "write", "edit", "subagent", "execute", "patch", "todowrite", "question", "skill"];

/// `toolDisplay(tool)`.
pub fn tool_display(tool: &str) -> &'static str {
    let normalized = super::display::canonical_tool_name(tool);
    TOOL_DISPLAYS.iter().find(|d| **d == normalized).copied().unwrap_or("generic")
}
