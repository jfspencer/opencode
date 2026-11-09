//! The prompt's metadata row (`component/prompt/metadata.tsx`, `PromptMetadataRow`): the agent (or "Shell"), the
//! auto-accept mark, the model, its provider, the variant and the usage limit, laid out to fit; and `createFadeIn`
//! (`util/signal.ts`) for their alphas.
use std::rc::Rc;

use opencode_tui_core::color::{RGBA, text_attributes};
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, FlexDirection};

use crate::ctx::tui;
use crate::util::{fade, rgba, string_width, truncate_width};

/// What the row shows (the props TS passes, as accessors).
#[derive(Clone)]
pub struct MetadataProps {
    pub shell: Rc<dyn Fn() -> bool>,
    pub agent: Rc<dyn Fn() -> Option<String>>,
    pub auto: Rc<dyn Fn() -> bool>,
    pub model: Rc<dyn Fn() -> String>,
    pub provider: Rc<dyn Fn() -> String>,
    pub variant: Rc<dyn Fn() -> Option<String>>,
    /// What the workspace's limits leave for the model, once it is running low: (text, danger).
    pub limit: Rc<dyn Fn() -> Option<(String, bool)>>,
    pub muted: Rc<dyn Fn() -> bool>,
    pub highlight: Rc<dyn Fn() -> RGBA>,
    pub agent_alpha: Rc<dyn Fn() -> f64>,
    pub model_alpha: Rc<dyn Fn() -> f64>,
    pub variant_alpha: Rc<dyn Fn() -> f64>,
}

/// `Layout`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub agent: Option<String>,
    pub auto: bool,
    pub model: String,
    pub provider: Option<String>,
    pub variant: Option<String>,
    pub limit: Option<String>,
}

fn layout_text(input: &Layout) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(agent) = input.agent.as_ref().filter(|a| !a.is_empty()) {
        parts.push(agent.clone());
    }
    if input.auto {
        parts.push("auto".into());
    }
    if !input.model.is_empty() {
        if input.agent.as_ref().is_some_and(|a| !a.is_empty()) {
            parts.push("·".into());
        }
        parts.push(input.model.clone());
    }
    if let Some(provider) = input.provider.as_ref().filter(|p| !p.is_empty()) {
        parts.push(provider.clone());
    }
    if let Some(variant) = input.variant.as_ref().filter(|v| !v.is_empty()) {
        parts.push("·".into());
        parts.push(variant.clone());
    }
    if let Some(limit) = input.limit.as_ref().filter(|l| !l.is_empty()) {
        parts.push(format!(" {limit}"));
    }
    parts.join(" ")
}

/// `promptMetadataLayout(input)`.
#[allow(clippy::too_many_arguments)]
pub fn prompt_metadata_layout(
    width: i64,
    terminal_width: u32,
    agent: &str,
    auto: bool,
    model: &str,
    provider: &str,
    variant: Option<String>,
    limit: Option<String>,
) -> Layout {
    let agent = (terminal_width >= 44).then(|| agent.to_string());
    let provider = if terminal_width < 44 { String::new() } else { provider.to_string() };
    let last = provider.rsplit(" / ").next().unwrap_or(&provider).to_string();
    let candidates = [
        Layout { agent: agent.clone(), auto, model: model.into(), provider: Some(provider.clone()), variant: variant.clone(), limit: limit.clone() },
        Layout { agent: agent.clone(), auto: false, model: model.into(), provider: Some(provider.clone()), variant: variant.clone(), limit: limit.clone() },
        Layout { agent: agent.clone(), auto: false, model: model.into(), provider: Some(last), variant: variant.clone(), limit: limit.clone() },
        Layout { agent: agent.clone(), auto: false, model: model.into(), provider: None, variant: variant.clone(), limit: limit.clone() },
    ];
    if let Some(fit) = candidates.into_iter().find(|c| string_width(&layout_text(c)) as i64 <= width) {
        return fit;
    }
    let prefix = agent.as_ref().filter(|a| !a.is_empty()).map(|a| format!("{a} · ")).unwrap_or_default();
    let suffix = format!(
        "{}{}",
        variant.as_ref().filter(|v| !v.is_empty()).map(|v| format!(" · {v}")).unwrap_or_default(),
        limit.as_ref().filter(|l| !l.is_empty()).map(|l| format!("  {l}")).unwrap_or_default()
    );
    let truncated = truncate_width(model, (width - string_width(&prefix) as i64 - string_width(&suffix) as i64).max(9));
    // `.replace(/\s+…$/, "…")`.
    let model = match truncated.strip_suffix('…') {
        Some(rest) if rest.ends_with(crate::prompt::text::is_js_whitespace) => format!("{}…", rest.trim_end_matches(crate::prompt::text::is_js_whitespace)),
        _ => truncated,
    };
    Layout { agent, auto: false, model, provider: None, variant, limit }
}

/// `<PromptMetadataRow>`.
pub fn metadata_row(props: MetadataProps) -> View {
    let t = tui();
    let theme = t.theme;
    let dimensions = t.dimensions;
    let layout = {
        let props = props.clone();
        create_memo(move || {
            if (props.shell)() {
                return Layout { agent: Some((props.agent)().unwrap_or_else(|| "Shell".to_string())), ..Layout::default() };
            }
            let (w, _) = dimensions.get();
            prompt_metadata_layout(
                (w as i64 - if w < 44 { 9 } else { 13 }).max(0),
                w,
                &(props.agent)().unwrap_or_default(),
                (props.auto)(),
                &(props.model)(),
                &(props.provider)(),
                (props.variant)(),
                (props.limit)().map(|(text, _)| text),
            )
        })
    };
    let row = box_();
    row.set_flex_direction(FlexDirection::Row);
    row.set_gap(2, Some(Value::Point(1.0)));
    row.set_flex_grow(Some(1.0));
    row.set_flex_shrink(Some(1.0));
    row.set_min_width(Some(Value::Point(0.0)));
    let content = {
        let props = props.clone();
        let when = {
            let props = props.clone();
            move || {
                ((props.shell)() || (props.agent)().is_some_and(|a| !a.is_empty()))
                    && layout.with(|l| l.agent.as_ref().is_some_and(|a| !a.is_empty()) || !l.model.is_empty())
            }
        };
        show(
            when,
            move || {
                let props = props.clone();
                let agent = {
                    let props = props.clone();
                    show_value(
                        move || layout.with(|l| l.agent.clone().filter(|a| !a.is_empty())),
                        move |agent| {
                            let el = text();
                            let props = props.clone();
                            bind_always(move || fade((props.highlight)(), (props.agent_alpha)()), move |c| el.set_fg(Some(c)));
                            insert(el, View::dynamic(move || View::from(agent.get().unwrap_or_default())), At::Only);
                            View::Node(el)
                        },
                        None,
                    )
                };
                let auto = {
                    let props = props.clone();
                    let props_when = props.clone();
                    show(
                        move || !(props_when.shell)() && layout.with(|l| l.auto),
                        move || {
                            let el = text();
                            let props = props.clone();
                            bind_always(move || fade(rgba(theme.with(|th| th.text.muted)), (props.agent_alpha)()), move |c| el.set_fg(Some(c)));
                            insert_node(el, opencode_tui_core::renderables::text::text_node_from_string("auto"), None);
                            View::Node(el)
                        },
                        None,
                    )
                };
                let model = {
                    let props_when = props.clone();
                    let props = props.clone();
                    show(
                        move || !(props_when.shell)() && layout.with(|l| !l.model.is_empty()),
                        move || model_group(props.clone(), layout),
                        None,
                    )
                };
                View::list([agent, auto, model])
            },
            Some(Rc::new(|| {
                let el = box_();
                el.set_height(Some(Value::Point(1.0)));
                View::Node(el)
            })),
        )
    };
    insert(row, content, At::Only);
    View::Node(row)
}

fn model_group(props: MetadataProps, layout: Memo<Layout>) -> View {
    let theme = tui().theme;
    let group = box_();
    group.set_flex_direction(FlexDirection::Row);
    group.set_gap(2, Some(Value::Point(1.0)));
    group.set_flex_grow(Some(1.0));
    group.set_flex_shrink(Some(1.0));
    group.set_min_width(Some(Value::Point(0.0)));
    let dot = {
        let props = props.clone();
        show(
            move || layout.with(|l| l.agent.as_ref().is_some_and(|a| !a.is_empty())),
            move || {
                let el = text();
                let props = props.clone();
                bind_always(move || fade(rgba(theme.with(|th| th.text.muted)), (props.model_alpha)()), move |c| el.set_fg(Some(c)));
                insert(el, View::from("·"), At::Only);
                View::Node(el)
            },
            None,
        )
    };
    insert(group, dot, At::End);
    let model = text();
    insert_node(group, model, None);
    model.set_flex_shrink(Some(1.0));
    model.set_min_width(Some(Value::Point(0.0)));
    model.set_wrap_mode(WrapMode::None);
    model.set_truncate(true);
    {
        let props = props.clone();
        bind_always(
            move || {
                let base = if (props.muted)() { theme.with(|th| th.text.muted) } else { theme.with(|th| th.text.base) };
                fade(rgba(base), (props.model_alpha)())
            },
            move |c| model.set_fg(Some(c)),
        );
    }
    insert(model, View::dynamic(move || View::from(layout.with(|l| l.model.clone()))), At::Only);
    let provider = {
        let props = props.clone();
        show_value(
            move || layout.with(|l| l.provider.clone().filter(|p| !p.is_empty())),
            move |provider| {
                let el = text();
                el.set_flex_shrink(Some(0.0));
                let props = props.clone();
                bind_always(move || fade(rgba(theme.with(|th| th.text.muted)), (props.model_alpha)()), move |c| el.set_fg(Some(c)));
                insert(el, View::dynamic(move || View::from(provider.get().unwrap_or_default())), At::Only);
                View::Node(el)
            },
            None,
        )
    };
    insert(group, provider, At::End);
    let variant = {
        let props = props.clone();
        show_value(
            move || layout.with(|l| l.variant.clone().filter(|v| !v.is_empty())),
            move |variant| {
                let dot = text();
                let alpha = props.variant_alpha.clone();
                bind_always(move || fade(rgba(theme.with(|th| th.text.muted)), alpha()), move |c| dot.set_fg(Some(c)));
                insert(dot, View::from("·"), At::Only);
                let value = text();
                let alpha = props.variant_alpha.clone();
                bind_always(
                    move || fade(rgba(theme.with(|th| th.text.feedback.warning.base)), alpha()),
                    move |c| value.set_fg(Some(c)),
                );
                value.set_attributes(Some(text_attributes::BOLD));
                insert(value, View::dynamic(move || View::from(variant.get().unwrap_or_default())), At::Only);
                View::list([View::Node(dot), View::Node(value)])
            },
            None,
        )
    };
    insert(group, variant, At::End);
    let limit = {
        let props_when = props.clone();
        show_value(
            move || layout.with(|l| l.limit.is_some()).then(|| (props_when.limit)()).flatten(),
            move |limit| {
                let el = text();
                el.set_flex_shrink(Some(0.0));
                el.set_margin(Edge::Left, Some(Value::Point(1.0)));
                el.set_wrap_mode(WrapMode::None);
                let alpha = props.model_alpha.clone();
                bind_always(
                    move || {
                        let danger = limit.with(|l| l.as_ref().is_some_and(|(_, danger)| *danger));
                        let base = if danger { theme.with(|th| th.text.feedback.error.base) } else { theme.with(|th| th.text.muted) };
                        fade(rgba(base), alpha())
                    },
                    move |c| el.set_fg(Some(c)),
                );
                insert(el, View::dynamic(move || View::from(limit.get().map(|(text, _)| text).unwrap_or_default())), At::Only);
                View::Node(el)
            },
            None,
        )
    };
    insert(group, limit, At::End);
    View::Node(group)
}

/// `createFadeIn(show, enabled)`: 1 once shown (a 160 ms smoothstep tween the first time, when animations are on),
/// 0 while hidden.
pub fn create_fade_in(show: impl Fn() -> bool + 'static, enabled: impl Fn() -> bool + 'static) -> Memo<f64> {
    let alpha = create_signal(if untrack(&show) { 1.0f64 } else { 0.0 }).0;
    let revealed = Rc::new(std::cell::Cell::new(untrack(&show)));
    let generation = Rc::new(std::cell::Cell::new(0u64));
    create_effect(move || {
        let visible = show();
        let animate = enabled();
        untrack(|| {
            generation.set(generation.get() + 1);
            if !visible {
                alpha.set(0.0);
                return;
            }
            if !animate || revealed.get() {
                revealed.set(true);
                alpha.set(1.0);
                return;
            }
            revealed.set(true);
            let started = std::time::Instant::now();
            let from = alpha.get_untracked();
            let current = generation.get();
            step(alpha, from, started, current, generation.clone());
        });
    });
    create_memo(move || alpha.get())
}

fn step(alpha: Signal<f64>, from: f64, started: std::time::Instant, current: u64, generation: Rc<std::cell::Cell<u64>>) {
    renderer::set_timeout(16, move || {
        if generation.get() != current || !alpha.is_alive() {
            return;
        }
        let progress = (started.elapsed().as_secs_f64() / 0.16).min(1.0);
        let eased = progress * progress * (3.0 - 2.0 * progress);
        alpha.set(from + (1.0 - from) * eased);
        renderer::request_render();
        if progress < 1.0 {
            step(alpha, from, started, current, generation);
        }
    });
}
