//! `component/model-options.tsx`: the model pickers' ordering (favorites first, then the server's order with the
//! provider grouping), the favorite marker and the detail line (price, input modalities, context window, reasoning).
use std::collections::HashSet;
use std::rc::Rc;

use opencode_tui_core::elements::{box_, span, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::renderables::text::{WrapMode, text_node_from_string};
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{FlexDirection, Justify};

use crate::data::ModelInfo;
use crate::model_preference::{ModelRef, model_preference_key};
use crate::prompt::text::locale_compare;
use crate::theme::SurfaceName;
use crate::ui::theme::use_theme;
use crate::util::rgba;

fn point(v: f32) -> Option<Value> {
    Some(Value::Point(v))
}

/// What `sortModelOptions` reads of an option.
pub trait Ranked {
    fn provider_id(&self) -> Option<&str>;
    fn provider_name(&self) -> Option<&str>;
    fn free(&self) -> bool;
}

/// `prioritizeFavorites(options, favorites)`: the favorite options first, each group in its original order.
pub fn prioritize_favorites<T: Clone>(
    options: &[T],
    favorites: &HashSet<String>,
    key: impl Fn(&T) -> String,
) -> Vec<T> {
    let mut out = options.to_vec();
    out.sort_by(|a, b| {
        favorites
            .contains(&key(b))
            .cmp(&favorites.contains(&key(a)))
    });
    out
}

/// `sortModelOptions(options, grouped)`: the server owns model order; this only applies the picker's grouping.
pub fn sort_model_options<T: Ranked + Clone>(options: &[T], grouped: bool) -> Vec<T> {
    let mut out = options.to_vec();
    out.sort_by(|a, b| {
        let provider = if grouped {
            let go = |item: &T| i32::from(item.provider_id() == Some("opencode-go"));
            let opencode = |item: &T| i32::from(item.provider_id() == Some("opencode"));
            let first = go(b) - go(a);
            if first != 0 {
                first
            } else {
                opencode(b) - opencode(a)
            }
        } else {
            0
        };
        if provider != 0 {
            return provider.cmp(&0);
        }
        let name = if grouped {
            locale_compare(
                a.provider_name().unwrap_or(""),
                b.provider_name().unwrap_or(""),
            )
        } else {
            std::cmp::Ordering::Equal
        };
        if name != std::cmp::Ordering::Equal {
            return name;
        }
        i32::from(b.free()).cmp(&i32::from(a.free()))
    });
    out
}

/// `isFreeModel`: at least one price tier, every input price zero.
pub fn is_free_model(model: &ModelInfo) -> bool {
    !model.cost.is_empty() && model.cost.iter().all(|cost| cost.input == 0.0)
}

/// `<FavoriteMarker>`: a styled span keeps its color on the highlighted row.
pub fn favorite_marker() -> View {
    let theme = use_theme().surface(SurfaceName::Dialog);
    let el = span();
    bind_always(
        move || rgba(theme.get().text.formfield.selected),
        move |c| el.set_span_style(Some(c), None, 0, 0),
    );
    insert_node(el, text_node_from_string("*"), None);
    View::Node(el)
}

/// `ModelDetail`'s usage: the left side of the line.
#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    pub text: String,
    pub danger: bool,
}

/// `<ModelDetail model price usage>`.
pub fn model_detail(model: ModelInfo, price: Option<bool>, usage: Option<Usage>) -> View {
    let theme = use_theme().surface(SurfaceName::Dialog);
    let el = box_();
    el.set_flex_direction(FlexDirection::Row);
    match usage {
        Some(usage) => {
            el.set_justify_content(Justify::SpaceBetween);
            el.set_gap(2, point(2.0));
            let usage_el = text();
            let danger = usage.danger;
            bind_always(
                move || {
                    let t = theme.get();
                    rgba(if danger {
                        t.text.feedback.error.base
                    } else {
                        t.text.muted
                    })
                },
                move |c| usage_el.set_fg(Some(c)),
            );
            usage_el.set_wrap_mode(WrapMode::None);
            insert_node(usage_el, text_node_from_string(&usage.text), None);
            insert_node(el, usage_el, None);
            let price = price_text(&model);
            let mut items = Vec::new();
            if !price.is_empty() {
                items.push(if price == "Free" {
                    price
                } else {
                    format!("{price} per 1M")
                });
            }
            items.extend(facts(&model));
            insert(el, facts_text(items), At::End);
        }
        None => {
            el.set_justify_content(if price == Some(false) {
                Justify::FlexStart
            } else {
                Justify::SpaceBetween
            });
            el.set_gap(2, point(2.0));
            if price != Some(false) {
                let price_el = text();
                bind_always(
                    move || rgba(theme.get().text.muted),
                    move |c| price_el.set_fg(Some(c)),
                );
                price_el.set_wrap_mode(WrapMode::None);
                insert_node(price_el, text_node_from_string(&price_text(&model)), None);
                insert_node(el, price_el, None);
            }
            insert(el, facts_text(facts(&model)), At::End);
        }
    }
    View::Node(el)
}

/// The facts list, ` · ` separated: `<text fg muted wrapMode="none" flexShrink={0}>`.
fn facts_text(items: Vec<String>) -> View {
    let theme = use_theme().surface(SurfaceName::Dialog);
    let el = text();
    bind_always(
        move || rgba(theme.get().text.muted),
        move |c| el.set_fg(Some(c)),
    );
    el.set_wrap_mode(WrapMode::None);
    el.set_flex_shrink(Some(0.0));
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            let separator = span();
            bind_always(
                move || rgba(theme.get().hue.neutral[400]),
                move |c| separator.set_span_style(Some(c), None, 0, 0),
            );
            insert_node(separator, text_node_from_string(" · "), None);
            insert_node(el, separator, None);
        }
        insert_node(el, text_node_from_string(item), None);
    }
    View::Node(el)
}

fn facts(model: &ModelInfo) -> Vec<String> {
    let mut facts = Vec::new();
    let modalities = model
        .input
        .iter()
        .map(|value| modality(value))
        .collect::<Vec<_>>()
        .join(", ");
    if !modalities.is_empty() {
        facts.push(modalities);
    }
    facts.push(tokens(model.context));
    if !model.variants.is_empty() {
        facts.push("Reasoning".to_string());
    }
    facts
}

fn price_text(model: &ModelInfo) -> String {
    if is_free_model(model) {
        return "Free".to_string();
    }
    let Some(cost) = model.cost.first() else {
        return String::new();
    };
    format!("${}/${}", dollars(cost.input), dollars(cost.output))
}

/// JS `String(Number(value.toFixed(2)))`.
fn dollars(value: f64) -> String {
    number((value * 100.0).round() / 100.0)
}

/// JS `String(Number(value))` for a finite number without exponent notation.
fn number(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e21 {
        return format!("{}", value as i64);
    }
    format!("{value}")
}

fn modality(value: &str) -> String {
    if value == "pdf" {
        return "PDF".to_string();
    }
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// JS `tokens(value)`.
fn tokens(value: f64) -> String {
    if value >= 1_000_000.0 {
        return format!("{}M", number((value / 1_000_000.0 * 10.0).round() / 10.0));
    }
    if value >= 1_000.0 {
        return format!("{}K", (value / 1_000.0).round() as i64);
    }
    number(value)
}

/// `modelPreferenceKey` of an option's value.
pub fn value_key(value: &ModelRef) -> String {
    model_preference_key(value)
}

/// A footer that renders the favorite marker.
pub fn favorite_footer() -> crate::ui::dialog_select::Footer {
    crate::ui::dialog_select::Footer::View(Rc::new(favorite_marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::ModelCost;

    fn model(context: f64, input: Vec<&str>, variants: usize, cost: Vec<(f64, f64)>) -> ModelInfo {
        ModelInfo {
            id: "m".into(),
            provider_id: "p".into(),
            name: "M".into(),
            variants: (0..variants).map(|i| format!("v{i}")).collect(),
            status: "active".into(),
            input: input.into_iter().map(str::to_string).collect(),
            context,
            cost: cost
                .into_iter()
                .map(|(input, output)| ModelCost { input, output })
                .collect(),
        }
    }

    #[test]
    fn free_models_need_a_price_tier_that_is_zero() {
        assert!(!is_free_model(&model(0.0, vec![], 0, vec![])));
        assert!(is_free_model(&model(0.0, vec![], 0, vec![(0.0, 0.0)])));
        assert!(!is_free_model(&model(
            0.0,
            vec![],
            0,
            vec![(0.0, 0.0), (1.0, 0.0)]
        )));
    }

    #[test]
    fn facts_match_the_detail_line() {
        assert_eq!(tokens(1_048_576.0), "1M");
        assert_eq!(tokens(200_000.0), "200K");
        assert_eq!(tokens(999.0), "999");
        assert_eq!(modality("image/jpeg"), "Image/jpeg");
        assert_eq!(modality("pdf"), "PDF");
        assert_eq!(dollars(0.0), "0");
        assert_eq!(dollars(1.5), "1.5");
        assert_eq!(dollars(1.234), "1.23");
        assert_eq!(
            facts(&model(1_048_576.0, vec!["text", "image/jpeg"], 2, vec![])),
            vec!["Text, Image/jpeg", "1M", "Reasoning"]
        );
    }

    #[test]
    fn favorites_sort_first_without_disturbing_their_order() {
        let favorites: HashSet<String> = ["a/1".to_string()].into_iter().collect();
        let items = vec![ModelRef::new("a", "2"), ModelRef::new("a", "1")];
        let sorted = prioritize_favorites(&items, &favorites, value_key);
        assert_eq!(
            sorted,
            vec![ModelRef::new("a", "1"), ModelRef::new("a", "2")]
        );
    }
}
