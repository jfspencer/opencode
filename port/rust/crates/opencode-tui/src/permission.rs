//! The permission prompt (`routes/session/permission.tsx`): `PermissionPrompt` (the request's presentation and the
//! allow once / always / reject choices, the edit's diff), `RejectPrompt` (a subagent's rejection with a reason)
//! and `SessionQuestion` (the bordered question with its options, fullscreen toggle and key hints, which
//! `location-missing.tsx` also uses).
//!
//! Divergences: `SimulationSemantics` (simulation/) is out of scope. TODO(port-tui): the session slice's `<code>`
//! renders a patch without a diff (`filetype="diff"`); until it lands that patch is plain text. The keymap commands
//! do not see the key event, so RejectPrompt's "ctrl+c clears the reason" is an inline ctrl+c binding ahead of
//! `app.exit` that falls through when the reason is empty (the same keys with the default bindings).
use std::cell::RefCell;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::border::{BorderCharacters, border_chars_to_array};
use opencode_tui_core::color::RGBA;
use opencode_tui_core::elements::{box_, scrollbox, span, text, textarea};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::renderables::diff::DiffView;
use opencode_tui_core::renderables::scrollbox::ScrollBarOptions;
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Align, Edge, Element, FlexDirection, Justify, MouseEventType};
use opencode_tui_sync::json::JsonExt;

use crate::ctx::tui;
use crate::keymap::{Bind, Command, Layer, create_layer, use_shortcuts};
use crate::tools::filetype::filetype;
use crate::tools::patch_diff::{DiffProps, PatchDiffProps, patch_diff};
use crate::tools::path_format::use_path_formatter;
use crate::tools::presentation::{PresentationInput, permission_always_lines, permission_option_label, permission_presentation};
use crate::tools::scroll::get_scroll_acceleration;
use crate::util::rgba;

/// `SplitBorder.customBorderChars`: the vertical bar only.
pub fn split_border_chars() -> [u32; 11] {
    border_chars_to_array(&BorderCharacters {
        top_left: "",
        top_right: "",
        bottom_left: "",
        bottom_right: "",
        horizontal: " ",
        vertical: "┃",
        top_t: "",
        bottom_t: "",
        left_t: "",
        right_t: "",
        cross: "",
    })
}

fn static_text(el: Element, value: &str) {
    insert_node(el, opencode_tui_core::renderables::text::text_node_from_string(value), None);
}

/// `<text fg={fg}>{value}</text>` with a reactive color.
fn label(value: impl Into<String>, fg: impl Fn() -> RGBA + 'static) -> Element {
    let el = text();
    static_text(el, &value.into());
    bind_always(fg, move |c| el.set_fg(Some(c)));
    el
}

/// `<text fg={base}>{key} <span style={{ fg: muted }}>{hint}</span></text>`.
fn key_hint(key: impl Fn() -> String + 'static, hint: impl Fn() -> String + 'static) -> Element {
    let theme = tui().theme;
    let el = text();
    bind_always(move || rgba(theme.with(|t| t.text.base)), move |c| el.set_fg(Some(c)));
    insert(el, View::dynamic(move || View::from(key())), At::End);
    static_text(el, " ");
    let muted = span();
    insert_node(el, muted, None);
    bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| muted.set_span_style(Some(c), None, 0, 0));
    insert(muted, View::dynamic(move || View::from(hint())), At::Only);
    el
}

/// `EditBody`: the edit's diff in a scroll box, else its patch, else a note.
fn edit_body(file: Option<String>, diff: Option<String>, patch: Option<String>) -> View {
    let t = tui();
    let theme = t.theme;
    let config = t.config_context.clone();
    let dimensions = t.dimensions;
    let filepath = file.unwrap_or_default();
    let diff = diff.unwrap_or_default();
    let view = {
        let config = config.clone();
        create_memo(move || match config.data().diffs.as_ref().and_then(|d| d.view.clone()).as_deref() {
            Some("unified") => DiffView::Unified,
            Some("split") => DiffView::Split,
            _ => {
                if dimensions.get().0 > 120 {
                    DiffView::Split
                } else {
                    DiffView::Unified
                }
            }
        })
    };
    let ft = filetype(Some(&filepath)).map(str::to_string);
    let scroll_box = {
        let config = config.clone();
        move || {
            let el = scrollbox();
            el.set_height(Some(Value::Percent(100.0)));
            {
                let config = config.clone();
                bind_always(move || get_scroll_acceleration(&config.data()), move |a| el.set_scroll_acceleration(a));
            }
            bind_always(
                move || theme.with(|t| (rgba(t.background.base), rgba(t.scrollbar.base))),
                move |(track, thumb)| {
                    el.set_scrollbar_options(
                        Some(true),
                        &ScrollBarOptions { track_background: Some(track), track_foreground: Some(thumb), ..Default::default() },
                    )
                },
            );
            el
        }
    };
    let root = box_();
    root.set_flex_direction(FlexDirection::Column);
    root.set_gap(2, Some(Value::Point(1.0)));
    let has_diff = !diff.is_empty();
    if has_diff {
        let el = scroll_box();
        let diff_text = diff.clone();
        let ft = ft.clone();
        let props = PatchDiffProps {
            diff: Rc::new(move || diff_text.clone()),
            hunk_fg: Rc::new(move || rgba(theme.with(|t| t.diff.text.hunk_header))),
            line_number_bg: Rc::new(move || rgba(theme.with(|t| t.diff.background.context))),
            on_ref: None,
            gutter_padding: None,
            diff_props: DiffProps::themed(theme, Rc::new(move || view.get()), Rc::new(move || ft.clone()), Rc::new(|| WrapMode::Word)),
        };
        insert(el, component(move || patch_diff(props)), At::Only);
        insert_node(root, el, None);
    }
    if !has_diff {
        match patch {
            Some(patch) => {
                let el = scroll_box();
                // TODO(port-tui): `<code filetype="diff" drawUnstyledText={false} streaming syntaxStyle>`.
                let code = text();
                code.set_content(&patch);
                bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| code.set_fg(Some(c)));
                insert_node(el, code, None);
                insert_node(root, el, None);
            }
            None => {
                let pad = box_();
                pad.set_padding(Edge::Left, Some(Value::Point(1.0)));
                insert_node(pad, label("No diff provided", move || rgba(theme.with(|t| t.text.muted))), None);
                insert_node(root, pad, None);
            }
        }
    }
    View::Node(root)
}

/// The tool call a request is for (`source()`): its input and metadata once it is no longer streaming.
fn request_source(request: &Json) -> (Option<Json>, Option<Json>) {
    let t = tui();
    let Some(tool) = request.get("source") else { return (None, None) };
    let session = request.str("sessionID").unwrap_or("");
    let Some(message) = t.data.store.session.message.get(session, tool.str("messageID").unwrap_or("")) else {
        return (None, None);
    };
    if message.str("type") != Some("assistant") {
        return (None, None);
    }
    let part = message.arr("content").into_iter().flatten().find(|part| {
        part.str("type") == Some("tool") && part.str("id") == tool.str("id")
    });
    match part {
        Some(part) if part.get("state").and_then(|s| s.str("status")) != Some("streaming") => {
            let state = part.get("state");
            (state.and_then(|s| s.get("input")).cloned(), state.and_then(|s| s.get("metadata")).cloned())
        }
        _ => (None, None),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Permission,
    Reject,
}

/// `<PermissionPrompt request directory>`.
pub fn permission_prompt(request: Rc<Json>, _directory: Option<String>) -> View {
    let t = tui();
    let theme = t.theme;
    let stage = create_signal(Stage::Permission).0;
    let path_formatter = use_path_formatter();
    let session_id = request.str("sessionID").unwrap_or("").to_string();
    let session = {
        let store = t.data.store.clone();
        let session_id = session_id.clone();
        create_memo_equals(move || store.session.get(&session_id), None)
    };
    let source = {
        let request = request.clone();
        create_memo(move || request_source(&request))
    };
    let reply: Rc<dyn Fn(&str, Option<String>)> = {
        let t = t.clone();
        let request = request.clone();
        Rc::new(move |decision: &str, message: Option<String>| {
            let mut input = opencode_tui_sync::obj! {
                "sessionID" => request.str("sessionID").unwrap_or(""),
                "requestID" => request.str("id").unwrap_or(""),
                "decision" => decision,
            };
            if let Some(message) = message {
                input.set("message", Json::String(message));
            }
            let promise = t.data.store.session.permission.reply(input);
            let toast = t.toast.clone();
            tokio::task::spawn_local(async move {
                if let Err(error) = promise.wait().await {
                    toast.error(error.message());
                }
            });
        })
    };
    let action = request.str("action").unwrap_or("").to_string();
    let request_id = request.str("id").unwrap_or("").to_string();

    let reject_view = {
        let reply = reply.clone();
        let action = action.clone();
        let request_id = request_id.clone();
        move || {
            let reply = reply.clone();
            reject_prompt(RejectProps {
                action: action.clone(),
                instance: request_id.clone(),
                on_confirm: Rc::new(move |message: String| reply("reject", (!message.is_empty()).then_some(message))),
                on_cancel: Rc::new(move || stage.set(Stage::Permission)),
            })
        }
    };

    // The Match's children are the JSX the IIFE returns, evaluated in Switch's memo: what it reads (the source,
    // the location) re-creates the question.
    let permission_view = {
        let request = request.clone();
        move || {
            let request = request.clone();
            let reply = reply.clone();
            let path_formatter = path_formatter.clone();
            memo_view(move || {
                let (input, tool_metadata) = source.get();
                let format = {
                    let path_formatter = path_formatter.clone();
                    move |value: &str| path_formatter.format(Some(value))
                };
                let resources: Vec<Json> = request.arr("resources").into_iter().flatten().cloned().collect();
                let current = permission_presentation(
                    &PresentationInput {
                        action: request.str("action").unwrap_or(""),
                        resources: &resources,
                        metadata: request.get("metadata"),
                        input: input.as_ref(),
                        tool_metadata: tool_metadata.as_ref(),
                    },
                    &format,
                );
                let action = request.str("action").unwrap_or("").to_string();
                let current = Rc::new(current);
                let presentation_body: Rc<dyn Fn() -> View> = {
                    let current = current.clone();
                    let action = action.clone();
                    Rc::new(move || {
                        if action == "edit" {
                            let current = current.clone();
                            return component(move || edit_body(current.file.clone(), current.diff.clone(), current.patch.clone()));
                        }
                        if action == "external_directory" {
                            let current = current.clone();
                            return show(
                                {
                                    let current = current.clone();
                                    move || !current.lines.is_empty()
                                },
                                move || {
                                    let el = box_();
                                    el.set_padding(Edge::Left, Some(Value::Point(1.0)));
                                    el.set_gap(2, Some(Value::Point(1.0)));
                                    insert_node(el, label("Patterns", move || rgba(theme.with(|t| t.text.muted))), None);
                                    let lines = box_();
                                    insert_node(el, lines, None);
                                    let items = current.lines.clone();
                                    insert(
                                        lines,
                                        for_each(
                                            move || items.clone(),
                                            |line: &String| line.clone(),
                                            move |line, _| View::Node(label(line.get(), move || rgba(theme.with(|t| t.text.base)))),
                                            None,
                                        ),
                                        At::Only,
                                    );
                                    View::Node(el)
                                },
                                None,
                            );
                        }
                        let el = box_();
                        el.set_padding(Edge::Left, Some(Value::Point(1.0)));
                        let items = current.lines.clone();
                        let strong = matches!(action.as_str(), "shell" | "subagent" | "task");
                        insert(
                            el,
                            for_each(
                                move || items.clone(),
                                |line: &String| line.clone(),
                                move |line, _| {
                                    View::Node(label(line.get(), move || {
                                        theme.with(|t| if strong { rgba(t.text.base) } else { rgba(t.text.muted) })
                                    }))
                                },
                                None,
                            ),
                            At::Only,
                        );
                        View::Node(el)
                    })
                };
                let header = {
                    let current = current.clone();
                    let action = action.clone();
                    let column = box_();
                    column.set_flex_direction(FlexDirection::Column);
                    column.set_gap(2, Some(Value::Point(0.0)));
                    let row = box_();
                    insert_node(column, row, None);
                    row.set_flex_direction(FlexDirection::Row);
                    row.set_gap(2, Some(Value::Point(1.0)));
                    row.set_flex_shrink(Some(0.0));
                    insert_node(row, label("△", move || rgba(theme.with(|t| t.text.feedback.warning.base))), None);
                    insert_node(row, label("Permission required", move || rgba(theme.with(|t| t.text.base))), None);
                    insert(
                        column,
                        show(
                            {
                                let current = current.clone();
                                move || action != "shell" && !current.title.is_empty()
                            },
                            move || {
                                let row = box_();
                                row.set_flex_direction(FlexDirection::Row);
                                row.set_gap(2, Some(Value::Point(1.0)));
                                row.set_padding(Edge::Left, Some(Value::Point(2.0)));
                                row.set_flex_shrink(Some(0.0));
                                let icon = label(current.icon, move || rgba(theme.with(|t| t.text.muted)));
                                icon.set_flex_shrink(Some(0.0));
                                insert_node(row, icon, None);
                                insert_node(row, label(current.title.clone(), move || rgba(theme.with(|t| t.text.base))), None);
                                View::Node(row)
                            },
                            None,
                        ),
                        At::End,
                    );
                    View::Node(column)
                };
                let save: Vec<String> = request
                    .arr("save")
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s.as_str_opt().map(str::to_string))
                    .collect();
                let mut options = vec![("once", permission_option_label("once").to_string())];
                if !save.is_empty() {
                    options.push(("always", permission_option_label("always").to_string()));
                }
                options.push(("reject", permission_option_label("reject").to_string()));
                let always_lines = permission_always_lines(&action, &save);
                let body: Rc<dyn Fn(&'static str) -> View> = {
                    let presentation_body = presentation_body.clone();
                    Rc::new(move |option: &'static str| {
                        let always_lines = always_lines.clone();
                        let presentation_body = presentation_body.clone();
                        show(
                            move || option == "always",
                            move || {
                                let el = box_();
                                el.set_padding(Edge::Left, Some(Value::Point(1.0)));
                                el.set_gap(2, Some(Value::Point(1.0)));
                                let lines = always_lines.clone();
                                insert(
                                    el,
                                    for_each(
                                        move || lines.clone(),
                                        |line: &String| line.clone(),
                                        move |line, index| {
                                            View::Node(label(line.get(), move || {
                                                theme.with(|t| if index.get() == 0 { rgba(t.text.muted) } else { rgba(t.text.base) })
                                            }))
                                        },
                                        None,
                                    ),
                                    At::Only,
                                );
                                View::Node(el)
                            },
                            Some(Rc::new(move || presentation_body())),
                        )
                    })
                };
                let reply = reply.clone();
                let has_parent = {
                    let session = session;
                    move || session.with(|s| s.as_ref().is_some_and(|s| s.str("parentID").is_some()))
                };
                let title = current.title.clone();
                let instance = request.str("id").unwrap_or("").to_string();
                component(move || {
                    session_question(SessionQuestionProps {
                        title: "Permission required".into(),
                        semantic_label: Some(permission_semantic_label(&action, Some(&title))),
                        instance: instance.clone(),
                        id: None,
                        group: None,
                        choices_label: None,
                        header: Some(header.clone()),
                        body: body.clone(),
                        options: options.clone(),
                        escape_key: Some("reject"),
                        fullscreen: true,
                        on_select: {
                            let reply = reply.clone();
                            let has_parent = has_parent.clone();
                            Rc::new(move |option: &'static str| {
                                if option == "always" {
                                    reply("always", None);
                                    return;
                                }
                                if option == "reject" {
                                    if has_parent() {
                                        stage.set(Stage::Reject);
                                        return;
                                    }
                                    reply("reject", None);
                                    return;
                                }
                                reply("once", None);
                            })
                        },
                    })
                })
            })
        }
    };

    switch(
        vec![when(move || stage.get() == Stage::Reject, reject_view), when(move || stage.get() == Stage::Permission, permission_view)],
        None,
    )
}

/// `permissionSemanticLabel(action, title)`.
pub fn permission_semantic_label(action: &str, title: Option<&str>) -> String {
    format!("Permission required: {}", title.unwrap_or(action))
}

trait AsStrOpt {
    fn as_str_opt(&self) -> Option<&str>;
}

impl AsStrOpt for Json {
    fn as_str_opt(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }
}

struct RejectProps {
    action: String,
    instance: String,
    on_confirm: Rc<dyn Fn(String)>,
    on_cancel: Rc<dyn Fn()>,
}

/// `<RejectPrompt>`: what to do differently, typed in a textarea.
fn reject_prompt(props: RejectProps) -> View {
    let t = tui();
    let theme = t.theme;
    let dimensions = t.dimensions;
    let enabled = crate::interactivity::use_interactivity();
    let narrow = create_memo(move || dimensions.get().0 < 80);
    let _ = (&props.action, &props.instance);
    let input: Rc<RefCell<Option<Element>>> = Rc::default();
    {
        let on_cancel = props.on_cancel.clone();
        let on_confirm = props.on_confirm.clone();
        let input = input.clone();
        create_layer(move || {
            let text_of = {
                let input = input.clone();
                move || input.borrow().map(|i| i.textarea_text()).unwrap_or_default()
            };
            let clear = {
                let input = input.clone();
                let text_of = text_of.clone();
                move || {
                    if text_of().is_empty() {
                        return false;
                    }
                    if let Some(i) = *input.borrow() {
                        i.set_textarea_text("");
                    }
                    true
                }
            };
            let cancel = on_cancel.clone();
            let cancel2 = on_cancel.clone();
            let confirm = on_confirm.clone();
            Layer {
                mode: Some("base"),
                commands: vec![
                    Command::inline("ctrl+c", clear).title("Cancel permission rejection").group("Permission"),
                    Command::new("app.exit", move || cancel()).title("Cancel permission rejection").group("Permission"),
                    Command::inline("escape", move || {
                        cancel2();
                        true
                    })
                    .title("Cancel permission rejection")
                    .group("Permission"),
                    Command::inline("return", move || {
                        confirm(text_of());
                        true
                    })
                    .title("Confirm permission rejection")
                    .group("Permission"),
                ],
                ..Layer::default()
            }
        });
    }

    let root = box_();
    root.set_id("session.permission.reject");
    bind_always(move || rgba(theme.with(|t| t.background.raised.base)), move |c| root.set_background_color(Some(c)));
    root.set_border(BorderProp::Sides(vec![Side::Left]));
    bind_always(move || rgba(theme.with(|t| t.text.feedback.error.base)), move |c| root.set_border_color(Some(c)));
    root.set_custom_border_chars(Some(split_border_chars()));

    let top = box_();
    insert_node(root, top, None);
    top.set_gap(2, Some(Value::Point(1.0)));
    top.set_padding(Edge::Left, Some(Value::Point(1.0)));
    top.set_padding(Edge::Right, Some(Value::Point(3.0)));
    top.set_padding(Edge::Top, Some(Value::Point(1.0)));
    top.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
    let title_row = box_();
    insert_node(top, title_row, None);
    title_row.set_flex_direction(FlexDirection::Row);
    title_row.set_gap(2, Some(Value::Point(1.0)));
    title_row.set_padding(Edge::Left, Some(Value::Point(1.0)));
    insert_node(title_row, label("△", move || rgba(theme.with(|t| t.text.feedback.error.base))), None);
    insert_node(title_row, label("Reject permission", move || rgba(theme.with(|t| t.text.base))), None);
    let note = box_();
    insert_node(top, note, None);
    note.set_padding(Edge::Left, Some(Value::Point(1.0)));
    insert_node(note, label("Tell OpenCode what to do differently", move || rgba(theme.with(|t| t.text.muted))), None);

    let bottom = box_();
    insert_node(root, bottom, None);
    bind(move || narrow.get(), move |narrow| {
        bottom.set_flex_direction(if narrow { FlexDirection::Column } else { FlexDirection::Row });
        bottom.set_justify_content(if narrow { Justify::FlexStart } else { Justify::SpaceBetween });
        bottom.set_align_items(if narrow { Align::FlexStart } else { Align::Center });
    });
    bottom.set_flex_shrink(Some(0.0));
    bottom.set_padding(Edge::Top, Some(Value::Point(1.0)));
    bottom.set_padding(Edge::Left, Some(Value::Point(2.0)));
    bottom.set_padding(Edge::Right, Some(Value::Point(3.0)));
    bottom.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
    bind_always(
        move || theme.with(|t| rgba(t.decrease(t.background.raised.base, 1.0))),
        move |c| bottom.set_background_color(Some(c)),
    );
    bottom.set_gap(2, Some(Value::Point(1.0)));

    let area = textarea();
    insert_node(bottom, area, None);
    area.set_id("session.permission.reject.message");
    *input.borrow_mut() = Some(area);
    bind(move || enabled(), move |focused| if focused { area.focus() } else { area.blur() });
    bind_always(move || rgba(theme.with(|t| t.text.base)), move |c| {
        area.set_text_color(c);
        area.set_focused_text_color(c);
        area.set_cursor_color_prop(c);
    });

    let actions = box_();
    insert_node(bottom, actions, None);
    actions.set_id("session.permission.reject.actions");
    actions.set_flex_direction(FlexDirection::Row);
    actions.set_gap(2, Some(Value::Point(2.0)));
    actions.set_flex_shrink(Some(0.0));
    let confirm = box_();
    insert_node(actions, confirm, None);
    confirm.set_id("session.permission.reject.confirm");
    {
        let on_confirm = props.on_confirm.clone();
        confirm.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| on_confirm(area.textarea_text()))));
    }
    insert_node(confirm, key_hint(|| "enter".into(), || "confirm".into()), None);
    let cancel = box_();
    insert_node(actions, cancel, None);
    cancel.set_id("session.permission.reject.cancel");
    {
        let on_cancel = props.on_cancel.clone();
        cancel.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| on_cancel())));
    }
    insert_node(cancel, key_hint(|| "esc".into(), || "cancel".into()), None);
    View::Node(root)
}

/// `SessionQuestion`'s props (`options` in TS key order).
pub struct SessionQuestionProps {
    pub title: String,
    pub semantic_label: Option<String>,
    pub instance: String,
    pub id: Option<String>,
    pub group: Option<String>,
    pub choices_label: Option<String>,
    pub header: Option<View>,
    pub body: Rc<dyn Fn(&'static str) -> View>,
    pub options: Vec<(&'static str, String)>,
    pub escape_key: Option<&'static str>,
    pub fullscreen: bool,
    pub on_select: Rc<dyn Fn(&'static str)>,
}

/// `<SessionQuestion>`.
pub fn session_question(props: SessionQuestionProps) -> View {
    let t = tui();
    let theme = t.theme;
    let dimensions = t.dimensions;
    let keys: Rc<Vec<&'static str>> = Rc::new(props.options.iter().map(|(k, _)| *k).collect());
    let selected = create_signal(keys[0]).0;
    let expanded = create_signal(false).0;
    let narrow = create_memo(move || dimensions.get().0 < 80);
    let shortcuts = use_shortcuts();
    let id = props.id.clone().unwrap_or_else(|| "session.permission".into());
    let group = props.group.clone().unwrap_or_else(|| "Permission".into());
    let dismiss: Rc<dyn Fn()> = {
        let escape_key = props.escape_key;
        let on_select = props.on_select.clone();
        Rc::new(move || {
            if expanded.get_untracked() {
                expanded.set(false);
                return;
            }
            if let Some(key) = escape_key {
                on_select(key);
            }
        })
    };

    {
        let keys = keys.clone();
        let group = group.clone();
        let dismiss = dismiss.clone();
        let on_select = props.on_select.clone();
        let escape_key = props.escape_key;
        let fullscreen = props.fullscreen;
        create_layer(move || {
            let mut commands = Vec::new();
            if escape_key.is_some() {
                let dismiss = dismiss.clone();
                commands.push(Command::new("app.exit", move || dismiss()).title("Reject permission").group(group.clone()).bind(Bind::Off));
            }
            if fullscreen {
                commands.push(
                    Command::new("permission.prompt.fullscreen", move || expanded.update(|v| *v = !*v))
                        .title("Toggle permission fullscreen")
                        .group(group.clone())
                        .bind(Bind::Off),
                );
            }
            if keys.len() > 1 {
                let previous = keys.clone();
                commands.push(
                    Command::inline("left,h", move || {
                        let index = previous.iter().position(|k| *k == selected.get_untracked()).unwrap_or(0);
                        selected.set(previous[(index + previous.len() - 1) % previous.len()]);
                        true
                    })
                    .title("Previous option")
                    .group(group.clone()),
                );
                let next = keys.clone();
                commands.push(
                    Command::inline("right,l", move || {
                        let index = next.iter().position(|k| *k == selected.get_untracked()).unwrap_or(0);
                        selected.set(next[(index + 1) % next.len()]);
                        true
                    })
                    .title("Next option")
                    .group(group.clone()),
                );
            }
            {
                let on_select = on_select.clone();
                commands.push(
                    Command::inline("return", move || {
                        on_select(selected.get_untracked());
                        true
                    })
                    .title("Select option")
                    .group(group.clone()),
                );
            }
            if escape_key.is_some() {
                let dismiss = dismiss.clone();
                commands.push(
                    Command::inline("escape", move || {
                        dismiss();
                        true
                    })
                    .title("Reject permission")
                    .group(group.clone()),
                );
            }
            let mut bindings = Vec::new();
            if escape_key.is_some() {
                bindings.push("app.exit");
            }
            if fullscreen {
                bindings.push("permission.prompt.fullscreen");
            }
            Layer { mode: Some("base"), commands, bindings, ..Layer::default() }
        });
    }

    let hint = create_memo(move || if expanded.get() { "minimize" } else { "fullscreen" });
    let header = props.header.clone();
    let body = props.body.clone();
    let options = Rc::new(props.options);
    let title = props.title.clone();
    let fullscreen = props.fullscreen;
    let on_select = props.on_select.clone();

    let content: Rc<dyn Fn() -> View> = Rc::new(move || {
        let root = box_();
        root.set_id(&id);
        bind_always(move || rgba(theme.with(|t| t.background.raised.base)), move |c| root.set_background_color(Some(c)));
        root.set_border(BorderProp::Sides(vec![Side::Left]));
        bind_always(
            move || rgba(theme.with(|t| t.background.action.primary.focused)),
            move |c| root.set_border_color(Some(c)),
        );
        root.set_custom_border_chars(Some(split_border_chars()));
        bind(
            move || (expanded.get(), dimensions.get().1),
            move |(expanded, height)| {
                if expanded {
                    root.set_positions(
                        Some(Some(Value::Point(2.0))),
                        Some(Some(Value::Point(-(height as f32) + 1.0))),
                        Some(Some(Value::Point(2.0))),
                        Some(Some(Value::Point(1.0))),
                    );
                    root.set_position_type(true);
                } else {
                    root.set_positions(
                        Some(Some(Value::Point(0.0))),
                        Some(Some(Value::Point(0.0))),
                        Some(Some(Value::Point(0.0))),
                        Some(Some(Value::Point(0.0))),
                    );
                    root.set_max_height(Some(Value::Point(15.0)));
                    root.set_position_type(false);
                }
            },
        );

        let main = box_();
        insert_node(root, main, None);
        main.set_gap(2, Some(Value::Point(1.0)));
        main.set_padding(Edge::Left, Some(Value::Point(1.0)));
        main.set_padding(Edge::Right, Some(Value::Point(3.0)));
        main.set_padding(Edge::Top, Some(Value::Point(1.0)));
        main.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
        main.set_flex_grow(Some(1.0));
        match header.clone() {
            Some(header) => {
                let el = box_();
                el.set_padding(Edge::Left, Some(Value::Point(1.0)));
                el.set_flex_shrink(Some(0.0));
                insert(el, header, At::Only);
                insert_node(main, el, None);
            }
            None => {
                let row = box_();
                row.set_flex_direction(FlexDirection::Row);
                row.set_gap(2, Some(Value::Point(1.0)));
                row.set_padding(Edge::Left, Some(Value::Point(1.0)));
                row.set_flex_shrink(Some(0.0));
                insert_node(row, label("△", move || rgba(theme.with(|t| t.text.feedback.warning.base))), None);
                insert_node(row, label(title.clone(), move || rgba(theme.with(|t| t.text.base))), None);
                insert_node(main, row, None);
            }
        }
        {
            let body = body.clone();
            insert(main, View::dynamic(move || body(selected.get())), At::End);
        }

        let footer = box_();
        insert_node(root, footer, None);
        bind(move || narrow.get(), move |narrow| {
            footer.set_flex_direction(if narrow { FlexDirection::Column } else { FlexDirection::Row });
            footer.set_justify_content(if narrow { Justify::FlexStart } else { Justify::SpaceBetween });
            footer.set_align_items(if narrow { Align::FlexStart } else { Align::Center });
        });
        footer.set_flex_shrink(Some(0.0));
        footer.set_gap(2, Some(Value::Point(1.0)));
        footer.set_padding(Edge::Top, Some(Value::Point(1.0)));
        footer.set_padding(Edge::Left, Some(Value::Point(2.0)));
        footer.set_padding(Edge::Right, Some(Value::Point(3.0)));
        footer.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
        bind_always(
            move || theme.with(|t| rgba(t.decrease(t.background.raised.base, 1.0))),
            move |c| footer.set_background_color(Some(c)),
        );

        let choices = box_();
        insert_node(footer, choices, None);
        choices.set_id(&format!("{id}.actions"));
        choices.set_flex_direction(FlexDirection::Row);
        choices.set_gap(2, Some(Value::Point(1.0)));
        choices.set_flex_shrink(Some(0.0));
        {
            let options = options.clone();
            let id = id.clone();
            let on_select = on_select.clone();
            let keys = keys.clone();
            insert(
                choices,
                for_each(
                    move || keys.to_vec(),
                    |key: &&'static str| *key,
                    move |option, _| {
                        let option = option.get_untracked();
                        let el = box_();
                        el.set_id(&format!("{id}.action.{option}"));
                        el.set_padding(Edge::Left, Some(Value::Point(1.0)));
                        el.set_padding(Edge::Right, Some(Value::Point(1.0)));
                        bind_always(
                            move || {
                                theme.with(|t| {
                                    if selected.get() == option {
                                        rgba(t.background.action.primary.focused)
                                    } else {
                                        rgba(t.background.action.primary.base)
                                    }
                                })
                            },
                            move |c| el.set_background_color(Some(c)),
                        );
                        el.set_on_mouse(Some(MouseEventType::Move), Some(Rc::new(move |_| selected.set(option))));
                        {
                            let on_select = on_select.clone();
                            el.set_on_mouse(
                                Some(MouseEventType::Up),
                                Some(Rc::new(move |_| {
                                    selected.set(option);
                                    on_select(option);
                                })),
                            );
                        }
                        let name = options.iter().find(|(k, _)| *k == option).map(|(_, v)| v.clone()).unwrap_or_default();
                        insert_node(
                            el,
                            label(name, move || {
                                theme.with(|t| {
                                    if selected.get() == option {
                                        rgba(t.text.action.primary.focused)
                                    } else {
                                        rgba(t.text.action.primary.base)
                                    }
                                })
                            }),
                            None,
                        );
                        View::Node(el)
                    },
                    None,
                ),
                At::Only,
            );
        }

        let hints = box_();
        insert_node(footer, hints, None);
        hints.set_flex_direction(FlexDirection::Row);
        hints.set_gap(2, Some(Value::Point(2.0)));
        hints.set_flex_shrink(Some(0.0));
        if fullscreen {
            let shortcuts = shortcuts.clone();
            insert_node(
                hints,
                key_hint(move || shortcuts.get("permission.prompt.fullscreen").unwrap_or_default(), move || hint.get().to_string()),
                None,
            );
        }
        if keys.len() > 1 {
            insert_node(hints, key_hint(|| "⇆".into(), || "select".into()), None);
        }
        insert_node(hints, key_hint(|| "enter".into(), || "confirm".into()), None);
        View::Node(root)
    });

    let inline = content.clone();
    show(move || !expanded.get(), move || inline(), Some(Rc::new(move || portal(None, None, content.clone()))))
}
