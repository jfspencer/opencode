//! Providers and models (packages/core/src/provider.ts, model.ts, variant.ts, aisdk-native.ts and the plugins that fill
//! them: config/plugin/provider.ts, config/plugin/policy.ts, plugin/provider/openrouter.ts), as they come out with no
//! model catalog (`OPENCODE_DISABLE_MODELS_FETCH` and an empty `OPENCODE_MODELS_PATH`, the reference setup): every
//! provider and model is defined in config `providers`.
//!
//! A configured provider starts from `Provider.Info.empty` and takes `activation: "enabled"` (so it is available with or
//! without a credential); each configured model starts from `Model.Info.default` and takes the config fields, then its
//! variants (from `capabilities.reasoning`, else the package's default effort levels, then config `variants`). Policies
//! (`provider.use`) remove denied providers. A model as listed is merged with its provider: the provider's package,
//! compatibility and settings (minus the provider-only `timeout`, `headerTimeout`, `chunkTimeout`, `transport`), the
//! provider's headers and body under the model's.
//!
//! The OpenRouter plugin's `HTTP-Referer` / `X-Title` headers apply only to providers that exist before config is
//! applied (catalog providers); with no catalog they never apply, as in TS.
//!
//! TODO(port): catalog providers and models (models.dev) and `canonical` providers copying a catalog source; the
//! provider plugins other than OpenRouter (all out of scope); variant spellings of packages other than
//! `@opencode/ai/providers/openrouter`; the `api_keys` header translation of legacy OpenRouter settings.

use opencode_protocol::types as t;
use opencode_protocol::wire::Object;
use opencode_protocol::{Json, Wire};

use super::config::{self, Entry};
use super::wildcard::wildcard_match;
use crate::json::{self, Obj};

const OPENROUTER: &str = "@opencode/ai/providers/openrouter";

/// The provider and model registries of a location.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// Every provider record left after policies, in registration order (`Provider.get` reads these).
    pub providers: Vec<t::ProviderInfo>,
    /// Chat models of the available providers, merged with their provider, newest release first (`Model.all`).
    pub models: Vec<t::ModelInfo>,
    /// Enabled models of other types (image, decision...), newest release first (`Model.available(types)`).
    pub other: Vec<t::ModelInfo>,
    /// The configured default model (`model`), as configured.
    pub default: Option<(String, String)>,
}

impl Catalog {
    /// Available providers (`Provider.available`): enabled ones (every configured provider is).
    pub fn available(&self) -> impl Iterator<Item = &t::ProviderInfo> {
        self.providers.iter().filter(|p| matches!(p.activation, t::ProviderInfoActivation::Enabled))
    }

    /// Enabled models (`Model.available` for chat).
    pub fn enabled(&self) -> impl Iterator<Item = &t::ModelInfo> {
        self.models.iter().filter(|m| m.enabled)
    }

    /// `Model.default`: the configured default when it is enabled, else the first enabled model that reads and writes
    /// text.
    pub fn default_model(&self) -> Option<&t::ModelInfo> {
        let configured = self
            .default
            .as_ref()
            .and_then(|(provider, model)| self.models.iter().find(|m| m.provider_id.0 == *provider && m.id == *model));
        if let Some(model) = configured.filter(|m| m.enabled) {
            return Some(model);
        }
        self.enabled().find(|m| supports_text(&m.capabilities))
    }
}

/// `Model.supportsText`.
pub fn supports_text(capabilities: &t::ModelCapabilities) -> bool {
    (capabilities.input.is_empty() || capabilities.input.iter().any(|i| i.starts_with("text")))
        && (capabilities.output.is_empty() || capabilities.output.iter().any(|o| o.starts_with("text")))
}

fn is_record(v: &Json) -> bool {
    matches!(v, Json::Object(_))
}

/// `Provider.mergeOverlay`: a deep merge of JSON records (the overlay's records merge into the base's; any other
/// overlay value replaces). `None` when both are absent.
pub fn merge_overlay(base: Option<&Json>, overlay: Option<&Json>) -> Option<Json> {
    match (base, overlay) {
        (None, None) => None,
        (None, Some(o)) => Some(o.clone()),
        (Some(b), None) => Some(b.clone()),
        (Some(b), Some(o)) => Some(merge_record(b, o)),
    }
}

fn merge_record(base: &Json, overlay: &Json) -> Json {
    let (Json::Object(b), Json::Object(o)) = (base, overlay) else { return overlay.clone() };
    let mut out = Object::new();
    for key in b.keys().chain(o.keys()) {
        if out.contains_key(key) {
            continue;
        }
        let value = match (b.get(key), o.get(key)) {
            (left, None) => left.cloned().unwrap_or_default(),
            (Some(left), Some(right)) if is_record(left) && is_record(right) => merge_record(left, right),
            (_, Some(right)) => right.clone(),
        };
        out.insert(key.clone(), value);
    }
    Json::Object(crate::inventory::jsonc::js_order(out))
}

/// `Provider.mergeHeaders`: names compare case-insensitively; a later header keeps the first one's position and takes
/// its own spelling and value.
pub fn merge_headers(base: Option<&Json>, overlay: Option<&Json>) -> Option<Json> {
    match (base, overlay) {
        (None, None) => None,
        (None, Some(o)) => Some(o.clone()),
        (Some(b), None) => Some(b.clone()),
        (Some(Json::Object(b)), Some(Json::Object(o))) => {
            let mut merged: Vec<(String, (String, Json))> = vec![];
            for (k, v) in b.iter().chain(o.iter()) {
                let lower = k.to_lowercase();
                match merged.iter_mut().find(|(l, _)| *l == lower) {
                    Some(entry) => entry.1 = (k.clone(), v.clone()),
                    None => merged.push((lower, (k.clone(), v.clone()))),
                }
            }
            let mut out = Object::new();
            for (_, (k, v)) in merged {
                out.insert(k, v);
            }
            Some(Json::Object(crate::inventory::jsonc::js_order(out)))
        }
        (_, Some(o)) => Some(o.clone()),
    }
}

/// `Provider.modelSettings`: the settings a model inherits (without the provider-only keys).
fn model_settings(settings: Option<&Json>) -> Option<Json> {
    let Some(Json::Object(s)) = settings else { return settings.cloned() };
    let mut out = s.clone();
    for key in ["timeout", "headerTimeout", "chunkTimeout", "transport"] {
        out.shift_remove(key);
    }
    Some(Json::Object(out))
}

/// `AISDKNative.rewrite` of a native OpenRouter target: AI SDK spellings in `settings` move to the request (`headers`,
/// `extraBody`, `appName`, `appUrl`), unread ones are dropped.
fn rewrite(target: &mut Object, specifier: Option<&str>) {
    if specifier != Some(OPENROUTER) {
        // TODO(port): the other native packages and `aisdk:` specifiers.
        return;
    }
    let settings = target.get("settings").cloned();
    let Some(Json::Object(settings)) = settings else { return };
    let legacy = ["headers", "extraBody", "useCompletionUrls", "appName", "appUrl", "api_keys", "compatibility"];
    if !settings.keys().any(|k| legacy.contains(&k.as_str())) {
        return;
    }
    let mut kept = settings.clone();
    for key in legacy {
        kept.shift_remove(key);
    }
    let mut headers = settings.get("headers").filter(|h| is_record(h)).cloned();
    let mut extra = Object::new();
    if let Some(Json::String(name)) = settings.get("appName") {
        extra.insert("X-OpenRouter-Title".into(), Json::String(name.clone()));
    }
    if let Some(Json::String(url)) = settings.get("appUrl") {
        extra.insert("HTTP-Referer".into(), Json::String(url.clone()));
    }
    if !extra.is_empty() {
        headers = merge_headers(Some(&Json::Object(extra)), headers.as_ref());
    }
    target.insert("settings".into(), Json::Object(kept));
    if let Some(merged) = merge_headers(headers.as_ref(), target.get("headers")) {
        target.insert("headers".into(), merged);
    }
    if let Some(body) = merge_overlay(settings.get("extraBody"), target.get("body")) {
        target.insert("body".into(), body);
    }
}

/// `Variant.supports`: the reasoning controls of a config `capabilities.reasoning`.
fn supports(reasoning: &Json) -> Vec<Support> {
    if !matches!(json::get(reasoning, "supported"), Some(Json::Bool(true))) {
        return vec![];
    }
    let mut out = vec![];
    if matches!(json::get(reasoning, "toggle"), Some(Json::Bool(true))) {
        out.push(Support::Toggle);
    }
    if let Some(Json::Array(efforts)) = json::get(reasoning, "effort") {
        let values = efforts.iter().filter_map(|e| match e {
            Json::String(s) if s != "default" => Some(s.clone()),
            _ => None,
        });
        out.push(Support::Effort(Some(values.collect())));
    }
    if let Some(budget) = json::get(reasoning, "budget") {
        let int = |key| match json::get(budget, key) {
            Some(Json::Number(n)) => Some(*n),
            _ => None,
        };
        out.push(Support::Budget { min: int("min"), max: int("max") });
    }
    out
}

#[derive(Debug, Clone)]
enum Support {
    Effort(Option<Vec<String>>),
    Toggle,
    Budget { min: Option<f64>, max: Option<f64> },
}

fn variant(id: &str, settings: Json) -> Json {
    let mut o = Obj::new();
    o.str("id", id).set("settings", settings);
    o.done()
}

fn reasoning(key: &str, value: Json) -> Json {
    let mut inner = Obj::new();
    inner.set(key, value);
    let mut o = Obj::new();
    o.set("reasoning", inner.done());
    o.done()
}

/// The OpenRouter protocol's variants for one control (`openrouter` in variant.ts).
fn openrouter(model: &Object, support: &Support) -> Vec<Json> {
    match support {
        Support::Effort(values) => {
            let defaults = ["low", "medium", "high"].map(String::from).to_vec();
            values
                .clone()
                .unwrap_or(defaults)
                .iter()
                .map(|e| variant(e, reasoning("effort", Json::String(e.clone()))))
                .collect()
        }
        Support::Toggle => vec![
            variant("none", reasoning("enabled", Json::Bool(false))),
            variant("thinking", reasoning("enabled", Json::Bool(true))),
        ],
        Support::Budget { min, max } => {
            let output = match model.get("limit").and_then(|l| json::get(l, "output")) {
                Some(Json::Number(n)) => *n,
                _ => 0.0,
            };
            let maximum = max.unwrap_or(output - 1.0).min(output - 1.0);
            if maximum <= 0.0 {
                return vec![];
            }
            let high = min.unwrap_or(0.0).max(((maximum + 1.0) / 2.0).floor()).min(maximum);
            vec![
                variant("high", reasoning("max_tokens", Json::Number(high))),
                variant("max", reasoning("max_tokens", Json::Number(maximum))),
            ]
        }
    }
}

/// `Variant.resolve`.
fn resolve_variants(model: &Object, package: Option<&str>, supports: &[Support]) -> Vec<Json> {
    if package != Some(OPENROUTER) {
        return vec![];
    }
    let toggle = if supports.iter().any(|s| matches!(s, Support::Toggle)) { openrouter(model, &Support::Toggle) } else { vec![] };
    let effort = supports.iter().find(|s| matches!(s, Support::Effort(_)));
    let budget = supports.iter().find(|s| matches!(s, Support::Budget { .. }));
    let main = match (effort, budget) {
        (Some(e), _) => openrouter(model, e),
        (None, Some(b)) => openrouter(model, b),
        _ => toggle.clone(),
    };
    let mut out: Vec<Json> = vec![];
    for v in toggle.into_iter().filter(|v| json::get_str(v, "id") == Some("none")).chain(main) {
        if !out.iter().any(|o| json::get_str(o, "id") == json::get_str(&v, "id")) {
            out.push(v);
        }
    }
    out
}

/// `Model.Info.default`.
fn default_model(provider: &str, id: &str) -> Object {
    let mut capabilities = Obj::new();
    capabilities.set("tools", Json::Bool(true));
    capabilities.set("input", Json::Array(vec![Json::String("text".into()), Json::String("image".into())]));
    capabilities.set("output", Json::Array(vec![Json::String("text".into())]));
    let mut time = Obj::new();
    time.int("released", 0);
    let mut limit = Obj::new();
    limit.int("context", 200_000).int("output", 32_000);
    let mut o = Obj::new();
    o.str("id", id).str("modelID", id).str("providerID", provider).str("name", id);
    o.set("capabilities", capabilities.done()).set("variants", Json::Array(vec![])).set("time", time.done());
    o.set("cost", Json::Array(vec![])).str("status", "active").set("enabled", Json::Bool(true)).set("limit", limit.done());
    let Json::Object(o) = o.done() else { unreachable!() };
    o
}

/// `Date.parse` of a `YYYY-MM-DD` or `YYYY-MM` release date (UTC midnight).
fn release(date: &str) -> f64 {
    let parts: Vec<i64> = date.split('-').filter_map(|p| p.parse().ok()).collect();
    let (y, m, d) = match parts.as_slice() {
        [y, m, d] => (*y, *m, *d),
        [y, m] => (*y, *m, 1),
        _ => return f64::NAN,
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return f64::NAN;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    ((era * 146_097 + doe - 719_468) * 86_400_000) as f64
}

struct Record {
    provider: Object,
    models: Vec<(String, Object)>,
}

/// Builds the providers and models of a location's config entries.
pub fn build(entries: &[Entry]) -> Catalog {
    let configured: Vec<(&String, &t::ConfigProviderPlain)> =
        config::documents(entries).flat_map(|d| d.providers.iter().flat_map(|p| p.0.iter())).collect();

    // Provider transform (config/plugin/provider.ts).
    let mut records: Vec<(String, Record)> = vec![];
    for (id, item) in &configured {
        let item_json = json::of(*item);
        let index = match records.iter().position(|(rid, _)| rid == *id) {
            Some(i) => i,
            None => {
                let mut provider = Obj::new();
                provider.str("id", id).str("name", id).str("activation", "auto").str("package", "");
                let Json::Object(provider) = provider.done() else { unreachable!() };
                records.push(((*id).clone(), Record { provider, models: vec![] }));
                records.len() - 1
            }
        };
        let record = &mut records[index].1;
        let provider = &mut record.provider;
        provider.insert("activation".into(), Json::String("enabled".into()));
        if let Some(canonical) = &item.canonical {
            provider.insert("canonical".into(), Json::String(canonical.0.clone()));
        }
        if let Some(name) = item.name.as_ref().filter(|n| !n.is_empty()) {
            provider.insert("name".into(), Json::String(name.clone()));
        }
        if let Some(package) = &item.package {
            provider.insert("package".into(), Json::String(package.clone()));
        }
        if let Some(compatibility) = json::get(&item_json, "compatibility") {
            let merged = merge_shallow(provider.get("compatibility"), compatibility);
            provider.insert("compatibility".into(), merged);
        }
        for key in ["settings", "body"] {
            if let Some(overlay) = json::get(&item_json, key) {
                if let Some(merged) = merge_overlay(provider.get(key), Some(overlay)) {
                    provider.insert(key.into(), merged);
                }
            }
        }
        if let Some(overlay) = json::get(&item_json, "headers") {
            if let Some(merged) = merge_headers(provider.get("headers"), Some(overlay)) {
                provider.insert("headers".into(), merged);
            }
        }
        let specifier = json::get_str(&Json::Object(provider.clone()), "package").map(str::to_owned);
        rewrite(provider, specifier.as_deref());
        for (model_id, _) in item.models.iter().flat_map(|m| m.0.iter()) {
            if !record.models.iter().any(|(m, _)| m == model_id) {
                record.models.push((model_id.clone(), default_model(id, model_id)));
            }
        }
    }
    for (id, item) in &configured {
        for (model_id, model) in item.models.iter().flat_map(|m| m.0.iter()) {
            let Some(limit) = &model.limit else { continue };
            let Some((_, record)) = records.iter_mut().find(|(rid, _)| rid == *id) else { continue };
            if let Some((_, m)) = record.models.iter_mut().find(|(m, _)| m == model_id) {
                let merged = merge_shallow(m.get("limit"), &json::of(limit));
                m.insert("limit".into(), merged);
            }
        }
    }

    // Policy transform (config/plugin/policy.ts): denied providers are removed.
    let statements = statements(entries);
    records.retain(|(id, _)| decision(&statements, "provider.use", id) != "deny");

    // Model transform (config/plugin/provider.ts): configured models, in config order.
    for (id, item) in &configured {
        let Some((_, record)) = records.iter_mut().find(|(rid, _)| rid == *id) else { continue };
        let provider_package = json::get_str(&Json::Object(record.provider.clone()), "package").map(str::to_owned);
        for (model_id, config) in item.models.iter().flat_map(|m| m.0.iter()) {
            let config_json = json::of(config);
            let position = record.models.iter().position(|(m, _)| m == model_id);
            let had_variants = position.is_some_and(|i| matches!(record.models[i].1.get("variants"), Some(Json::Array(v)) if !v.is_empty()));
            let index = match position {
                Some(i) => i,
                None => {
                    record.models.push((model_id.clone(), default_model(id, model_id)));
                    record.models.len() - 1
                }
            };
            let model = &mut record.models[index].1;
            overlay(model, &config_json, provider_package.as_deref());
            let reasoning = json::get(&config_json, "capabilities").and_then(|c| json::get(c, "reasoning"));
            if config.variants.is_none() && reasoning.is_none() && !had_variants {
                let package = json::get_str(&Json::Object(model.clone()), "package").map(str::to_owned).or(provider_package.clone());
                let variants = resolve_variants(model, package.as_deref(), &[Support::Effort(None)]);
                model.insert("variants".into(), Json::Array(variants));
            }
        }
    }

    let default = config::latest(entries, |info| info.model.as_ref()).map(|m| (m.provider_id.clone(), m.model.clone()));

    // Materialization (Model snapshot): each model merged with its provider.
    let mut models: Vec<t::ModelInfo> = vec![];
    let mut other: Vec<t::ModelInfo> = vec![];
    for (_, record) in &records {
        let provider = Json::Object(record.provider.clone());
        for (_, model) in &record.models {
            let mut merged = model.clone();
            if let Some(canonical) = record.provider.get("canonical") {
                merged.insert("canonical".into(), canonical.clone());
            }
            if !merged.contains_key("package") {
                if let Some(p) = record.provider.get("package") {
                    merged.insert("package".into(), p.clone());
                }
            }
            if let Some(c) = record.provider.get("compatibility") {
                let value = match model.get("compatibility") {
                    Some(m) => merge_shallow(Some(c), m),
                    None => c.clone(),
                };
                merged.insert("compatibility".into(), value);
            }
            let settings = merge_overlay(model_settings(json::get(&provider, "settings")).as_ref(), model_settings(model.get("settings")).as_ref());
            set_opt(&mut merged, "settings", settings);
            set_opt(&mut merged, "headers", merge_headers(json::get(&provider, "headers"), model.get("headers")));
            set_opt(&mut merged, "body", merge_overlay(json::get(&provider, "body"), model.get("body")));
            // Non-chat models (image, decision...) are only listed by type, enabled ones only.
            let chat = json::get_str(&Json::Object(merged.clone()), "type").is_none_or(|t| t == "chat");
            match t::ModelInfo::decode(&Json::Object(merged)) {
                Ok(info) if chat => models.push(info),
                Ok(info) => {
                    if info.enabled {
                        other.push(info);
                    }
                }
                Err(issue) => eprintln!("model {}/{} skipped: {issue}", record.provider.get("id").map(json::text).unwrap_or_default(), json::text(model.get("id").unwrap_or(&Json::Null))),
            }
        }
    }
    // Newest release first; a stable sort keeps registration order among equals.
    models.sort_by(|a, b| b.time.released.partial_cmp(&a.time.released).unwrap_or(std::cmp::Ordering::Equal));
    other.sort_by(|a, b| b.time.released.partial_cmp(&a.time.released).unwrap_or(std::cmp::Ordering::Equal));

    let providers = records
        .iter()
        .filter_map(|(_, r)| t::ProviderInfo::decode(&Json::Object(r.provider.clone())).ok())
        .collect();
    Catalog { providers, models, other, default }
}

fn set_opt(target: &mut Object, key: &str, value: Option<Json>) {
    match value {
        Some(v) => {
            target.insert(key.into(), v);
        }
        None => {
            target.shift_remove(key);
        }
    }
}

/// `{ ...base, ...overlay }`.
fn merge_shallow(base: Option<&Json>, overlay: &Json) -> Json {
    let mut out = match base {
        Some(Json::Object(b)) => b.clone(),
        _ => Object::new(),
    };
    if let Json::Object(o) = overlay {
        for (k, v) in o {
            out.insert(k.clone(), v.clone());
        }
    }
    Json::Object(crate::inventory::jsonc::js_order(out))
}

/// `overlay` in config/plugin/provider.ts for a model without a catalog base.
fn overlay(model: &mut Object, config: &Json, provider_package: Option<&str>) {
    let get = |key: &str| json::get(config, key);
    if let Some(v) = get("type") {
        model.insert("type".into(), v.clone());
    }
    if let Some(v) = get("family") {
        model.insert("family".into(), v.clone());
    }
    if let Some(Json::String(name)) = get("name").filter(|n| !matches!(n, Json::String(s) if s.is_empty())) {
        model.insert("name".into(), Json::String(name.clone()));
    }
    if let Some(v) = get("modelID") {
        model.insert("modelID".into(), v.clone());
    }
    if let Some(Json::String(date)) = get("releaseDate") {
        let mut time = Obj::new();
        time.set("released", Json::Number(release(date)));
        model.insert("time".into(), time.done());
    }
    if let Some(c) = get("compatibility") {
        let merged = merge_shallow(model.get("compatibility"), c);
        model.insert("compatibility".into(), merged);
    }
    if let Some(v) = get("package") {
        model.insert("package".into(), v.clone());
    }
    if let Some(v) = get("settings") {
        set_opt(model, "settings", merge_overlay(model.get("settings"), Some(v)));
    }
    if let Some(v) = get("headers") {
        set_opt(model, "headers", merge_headers(model.get("headers"), Some(v)));
    }
    if let Some(v) = get("body") {
        set_opt(model, "body", merge_overlay(model.get("body"), Some(v)));
    }
    if let Some(c) = get("capabilities") {
        // `Model.mergeCapabilities`: unset fields fall back to the base model's (or the defaults).
        let base = model.get("capabilities").cloned().unwrap_or_default();
        let pick = |key: &str| json::get(c, key).or_else(|| json::get(&base, key)).cloned();
        let mut caps = Obj::new();
        caps.opt("tools", pick("tools")).opt("input", pick("input")).opt("output", pick("output"));
        caps.opt("temperature", pick("temperature"));
        model.insert("capabilities".into(), caps.done());
    }
    if let Some(v) = get("limit") {
        let merged = merge_shallow(model.get("limit"), v);
        model.insert("limit".into(), merged);
    }
    let package = json::get_str(&Json::Object(model.clone()), "package").map(str::to_owned).or(provider_package.map(str::to_owned));
    rewrite(model, package.as_deref());

    // Variants are spelled for the final package.
    if let Some(reasoning) = get("capabilities").and_then(|c| json::get(c, "reasoning")) {
        let variants = resolve_variants(model, package.as_deref(), &supports(reasoning));
        model.insert("variants".into(), Json::Array(variants));
    }
    if let Some(Json::Array(configured)) = get("variants") {
        let mut variants = match model.get("variants") {
            Some(Json::Array(v)) => v.clone(),
            _ => vec![],
        };
        for item in configured {
            let Some(id) = json::get_str(item, "id") else { continue };
            let index = match variants.iter().position(|v| json::get_str(v, "id") == Some(id)) {
                Some(i) => i,
                None => {
                    let mut o = Obj::new();
                    o.str("id", id);
                    variants.push(o.done());
                    variants.len() - 1
                }
            };
            let Json::Object(existing) = &mut variants[index] else { continue };
            if let Some(v) = json::get(item, "settings") {
                set_opt(existing, "settings", merge_overlay(existing.get("settings"), Some(v)));
            }
            if let Some(v) = json::get(item, "headers") {
                set_opt(existing, "headers", merge_headers(existing.get("headers"), Some(v)));
            }
            if let Some(v) = json::get(item, "body") {
                set_opt(existing, "body", merge_overlay(existing.get("body"), Some(v)));
            }
        }
        model.insert("variants".into(), Json::Array(variants));
    }
    if let Some(cost) = get("cost") {
        let list = match cost {
            Json::Array(items) => items.clone(),
            other => vec![other.clone()],
        };
        let costs = list
            .iter()
            .map(|c| {
                let mut cache = Obj::new();
                let zero = || Json::Number(0.0);
                let cached = json::get(c, "cache");
                cache.set("read", cached.and_then(|x| json::get(x, "read")).cloned().unwrap_or_else(zero));
                cache.set("write", cached.and_then(|x| json::get(x, "write")).cloned().unwrap_or_else(zero));
                let mut o = Obj::new();
                o.opt("tier", json::get(c, "tier").cloned());
                o.opt("input", json::get(c, "input").cloned()).opt("output", json::get(c, "output").cloned());
                o.set("cache", cache.done());
                o.done()
            })
            .collect();
        model.insert("cost".into(), Json::Array(costs));
    }
    if let Some(Json::Bool(disabled)) = get("disabled") {
        model.insert("enabled".into(), Json::Bool(!disabled));
    }
}

/// One policy statement (`ManagedPolicy.statements`): every document's policies, the latest-loaded document first.
#[derive(Debug, Clone)]
pub struct Statement {
    pub action: String,
    pub resource: String,
    pub effect: String,
}

pub fn statements(entries: &[Entry]) -> Vec<Statement> {
    config::documents(entries)
        .rev()
        .flat_map(|d| d.policies.iter().flatten())
        .map(|p| {
            let v = json::of(p);
            Statement {
                action: json::get_str(&v, "action").unwrap_or_default().to_owned(),
                resource: json::get_str(&v, "resource").unwrap_or_default().to_owned(),
                effect: json::get_str(&v, "effect").unwrap_or_default().to_owned(),
            }
        })
        .collect()
}

/// `ManagedPolicy.decision`: the effect of the last matching statement (the earliest-loaded document's), else allow.
pub fn decision<'a>(statements: &'a [Statement], action: &str, resource: &str) -> &'a str {
    statements
        .iter()
        .rev()
        .find(|s| s.action == action && wildcard_match(resource, &s.resource))
        .map(|s| s.effect.as_str())
        .unwrap_or("allow")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference configuration (port/conformance/lib/model.ts), providers and policies.
    const REFERENCE: &str = r##"{"model":"openrouter/deepseek/deepseek-v4.1-flash-1017","providers":{"openrouter":{"name":"OpenRouter","env":["OPENROUTER_API_KEY"],"package":"@opencode/ai/providers/openrouter","settings":{"baseURL":"https://openrouter.ai/api/v1"},"models":{"deepseek/deepseek-v4.1-flash-1017":{"name":"DeepSeek V4.1 Flash 1017","family":"deepseek-flash","package":"@opencode/ai/providers/openrouter","capabilities":{"tools":true,"input":["text","image/jpeg","image/png","image/gif","image/webp"],"output":["text"],"temperature":true,"reasoning":{"supported":true,"effort":["none","low","high","max"]}},"limit":{"context":1048576,"output":393216}}}}},"policies":[{"action":"provider.use","resource":"*","effect":"deny"},{"action":"provider.use","resource":"openrouter","effect":"allow"}]}"##;

    #[test]
    fn reference_configuration() {
        let info = config::parse_info(REFERENCE).expect("valid");
        let catalog = build(&[Entry::Document { path: None, info }]);
        // Recorded from the TS reference (GET /api/provider, GET /api/model).
        assert_eq!(
            opencode_protocol::to_string(&catalog.providers),
            r#"[{"id":"openrouter","name":"OpenRouter","activation":"enabled","package":"@opencode/ai/providers/openrouter","settings":{"baseURL":"https://openrouter.ai/api/v1"}}]"#
        );
        assert_eq!(
            opencode_protocol::to_string(&catalog.models),
            r#"[{"id":"deepseek/deepseek-v4.1-flash-1017","modelID":"deepseek/deepseek-v4.1-flash-1017","providerID":"openrouter","family":"deepseek-flash","name":"DeepSeek V4.1 Flash 1017","package":"@opencode/ai/providers/openrouter","settings":{"baseURL":"https://openrouter.ai/api/v1"},"capabilities":{"tools":true,"input":["text","image/jpeg","image/png","image/gif","image/webp"],"output":["text"],"temperature":true},"variants":[{"id":"none","settings":{"reasoning":{"effort":"none"}}},{"id":"low","settings":{"reasoning":{"effort":"low"}}},{"id":"high","settings":{"reasoning":{"effort":"high"}}},{"id":"max","settings":{"reasoning":{"effort":"max"}}}],"time":{"released":0},"cost":[],"status":"active","enabled":true,"limit":{"context":1048576,"output":393216}}]"#
        );
        assert_eq!(catalog.default_model().map(|m| m.id.as_str()), Some("deepseek/deepseek-v4.1-flash-1017"));
    }
}
