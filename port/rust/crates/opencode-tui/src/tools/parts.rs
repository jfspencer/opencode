//! `ToolPart` and every tool's view (`routes/session/index.tsx`): the generic view, InlineTool and BlockTool,
//! shell (with a background shell's output), write, glob, read, grep, webfetch, websearch, subagent, execute (its
//! nested calls), edit, patch, todos, questions, skill and diagnostics.
//!
//! A part is the store's JSON record (`SessionMessageAssistantTool`) behind an accessor; each prop TS reads
//! through `toolprops`' getters is a memo of it.
//!
//! Divergences: `renderer.getSelection()` (a click that ends a text selection) is not checked (the renderer has
//! no selection yet). TODO(port-tui): images (`<image>`, `image_preview`), the websearch provider's
//! RetryProvider animation (its name is shown as plain text), DialogExecute (the dialogs slice), and the session
//! slice's `<code>` for written files (plain text in the gutter until it lands).
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::RGBA;
use opencode_tui_core::elements::{box_, line_number, span, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::renderables::diff::DiffView;
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, FlexDirection, MouseEventType};
use opencode_tui_sync::data::SessionStatus;
use opencode_tui_sync::json::JsonExt;

use super::collapse::{collapse_shell_output, collapse_tool_output};
use super::display::*;
use super::filetype::filetype;
use super::patch_diff::{DiffProps, PatchDiffProps, patch_diff};
use super::path_format::use_path_formatter;
use super::standin::{
    DiffWrapMode, INLINE_TOOL_ICON_WIDTH, InlineToolRowProps, SpinnerProps, inline_tool_row, spinner, tool_display, use_ctx,
};
use super::todo::{parse_todos, todo_item};
use crate::ctx::{Route, tui};
use crate::util::{rgba, string_width, titlecase};

/// The part accessor (`props.part`).
pub type PartFn = Rc<dyn Fn() -> Rc<Json>>;

fn txt(value: &str) -> View {
    View::from(value.to_string())
}

/// What every tool view gets (`toolprops`).
#[derive(Clone)]
pub struct ToolProps {
    pub part: PartFn,
    pub status: Memo<String>,
    pub input: Memo<Json>,
    pub metadata: Memo<Json>,
    pub output: Memo<Option<String>>,
    pub tool: Memo<String>,
}

impl ToolProps {
    fn new(part: PartFn) -> ToolProps {
        let p = part.clone();
        let status = create_memo(move || p().get("state").and_then(|s| s.str("status")).unwrap_or("").to_string());
        let p = part.clone();
        let input = create_memo(move || match p().get("state").and_then(|s| s.get("input")) {
            Some(input @ Json::Object(_)) => input.clone(),
            _ => Json::Object(Default::default()),
        });
        let p = part.clone();
        let metadata = create_memo(move || tool_display_metadata(p().get("state")));
        let p = part.clone();
        let output = create_memo(move || {
            let part = p();
            let state = part.get("state");
            if state.and_then(|s| s.str("status")) == Some("streaming") {
                return None;
            }
            Some(
                tool_display_content(state)
                    .iter()
                    .map(|content| {
                        if content.str("type") == Some("text") {
                            content.str("text").unwrap_or("").to_string()
                        } else {
                            content.str("name").or(content.str("uri")).unwrap_or("").to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        });
        let p = part.clone();
        let tool = create_memo(move || p().str("name").unwrap_or("").to_string());
        ToolProps { part, status, input, metadata, output, tool }
    }

    fn input_str(&self, key: &'static str) -> impl Fn() -> Option<String> + Clone + 'static {
        let input = self.input;
        move || input.with(|i| i.str(key).map(str::to_string))
    }

    fn metadata_str(&self, key: &'static str) -> impl Fn() -> Option<String> + Clone + 'static {
        let metadata = self.metadata;
        move || metadata.with(|m| m.str(key).map(str::to_string))
    }
}

/// `<ToolPart part images>`.
pub fn tool_part(part: PartFn, images: Option<bool>) -> View {
    let display = {
        let part = part.clone();
        create_memo(move || tool_display(part().str("name").unwrap_or("")))
    };
    let props = ToolProps::new(part.clone());
    let case = |name: &'static str, view: fn(ToolProps) -> View| {
        let props = props.clone();
        when(move || display.get() == name, move || {
            let props = props.clone();
            component(move || view(props))
        })
    };
    let fallback = {
        let props = props.clone();
        Rc::new(move || {
            let props = props.clone();
            component(move || generic_tool(props))
        }) as Rc<dyn Fn() -> View>
    };
    let content = switch(
        vec![
            case("shell", shell),
            case("glob", glob),
            case("read", read),
            case("grep", grep),
            case("webfetch", webfetch),
            case("websearch", websearch),
            case("write", write),
            case("edit", edit),
            case("subagent", subagent),
            case("execute", execute),
            case("patch", apply_patch),
            case("todowrite", todo_write),
            case("question", question),
            case("skill", skill),
        ],
        Some(fallback),
    );
    View::list([content, show(move || images != Some(false), move || tool_images(vec![part.clone()]), None)])
}

/// `<ToolImages parts>`.
pub fn tool_images(parts: Vec<PartFn>) -> View {
    // TODO(port-tui): SessionImages (`<image>` previews when `session.image_preview` is on).
    let ctx = use_ctx();
    let _ = parts;
    show(move || (ctx.config)().session.image_preview.unwrap_or(false), || View::Empty, None)
}

/// `local.permission.mode` (context/permission.tsx): `--auto`, else `session.permissions`.
fn autoaccept() -> bool {
    let t = tui();
    t.args.auto || t.config_context.data().session.permissions == "autoaccept"
}

/// `useToolPermission(part)`: the session's first permission request is for this call.
fn use_tool_permission(part: Option<PartFn>) -> Memo<bool> {
    let ctx = use_ctx();
    let store = tui().data.store.clone();
    create_memo(move || {
        if autoaccept() {
            return false;
        }
        let Some(list) = store.session.permission.list(&ctx.session_id) else { return false };
        let Some(request) = list.first() else { return false };
        let source = request.get("source");
        source.and_then(|s| s.str("type")) == Some("tool")
            && part.as_ref().is_some_and(|p| source.and_then(|s| s.str("id")) == p().str("id"))
    })
}

/// `InlineTool`'s props.
struct InlineToolProps {
    icon: Rc<dyn Fn() -> String>,
    color: Option<Rc<dyn Fn() -> Option<RGBA>>>,
    complete: Rc<dyn Fn() -> bool>,
    pending: String,
    spinner: Option<Rc<dyn Fn() -> bool>>,
    running: Option<Rc<dyn Fn() -> bool>>,
    status: Option<Rc<dyn Fn() -> View>>,
    children: Rc<dyn Fn() -> View>,
    part: PartFn,
    on_click: Option<Rc<dyn Fn()>>,
    on_error_click: Option<Rc<dyn Fn()>>,
}

/// `<InlineTool>`.
fn inline_tool(props: InlineToolProps) -> View {
    let theme = tui().theme;
    let (hover, set_hover) = create_signal(false);
    let (error_expanded, set_error_expanded) = create_signal(false);
    let permission = use_tool_permission(Some(props.part.clone()));
    let error = {
        let part = props.part.clone();
        let running = props.running.clone();
        create_memo(move || {
            if running.as_ref().is_some_and(|r| r()) {
                return None;
            }
            let part = part();
            let state = part.get("state")?;
            if state.str("status") != Some("error") {
                return None;
            }
            Some(strip_ansi(state.get("error").and_then(|e| e.str("message")).unwrap_or("")))
        })
    };
    let denied = create_memo(move || {
        error.with(|e| {
            e.as_ref().is_some_and(|e| {
                e.contains("QuestionRejectedError") || e.contains("rejected permission") || e.contains("specified a rule") || e.contains("user dismissed")
            })
        })
    });
    let failed = create_memo(move || error.with(Option::is_some) && !denied.get());
    let has_click = props.on_click.is_some();
    let clickable = create_memo(move || has_click || failed.get());
    let color = props.color.clone();
    let fg = create_memo_equals(
        move || {
            if let Some(color) = color.as_ref().and_then(|c| c()) {
                return Some(color);
            }
            theme.with(|t| {
                Some(if permission.get() {
                    rgba(t.text.feedback.warning.base)
                } else if failed.get() {
                    rgba(t.text.feedback.error.base)
                } else if hover.get() && has_click {
                    rgba(t.text.base)
                } else {
                    rgba(t.text.muted)
                })
            })
        },
        None,
    );
    let on_click = props.on_click.clone();
    let on_error_click = props.on_error_click.clone();
    inline_tool_row(InlineToolRowProps {
        icon: props.icon.clone(),
        icon_color: None,
        color: Rc::new(move || fg.get()),
        error_color: Rc::new(move || Some(rgba(theme.with(|t| t.text.feedback.error.base)))),
        failed: Rc::new(move || failed.get()),
        denied: Rc::new(move || denied.get()),
        error: Rc::new(move || error.get()),
        error_expanded: Rc::new(move || error_expanded.get()),
        complete: props.complete.clone(),
        pending: {
            let pending = props.pending.clone();
            Rc::new(move || pending.clone())
        },
        failure: None,
        spinner: props.spinner.clone().unwrap_or_else(|| Rc::new(|| false)),
        status: props.status.clone(),
        children: props.children.clone(),
        on_mouse_over: Some(Rc::new(move || {
            if clickable.get_untracked() {
                set_hover.set(true);
            }
        })),
        on_mouse_out: Some(Rc::new(move || set_hover.set(false))),
        on_mouse_up: Some(Rc::new(move || {
            if failed.get_untracked() {
                if let Some(f) = &on_error_click {
                    return f();
                }
                set_error_expanded.update(|v| *v = !*v);
                return;
            }
            if let Some(f) = &on_click {
                f();
            }
        })),
    })
}

/// `<StatusBadge raised>{text}</StatusBadge>`.
fn status_badge(label: &'static str, raised: bool) -> View {
    let theme = tui().theme;
    let el = text();
    el.set_flex_shrink(Some(0.0));
    bind_always(
        move || theme.with(|t| rgba(t.decrease(if raised { t.background.raised.base } else { t.background.base }, 1.0))),
        move |c| el.set_bg(Some(c)),
    );
    bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| el.set_fg(Some(c)));
    insert(el, View::from(format!(" {label} ")), At::Only);
    View::Node(el)
}

/// `BlockTool`'s props.
#[derive(Default)]
struct BlockToolProps {
    title: Option<Rc<dyn Fn() -> Option<String>>>,
    /// `{ label, value }`.
    path: Option<Rc<dyn Fn() -> Option<(String, String)>>>,
    header_color: Option<Rc<dyn Fn() -> Option<RGBA>>>,
    children: Option<Rc<dyn Fn() -> View>>,
    on_click: Option<Rc<dyn Fn() -> Option<Rc<dyn Fn()>>>>,
    part: Option<PartFn>,
    spinner: Option<Rc<dyn Fn() -> bool>>,
    error: Option<Rc<dyn Fn() -> Option<String>>>,
    error_color: Option<Rc<dyn Fn() -> Option<RGBA>>>,
}

/// `<FilePath value maxWidth fg basenameFg>` (`ui/file-path.tsx`).
fn file_path(value: Rc<dyn Fn() -> String>, max_width: Rc<dyn Fn() -> i64>, fg: Rc<dyn Fn() -> RGBA>) -> View {
    let el = text();
    {
        let fg = fg.clone();
        bind_always(move || fg(), move |c| el.set_fg(Some(c)));
    }
    el.set_wrap_mode(WrapMode::None);
    el.set_truncate(true);
    let display = create_memo(move || {
        let value = crate::util::truncate_file_path(&value(), max_width());
        let index = value.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
        (value[..index].to_string(), value[index..].to_string())
    });
    let parent = span();
    let basename = span();
    insert_node(el, parent, None);
    insert_node(el, basename, None);
    {
        let fg = fg.clone();
        bind_always(move || fg(), move |c| parent.set_span_style(Some(c), None, 0, 0));
    }
    bind_always(move || fg(), move |c| basename.set_span_style(Some(c), None, 0, 0));
    insert(parent, View::dynamic(move || View::from(display.get().0)), At::Only);
    insert(basename, View::dynamic(move || View::from(display.get().1)), At::Only);
    View::Node(el)
}

/// `<BlockTool>`.
fn block_tool(props: BlockToolProps) -> View {
    let theme = tui().theme;
    let ctx = use_ctx();
    let (hover, set_hover) = create_signal(false);
    let error = {
        let error_prop = props.error.clone();
        let part = props.part.clone();
        create_memo(move || {
            let explicit = error_prop.as_ref().and_then(|e| e());
            let from_part = part.as_ref().and_then(|p| {
                let part = p();
                let state = part.get("state")?;
                (state.str("status") == Some("error")).then(|| state.get("error").and_then(|e| e.str("message")).unwrap_or("").to_string())
            });
            strip_ansi(&explicit.or(from_part).unwrap_or_default())
        })
    };
    let permission = use_tool_permission(props.part.clone());
    let on_click = props.on_click.clone();
    let header_color: Rc<dyn Fn() -> RGBA> = {
        let header_color = props.header_color.clone();
        Rc::new(move || {
            if permission.get() {
                return rgba(theme.with(|t| t.text.feedback.warning.base));
            }
            header_color.as_ref().and_then(|c| c()).unwrap_or_else(|| rgba(theme.with(|t| t.text.muted)))
        })
    };
    let spinner_color: Rc<dyn Fn() -> RGBA> = Rc::new(move || {
        theme.with(|t| if permission.get() { rgba(t.text.feedback.warning.base) } else { rgba(t.text.muted) })
    });

    let root = box_();
    root.set_border(BorderProp::Sides(vec![Side::Left]));
    root.set_flex_shrink(Some(0.0));
    root.set_padding(Edge::Top, Some(Value::Point(1.0)));
    root.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
    root.set_padding(Edge::Left, Some(Value::Point(2.0)));
    root.set_padding(Edge::Right, Some(Value::Point(2.0)));
    root.set_gap(2, Some(Value::Point(1.0)));
    bind_always(
        move || theme.with(|t| if hover.get() { rgba(t.decrease(t.background.raised.base, 1.0)) } else { rgba(t.background.raised.base) }),
        move |c| root.set_background_color(Some(c)),
    );
    root.set_custom_border_chars(Some(crate::permission::split_border_chars()));
    bind_always(move || rgba(theme.with(|t| t.background.base)), move |c| root.set_border_color(Some(c)));
    {
        let on_click = on_click.clone();
        root.set_on_mouse(
            Some(MouseEventType::Over),
            Some(Rc::new(move |_| {
                if on_click.as_ref().and_then(|f| f()).is_some() {
                    set_hover.set(true);
                }
            })),
        );
    }
    root.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| set_hover.set(false))));
    {
        let on_click = on_click.clone();
        root.set_on_mouse(
            Some(MouseEventType::Up),
            Some(Rc::new(move |_| {
                if let Some(f) = on_click.as_ref().and_then(|f| f()) {
                    f();
                }
            })),
        );
    }
    let spinning = props.spinner.clone().unwrap_or_else(|| Rc::new(|| false));
    let title_view: Rc<dyn Fn() -> View> = {
        let title = props.title.clone();
        let spinning = spinning.clone();
        let header_color = header_color.clone();
        let spinner_color = spinner_color.clone();
        Rc::new(move || {
            let Some(title) = title.clone() else { return View::Empty };
            let spinning = spinning.clone();
            let header_color = header_color.clone();
            let spinner_color = spinner_color.clone();
            show_value(
                move || title(),
                move |title| {
                    let spinning = spinning.clone();
                    let header_color = header_color.clone();
                    let spinner_color = spinner_color.clone();
                    let plain = Rc::new(move || {
                        let el = text();
                        let header_color = header_color.clone();
                        bind_always(move || header_color(), move |c| el.set_fg(Some(c)));
                        insert(el, View::dynamic(move || View::from(title.get().unwrap_or_default())), At::Only);
                        View::Node(el)
                    });
                    show(
                        move || spinning(),
                        move || {
                            let spinner_color = spinner_color.clone();
                            spinner(SpinnerProps {
                                children: Some(Rc::new(move || View::from(title.get().unwrap_or_default().trim_start_matches("# ").to_string()))),
                                color: Some(Rc::new(move || spinner_color())),
                            })
                        },
                        Some(plain),
                    )
                },
                None,
            )
        })
    };
    let path = props.path.clone();
    let header = match path {
        None => title_view(),
        Some(path) => {
            let spinning = spinning.clone();
            let header_color = header_color.clone();
            let spinner_color = spinner_color.clone();
            let ctx = ctx.clone();
            show_value(
                move || path(),
                move |path| {
                    let row = box_();
                    row.set_flex_direction(FlexDirection::Row);
                    row.set_gap(2, Some(Value::Point(1.0)));
                    row.set_min_width(Some(Value::Point(0.0)));
                    let label = move || path.with(|p| p.as_ref().map(|p| p.0.clone()).unwrap_or_default());
                    let value = move || path.with(|p| p.as_ref().map(|p| p.1.clone()).unwrap_or_default());
                    {
                        let header_color = header_color.clone();
                        let spinner_color = spinner_color.clone();
                        let spinning = spinning.clone();
                        insert(
                            row,
                            show(
                                move || spinning(),
                                move || {
                                    let spinner_color = spinner_color.clone();
                                    spinner(SpinnerProps {
                                        children: Some(Rc::new(move || View::from(label().trim_start_matches("# ").to_string()))),
                                        color: Some(Rc::new(move || spinner_color())),
                                    })
                                },
                                Some(Rc::new(move || {
                                    let el = text();
                                    el.set_flex_shrink(Some(0.0));
                                    let header_color = header_color.clone();
                                    bind_always(move || header_color(), move |c| el.set_fg(Some(c)));
                                    insert(el, View::dynamic(move || View::from(label())), At::Only);
                                    View::Node(el)
                                })),
                            ),
                            At::End,
                        );
                    }
                    let width = ctx.width.clone();
                    let spinning = spinning.clone();
                    insert(
                        row,
                        file_path(
                            Rc::new(value),
                            Rc::new(move || {
                                2.max(width() as i64 - 6 - string_width(&label()) as i64 - if spinning() { 2 } else { 0 })
                            }),
                            header_color.clone(),
                        ),
                        At::End,
                    );
                    View::Node(row)
                },
                Some(title_view.clone()),
            )
        }
    };
    insert(root, header, At::End);
    if let Some(children) = props.children.clone() {
        insert(root, View::dynamic(move || children()), At::End);
    }
    let error_color = props.error_color.clone();
    insert(
        root,
        show(
            move || error.with(|e| !e.is_empty()),
            move || {
                let el = text();
                let error_color = error_color.clone();
                bind_always(
                    move || error_color.as_ref().and_then(|c| c()).unwrap_or_else(|| rgba(theme.with(|t| t.text.feedback.error.base))),
                    move |c| el.set_fg(Some(c)),
                );
                insert(el, View::dynamic(move || View::from(error.get())), At::Only);
                View::Node(el)
            },
            None,
        ),
        At::End,
    );
    View::Node(root)
}

// ---- the generic tool ---------------------------------------------------------------------------------------

/// `genericToolSummary(tool, input)`.
pub fn generic_tool_summary(tool: &str, input: &Json) -> String {
    let args = collapse_whitespace(&primitive_input_summary(input, &[]));
    if args.is_empty() { tool.to_string() } else { format!("{tool} {args}") }
}

/// The expanded `key: value` lines of an input (and its output).
fn key_value_rows(entries: Rc<dyn Fn() -> Vec<(String, Json)>>, extra: Option<Rc<dyn Fn() -> String>>) -> View {
    let theme = tui().theme;
    let row = move |key: String, value: Rc<dyn Fn() -> String>| {
        let el = box_();
        el.set_flex_direction(FlexDirection::Row);
        let name = text();
        insert_node(el, name, None);
        name.set_flex_shrink(Some(0.0));
        bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| name.set_fg(Some(c)));
        insert(name, View::from(format!("{key}: ")), At::Only);
        let body = text();
        insert_node(el, body, None);
        body.set_flex_grow(Some(1.0));
        body.set_wrap_mode(WrapMode::Word);
        bind_always(move || rgba(theme.with(|t| t.text.base)), move |c| body.set_fg(Some(c)));
        insert(body, View::dynamic(move || View::from(value())), At::Only);
        View::Node(el)
    };
    let list = for_each(
        move || entries(),
        |(key, _): &(String, Json)| key.clone(),
        move |item, _| row(item.get_untracked().0, Rc::new(move || item.with(|(_, v)| display_value(v)))),
        None,
    );
    match extra {
        None => list,
        Some(extra) => {
            let output = {
                let when_extra = extra.clone();
                show(move || !when_extra().is_empty(), move || {
                    let extra = extra.clone();
                    row("output".into(), Rc::new(move || extra()))
                }, None)
            };
            View::list([list, output])
        }
    }
}

fn input_entries(input: Memo<Json>) -> Rc<dyn Fn() -> Vec<(String, Json)>> {
    Rc::new(move || input.with(|i| i.as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default()))
}

/// `<GenericTool>`.
fn generic_tool(props: ToolProps) -> View {
    let output = {
        let out = props.output;
        create_memo(move || strip_ansi(out.with(|o| o.as_deref().map(|o| opencode_tui_sync::js::trim(o).to_string()).unwrap_or_default()).as_str()))
    };
    let input = props.input;
    let (expanded, set_expanded) = create_signal(false);
    let expandable = create_memo(move || input.with(|i| i.as_object().is_some_and(|o| !o.is_empty())) || output.with(|o| !o.is_empty()));
    let status = props.status;
    let loading = create_memo(move || matches!(status.get().as_str(), "streaming" | "running"));
    let tool = props.tool;
    let row = inline_tool(InlineToolProps {
        icon: Rc::new(move || if status.get() == "error" { "✗".into() } else { "✓".into() }),
        color: None,
        complete: Rc::new(move || status.get() == "completed"),
        pending: tool.get_untracked(),
        spinner: Some(Rc::new(move || loading.get())),
        running: None,
        status: None,
        children: Rc::new(move || View::from(generic_tool_summary(&tool.get(), &input.get()))),
        part: props.part.clone(),
        on_click: Some(Rc::new(move || {
            if expandable.get_untracked() {
                set_expanded.update(|v| *v = !*v);
            }
        })),
        on_error_click: None,
    });
    let details = show(
        move || expanded.get(),
        move || {
            let el = box_();
            el.set_padding(Edge::Left, Some(Value::Point(3.0 + INLINE_TOOL_ICON_WIDTH)));
            insert(el, key_value_rows(input_entries(input), Some(Rc::new(move || output.get()))), At::Only);
            View::Node(el)
        },
        None,
    );
    View::list([row, details])
}

// ---- shell --------------------------------------------------------------------------------------------------

const SHELL_DISPLAY_LIMIT: usize = 1024 * 1024;

/// `<Shell>`.
fn shell(props: ToolProps) -> View {
    let shell_id = props.metadata_str("shellID");
    let metadata = props.metadata;
    let status = props.status;
    let output = props.output;
    let shell_for_output = shell_id.clone();
    shell_display(ShellDisplayProps {
        part: Some(props.part.clone()),
        shell_id: Rc::new(shell_id),
        command: Rc::new(props.input_str("command")),
        workdir: Rc::new(props.input_str("workdir")),
        status: Rc::new(move || status.get()),
        background: Rc::new(move || status.get() == "completed" && metadata.with(|m| m.str("status") == Some("running"))),
        output: Rc::new(move || if shell_for_output().is_some() { None } else { output.get() }),
        error: None,
    })
}

/// `ShellDisplay`'s props.
pub struct ShellDisplayProps {
    pub part: Option<PartFn>,
    pub shell_id: Rc<dyn Fn() -> Option<String>>,
    pub command: Rc<dyn Fn() -> Option<String>>,
    pub workdir: Rc<dyn Fn() -> Option<String>>,
    pub status: Rc<dyn Fn() -> String>,
    pub background: Rc<dyn Fn() -> bool>,
    pub output: Rc<dyn Fn() -> Option<String>>,
    pub error: Option<Rc<dyn Fn() -> Option<String>>>,
}

/// `<ShellDisplay>` (also the transcript's shell messages).
pub fn shell_display(props: ShellDisplayProps) -> View {
    let t = tui();
    let theme = t.theme;
    let ctx = use_ctx();
    let store = t.data.store.clone();
    let client = t.data.client_context.clone();
    let path_formatter = use_path_formatter();
    // A session can move while its shell is still running in the original location.
    let location = {
        let id = (props.shell_id)().unwrap_or_default();
        store
            .shell
            .get(&id)
            .and_then(|s| s.get("location").cloned())
            .or_else(|| store.session.get(&ctx.session_id).and_then(|s| s.get("location").cloned()))
    };
    let permission = use_tool_permission(props.part.clone());
    let color = create_memo_equals(
        move || theme.with(|t| if permission.get() { rgba(t.text.feedback.warning.base) } else { rgba(t.text.base) }),
        None,
    );
    let background_running = {
        let shell_id = props.shell_id.clone();
        let store = store.clone();
        create_memo(move || shell_id().is_some_and(|id| store.shell.get(&id).is_some()))
    };
    let status = props.status.clone();
    let is_running = {
        let status = status.clone();
        create_memo(move || status() == "running" || background_running.get())
    };
    let workdir = {
        let workdir = props.workdir.clone();
        let path_formatter = path_formatter.clone();
        create_memo(move || path_formatter.format(workdir().as_deref()))
    };
    let (expanded, set_expanded) = create_signal(false);
    let (background_output, set_background_output) = create_signal(String::new());
    let (output_truncated, set_output_truncated) = create_signal(false);
    struct Loader {
        loading: Cell<bool>,
        drain_requested: Cell<bool>,
        cursor: Cell<f64>,
        was_running: Cell<bool>,
    }
    let loader = Rc::new(Loader { loading: Cell::new(false), drain_requested: Cell::new(false), cursor: Cell::new(0.0), was_running: Cell::new(false) });
    type Load = Rc<RefCell<Option<Rc<dyn Fn(bool)>>>>;
    let load_background_output: Load = Rc::new(RefCell::new(None));
    {
        let loader = loader.clone();
        let status = status.clone();
        let output = props.output.clone();
        let shell_id = props.shell_id.clone();
        let this = load_background_output.clone();
        *load_background_output.borrow_mut() = Some(Rc::new(move |drain: bool| {
            if status() == "completed" && output().is_some() {
                return;
            }
            let Some(id) = shell_id() else { return };
            if loader.loading.get() {
                if drain {
                    loader.drain_requested.set(true);
                }
                return;
            }
            loader.loading.set(true);
            let loader = loader.clone();
            let client = client.clone();
            let location = location.clone();
            let this = this.clone();
            tokio::task::spawn_local(async move {
                loop {
                    let mut input = opencode_tui_sync::obj! {
                        "id" => id.as_str(),
                        "cursor" => loader.cursor.get(),
                        "limit" => SHELL_DISPLAY_LIMIT as f64,
                    };
                    if let Some(directory) = location.as_ref().and_then(|l| l.str("directory")) {
                        input.set("location", opencode_tui_sync::obj! { "directory" => directory });
                    }
                    let response = client
                        .api()
                        .call_named("shell.output", &input, opencode_tui_sync::client::RequestOptions::default())
                        .await
                        .ok()
                        .map(|r| r.json());
                    let Some(response) = response else { break };
                    let data = response.get("data").cloned().unwrap_or(Json::Null);
                    if let Some(chunk) = data.str("output").filter(|o| !o.is_empty()) {
                        let chunk = chunk.to_string();
                        set_background_output.update(|output| {
                            let next = strip_ansi(&(output.clone() + &chunk));
                            let units = opencode_tui_sync::js::length(&next);
                            if units <= SHELL_DISPLAY_LIMIT {
                                *output = next;
                                return;
                            }
                            set_output_truncated.set(true);
                            let chars: Vec<char> = next.chars().collect();
                            *output = chars[chars.len().saturating_sub(SHELL_DISPLAY_LIMIT)..].iter().collect();
                        });
                    }
                    let next_cursor = data.num("cursor").unwrap_or(0.0);
                    if next_cursor <= loader.cursor.get() {
                        break;
                    }
                    loader.cursor.set(next_cursor);
                    let size = data.num("size").unwrap_or(0.0);
                    if !drain || loader.cursor.get() >= size {
                        break;
                    }
                    let tail = loader.cursor.get().max(size - SHELL_DISPLAY_LIMIT as f64);
                    if tail > loader.cursor.get() {
                        loader.cursor.set(tail);
                        set_output_truncated.set(true);
                    }
                }
                loader.loading.set(false);
                if loader.drain_requested.replace(false) {
                    if let Some(load) = this.borrow().clone() {
                        load(true);
                    }
                }
            });
        }));
    }
    let load = {
        let load_background_output = load_background_output.clone();
        move |drain: bool| {
            if let Some(load) = load_background_output.borrow().clone() {
                load(drain);
            }
        }
    };
    {
        let loader = loader.clone();
        let load = load.clone();
        let background = props.background.clone();
        create_effect(move || {
            let running = background_running.get();
            if !running {
                if loader.was_running.get() {
                    load(true);
                }
                loader.was_running.set(false);
                return;
            }
            loader.was_running.set(true);
            if background() && !expanded.get() {
                return;
            }
            load(false);
            let load = load.clone();
            let interval = Rc::new(Cell::new(0u64));
            fn tick(interval: Rc<Cell<u64>>, load: Rc<dyn Fn()>) {
                let next = interval.clone();
                let again = load.clone();
                interval.set(renderer::set_timeout(1_000, move || {
                    again();
                    tick(next, again.clone());
                }));
            }
            tick(interval.clone(), Rc::new(move || load(false)));
            on_cleanup(move || renderer::clear_timeout(interval.get()));
        });
    }
    let output = {
        let status = status.clone();
        let shell_id = props.shell_id.clone();
        let background = props.background.clone();
        let output = props.output.clone();
        create_memo(move || {
            if status() == "streaming" {
                return String::new();
            }
            let trim = |s: &str| opencode_tui_sync::js::trim(s).to_string();
            if shell_id().is_some() {
                if background() && !expanded.get() {
                    return String::new();
                }
                if status() == "completed" {
                    if let Some(output) = output() {
                        return strip_ansi(&trim(&output));
                    }
                }
                let source = background_output.get();
                let source = if source.is_empty() { output().unwrap_or_default() } else { source };
                let text = strip_ansi(&trim(&source));
                return if output_truncated.get() { format!("[earlier output omitted]\n{text}") } else { text };
            }
            strip_ansi(&trim(&output().unwrap_or_default()))
        })
    };
    let max_lines = 10usize;
    let max_chars = {
        let width = ctx.width.clone();
        create_memo(move || max_lines * 20.max(width() - 8 - if is_running.get() { 2 } else { 0 }) as usize)
    };
    let prefix = create_memo(move || {
        let workdir = workdir.get();
        if !workdir.is_empty() && workdir != "." { format!("cd {workdir} && ") } else { String::new() }
    });
    let input = {
        let command = props.command.clone();
        create_memo(move || match command() {
            Some(command) if !command.is_empty() => format!("{}{}{command}", if is_running.get() { "" } else { "$ " }, prefix.get()),
            _ => String::new(),
        })
    };
    let collapsed = create_memo(move || collapse_shell_output(&input.get(), &output.get(), max_lines, max_chars.get()));
    let limited_input = create_memo(move || if expanded.get() { input.get() } else { collapsed.with(|c| c.input.clone()) });
    let limited_output = create_memo(move || if expanded.get() { output.get() } else { collapsed.with(|c| c.output.clone()) });
    let expandable = {
        let shell_id = props.shell_id.clone();
        create_memo(move || shell_id().is_some() || collapsed.with(|c| c.overflow))
    };
    let toggle: Rc<dyn Fn()> = {
        let load = load.clone();
        Rc::new(move || {
            let next = !expanded.get_untracked();
            set_expanded.set(next);
            if next {
                load(!background_running.get_untracked());
            }
        })
    };

    let command_view = {
        let command = props.command.clone();
        let status = status.clone();
        let background = props.background.clone();
        move || {
            let body = box_();
            body.set_gap(2, Some(Value::Point(1.0)));
            let has_command = {
                let command = command.clone();
                move || command().is_some_and(|c| !c.is_empty())
            };
            let status = status.clone();
            insert(
                body,
                show(
                    has_command,
                    move || {
                        let running = show(
                            move || is_running.get(),
                            move || {
                                let row = box_();
                                row.set_flex_direction(FlexDirection::Row);
                                row.set_gap(2, Some(Value::Point(1.0)));
                                insert(row, spinner(SpinnerProps { children: None, color: Some(Rc::new(move || color.get())) }), At::End);
                                let el = text();
                                insert_node(row, el, None);
                                bind_always(move || color.get(), move |c| el.set_fg(Some(c)));
                                bind(move || expanded.get(), move |e| {
                                    el.set_wrap_mode(if e { WrapMode::Word } else { WrapMode::Char });
                                    el.set_max_height(if e { None } else { Some(Value::Point(2.0)) });
                                });
                                el.set_flex_grow(Some(1.0));
                                el.set_min_width(Some(Value::Point(0.0)));
                                insert(el, View::dynamic(move || View::from(limited_input.get())), At::Only);
                                View::Node(row)
                            },
                            Some(Rc::new(move || {
                                let el = text();
                                bind_always(move || rgba(theme.with(|t| t.text.base)), move |c| el.set_fg(Some(c)));
                                bind(move || expanded.get(), move |e| {
                                    el.set_wrap_mode(if e { WrapMode::Word } else { WrapMode::Char });
                                    el.set_max_height(if e { None } else { Some(Value::Point(2.0)) });
                                });
                                insert(el, View::dynamic(move || View::from(limited_input.get())), At::Only);
                                View::Node(el)
                            })),
                        );
                        let output = show(
                            move || !limited_output.with(String::is_empty),
                            move || {
                                let el = text();
                                bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| el.set_fg(Some(c)));
                                insert(el, View::dynamic(move || View::from(limited_output.get())), At::Only);
                                View::Node(el)
                            },
                            None,
                        );
                        View::list([running, output])
                    },
                    Some(Rc::new(move || {
                        let status = status.clone();
                        memo_view(move || {
                            if is_running.get() || status() == "streaming" {
                                spinner(SpinnerProps { children: Some(Rc::new(|| txt("Writing command…"))), color: Some(Rc::new(move || color.get())) })
                            } else {
                                let el = text();
                                bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| el.set_fg(Some(c)));
                                insert(el, txt("Writing command…"), At::Only);
                                View::Node(el)
                            }
                        })
                    })),
                ),
                At::End,
            );
            let background = background.clone();
            insert(body, show(move || background(), || status_badge("Background", true), None), At::End);
            View::Node(body)
        }
    };
    block_tool(BlockToolProps {
        part: props.part.clone(),
        error: props.error.clone(),
        on_click: Some(Rc::new(move || if expandable.get() { Some(toggle.clone()) } else { None })),
        children: Some(Rc::new(command_view)),
        ..BlockToolProps::default()
    })
}

// ---- write, glob, read, grep, webfetch, websearch -----------------------------------------------------------

/// `<Write>`.
fn write(props: ToolProps) -> View {
    let theme = tui().theme;
    let path_formatter = use_path_formatter();
    let status = props.status;
    let path = props.input_str("path");
    let code = {
        let input = props.input;
        create_memo(move || input.with(|i| i.str("content").unwrap_or("").to_string()))
    };
    let completed = {
        let props = props.clone();
        let path = path.clone();
        let path_formatter = path_formatter.clone();
        move || {
            let metadata = props.metadata;
            let path_for_diag = path.clone();
            let path_formatter = path_formatter.clone();
            let path = path.clone();
            block_tool(BlockToolProps {
                path: Some(Rc::new(move || Some(("# Wrote".to_string(), path_formatter.format(path().as_deref()))))),
                part: Some(props.part.clone()),
                children: Some(Rc::new(move || {
                    let ln = line_number();
                    ln.set_line_number_fg(Some(rgba(theme.with_untracked(|t| t.text.muted))));
                    bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| ln.set_line_number_fg(Some(c)));
                    ln.set_min_width(Some(Value::Point(3.0)));
                    ln.set_padding(Edge::Right, Some(Value::Point(1.0)));
                    // TODO(port-tui): `<code conceal={false} filetype syntaxStyle>` once Code lands.
                    let body = text();
                    bind_always(move || rgba(theme.with(|t| t.text.base)), move |c| body.set_fg(Some(c)));
                    bind(move || code.get(), move |c| body.set_content(&c));
                    let _ = filetype(path_for_diag().as_deref());
                    insert_node(ln, body, None);
                    let path_for_diag = path_for_diag.clone();
                    View::list([View::Node(ln), diagnostics(Rc::new(move || metadata.with(|m| m.get("diagnostics").cloned())), Rc::new(move || path_for_diag().unwrap_or_default()))])
                })),
                ..BlockToolProps::default()
            })
        }
    };
    let fallback = {
        let props = props.clone();
        Rc::new(move || {
            let path = path.clone();
            let complete_path = path.clone();
            let path_formatter = path_formatter.clone();
            inline_tool(InlineToolProps {
                icon: Rc::new(|| "←".into()),
                color: None,
                complete: Rc::new(move || complete_path().is_some_and(|p| !p.is_empty())),
                pending: "Preparing write…".into(),
                spinner: None,
                running: None,
                status: None,
                children: Rc::new(move || View::from(format!("Write {}", path_formatter.format(path().as_deref())))),
                part: props.part.clone(),
                on_click: None,
                on_error_click: None,
            })
        }) as Rc<dyn Fn() -> View>
    };
    switch(vec![when(move || status.get() == "completed", completed)], Some(fallback))
}

fn truthy_str(value: Option<String>) -> bool {
    value.is_some_and(|v| !v.is_empty())
}

/// `<Glob>` and `<Grep>` (they differ in icon text, pending label and count key).
fn search(props: ToolProps, name: &'static str, pending: &'static str, count_key: &'static str) -> View {
    let path_formatter = use_path_formatter();
    let pattern = props.input_str("pattern");
    let path = props.input_str("path");
    let metadata = props.metadata;
    let complete_pattern = pattern.clone();
    inline_tool(InlineToolProps {
        icon: Rc::new(|| "✱".into()),
        color: None,
        complete: Rc::new(move || truthy_str(complete_pattern())),
        pending: pending.into(),
        spinner: None,
        running: None,
        status: None,
        children: Rc::new(move || {
            let mut out = format!("{name} \"{}\" ", pattern().unwrap_or_default());
            if let Some(path) = path().filter(|p| !p.is_empty()) {
                out.push_str(&format!("in {} ", path_formatter.format(Some(&path))));
            }
            if let Some(count) = metadata.with(|m| finite_number(m.get(count_key))).filter(|c| *c != 0.0) {
                out.push_str(&format!("({} {})", opencode_tui_sync::js::number(count), if count == 1.0 { "match" } else { "matches" }));
            }
            View::from(out)
        }),
        part: props.part.clone(),
        on_click: None,
        on_error_click: None,
    })
}

fn glob(props: ToolProps) -> View {
    search(props, "Glob", "Finding files…", "count")
}

fn grep(props: ToolProps) -> View {
    search(props, "Grep", "Searching content…", "matches")
}

/// `<Read>`.
fn read(props: ToolProps) -> View {
    let theme = tui().theme;
    let path_formatter = use_path_formatter();
    let status = props.status;
    let is_running = create_memo(move || status.get() == "running");
    let metadata = props.metadata;
    let loaded = create_memo(move || {
        if status.get() != "completed" {
            return Vec::new();
        }
        metadata.with(|m| match m.get("loaded") {
            Some(Json::Array(items)) => items.iter().filter_map(|p| p.str_opt().map(str::to_string)).collect(),
            _ => Vec::new(),
        })
    });
    let input = props.input;
    let path = props.input_str("path");
    let complete_path = path.clone();
    let row = {
        let path_formatter = path_formatter.clone();
        inline_tool(InlineToolProps {
            icon: Rc::new(|| "→".into()),
            color: None,
            complete: Rc::new(move || truthy_str(complete_path())),
            pending: "Reading file…".into(),
            spinner: Some(Rc::new(move || is_running.get())),
            running: None,
            status: None,
            children: Rc::new(move || {
                let mut out = format!("Read {}", path_formatter.format(path().as_deref()));
                let (offset, limit) = input.with(|i| (i.get("offset").cloned(), i.get("limit").cloned()));
                if offset.is_some() || limit.is_some() {
                    let offset_n = finite_number(offset.as_ref()).filter(|n| *n != 0.0).unwrap_or(1.0);
                    let end = match &limit {
                        Some(l) if opencode_tui_sync::json::truthy(l) => {
                            opencode_tui_sync::js::number(offset_n + finite_number(Some(l)).filter(|n| *n != 0.0).unwrap_or(0.0) - 1.0)
                        }
                        _ => String::new(),
                    };
                    out.push_str(&format!(":{}-{end}", opencode_tui_sync::js::number(offset_n)));
                }
                View::from(out)
            }),
            part: props.part.clone(),
            on_click: None,
            on_error_click: None,
        })
    };
    let loaded_rows = for_each(
        move || loaded.get(),
        |p: &String| p.clone(),
        move |filepath, _| {
            let outer = box_();
            outer.set_padding(Edge::Left, Some(Value::Point(3.0)));
            let el = text();
            insert_node(outer, el, None);
            el.set_padding(Edge::Left, Some(Value::Point(3.0)));
            bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| el.set_fg(Some(c)));
            let path_formatter = path_formatter.clone();
            insert(el, View::dynamic(move || View::from(format!("↳ Loaded {}", path_formatter.format(Some(&filepath.get()))))), At::Only);
            View::Node(outer)
        },
        None,
    );
    View::list([row, loaded_rows])
}

/// `<WebFetch>`.
fn webfetch(props: ToolProps) -> View {
    let url = props.input_str("url");
    let complete_url = url.clone();
    inline_tool(InlineToolProps {
        icon: Rc::new(|| "%".into()),
        color: None,
        complete: Rc::new(move || truthy_str(complete_url())),
        pending: "Fetching from the web…".into(),
        spinner: None,
        running: None,
        status: None,
        children: Rc::new(move || View::from(format!("WebFetch {}", url().unwrap_or_default()))),
        part: props.part.clone(),
        on_click: None,
        on_error_click: None,
    })
}

/// `<WebSearch>`.
fn websearch(props: ToolProps) -> View {
    let provider = props.metadata_str("provider");
    let query = props.input_str("query");
    let complete_query = query.clone();
    inline_tool(InlineToolProps {
        icon: Rc::new(|| "◈".into()),
        color: None,
        complete: Rc::new(move || truthy_str(complete_query())),
        pending: "Searching web…".into(),
        spinner: None,
        running: None,
        status: None,
        children: Rc::new(move || {
            // TODO(port-tui): RetryProvider's provider-name transition (animations); the name is static here.
            let label = match provider().filter(|p| !p.is_empty()) {
                Some(p) => format!("Web Search via {}", web_search_provider_name(Some(&Json::String(p)))),
                None => "Web Search".into(),
            };
            View::from(format!("{label} \"{}\"", query().unwrap_or_default()))
        }),
        part: props.part.clone(),
        on_click: None,
        on_error_click: None,
    })
}

// ---- subagent and execute -----------------------------------------------------------------------------------

/// `subagentModelLabel(value, models)`.
pub fn subagent_model_label(value: Option<&str>, models: &[Rc<Json>]) -> Option<String> {
    let value = value.filter(|v| !v.is_empty())?;
    let (reference, variant) = match value.split_once('#') {
        Some((r, v)) => (r, Some(v.split('#').next().unwrap_or(v))),
        None => (value, None),
    };
    let (provider_id, model_id) = match reference.find('/') {
        Some(i) => (&reference[..i], &reference[i + 1..]),
        None => ("", reference),
    };
    let name = models
        .iter()
        .find(|m| m.str("providerID") == Some(provider_id) && m.str("id") == Some(model_id))
        .and_then(|m| m.str("name"))
        .map(str::to_string);
    let variant = variant.filter(|v| !v.is_empty()).map(|v| format!(" ({v})")).unwrap_or_default();
    Some(format!("{}{variant}", name.unwrap_or_else(|| reference.to_string())))
}

/// `isBackgroundSubagent(metadata, status)`.
pub fn is_background_subagent(metadata: &Json, status: &str) -> bool {
    status == "completed" && metadata.str("status") == Some("running")
}

/// `<Subagent>`.
fn subagent(props: ToolProps) -> View {
    let t = tui();
    let route = t.route;
    let store = t.data.store.clone();
    let ctx = use_ctx();
    let status = props.status;
    let metadata = props.metadata;
    let input = props.input;
    let session_id = create_memo(move || {
        metadata
            .with(|m| m.str("sessionID").or(m.str("sessionId")).map(str::to_string))
            // A failed continuation may name an unrelated session, such as the parent itself.
            .or_else(|| if status.get() == "error" { None } else { input.with(|i| i.str("sessionID").map(str::to_string)) })
    });
    let description = props.input_str("description");
    let continuation = create_memo(move || input.with(|i| i.str("sessionID").is_some()));
    let model = {
        let models = ctx.models.clone();
        create_memo(move || subagent_model_label(input.with(|i| i.str("model").map(str::to_string)).as_deref(), &models()))
    };
    let is_running = create_memo(move || {
        status.get() == "running" || session_id.get().is_some_and(|id| store.session.status(&id) == SessionStatus::Running)
    });
    let complete_description = description.clone();
    inline_tool(InlineToolProps {
        icon: Rc::new(move || {
            if is_running.get() {
                "│"
            } else if continuation.get() {
                "↳"
            } else if status.get() == "completed" {
                "✓"
            } else {
                "│"
            }
            .to_string()
        }),
        color: None,
        complete: Rc::new(move || truthy_str(complete_description())),
        pending: "Delegating…".into(),
        spinner: Some(Rc::new(move || is_running.get())),
        running: Some(Rc::new(move || is_running.get())),
        status: Some(Rc::new(move || {
            if metadata.with(|m| is_background_subagent(m, &status.get())) {
                status_badge("Background", false)
            } else {
                View::Empty
            }
        })),
        children: Rc::new(move || {
            let head = if continuation.get() {
                "Continue subagent".to_string()
            } else {
                let agent = input.with(|i| i.str("agent").or(i.str("subagent_type")).unwrap_or("General").to_string());
                format!("{} Subagent", titlecase(&agent))
            };
            let model = model.get().map(|m| format!(" · {m}")).unwrap_or_default();
            View::from(format!("{head} — {}{model}", description().unwrap_or_else(|| "Subagent".into())))
        }),
        part: props.part.clone(),
        on_click: Some(Rc::new(move || {
            if let Some(id) = session_id.get_untracked() {
                route.set(Route::Session { session_id: id });
            }
        })),
        on_error_click: None,
    })
}

/// `<ExecuteCallView call>`.
fn execute_call_view(call: Signal<ExecuteCall>) -> View {
    let theme = tui().theme;
    let (expanded, set_expanded) = create_signal(false);
    let (hover, set_hover) = create_signal(false);
    let input = create_memo(move || {
        call.with(|c| c.input.as_ref().and_then(|i| i.as_object()).map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<Vec<_>>()).unwrap_or_default())
    });
    let expandable = create_memo(move || input.with(|i| !i.is_empty()));
    let expanded_color = create_memo_equals(move || theme.with(|t| rgba(t.decrease(t.text.muted, 1.0))), None);
    let color = create_memo_equals(
        move || {
            if call.with(|c| c.status == "error") {
                return theme.with(|t| rgba(t.text.feedback.error.base));
            }
            if hover.get() {
                return theme.with(|t| rgba(t.text.base));
            }
            if expanded.get() { expanded_color.get() } else { theme.with(|t| rgba(t.text.muted)) }
        },
        None,
    );
    let root = box_();
    root.set_padding(Edge::Left, Some(Value::Point(3.0)));
    root.set_on_mouse(
        Some(MouseEventType::Over),
        Some(Rc::new(move |_| {
            if expandable.get_untracked() {
                set_hover.set(true);
            }
        })),
    );
    root.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| set_hover.set(false))));
    root.set_on_mouse(
        Some(MouseEventType::Up),
        Some(Rc::new(move |_| {
            if !expandable.get_untracked() {
                return;
            }
            set_expanded.update(|v| *v = !*v);
        })),
    );
    let row = box_();
    insert_node(root, row, None);
    row.set_flex_direction(FlexDirection::Row);
    let icon_box = box_();
    insert_node(row, icon_box, None);
    icon_box.set_width(Some(Value::Point(INLINE_TOOL_ICON_WIDTH)));
    icon_box.set_flex_shrink(Some(0.0));
    let icon = text();
    insert_node(icon_box, icon, None);
    bind_always(move || color.get(), move |c| icon.set_fg(Some(c)));
    insert(icon, View::dynamic(move || txt(if call.with(|c| c.status == "error") { "✗" } else { "›" })), At::Only);
    let label = text();
    insert_node(row, label, None);
    label.set_flex_grow(Some(1.0));
    label.set_wrap_mode(WrapMode::None);
    label.set_truncate(true);
    bind_always(move || color.get(), move |c| label.set_fg(Some(c)));
    insert(
        label,
        View::dynamic(move || View::from(if expanded.get() { call.with(|c| c.tool.clone()) } else { call.with(execute_call_summary) })),
        At::Only,
    );
    insert(
        root,
        show(
            move || expanded.get(),
            move || {
                let el = box_();
                el.set_padding(Edge::Left, Some(Value::Point(1.0)));
                el.set_border(BorderProp::Sides(vec![Side::Left]));
                bind_always(move || expanded_color.get(), move |c| el.set_border_color(Some(c)));
                insert(el, key_value_rows(Rc::new(move || input.get()), None), At::Only);
                View::Node(el)
            },
            None,
        ),
        At::End,
    );
    View::Node(root)
}

/// `<Execute>`: Code Mode's nested tool calls stream through metadata.
fn execute(props: ToolProps) -> View {
    let theme = tui().theme;
    let ctx = use_ctx();
    let status = props.status;
    let metadata = props.metadata;
    let is_loading = create_memo(move || matches!(status.get().as_str(), "streaming" | "running"));
    let calls = create_memo(move || metadata.with(|m| execute_calls(m.get("toolCalls"))));
    let output = {
        let out = props.output;
        create_memo(move || strip_ansi(&out.with(|o| o.as_deref().map(|o| opencode_tui_sync::js::trim(o).to_string()).unwrap_or_default())))
    };
    let has_runtime_error = create_memo(move || metadata.with(|m| m.get("error") == Some(&Json::Bool(true))) || status.get() == "error");
    let output_preview = {
        let width = ctx.width.clone();
        create_memo(move || collapse_tool_output(&output.get(), 4, 4 * 20.max(width() - 6) as usize).output)
    };
    let show_output = create_memo(move || !output.with(String::is_empty) && has_runtime_error.get());
    // TODO(port-tui): `dialog.replace(() => <DialogExecute part />)` (the dialogs slice).
    let open_details: Rc<dyn Fn()> = Rc::new(|| {});
    let row = inline_tool(InlineToolProps {
        icon: Rc::new(move || {
            if has_runtime_error.get() {
                "✗"
            } else if status.get() == "completed" {
                "✓"
            } else {
                "│"
            }
            .to_string()
        }),
        color: Some(Rc::new(move || has_runtime_error.get().then(|| rgba(theme.with(|t| t.text.feedback.error.base))))),
        complete: Rc::new(|| true),
        pending: "execute".into(),
        spinner: Some(Rc::new(move || is_loading.get())),
        running: None,
        status: None,
        children: Rc::new(|| txt("execute")),
        part: props.part.clone(),
        on_click: Some(open_details.clone()),
        on_error_click: Some(open_details),
    });
    let call_rows = index(move || calls.get(), |call, _| execute_call_view(call), None);
    let error_rows = show(
        move || show_output.get(),
        move || {
            let el = box_();
            el.set_padding(Edge::Left, Some(Value::Point(3.0)));
            insert(
                el,
                for_each(
                    move || output_preview.get().split('\n').map(str::to_string).enumerate().collect::<Vec<_>>(),
                    |(i, line): &(usize, String)| format!("{i}:{line}"),
                    move |item, _| {
                        let line = text();
                        line.set_padding(Edge::Left, Some(Value::Point(3.0)));
                        bind_always(move || rgba(theme.with(|t| t.text.feedback.error.base)), move |c| line.set_fg(Some(c)));
                        insert(
                            line,
                            View::dynamic(move || item.with(|(i, l)| View::from(format!("{}{l}", if *i == 0 { "↳ " } else { "  " })))),
                            At::Only,
                        );
                        View::Node(line)
                    },
                    None,
                ),
                At::Only,
            );
            View::Node(el)
        },
        None,
    );
    View::list([row, call_rows, error_rows])
}

// ---- edit and patch -----------------------------------------------------------------------------------------

/// `parseApplyPatchFiles(value)`.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchFile {
    pub kind: String,
    pub relative_path: String,
    pub file_path: String,
    pub patch: String,
    pub additions: f64,
    pub deletions: f64,
    pub move_path: Option<String>,
}

pub fn parse_apply_patch_files(value: Option<&Json>) -> Vec<PatchFile> {
    let Some(Json::Array(items)) = value else { return Vec::new() };
    items
        .iter()
        .filter_map(|file| {
            if !is_record(Some(file)) {
                return None;
            }
            let status = file.str("status");
            let kind = file.str("type").map(str::to_string).or(match status {
                Some("added") => Some("add".into()),
                Some("deleted") => Some("delete".into()),
                Some("modified") => Some("update".into()),
                _ => None,
            })?;
            let relative_path = file.str("file").or(file.str("relativePath"))?.to_string();
            let file_path = file.str("filePath").map(str::to_string).unwrap_or_else(|| relative_path.clone());
            let patch = file.str("patch")?.to_string();
            let additions = finite_number(file.get("additions"))?;
            let deletions = finite_number(file.get("deletions"))?;
            if kind.is_empty() || relative_path.is_empty() || file_path.is_empty() {
                return None;
            }
            Some(PatchFile { kind, relative_path, file_path, patch, additions, deletions, move_path: file.str("movePath").map(str::to_string) })
        })
        .collect()
}

/// The diff view the config and the width choose (`view()`).
fn diff_view(ctx: &super::standin::RenderCtx) -> Rc<dyn Fn() -> DiffView> {
    let config = ctx.config.clone();
    let width = ctx.width.clone();
    Rc::new(move || match config().diffs.as_ref().and_then(|d| d.view.clone()).as_deref() {
        Some("unified") => DiffView::Unified,
        Some("split") => DiffView::Split,
        // "auto"
        _ => {
            if width() > 120 {
                DiffView::Split
            } else {
                DiffView::Unified
            }
        }
    })
}

/// A tool view's `<box paddingLeft=1 paddingRight=1><PatchDiff .../></box>`.
fn tool_patch_diff(patch: Rc<dyn Fn() -> String>, path: Rc<dyn Fn() -> Option<String>>) -> View {
    let theme = tui().theme;
    let ctx = use_ctx();
    let wrap = ctx.diff_wrap_mode.clone();
    let el = box_();
    el.set_padding(Edge::Left, Some(Value::Point(1.0)));
    el.set_padding(Edge::Right, Some(Value::Point(1.0)));
    let props = PatchDiffProps {
        diff: patch,
        hunk_fg: Rc::new(move || rgba(theme.with(|t| t.diff.text.hunk_header))),
        line_number_bg: Rc::new(move || rgba(theme.with(|t| t.diff.background.context))),
        on_ref: None,
        gutter_padding: None,
        diff_props: DiffProps::themed(
            theme,
            diff_view(&ctx),
            Rc::new(move || filetype(path().as_deref()).map(str::to_string)),
            Rc::new(move || if wrap() == DiffWrapMode::None { WrapMode::None } else { WrapMode::Word }),
        ),
    };
    insert(el, component(move || patch_diff(props)), At::Only);
    View::Node(el)
}

/// `<Edit>`.
fn edit(props: ToolProps) -> View {
    let path_formatter = use_path_formatter();
    let metadata = props.metadata;
    let file = create_memo(move || metadata.with(|m| parse_apply_patch_files(m.get("files")).into_iter().next()));
    let input_path = props.input_str("path");
    let path = {
        let input_path = input_path.clone();
        create_memo(move || file.with(|f| f.as_ref().map(|f| f.relative_path.clone())).or_else(|| input_path()))
    };
    let with_file = {
        let props = props.clone();
        let path_formatter = path_formatter.clone();
        move |item: Memo<Option<PatchFile>>| {
            let path_formatter = path_formatter.clone();
            let input_path = props.input_str("path");
            block_tool(BlockToolProps {
                path: Some(Rc::new(move || Some(("← Edit".to_string(), path_formatter.format(path.get().as_deref()))))),
                part: Some(props.part.clone()),
                children: Some(Rc::new(move || {
                    let input_path = input_path.clone();
                    View::list([
                        tool_patch_diff(
                            Rc::new(move || item.with(|f| f.as_ref().map(|f| f.patch.clone()).unwrap_or_default())),
                            Rc::new(move || path.get()),
                        ),
                        diagnostics(Rc::new(move || metadata.with(|m| m.get("diagnostics").cloned())), Rc::new(move || input_path().unwrap_or_default())),
                    ])
                })),
                ..BlockToolProps::default()
            })
        }
    };
    let status = props.status;
    let fallback = {
        let part = props.part.clone();
        Rc::new(move || {
            let path_formatter = path_formatter.clone();
            let input_path = input_path.clone();
            let title_path = input_path.clone();
            block_tool(BlockToolProps {
                path: Some(Rc::new(move || {
                    input_path().filter(|p| !p.is_empty()).map(|p| ("← Edit".to_string(), path_formatter.format(Some(&p))))
                })),
                title: Some(Rc::new(move || if truthy_str(title_path()) { None } else { Some("# Preparing edit…".into()) })),
                part: Some(part.clone()),
                spinner: Some(Rc::new(move || status.get() == "streaming")),
                ..BlockToolProps::default()
            })
        }) as Rc<dyn Fn() -> View>
    };
    show_value(move || file.get(), with_file, Some(fallback))
}

/// `<ApplyPatch>`.
fn apply_patch(props: ToolProps) -> View {
    let theme = tui().theme;
    let ctx = use_ctx();
    let path_formatter = use_path_formatter();
    let metadata = props.metadata;
    let input = props.input;
    let status = props.status;
    let files = create_memo(move || metadata.with(|m| parse_apply_patch_files(m.get("files"))));
    let targets = create_memo(move || {
        let Some(patch) = input.with(|i| i.str("patchText").map(str::to_string)) else { return Vec::new() };
        patch
            .split(['\r', '\n'])
            .filter_map(|line| {
                ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
                    .iter()
                    .find_map(|prefix| line.find(prefix).map(|i| line[i + prefix.len()..].trim().to_string()))
            })
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
    });
    let applied = create_memo(move || {
        metadata.with(|m| match m.get("applied") {
            Some(Json::Array(items)) => items
                .iter()
                .filter_map(|v| {
                    let kind = v.str("type")?.to_string();
                    let resource = v.str("resource")?.to_string();
                    (!kind.is_empty() && !resource.is_empty()).then_some((kind, resource))
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
    });
    let label_of = |kind: &str| match kind {
        "add" => "# Created",
        "delete" => "# Deleted",
        _ => "← Patched",
    };
    let files_view = {
        let props = props.clone();
        let path_formatter = path_formatter.clone();
        move || {
            let el = box_();
            el.set_flex_direction(FlexDirection::Column);
            el.set_gap(2, Some(Value::Point(1.0)));
            let props = props.clone();
            let path_formatter = path_formatter.clone();
            insert(
                el,
                for_each(
                    move || files.get(),
                    |f: &PatchFile| format!("{}:{}", f.relative_path, f.patch),
                    move |file, _| {
                        let f = file.get_untracked();
                        let path_formatter = path_formatter.clone();
                        block_tool(BlockToolProps {
                            path: Some(Rc::new(move || Some((label_of(&f.kind).to_string(), path_formatter.format(Some(&f.relative_path)))))),
                            part: Some(props.part.clone()),
                            children: Some(Rc::new(move || {
                                let f = file.get_untracked();
                                if f.kind == "delete" {
                                    let el = text();
                                    bind_always(move || rgba(theme.with(|t| t.diff.text.removed)), move |c| el.set_fg(Some(c)));
                                    let n = f.deletions;
                                    insert(el, View::from(format!("-{} line{}", opencode_tui_sync::js::number(n), if n != 1.0 { "s" } else { "" })), At::Only);
                                    return View::Node(el);
                                }
                                tool_patch_diff(
                                    Rc::new(move || file.with(|f| f.patch.clone())),
                                    Rc::new(move || Some(file.with(|f| f.relative_path.clone()))),
                                )
                            })),
                            ..BlockToolProps::default()
                        })
                    },
                    None,
                ),
                At::Only,
            );
            View::Node(el)
        }
    };
    let applied_view = {
        let props = props.clone();
        let path_formatter = path_formatter.clone();
        let ctx = ctx.clone();
        move || {
            let el = box_();
            el.set_flex_direction(FlexDirection::Column);
            el.set_gap(2, Some(Value::Point(1.0)));
            let props = props.clone();
            let path_formatter = path_formatter.clone();
            let ctx = ctx.clone();
            insert(
                el,
                for_each(
                    move || applied.get(),
                    |(k, r): &(String, String)| format!("{k}:{r}"),
                    move |file, _| {
                        let (kind, resource) = file.get_untracked();
                        let path_formatter = path_formatter.clone();
                        let width = ctx.width.clone();
                        let label = label_of(&kind).to_string();
                        let shown = path_formatter.format(Some(&resource));
                        block_tool(BlockToolProps {
                            path: Some(Rc::new(move || Some((label.clone(), shown.clone())))),
                            part: Some(props.part.clone()),
                            children: Some(Rc::new(move || {
                                let kind = kind.clone();
                                let resource = resource.clone();
                                let width = width.clone();
                                file_path(
                                    Rc::new(move || resource.clone()),
                                    Rc::new(move || 2.max(width() as i64 - 5)),
                                    Rc::new(move || theme.with(|t| if kind == "delete" { rgba(t.diff.text.removed) } else { rgba(t.text.muted) })),
                                )
                            })),
                            ..BlockToolProps::default()
                        })
                    },
                    None,
                ),
                At::Only,
            );
            View::Node(el)
        }
    };
    let fallback = {
        let part = props.part.clone();
        Rc::new(move || {
            let path_formatter = path_formatter.clone();
            block_tool(BlockToolProps {
                path: Some(Rc::new(move || {
                    targets.with(|t| {
                        (t.len() == 1).then(|| {
                            (
                                if status.get() == "error" { "# Patch failed" } else { "Patching" }.to_string(),
                                path_formatter.format(Some(&t[0])),
                            )
                        })
                    })
                })),
                title: Some(Rc::new(move || {
                    if targets.with(|t| t.len() == 1) {
                        None
                    } else {
                        Some(if status.get() == "error" { "# Patch failed" } else { "Patching" }.to_string())
                    }
                })),
                part: Some(part.clone()),
                spinner: Some(Rc::new(move || matches!(status.get().as_str(), "streaming" | "running"))),
                header_color: Some(Rc::new(move || (status.get() == "error").then(|| rgba(theme.with(|t| t.text.feedback.error.base))))),
                error_color: Some(Rc::new(move || (status.get() == "error").then(|| rgba(theme.with(|t| t.text.muted))))),
                ..BlockToolProps::default()
            })
        }) as Rc<dyn Fn() -> View>
    };
    switch(
        vec![when(move || files.with(|f| !f.is_empty()), files_view), when(move || applied.with(|a| !a.is_empty()), applied_view)],
        Some(fallback),
    )
}

// ---- todos, questions, skill, diagnostics -------------------------------------------------------------------

/// `<TodoWrite>`.
fn todo_write(props: ToolProps) -> View {
    let theme = tui().theme;
    let input = props.input;
    let status = props.status;
    let todos = create_memo(move || input.with(|i| parse_todos(i.get("todos"))));
    let list = {
        let part = props.part.clone();
        move || {
            block_tool(BlockToolProps {
                title: Some(Rc::new(|| Some("# Todos".into()))),
                part: Some(part.clone()),
                children: Some(Rc::new(move || {
                    let el = box_();
                    insert(
                        el,
                        for_each(
                            move || todos.get(),
                            |t: &super::todo::Todo| t.content.clone(),
                            move |todo, _| todo_item(theme, Rc::new(move || todo.get())),
                            None,
                        ),
                        At::Only,
                    );
                    View::Node(el)
                })),
                ..BlockToolProps::default()
            })
        }
    };
    let fallback = {
        let part = props.part.clone();
        Rc::new(move || {
            inline_tool(InlineToolProps {
                icon: Rc::new(|| "⚙".into()),
                color: None,
                complete: Rc::new(move || matches!(status.get().as_str(), "completed" | "error")),
                pending: "Updating todos…".into(),
                spinner: None,
                running: None,
                status: None,
                children: Rc::new(move || txt(if status.get() == "error" { "Todo update failed" } else { "Cleared todos" })),
                part: part.clone(),
                on_click: None,
                on_error_click: None,
            })
        }) as Rc<dyn Fn() -> View>
    };
    switch(vec![when(move || status.get() == "completed" && todos.with(|t| !t.is_empty()), list)], Some(fallback))
}

/// `parseQuestions(value)`.
pub fn parse_questions(value: Option<&Json>) -> Vec<String> {
    let Some(Json::Array(items)) = value else { return Vec::new() };
    items.iter().filter_map(|item| item.str("question").filter(|q| !q.is_empty()).map(str::to_string)).collect()
}

/// `parseQuestionAnswers(value)`.
pub fn parse_question_answers(value: Option<&Json>) -> Option<Vec<Vec<String>>> {
    let Some(Json::Array(items)) = value else { return None };
    Some(
        items
            .iter()
            .map(|answer| match answer {
                Json::Array(values) => values.iter().filter_map(|v| v.str_opt().map(str::to_string)).collect(),
                _ => Vec::new(),
            })
            .collect(),
    )
}

/// `<Question>`.
fn question(props: ToolProps) -> View {
    let theme = tui().theme;
    let input = props.input;
    let metadata = props.metadata;
    let questions = create_memo(move || input.with(|i| parse_questions(i.get("questions"))));
    let answers = create_memo(move || metadata.with(|m| parse_question_answers(m.get("answers"))));
    let count = create_memo(move || questions.with(Vec::len));
    let format = |answer: Option<Vec<String>>| match answer {
        Some(a) if !a.is_empty() => a.join(", "),
        _ => "(no answer)".into(),
    };
    let answered = {
        let part = props.part.clone();
        move || {
            block_tool(BlockToolProps {
                title: Some(Rc::new(|| Some("# Questions".into()))),
                part: Some(part.clone()),
                children: Some(Rc::new(move || {
                    let el = box_();
                    el.set_gap(2, Some(Value::Point(1.0)));
                    insert(
                        el,
                        for_each(
                            move || questions.get().into_iter().enumerate().collect::<Vec<_>>(),
                            |(i, q): &(usize, String)| format!("{i}:{q}"),
                            move |item, _| {
                                let col = box_();
                                col.set_flex_direction(FlexDirection::Column);
                                let q = text();
                                insert_node(col, q, None);
                                bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| q.set_fg(Some(c)));
                                insert(q, View::dynamic(move || View::from(item.with(|(_, q)| q.clone()))), At::Only);
                                let a = text();
                                insert_node(col, a, None);
                                bind_always(move || rgba(theme.with(|t| t.text.base)), move |c| a.set_fg(Some(c)));
                                insert(
                                    a,
                                    View::dynamic(move || {
                                        let i = item.with(|(i, _)| *i);
                                        View::from(format(answers.with(|a| a.as_ref().and_then(|a| a.get(i).cloned()))))
                                    }),
                                    At::Only,
                                );
                                View::Node(col)
                            },
                            None,
                        ),
                        At::Only,
                    );
                    View::Node(el)
                })),
                ..BlockToolProps::default()
            })
        }
    };
    let fallback = {
        let part = props.part.clone();
        Rc::new(move || {
            inline_tool(InlineToolProps {
                icon: Rc::new(|| "→".into()),
                color: None,
                complete: Rc::new(move || count.get() != 0),
                pending: "Asking questions…".into(),
                spinner: None,
                running: None,
                status: None,
                children: Rc::new(move || View::from(format!("Asked {} question{}", count.get(), if count.get() != 1 { "s" } else { "" }))),
                part: part.clone(),
                on_click: None,
                on_error_click: None,
            })
        }) as Rc<dyn Fn() -> View>
    };
    switch(vec![when(move || answers.with(Option::is_some), answered)], Some(fallback))
}

/// `<Skill>`.
fn skill(props: ToolProps) -> View {
    let metadata = props.metadata;
    let input = props.input;
    let name = create_memo(move || metadata.with(|m| m.str("name").map(str::to_string)).or_else(|| input.with(|i| i.str("id").map(str::to_string))));
    inline_tool(InlineToolProps {
        icon: Rc::new(|| "→".into()),
        color: None,
        complete: Rc::new(move || truthy_str(name.get())),
        pending: "Loading skill…".into(),
        spinner: None,
        running: None,
        status: None,
        children: Rc::new(move || View::from(format!("Skill \"{}\"", name.get().unwrap_or_default()))),
        part: props.part.clone(),
        on_click: None,
        on_error_click: None,
    })
}

/// `parseDiagnostics(value, filePath)`: up to three errors of the file.
pub fn parse_diagnostics(value: Option<&Json>, file_path: &str) -> Vec<(f64, f64, String)> {
    let Some(Json::Array(diagnostics)) = value.and_then(|v| v.get(file_path)) else { return Vec::new() };
    diagnostics
        .iter()
        .filter_map(|d| {
            let start = d.get("range").and_then(|r| r.get("start"));
            let line = finite_number(start.and_then(|s| s.get("line")))?;
            let character = finite_number(start.and_then(|s| s.get("character")))?;
            let message = d.str("message").filter(|m| !m.is_empty())?;
            (d.get("severity") == Some(&Json::Number(1.0))).then(|| (line, character, message.to_string()))
        })
        .take(3)
        .collect()
}

/// `<Diagnostics diagnostics filePath>`.
fn diagnostics(value: Rc<dyn Fn() -> Option<Json>>, file_path: Rc<dyn Fn() -> String>) -> View {
    let theme = tui().theme;
    let errors = create_memo(move || {
        let normalized = file_path();
        parse_diagnostics(value().as_ref(), &normalized)
    });
    show(
        move || errors.with(|e| !e.is_empty()),
        move || {
            let el = box_();
            insert(
                el,
                for_each(
                    move || errors.get(),
                    |(l, c, m): &(f64, f64, String)| format!("{l}:{c}:{m}"),
                    move |item, _| {
                        let line = text();
                        bind_always(move || rgba(theme.with(|t| t.text.feedback.error.base)), move |c| line.set_fg(Some(c)));
                        insert(
                            line,
                            View::dynamic(move || {
                                item.with(|(l, c, m)| {
                                    View::from(format!(
                                        "Error [{}:{}] {m}",
                                        opencode_tui_sync::js::number(l + 1.0),
                                        opencode_tui_sync::js::number(c + 1.0)
                                    ))
                                })
                            }),
                            At::Only,
                        );
                        View::Node(line)
                    },
                    None,
                ),
                At::Only,
            );
            View::Node(el)
        },
        None,
    )
}

trait StrOpt {
    fn str_opt(&self) -> Option<&str>;
}

impl StrOpt for Json {
    fn str_opt(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }
}
