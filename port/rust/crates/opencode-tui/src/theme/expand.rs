//! `packages/theme/src/tui/expand.ts`: fills in the implied tokens of a definition (`text.muted` from `text.base`,
//! every action state from the variant's base, ...). `mergeTheme` is in [`super::value::merge_theme`].

use super::schema::ACTION_STATES;
use super::value::{Rec, Value};

/// `expandTheme(definition)`.
pub fn expand_theme(definition: &Rec) -> Rec {
    let mut out = definition.clone();
    for (key, value) in expand_tokens(definition).entries() {
        out.set(key, value.clone());
    }
    if definition.get("@dialog").is_some_and(Value::truthy) {
        let dialog = definition.get("@dialog").unwrap();
        out.set("@dialog", Value::Rec(expand_dialog(dialog.as_rec().unwrap_or(&Rec::new()))));
    }
    out
}

fn expand_dialog(definition: &Rec) -> Rec {
    let mut out = expand_tokens(definition);
    if let Some(footer) = definition.get("@footer").filter(|v| v.truthy()) {
        out.set("@footer", Value::Rec(expand_tokens(footer.as_rec().unwrap_or(&Rec::new()))));
    }
    out
}

/// `expandTokens(definition)`.
pub fn expand_tokens(definition: &Rec) -> Rec {
    let mut out = definition.clone();
    out.set("text", expand_text(definition.get("text").unwrap_or(&Value::Undefined)));
    out.set("background", expand_background(definition.get("background").unwrap_or(&Value::Undefined)));
    out
}

/// `a ?? b`.
fn or(value: &Value, fallback: impl FnOnce() -> Value) -> Value {
    if matches!(value, Value::Undefined | Value::Null) { fallback() } else { value.clone() }
}

fn spread(value: &Value) -> Rec {
    value.as_rec().cloned().unwrap_or_default()
}

fn expand_text(definition: &Value) -> Value {
    if !definition.truthy() {
        return Value::Undefined;
    }
    let mut out = spread(definition);
    let base = definition.at("base");
    let muted = or(definition.at("muted"), || if base.truthy() { Value::str("$text.base") } else { Value::Undefined });
    let faint = or(definition.at("faint"), || {
        if definition.at("muted").truthy() || base.truthy() { Value::str("$text.muted") } else { Value::Undefined }
    });
    out.set("muted", muted);
    out.set("faint", faint);
    out.set("action", expand_actions(definition.at("action"), "text.action"));
    out.set("formfield", expand_formfield(definition.at("formfield"), "text.formfield"));
    let feedback = definition.at("feedback");
    let feedback = if feedback.truthy() {
        let mut entries = Rec::new();
        for (kind, item) in feedback.as_rec().map(|r| r.entries().collect::<Vec<_>>()).unwrap_or_default() {
            let mut expanded = spread(item);
            let muted = or(item.at("muted"), || {
                if item.at("base").truthy() { Value::Str(format!("$text.feedback.{kind}.base")) } else { Value::Undefined }
            });
            expanded.set("muted", muted);
            entries.set(kind, Value::Rec(expanded));
        }
        Value::Rec(entries)
    } else {
        Value::Undefined
    };
    out.set("feedback", feedback);
    Value::Rec(out)
}

fn expand_background(definition: &Value) -> Value {
    if !definition.truthy() {
        return Value::Undefined;
    }
    let mut out = spread(definition);
    out.set("action", expand_actions(definition.at("action"), "background.action"));
    out.set("formfield", expand_formfield(definition.at("formfield"), "background.formfield"));
    Value::Rec(out)
}

fn expand_formfield(definition: &Value, path: &str) -> Value {
    if !definition.at("base").truthy() {
        return definition.clone();
    }
    let mut out = spread(definition);
    for state in ACTION_STATES {
        let key = format!("${state}");
        let value = or(definition.at(&key), || Value::Str(format!("${path}.base")));
        out.set(key, value);
    }
    Value::Rec(out)
}

fn expand_actions(definition: &Value, path: &str) -> Value {
    if !definition.truthy() {
        return Value::Undefined;
    }
    let mut out = Rec::new();
    for (variant, value) in definition.as_rec().map(|r| r.entries().collect::<Vec<_>>()).unwrap_or_default() {
        if !value.at("base").truthy() {
            out.set(variant, value.clone());
            continue;
        }
        let mut expanded = spread(value);
        for state in ACTION_STATES {
            let key = format!("${state}");
            let state_value = or(value.at(&key), || Value::Str(format!("${path}.{variant}.base")));
            expanded.set(key, state_value);
        }
        out.set(variant, Value::Rec(expanded));
    }
    Value::Rec(out)
}
