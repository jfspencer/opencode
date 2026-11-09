//! `ui/dialog-prompt.tsx`: a one-line text prompt in a dialog (submit, extra actions bound while it is focused,
//! back, the busy state).
//!
//! TODO(port-tui): the busy state's `<Spinner>` (component/spinner.tsx) is the spinner's slice; until it lands the
//! busy text shows without the spinner frame.
use std::cell::Cell;
use std::rc::Rc;

use opencode_tui_core::elements::{box_, text, textarea};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::text_node_from_string;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::Element;

use crate::keymap::{Command, Enabled, Layer, create_layer};
use crate::theme::SurfaceName;
use crate::ui::dialog::{
    DialogSize, FooterAction, FooterActions, FooterProps, HeaderProps, LayoutProps, dialog_footer, dialog_header, dialog_layout,
    use_dialog,
};
use crate::ui::theme::use_theme;
use crate::util::rgba;

/// The prompt handle actions get (`{ setValue }`).
#[derive(Clone, Copy)]
pub struct PromptHandle(Element);

impl PromptHandle {
    pub fn set_value(&self, value: &str) {
        self.0.set_textarea_text(value);
    }
}

/// `DialogPromptAction`.
#[derive(Clone)]
pub struct PromptAction {
    pub title: String,
    /// A keymap command id, bound while the input is focused and shown in the footer.
    pub command: String,
    pub command_title: Option<String>,
    pub on_trigger: Rc<dyn Fn(PromptHandle)>,
}

/// `DialogPromptProps`.
#[derive(Default)]
pub struct PromptProps {
    pub title: String,
    pub size: Option<DialogSize>,
    pub subtitle: Option<String>,
    pub description: Option<Rc<dyn Fn() -> View>>,
    pub placeholder: Option<String>,
    pub value: Option<String>,
    pub busy: Option<Rc<dyn Fn() -> bool>>,
    pub busy_text: Option<String>,
    pub actions: Vec<PromptAction>,
    pub on_confirm: Option<Rc<dyn Fn(String)>>,
    pub on_cancel: Option<Rc<dyn Fn()>>,
}

/// `<DialogPrompt>`.
pub fn dialog_prompt(props: PromptProps) -> View {
    let dialog = use_dialog();
    let theme = use_theme().surface(SurfaceName::Dialog);
    let props = Rc::new(props);
    let busy = {
        let props = props.clone();
        Rc::new(move || props.busy.as_ref().is_some_and(|b| b()))
    };
    let (target, set_target) = create_signal::<Option<Element>>(None);
    let input: Rc<Cell<Option<Element>>> = Rc::new(Cell::new(None));
    let confirm: Rc<dyn Fn()> = {
        let props = props.clone();
        let busy = busy.clone();
        let input = input.clone();
        Rc::new(move || {
            if busy() {
                return;
            }
            let Some(el) = input.get() else { return };
            if let Some(on_confirm) = &props.on_confirm {
                on_confirm(el.textarea_text());
            }
        })
    };
    let handle = {
        let input = input.clone();
        move || input.get().map(PromptHandle)
    };
    {
        let props = props.clone();
        let busy = busy.clone();
        let confirm = confirm.clone();
        let handle = handle.clone();
        create_layer(move || {
            let confirm = confirm.clone();
            let mut commands = vec![Command::new("dialog.prompt.submit", move || confirm()).title("Submit dialog prompt").group("Dialog")];
            for action in &props.actions {
                let run = action.on_trigger.clone();
                let handle = handle.clone();
                commands.push(
                    Command::new(&action.command, move || {
                        if let Some(handle) = handle() {
                            run(handle)
                        }
                    })
                    .title(action.command_title.clone().unwrap_or_else(|| action.title.clone()))
                    .group("Dialog"),
                );
            }
            Layer {
                mode: Some("modal"),
                target: Some(Rc::new(move || target.get())),
                enabled: Some(Enabled::Bool(target.get().is_some() && !busy())),
                // Dialog form semantics must win over the global managed textarea input layer.
                priority: Some(1),
                commands,
                ..Layer::default()
            }
        });
    }
    {
        let props = props.clone();
        let busy = busy.clone();
        create_layer(move || {
            let on_cancel = props.on_cancel.clone();
            let busy = busy.clone();
            Layer {
                mode: Some("modal"),
                enabled: Some(Enabled::Bool(props.on_cancel.is_some())),
                priority: Some(1),
                commands: vec![
                    // TODO(port-tui): `renderer.getSelection()` clears a selection first.
                    Command::inline("escape", move || {
                        if !busy() {
                            if let Some(cancel) = &on_cancel {
                                cancel();
                            }
                        }
                        true
                    })
                    .title("Back")
                    .group("Dialog"),
                ],
                ..Layer::default()
            }
        });
    }
    {
        let props = props.clone();
        let dialog = dialog.clone();
        let input = input.clone();
        let busy = busy.clone();
        on_mount(move || {
            dialog.set_size(props.size.unwrap_or_default());
            let input = input.clone();
            let busy = busy.clone();
            renderer::set_timeout(1, move || {
                let Some(el) = input.get().filter(|e| e.is_alive() && !e.is_destroyed()) else { return };
                if busy() {
                    return;
                }
                el.focus();
            });
        });
    }
    {
        let input = input.clone();
        let busy = busy.clone();
        create_effect(move || {
            let Some(el) = input.get().filter(|e| e.is_alive() && !e.is_destroyed()) else { return };
            if busy() {
                el.blur();
                return;
            }
            el.focus();
        });
    }
    let extra_actions = {
        let props = props.clone();
        let busy = busy.clone();
        let handle = handle.clone();
        Rc::new(move || {
            props
                .actions
                .iter()
                .map(|action| {
                    let run = action.on_trigger.clone();
                    let handle = handle.clone();
                    FooterAction {
                        title: action.title.clone(),
                        command: Some(action.command.clone()),
                        disabled: busy(),
                        on_trigger: Some(Rc::new(move || {
                            if let Some(handle) = handle() {
                                run(handle)
                            }
                        })),
                        ..FooterAction::default()
                    }
                })
                .collect::<Vec<_>>()
        })
    };
    let header = {
        let props = props.clone();
        let busy = busy.clone();
        let dialog = dialog.clone();
        move || {
            let title = props.title.clone();
            let subtitle = props.subtitle.clone();
            let on_cancel = props.on_cancel.clone();
            component(move || {
                let busy = busy.clone();
                let dialog = dialog.clone();
                let on_cancel = on_cancel.clone();
                dialog_header(HeaderProps {
                    title: Some(Rc::new(move || title.clone())),
                    description: subtitle.clone().map(|s| Rc::new(move || Some(s.clone())) as Rc<dyn Fn() -> Option<String>>),
                    on_close: Some(Rc::new(move || {
                        if busy() {
                            return;
                        }
                        match &on_cancel {
                            Some(cancel) => cancel(),
                            None => dialog.clear(),
                        }
                    })),
                    ..HeaderProps::default()
                })
            })
        }
    };
    let footer = {
        let extra = extra_actions.clone();
        let confirm = confirm.clone();
        let busy = busy.clone();
        FooterProps {
            actions: Some(Rc::new(move || {
                let mut primary = extra();
                primary.push(FooterAction {
                    title: "submit".into(),
                    command: Some("dialog.prompt.submit".into()),
                    disabled: busy(),
                    on_trigger: Some(confirm.clone()),
                    ..FooterAction::default()
                });
                FooterActions { secondary: Vec::new(), primary }
            })),
            ..FooterProps::default()
        }
    };
    let legacy = {
        let extra = extra_actions.clone();
        let confirm = confirm.clone();
        let busy = busy.clone();
        Rc::new(move || {
            let extra = extra.clone();
            let confirm = confirm.clone();
            let busy = busy.clone();
            show(
                move || !busy(),
                move || {
                    let extra = extra.clone();
                    let confirm = confirm.clone();
                    component(move || {
                        let extra = extra.clone();
                        let confirm = confirm.clone();
                        dialog_footer(FooterProps {
                            actions: Some(Rc::new(move || {
                                let mut secondary = vec![FooterAction {
                                    title: "submit".into(),
                                    command: Some("dialog.prompt.submit".into()),
                                    on_trigger: Some(confirm.clone()),
                                    ..FooterAction::default()
                                }];
                                secondary.extend(extra());
                                FooterActions { secondary, primary: Vec::new() }
                            })),
                            ..FooterProps::default()
                        })
                    })
                },
                Some(Rc::new(move || {
                    let t = text();
                    bind_always(move || rgba(theme.get().text.muted), move |c| t.set_fg(Some(c)));
                    insert_node(t, text_node_from_string("processing…"), None);
                    View::Node(t)
                })),
            )
        }) as Rc<dyn Fn() -> View>
    };
    let body = {
        let props = props.clone();
        let busy = busy.clone();
        let input = input.clone();
        move || {
            let el = box_();
            el.set_gap(2, Some(Value::Point(1.0)));
            if let Some(description) = &props.description {
                insert(el, description(), At::End);
            }
            let area = textarea();
            area.set_height(Some(Value::Point(1.0)));
            if let Some(view) = area.editor_view() {
                view.set_wrap_mode(opencode_tui_text::WrapMode::None);
            }
            input.set(Some(area));
            set_target.set(Some(area));
            if let Some(value) = &props.value {
                area.set_textarea_text(value);
            }
            area.set_placeholder(props.placeholder.as_deref().unwrap_or("Enter text"));
            bind_always(move || rgba(theme.get().text.muted), move |c| area.set_placeholder_color(c));
            {
                let busy = busy.clone();
                bind_always(
                    move || {
                        let t = theme.get();
                        if busy() { rgba(t.text.formfield.disabled) } else { rgba(t.text.formfield.base) }
                    },
                    move |c| {
                        area.set_text_color(c);
                        area.set_focused_text_color(c);
                    },
                );
            }
            {
                let busy = busy.clone();
                bind_always(
                    move || {
                        let t = theme.get();
                        if busy() { rgba(t.background.formfield.disabled) } else { rgba(t.text.base) }
                    },
                    move |c| area.set_cursor_color_prop(c),
                );
            }
            insert_node(el, area, None);
            let busy_text = props.busy_text.clone().unwrap_or_else(|| "Working…".into());
            insert(
                el,
                show(
                    move || busy(),
                    move || {
                        let t = text();
                        bind_always(move || rgba(theme.get().text.muted), move |c| t.set_fg(Some(c)));
                        insert_node(t, text_node_from_string(&busy_text), None);
                        View::Node(t)
                    },
                    None,
                ),
                At::End,
            );
            View::Node(el)
        }
    };
    dialog_layout(LayoutProps {
        footer_gap: Some(Rc::new(|| Some(2.0))),
        header: Box::new(header),
        footer: Some(footer),
        legacy_footer: Some(legacy),
        children: Box::new(body),
        ..LayoutProps::default()
    })
}
