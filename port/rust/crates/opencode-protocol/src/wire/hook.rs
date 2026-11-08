//! Hand-written reproductions of the custom (non-Effect) decode transformations in the TS schemas. The generator
//! recognizes each by the exact source of its TS function (port/rust/gen/src/lower.ts, `CUSTOM`) and stops when
//! upstream changes it, so these are re-checked whenever their TS counterpart changes.
//!
//! Each hook maps the already validated input JSON to the JSON the TS transformation produces, which is then
//! decoded as the target type.

use super::{Issue, Json, Object};

/// `Config.ModelSelection` (packages/schema/src/config/model.ts): a `provider/model#variant` string becomes
/// `{ providerID, model, variant? }` via `Model.Ref.parse`; objects pass through.
pub fn config_model_selection(v: &Json) -> Result<Json, Issue> {
    let Json::String(input) = v else { return Ok(v.clone()) };
    let invalid = || Issue::new(format!("Invalid model reference: {input}"));
    // Model.Ref.parse (packages/schema/src/model.ts); '/' and '#' are ASCII, so byte offsets match JS indices.
    let provider_end = input.find('/').filter(|&i| i > 0).ok_or_else(invalid)?;
    let provider = &input[..provider_end];
    let variant_start = input[provider_end + 1..].find('#').map(|i| i + provider_end + 1);
    let id = &input[provider_end + 1..variant_start.unwrap_or(input.len())];
    let variant = variant_start.map(|i| &input[i + 1..]);
    if id.is_empty() || provider.contains('#') || variant.is_some_and(|v| v.is_empty() || v.contains('#')) {
        return Err(invalid());
    }
    let mut o = Object::new();
    o.insert("providerID".into(), Json::String(provider.into()));
    o.insert("model".into(), Json::String(id.into()));
    if let Some(variant) = variant {
        o.insert("variant".into(), Json::String(variant.into()));
    }
    Ok(Json::Object(o))
}

/// `Config.Policy` (packages/schema/src/config/policy.ts): before `mcp.use`, `skill.use` and `plugin.use` existed,
/// `integration.use` covered them with a resource prefix; `{ action: "integration.use", resource: "mcp:x" }`
/// becomes `{ action: "mcp.use", resource: "x" }`.
pub fn config_policy(v: &Json) -> Result<Json, Issue> {
    let Some(o) = v.as_object() else { return Ok(v.clone()) };
    if !matches!(o.get("action"), Some(Json::String(a)) if a == "integration.use") {
        return Ok(v.clone());
    }
    let Some(Json::String(resource)) = o.get("resource") else { return Ok(v.clone()) };
    let Some(separator) = resource.find(':') else { return Ok(v.clone()) };
    let action = match &resource[..separator] {
        "mcp" => "mcp.use",
        "skill" => "skill.use",
        "plugin" => "plugin.use",
        _ => return Ok(v.clone()),
    };
    let mut out = o.clone();
    out.insert("action".into(), Json::String(action.into()));
    out.insert("resource".into(), Json::String(resource[separator + 1..].into()));
    Ok(Json::Object(out))
}
