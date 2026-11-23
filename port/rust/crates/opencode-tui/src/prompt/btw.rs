//! `feature-plugins/prompt/btw.tsx`: `/btw`, a one-shot side question answered from the session's context in a
//! dialog without adding to the conversation.
//!
//! TODO(port-tui): the `/btw` spinner in `prompt.footer.status` while the answer generates, session links in the
//! answer, and the answer's scroll box (see the body's note).
use std::cell::Cell;
use std::rc::Rc;

use opencode_tui_core::color::text_attributes;
use opencode_tui_core::elements::{box_, markdown, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_markup::markdown::{InternalBlockMode, MarkdownTableOptions, MarkdownTableStyle};
use opencode_tui_core::renderables::scrollbox::ScrollUnit;
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element};
use opencode_tui_sync::client::RequestOptions;
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::obj;

use crate::ctx::{Route, tui};
use crate::keymap::{Command, Layer, create_layer};
use crate::syntax::current_syntax;
use crate::theme::SurfaceName;
use crate::toast::{ToastInput, Variant};
use crate::ui::dialog::{
    DIALOG_INSET, DialogSize, FooterAction, FooterActions, FooterProps, HeaderProps, LayoutProps, dialog_bleed, dialog_footer,
    dialog_header, dialog_layout, use_dialog,
};
use crate::ui::dialog_prompt::{PromptProps as DialogPromptProps, dialog_prompt};
use crate::ui::theme::use_theme;
use crate::util::rgba;

// session.generate exposes the session's tools but runs no tool loop, so a tool call would surface as an empty answer.
const INSTRUCTIONS: [&str; 3] = [
    "The user is asking a quick side question about the conversation so far.",
    "Answer directly and concisely in markdown from what you already know.",
    "Do not call any tools and do not take any actions.",
];

/// The `/btw` plugin's command (`session.aside`), registered from the `app` slot.
pub fn install() {
    let t = tui();
    let (toast, route, dialog) = (t.toast.clone(), t.route, crate::ui::dialog::use_dialog());
    let client = t.data.client.clone();
    let ask = {
        let (toast, dialog) = (toast.clone(), dialog.clone());
        let (route, client) = (route, client);
        Rc::new(move |question: String| {
            let session_id = match route.get_untracked() {
                Route::Session { session_id } => session_id,
                Route::Home => {
                    toast.show(ToastInput::new(Variant::Warning, "Open a session first"));
                    return;
                }
            };
            let prompt = format!("{}\n\n{question}", INSTRUCTIONS.join(" "));
            let (dialog, toast) = (dialog.clone(), toast.clone());
            let client = client.clone();
            tokio::task::spawn_local(async move {
                let input = obj! { "sessionID" => session_id.as_str(), "prompt" => prompt.as_str() };
                match client.call_named("session.generate", &input, RequestOptions::default()).await {
                    Ok(response) => {
                        let answer = response.json().str("text").unwrap_or_default().trim().to_string();
                        dialog.replace(
                            {
                                let question = question.replace(char::is_whitespace, " ");
                                move || answer_dialog(question.clone(), answer.clone())
                            },
                            None,
                            None,
                            None,
                        );
                    }
                    Err(error) => toast.error(error.message()),
                }
            });
        })
    };
    create_layer(move || {
        let (toast, dialog, ask) = (toast.clone(), dialog.clone(), ask.clone());
        Layer {
            mode: Some("global"),
            commands: vec![Command::with_input(
                "session.aside",
                move |input| {
                    if !route.with_untracked(|r| matches!(r, Route::Session { .. })) {
                        toast.show(ToastInput::new(Variant::Warning, "Open a session first"));
                        return true;
                    }
                    let question = input.as_deref().map(opencode_tui_sync::js::trim).filter(|q| !q.is_empty()).map(str::to_string);
                    match question {
                        Some(question) => ask(question),
                        None => {
                            let ask = ask.clone();
                            let prompt_dialog = dialog.clone();
                            dialog.replace(
                                move || {
                                    let ask = ask.clone();
                                    let prompt_dialog = prompt_dialog.clone();
                                    let cancel = prompt_dialog.clone();
                                    dialog_prompt(DialogPromptProps {
                                        title: "/btw".into(),
                                        placeholder: Some("Ask anything".into()),
                                        on_confirm: Some(Rc::new(move |value: String| {
                                            let question = opencode_tui_sync::js::trim(&value).to_string();
                                            if question.is_empty() {
                                                return;
                                            }
                                            prompt_dialog.clear();
                                            ask(question);
                                        })),
                                        on_cancel: Some(Rc::new(move || cancel.clear())),
                                        ..DialogPromptProps::default()
                                    })
                                },
                                None,
                                None,
                                None,
                            );
                        }
                    }
                    true
                },
            )
            .title("Ask a side question")
            .description("One-shot answer from the session's context without adding to the conversation")
            .group("Session")
            .palette(true)
            .slash_arguments("btw")],
            ..Layer::default()
        }
    });
}

/// `<Answer question answer markdown>`.
fn answer_dialog(question: String, answer: String) -> View {
    let dialog = use_dialog();
    let theme = use_theme();
    let surface = use_theme().surface(SurfaceName::Dialog);
    let dimensions = tui().dimensions;
    let syntax = current_syntax();
    let toast = tui().toast.clone();
    let max_height = create_memo(move || (dimensions.get().1 as f32 * 0.6).floor().max(8.0));
    let (copied, set_copied) = create_signal(false);
    let scroll: Rc<Cell<Option<Element>>> = Rc::new(Cell::new(None));
    let answer_for_copy = answer.clone();

    dialog.set_size(DialogSize::Large);
    dialog.set_centered(true);

    let copy = {
        let toast = toast.clone();
        Rc::new(move || {
            let value = answer_for_copy.clone();
            let toast = toast.clone();
            tokio::task::spawn_local(async move {
                match crate::selection::write_clipboard(&value).await {
                    Ok(()) => set_copied.set(true),
                    Err(_) => toast.error("Failed to copy to clipboard"),
                }
            });
        })
    };
    {
        let (copy, scroll) = (copy.clone(), scroll.clone());
        create_layer(move || {
            let copy = copy.clone();
            let mut commands = vec![Command::inline("c", move || {
                copy();
                true
            })
            .title("Copy answer")
            .group("Dialog")];
            let up = scroll.clone();
            commands.push(Command::inline("up,k", move || {
                if let Some(scroll) = up.get() {
                    scroll.scroll_by(-1.0, ScrollUnit::Absolute);
                }
                true
            })
            .title("Scroll answer up")
            .group("Dialog"));
            let down = scroll.clone();
            commands.push(Command::inline("down,j", move || {
                if let Some(scroll) = down.get() {
                    scroll.scroll_by(1.0, ScrollUnit::Absolute);
                }
                true
            })
            .title("Scroll answer down")
            .group("Dialog"));
            let pageup = scroll.clone();
            let height = max_height;
            commands.push(Command::inline("pageup", move || {
                if let Some(scroll) = pageup.get() {
                    scroll.scroll_by(-(height.get() as f64), ScrollUnit::Absolute);
                }
                true
            })
            .title("Previous answer page")
            .group("Dialog"));
            let pagedown = scroll.clone();
            commands.push(Command::inline("pagedown", move || {
                if let Some(scroll) = pagedown.get() {
                    scroll.scroll_by(height.get() as f64, ScrollUnit::Absolute);
                }
                true
            })
            .title("Next answer page")
            .group("Dialog"));
            let home = scroll.clone();
            commands.push(Command::inline("home", move || {
                if let Some(scroll) = home.get() {
                    scroll.scroll_to(0.0);
                }
                true
            })
            .title("First answer line")
            .group("Dialog"));
            let end = scroll.clone();
            commands.push(Command::inline("end", move || {
                if let Some(scroll) = end.get() {
                    scroll.scroll_to(scroll.scroll_height());
                }
                true
            })
            .title("Last answer line")
            .group("Dialog"));
            Layer { mode: Some("modal"), commands, ..Layer::default() }
        });
    }
    let footer = {
        let copy = copy.clone();
        FooterProps {
            actions: Some(Rc::new(move || FooterActions {
                primary: vec![if copied.get() {
                    FooterAction {
                        title: "✓ copied".into(),
                        color: Some(rgba(surface.get().text.feedback.success.base)),
                        on_trigger: Some(copy.clone()),
                        ..FooterAction::default()
                    }
                } else {
                    FooterAction { title: "copy response".into(), key: Some("c".into()), on_trigger: Some(copy.clone()), ..FooterAction::default() }
                }],
                ..FooterActions::default()
            })),
            ..FooterProps::default()
        }
    };
    let legacy_footer = {
        let copy = copy.clone();
        Rc::new(move || {
            let copy = copy.clone();
            dialog_footer(FooterProps {
                actions: Some(Rc::new(move || FooterActions {
                    secondary: vec![
                        if copied.get() {
                            FooterAction {
                                title: "✓ copied".into(),
                                color: Some(rgba(surface.get().text.feedback.success.base)),
                                on_trigger: Some(copy.clone()),
                                ..FooterAction::default()
                            }
                        } else {
                            FooterAction { title: "copy".into(), key: Some("c".into()), on_trigger: Some(copy.clone()), ..FooterAction::default() }
                        },
                        FooterAction { title: "scroll".into(), key: Some("j/k ↑/↓".into()), ..FooterAction::default() },
                    ],
                    ..FooterActions::default()
                })),
                ..FooterProps::default()
            })
        })
    };

    let header = dialog_header(HeaderProps {
        gap: Some(2.0),
        title_view: Some(Rc::new(move || {
            let el = text();
            el.set_flex_shrink(Some(0.0));
            el.set_attributes(Some(text_attributes::BOLD));
            bind_always(move || rgba(surface.get().text.base), move |c| el.set_fg(Some(c)));
            insert(el, View::from("/btw"), At::Only);
            View::Node(el)
        })),
        children: Some(Rc::new(move || {
            let el = text();
            el.set_flex_grow(Some(1.0));
            el.set_wrap_mode(WrapMode::None);
            el.set_truncate(true);
            bind_always(move || rgba(surface.get().text.muted), move |c| el.set_fg(Some(c)));
            insert(el, View::from(question.clone()), At::Only);
            View::Node(el)
        })),
        ..HeaderProps::default()
    });

    dialog_layout(LayoutProps {
        header: Box::new(|| header),
        footer: Some(footer),
        legacy_footer: Some(legacy_footer),
        children: Box::new(move || {
            // TODO(port-tui): the reference scrolls the answer in a `maxHeight` scroll box; our scroll box's
            // intrinsic size squeezes the route behind the dialog, so the body is a padded block for now.
            let el = box_();
            el.set_padding(Edge::Left, Some(Value::Point(DIALOG_INSET)));
            el.set_padding(Edge::Right, Some(Value::Point(DIALOG_INSET)));
            el.set_padding(Edge::Top, Some(Value::Point(1.0)));
            el.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
            bind_always(move || rgba(theme.get().background.raised.high), move |c| el.set_background_color(Some(c)));
            let md = markdown();
            insert_node(el, md, None);
            md.set_markdown_internal_block_mode(InternalBlockMode::TopLevel);
            md.set_markdown_syntax_style(syntax);
            md.set_markdown_conceal(true);
            md.set_markdown_table_options(Some(MarkdownTableOptions {
                style: Some(MarkdownTableStyle::Grid),
                cell_padding_x: Some(1.0),
                ..MarkdownTableOptions::default()
            }));
            md.set_markdown_content(&answer);
            bind_always(move || rgba(theme.get().markdown.text), move |c| md.set_markdown_fg(Some(c)));
            bind_always(move || rgba(theme.get().background.raised.high), move |c| md.set_markdown_bg(Some(c)));
            dialog_bleed(move || View::Node(el))
        }),
        ..LayoutProps::default()
    })
}
