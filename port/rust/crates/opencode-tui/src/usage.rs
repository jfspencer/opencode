//! `component/usage.tsx`: the OpenCode Console usage limits: [`use_usage_limits`], the `/usage` dialog
//! ([`dialog_usage`], [`dialog_usage_model`]), the session route's alerts as the selected model runs low
//! ([`use_usage_alerts`]) and the notice of a turn that stopped at a limit ([`use_limit_notice`],
//! [`usage_limit_notice`]).
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::text_attributes;
use opencode_tui_core::elements::{box_, span, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::renderables::text::{WrapMode, text_node_from_string};
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::Edge;
use opencode_tui_sync::json::JsonExt;

use crate::border::split_border_chars;
use crate::ctx::{Tui, tui};
use crate::theme::SurfaceName;
use crate::toast::{ToastInput, Variant};
use crate::ui::dialog::{DIALOG_LIST_INSET, HeaderProps, LayoutProps, dialog_header, dialog_layout, use_dialog};
use crate::ui::dialog_select::{Footer, SelectOption, SelectProps, dialog_select};
use crate::ui::link::{LinkProps, link};
use crate::ui::theme::use_theme;
use crate::usage_limits::{
    ModelUsage, UsageLevel, format_dollars, format_reset, go_limit_link, go_limit_name, limit_scope, model_usage,
    refusal_reset, retry_reset, served_by_console, usage_level,
};
use crate::util::rgba;

fn items(record: Option<Rc<Json>>) -> Vec<Json> {
    match record.as_deref() {
        Some(Json::Array(items)) => items.clone(),
        _ => Vec::new(),
    }
}

/// `true` for a loaded, signed-in value (`Boolean(info())`).
fn truthy(info: &Option<Rc<Json>>) -> bool {
    info.as_deref().is_some_and(|i| !matches!(i, Json::Null | Json::Bool(false)))
}

/// `useUsageLimits()`: the OpenCode Console limits on this location's models. Limits load once; `load(true)`
/// reloads them, since spend moves with every request, and keeps showing the ones already loaded meanwhile.
#[derive(Clone)]
pub struct UsageLimits {
    t: Tui,
    providers: Memo<Rc<HashMap<String, Json>>>,
}

/// `useUsageLimits()`.
pub fn use_usage_limits() -> UsageLimits {
    let t = tui();
    let data = t.data.clone();
    let providers = create_memo_equals(
        move || {
            let location = data.location.r#ref();
            Rc::new(
                items(data.store.location.provider.list(location.as_ref()))
                    .into_iter()
                    .map(|item| (item.str("id").unwrap_or_default().to_string(), item))
                    .collect::<HashMap<_, _>>(),
            )
        },
        None,
    );
    UsageLimits { t, providers }
}

impl UsageLimits {
    /// `ref()`: the location context's, else the default location (`None`).
    fn location(&self) -> Option<Json> {
        self.t.data.location.r#ref()
    }

    /// `None` until loaded, and `Some(null)` when the location isn't signed in to OpenCode Console.
    pub fn info(&self) -> Option<Rc<Json>> {
        self.t.data.store.location.usage.limits(self.location().as_ref())
    }

    /// `load(refresh)`.
    pub fn load(&self, refresh: bool) {
        let location = self.location();
        if refresh {
            self.t.data.store.location.usage.invalidate(location.as_ref());
        }
        // `void data.location.usage.sync(ref()).catch(() => undefined)`.
        let _ = self.t.data.store.location.usage.sync(location.as_ref());
    }

    /// `models()`.
    pub fn models(&self) -> Vec<Json> {
        items(self.t.data.store.location.model.list(self.location().as_ref()))
    }

    /// `usage(model)`.
    pub fn usage(&self, model: &Json) -> Option<ModelUsage> {
        let info = self.info();
        if !truthy(&info) {
            return None;
        }
        let provider = model.str("providerID").and_then(|id| self.providers.with(|p| p.get(id).cloned()));
        model_usage(info.as_deref()?, model, provider.as_ref())
    }

    /// `served(providerID)`.
    pub fn served(&self, provider_id: &str) -> bool {
        served_by_console(self.providers.with(|p| p.get(provider_id).cloned()).as_ref())
    }

    /// `unlimited(model)`: a paid model OpenCode Console serves without any limit on it.
    pub fn unlimited(&self, model: &Json) -> bool {
        truthy(&self.info())
            && self.served(model.str("providerID").unwrap_or_default())
            && !model.arr("cost").is_some_and(|c| c.iter().all(|cost| cost.num("input") == Some(0.0)))
            && self.usage(model).is_none()
    }
}

/// `limitLabel(usage, model)`: how a model's limits read where they are short on space: nothing while there's
/// plenty left. The bool is `danger`.
pub fn limit_label(usage: Option<&ModelUsage>, model: &Json) -> Option<(String, bool)> {
    let usage = usage?;
    match usage_level(usage, model) {
        UsageLevel::Plenty => None,
        UsageLevel::Reached => Some(("Limit reached".into(), true)),
        level => Some((format!("{} left", format_dollars(usage.left)), level == UsageLevel::Out)),
    }
}

/// `a.localeCompare(b)` (ICU's root collation), approximated: letters compared without case first, then lower case
/// before upper case.
fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| b.cmp(a))
}

fn model_key(model: &Json) -> String {
    format!("{}/{}", model.str("providerID").unwrap_or_default(), model.str("id").unwrap_or_default())
}

fn model_name(model: &Json) -> String {
    model.get("name").map(opencode_tui_sync::js::string).unwrap_or_else(|| "undefined".into())
}

/// `<DialogUsage>`: `/usage`, the models the workspace limits, with what each has left.
pub fn dialog_usage() -> View {
    let limits = use_usage_limits();
    let dialog = use_dialog();
    let theme = use_theme().surface(SurfaceName::Dialog);
    {
        let limits = limits.clone();
        on_mount(move || limits.load(true));
    }
    let rows = {
        let limits = limits.clone();
        create_memo_equals(
            move || {
                let mut rows: Vec<(Json, ModelUsage, UsageLevel)> = limits
                    .models()
                    .into_iter()
                    .filter_map(|model| {
                        let usage = limits.usage(&model)?;
                        let level = usage_level(&usage, &model);
                        Some((model, usage, level))
                    })
                    .collect();
                // `localeCompare` on the names.
                rows.sort_by(|a, b| locale_compare(&model_name(&a.0), &model_name(&b.0)));
                Rc::new(rows)
            },
            None,
        )
    };
    // Spend shared between models would look counted twice on every row, so only the selected one shows it.
    let (active, set_active) = create_signal(None::<String>);
    let selected = move || active.get().or_else(|| rows.with(|r| r.first().map(|row| model_key(&row.0))));
    let resets = move || rows.with(|r| r.iter().map(|row| row.1.resets_at).fold(f64::INFINITY, f64::min));
    let info_present = {
        let limits = limits.clone();
        create_memo(move || truthy(&limits.info()))
    };
    let signed_out = {
        let limits = limits.clone();
        create_memo(move || matches!(limits.info().as_deref(), Some(Json::Null)))
    };
    let header = |title: &'static str| -> Box<dyn FnOnce() -> View> {
        Box::new(move || component(move || dialog_header(HeaderProps { title: Some(Rc::new(move || title.to_string())), ..HeaderProps::default() })))
    };
    let muted = move |content: &'static str, wrap: bool| {
        let el = text();
        bind_always(move || rgba(theme.get().text.muted), move |c| el.set_fg(Some(c)));
        if wrap {
            el.set_wrap_mode(WrapMode::Word);
        }
        insert_node(el, text_node_from_string(content), None);
        View::Node(el)
    };
    switch(
        vec![
            when(move || info_present.get(), {
                let limits = limits.clone();
                move || {
                    let limits = limits.clone();
                    let dialog = dialog.clone();
                    let mut props = SelectProps::new("Usage", move || {
                        let selected = selected();
                        let t = theme.get();
                        rows.with(|rows| {
                            rows.iter()
                                .map(|(model, usage, level)| {
                                    let key = model_key(model);
                                    let mut option = SelectOption::new(model_name(model), key.clone());
                                    if selected.as_deref() == Some(key.as_str()) {
                                        option.footer = Some(Footer::Text(format!("{} left", format_dollars(usage.left))));
                                    }
                                    if matches!(level, UsageLevel::Out | UsageLevel::Reached) {
                                        option.footer_color = Some(rgba(t.text.feedback.error.base));
                                    }
                                    let (model, dialog) = (model.clone(), dialog.clone());
                                    option.on_select = Some(Rc::new(move |_| {
                                        let model = model.clone();
                                        dialog.replace(move || dialog_usage_model(model.clone()), None, None, None);
                                    }));
                                    option
                                })
                                .collect()
                        })
                    });
                    props.description = Some(Rc::new(move || {
                        let workspace = limits.info().and_then(|i| i.get("workspace").and_then(|w| w.str("name")).map(str::to_string));
                        let parts: Vec<String> = [
                            workspace.filter(|w| !w.is_empty()).map(|w| format!("Workspace: {w}")),
                            (rows.with(|r| !r.is_empty())).then(|| format!("Resets {}", format_reset(resets()))),
                        ]
                        .into_iter()
                        .flatten()
                        .collect();
                        let text = parts.join(" · ");
                        (!text.is_empty()).then_some(text)
                    }));
                    props.placeholder = Some("Search models".into());
                    props.on_move = Some(Rc::new(move |option: &SelectOption<String>| set_active.set(Some(option.value.clone()))));
                    props.empty_view = Some(Rc::new(move || {
                        let el = box_();
                        el.set_padding(Edge::Left, Some(Value::Point(DIALOG_LIST_INSET)));
                        el.set_padding(Edge::Right, Some(Value::Point(DIALOG_LIST_INSET)));
                        insert(
                            el,
                            muted("No limits apply to your models. Paid models stop only if the workspace runs out of credits.", true),
                            At::End,
                        );
                        View::Node(el)
                    }));
                    dialog_select(props)
                }
            }),
            when(move || signed_out.get(), move || {
                dialog_layout(LayoutProps {
                    header: header("Usage"),
                    children: Box::new(move || muted("Sign in to OpenCode Console with /connect to see your workspace's usage limits.", true)),
                    ..LayoutProps::default()
                })
            }),
            when(|| true, move || {
                dialog_layout(LayoutProps {
                    header: header("Usage"),
                    children: Box::new(move || muted("Loading…", false)),
                    ..LayoutProps::default()
                })
            }),
        ],
        None,
    )
}

/// `<DialogUsageModel model>`: every limit on one model, the one setting what's left marked.
pub fn dialog_usage_model(model: Json) -> View {
    let limits = use_usage_limits();
    let theme = use_theme().surface(SurfaceName::Dialog);
    let model = Rc::new(model);
    let usage = {
        let (limits, model) = (limits.clone(), model.clone());
        create_memo(move || limits.usage(&model))
    };
    let short = {
        let model = model.clone();
        move || usage.with(|u| u.as_ref().is_some_and(|u| matches!(usage_level(u, &model), UsageLevel::Out | UsageLevel::Reached)))
    };
    let workspace = {
        let limits = limits.clone();
        move || limits.info().and_then(|i| i.get("workspace").and_then(|w| w.str("name")).map(str::to_string))
    };
    let name = model_name(&model);
    let colored = move |color: Rc<dyn Fn() -> opencode_tui_core::color::RGBA>| {
        let el = text();
        bind_always(move || color(), move |c| el.set_fg(Some(c)));
        el
    };
    let muted = move || -> Rc<dyn Fn() -> opencode_tui_core::color::RGBA> { Rc::new(move || rgba(theme.get().text.muted)) };
    let base = move || -> Rc<dyn Fn() -> opencode_tui_core::color::RGBA> { Rc::new(move || rgba(theme.get().text.base)) };
    let title_name = name.clone();
    dialog_layout(LayoutProps {
        header: Box::new(move || {
            component(move || {
                let title_name = title_name.clone();
                dialog_header(HeaderProps {
                    title_view: Some(Rc::new(move || {
                        let el = colored(base());
                        let prefix = span();
                        bind_always(move || rgba(theme.get().text.muted), move |c| prefix.set_span_style(Some(c), None, 0, 0));
                        insert_node(prefix, text_node_from_string("Usage › "), None);
                        insert_node(el, prefix, None);
                        insert_node(el, text_node_from_string(&title_name), None);
                        View::Node(el)
                    })),
                    ..HeaderProps::default()
                })
            })
        }),
        children: Box::new(move || {
            let fallback_name = name.clone();
            show(
                move || usage.with(Option::is_some),
                move || {
                    let column = box_();
                    column.set_gap(2, Some(Value::Point(1.0)));
                    let summary = box_();
                    insert_node(column, summary, None);
                    let remaining = colored(Rc::new({
                        let short = short.clone();
                        move || {
                            let t = theme.get();
                            if short() { rgba(t.text.feedback.error.base) } else { rgba(t.text.base) }
                        }
                    }));
                    remaining.set_attributes(Some(text_attributes::BOLD));
                    insert(
                        remaining,
                        View::dynamic(move || View::from(format!("{} remaining", usage.with(|u| u.as_ref().map(|u| format_dollars(u.left)).unwrap_or_default())))),
                        At::Only,
                    );
                    insert_node(summary, remaining, None);
                    let reset = colored(muted());
                    insert(
                        reset,
                        View::dynamic(move || View::from(format!("Resets {}", usage.with(|u| u.as_ref().map(|u| format_reset(u.resets_at)).unwrap_or_default())))),
                        At::Only,
                    );
                    insert_node(summary, reset, None);
                    let list = box_();
                    insert_node(column, list, None);
                    let heading = colored(muted());
                    insert_node(heading, text_node_from_string("Limits"), None);
                    insert_node(list, heading, None);
                    let workspace = workspace.clone();
                    insert(
                        list,
                        index(
                            move || usage.with(|u| u.as_ref().map(|u| u.limits.clone()).unwrap_or_default()),
                            move |item, position| {
                                let row = box_();
                                let line = colored(base());
                                insert_node(row, line, None);
                                let marker = span();
                                bind_always(
                                    move || {
                                        let t = theme.get();
                                        if position > 0 {
                                            rgba(t.text.muted)
                                        } else if usage.with(|u| u.as_ref().is_some_and(|u| u.reached)) {
                                            rgba(t.text.feedback.error.base)
                                        } else {
                                            rgba(t.text.feedback.warning.base)
                                        }
                                    },
                                    move |c| marker.set_span_style(Some(c), None, 0, 0),
                                );
                                insert_node(marker, text_node_from_string(if position == 0 { "• " } else { "- " }), None);
                                insert_node(line, marker, None);
                                insert(
                                    line,
                                    View::dynamic(move || {
                                        item.with(|item| {
                                            View::from(format!(
                                                "{} used of {}",
                                                format_dollars(item.spent),
                                                format_dollars(item.budget.num("limit").unwrap_or(f64::NAN))
                                            ))
                                        })
                                    }),
                                    At::End,
                                );
                                let scope = box_();
                                scope.set_padding(Edge::Left, Some(Value::Point(2.0)));
                                insert_node(row, scope, None);
                                let scope_text = colored(muted());
                                scope_text.set_wrap_mode(WrapMode::Word);
                                let workspace = workspace.clone();
                                insert(
                                    scope_text,
                                    View::dynamic(move || View::from(item.with(|item| limit_scope(&item.budget, workspace().as_deref())))),
                                    At::Only,
                                );
                                insert_node(scope, scope_text, None);
                                View::Node(row)
                            },
                            None,
                        ),
                        At::End,
                    );
                    View::Node(column)
                },
                Some(Rc::new(move || {
                    let el = colored(muted());
                    insert_node(el, text_node_from_string(&format!("No limits apply to {fallback_name}.")), None);
                    View::Node(el)
                })),
            )
        }),
        ..LayoutProps::default()
    })
}

thread_local! {
    /// Each warning shows once per model while OpenCode runs, however many sessions use the model.
    static WARNED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// `useUsageAlerts(sessionID)`: reloads limits as the session's turns end, and warns once as the selected model runs
/// low and once at its limit.
pub fn use_usage_alerts(session_id: Rc<dyn Fn() -> String>) {
    let t = tui();
    let limits = use_usage_limits();
    let local = crate::local::local();
    {
        let limits = limits.clone();
        let unsubscribe = t.data.store.on("session.execution.succeeded", move |event| {
            if event.data().str("sessionID") == Some(session_id().as_str()) && truthy(&limits.info()) {
                limits.load(true);
            }
        });
        on_cleanup(move || unsubscribe.call());
    }
    let toast = t.toast.clone();
    create_effect(move || {
        let selected = local.model_current();
        let Some(model) = limits.models().into_iter().find(|item| {
            selected.as_ref().is_some_and(|s| item.str("providerID") == Some(s.provider_id.as_str()) && item.str("id") == Some(s.model_id.as_str()))
        }) else {
            return;
        };
        let Some(usage) = limits.usage(&model) else { return };
        let level = usage_level(&usage, &model);
        if level == UsageLevel::Plenty {
            return;
        }
        let key = format!("{}:{}", model_key(&model), if level == UsageLevel::Reached { "reached" } else { "low" });
        if !WARNED.with(|w| w.borrow_mut().insert(key)) {
            return;
        }
        let name = model_name(&model);
        untrack(|| {
            if level == UsageLevel::Reached {
                let mut input = ToastInput::new(Variant::Error, format!("Resets {}", format_reset(usage.resets_at))).title(format!("{name} limit reached"));
                input.action = Some(("Switch model".into(), Rc::new(|| crate::keymap::dispatch("model.list", None))));
                toast.show(input);
                return;
            }
            let mut input = ToastInput::new(
                Variant::Warning,
                format!("{} left · resets {}", format_dollars(usage.left), format_reset(usage.resets_at)),
            )
            .title(format!("Running low on {name}"));
            input.action = Some(("View usage".into(), Rc::new(|| crate::keymap::dispatch("usage.show", None))));
            toast.show(input);
        });
    });
}

/// What [`use_limit_notice`] gives: the title, the body, the shortcut that continues, and a link.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitNotice {
    pub title: String,
    pub body: String,
    pub key: Option<String>,
    /// `(label, href)`.
    pub link: Option<(String, String)>,
}

/// `useLimitNotice(message)`: why a turn stopped at a usage limit or for lack of credits, and how to keep going;
/// nothing for other errors.
pub fn use_limit_notice(message: Rc<dyn Fn() -> Rc<Json>>) -> Memo<Option<LimitNotice>> {
    let limits = use_usage_limits();
    let shortcut = crate::keymap::use_shortcut("model.list");
    create_memo(move || {
        let value = message();
        let error = value.get("error").filter(|e| !matches!(e, Json::Null))?;
        let time = value.get("time");
        let base = time.and_then(|t| t.num("completed")).or_else(|| time.and_then(|t| t.num("created"))).unwrap_or(f64::NAN);
        let reset = refusal_reset(Some(error)).or_else(|| retry_reset(Some(error), base));
        let resets = reset.map(|r| format!("Resets {}. ", format_reset(r))).unwrap_or_default();
        let model = value.get("model");
        let (provider_id, model_id) =
            (model.and_then(|m| m.str("providerID")).unwrap_or_default(), model.and_then(|m| m.str("id")).unwrap_or_default());
        let name = limits
            .models()
            .iter()
            .find(|item| item.str("providerID") == Some(provider_id) && item.str("id") == Some(model_id))
            .map(model_name)
            .unwrap_or_else(|| model_id.to_string());
        let key = || Some(shortcut().unwrap_or_else(|| "/models".into()));
        match error.str("type") {
            Some("provider.go-limit") => {
                let title = ["OpenCode Go".to_string()]
                    .into_iter()
                    .chain(go_limit_name(Some(error)).filter(|n| !n.is_empty()))
                    .chain(["usage limit reached".to_string()])
                    .collect::<Vec<_>>()
                    .join(" ");
                return Some(LimitNotice {
                    title,
                    body: format!("{resets}Switch to a free model with"),
                    key: key(),
                    link: Some(("Or turn on Extra Usage on your Go page to keep going now:".into(), go_limit_link(Some(error)))),
                });
            }
            Some("provider.model-budget-limit") => {
                return Some(LimitNotice { title: format!("{name} usage limit reached"), body: format!("{resets}Switch model with"), key: key(), link: None });
            }
            Some("provider.budget-limit") => {
                return Some(LimitNotice {
                    title: "Usage limit reached".into(),
                    body: format!("{resets}Paid models are paused. Switch to a free model with"),
                    key: key(),
                    link: None,
                });
            }
            _ => {}
        }
        if error.num("status") == Some(402.0) && limits.served(provider_id) {
            let workspace = limits.info().and_then(|i| i.get("workspace").and_then(|w| w.str("name")).map(str::to_string));
            return Some(LimitNotice {
                title: format!("{} is out of credits", workspace.unwrap_or_else(|| "Your workspace".into())),
                body: "Paid models are paused until an admin adds credits in OpenCode Console.".into(),
                key: None,
                link: None,
            });
        }
        None
    })
}

/// `<UsageLimitNotice notice>`.
pub fn usage_limit_notice(notice: Rc<dyn Fn() -> Option<LimitNotice>>) -> View {
    let theme = tui().theme;
    let el = box_();
    el.set_margin(Edge::Left, Some(Value::Point(2.0)));
    el.set_border(BorderProp::Sides(vec![Side::Left]));
    el.set_custom_border_chars(Some(split_border_chars()));
    bind_always(move || rgba(theme.with(|th| th.border.base)), move |c| el.set_border_color(Some(c)));
    el.set_padding(Edge::Left, Some(Value::Point(1.0)));
    el.set_padding(Edge::Top, Some(Value::Point(1.0)));
    el.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
    let field = {
        let notice = notice.clone();
        move |f: fn(&LimitNotice) -> Option<String>| {
            let notice = notice.clone();
            move || notice().as_ref().and_then(f)
        }
    };
    let title = text();
    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| title.set_fg(Some(c)));
    let title_of = field(|n| Some(n.title.clone()));
    insert(title, View::dynamic(move || View::from(title_of().unwrap_or_default())), At::Only);
    insert_node(el, title, None);
    let body = text();
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| body.set_fg(Some(c)));
    body.set_wrap_mode(WrapMode::Word);
    let body_of = field(|n| Some(n.body.clone()));
    insert(body, View::dynamic(move || View::from(body_of().unwrap_or_default())), At::End);
    let key_of = field(|n| n.key.clone());
    insert(
        body,
        show_value(
            key_of,
            move |key| {
                let key_span = span();
                bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| key_span.set_span_style(Some(c), None, 0, 0));
                insert(key_span, View::dynamic(move || View::from(key.get().unwrap_or_default())), At::Only);
                View::list([View::from(" "), View::Node(key_span), View::from(" to continue.")])
            },
            None,
        ),
        At::End,
    );
    insert_node(el, body, None);
    let link_of = {
        let notice = notice.clone();
        move || notice().and_then(|n| n.link)
    };
    insert(
        el,
        show_value(
            link_of,
            move |value| {
                let wrap = box_();
                wrap.set_padding(Edge::Top, Some(Value::Point(1.0)));
                let label = text();
                bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| label.set_fg(Some(c)));
                label.set_wrap_mode(WrapMode::Word);
                insert(label, View::dynamic(move || View::from(value.get().map(|(l, _)| l).unwrap_or_default())), At::Only);
                insert_node(wrap, label, None);
                let href = untrack(|| value.get()).map(|(_, h)| h).unwrap_or_default();
                let anchor = link(LinkProps { href, ..LinkProps::default() });
                if let View::Node(anchor) = &anchor {
                    let anchor = *anchor;
                    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| anchor.set_fg(Some(c)));
                }
                insert(wrap, anchor, At::End);
                View::Node(wrap)
            },
            None,
        ),
        At::End,
    );
    View::Node(el)
}
