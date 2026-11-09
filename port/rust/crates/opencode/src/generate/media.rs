//! Image and decision generation on OpenRouter's media routes (packages/core/src/generate.ts `image` / `decisions`,
//! packages/ai: providers/openrouter.ts `image` and `decision`, protocols/openai-images.ts, protocols/system-one.ts,
//! route/media.ts, media.ts).
//!
//! An image request goes to `<baseURL>/images/generations` as JSON: input images as `input_references` data URLs,
//! sizing as `aspect_ratio` and a `resolution` tier, the model's provider options and body overlay merged in. The JSON
//! answer's images come back inline: base64 results are decoded (typed by `output_format`, else their bytes), data URLs
//! keep their type, other URLs are downloaded. A decision request goes to `<baseURL>/systemone`: the questions keyed
//! by name, the input as `state` (media as chat-style parts), and the answers are mapped back in question order.
//!
//! Both retry only failures the provider never ran (`retryOneShot`, at most three times); failures map to the API's
//! typed errors as text generation's do.

use opencode_protocol::Json;

use crate::http::{Failure, Result};
use crate::json::{self, Obj};
use crate::llm::error::{self, AIError, HttpContext, Kind};
use crate::llm::http;
use crate::session::retry::Retry;

const IMAGES_PACKAGE: &str = "@opencode/ai/providers/openrouter/images";
const DECISIONS_PACKAGE: &str = "@opencode/ai/providers/openrouter/decisions";
const IMAGES: &str = "OpenAI Images";
const SYSTEM_ONE: &str = "System One";

/// Settings `Provider.nativeSettings` keeps from a package, and the connection keys `toConfig` takes out.
const NOT_OPTIONS: &[&str] = &[
    "baseURL", "apiKey", "headers", "body", "encoding", "timeout", "headerTimeout", "chunkTimeout", "compaction", "cache", "transport",
];

/// A resolved media model (`resolveImage` / `resolveDecision` up to the route's deployment).
pub struct Model {
    provider: String,
    model_id: String,
    base_url: String,
    api_key: Option<String>,
    headers: Vec<(String, String)>,
    /// Provider options (the settings the route does not read itself).
    options: Json,
    /// The body overlay (`http.body`).
    body: Option<Json>,
    /// `compatibility.mediaInput` of a decision model.
    media_input: Option<String>,
}

fn invalid(message: &str, field: Option<&str>) -> Failure {
    Failure::invalid(message, field, None)
}

/// `loadMedia`: the enabled model of that type with the reference, at its variant, from the package of that type; the
/// 422 or 404 of `missing` when there is none.
pub fn resolve(inventory: &crate::inventory::Inventory, reference: &Json, capability: &str) -> std::result::Result<Model, Failure> {
    let provider = json::get_str(reference, "providerID").unwrap_or_default();
    let id = json::get_str(reference, "id").unwrap_or_default();
    let info = inventory
        .catalog
        .other
        .iter()
        .find(|m| json::get_str(&json::of(*m), "type") == Some(capability) && m.provider_id.0 == provider && m.id == id);
    let Some(info) = info else { return Err(super::missing_media(inventory, reference, capability)) };
    let variant = json::get_str(reference, "variant");
    let model = crate::inventory::providers::with_variant(info, variant).map_err(|_| {
        invalid(&format!("Variant unavailable for {provider}/{id}: {}", variant.unwrap_or_default()), Some("model.variant"))
    })?;
    let model = json::of(&model);
    let package = json::get_str(&model, "package");
    let expected = if capability == "image" { IMAGES_PACKAGE } else { DECISIONS_PACKAGE };
    if package != Some(expected) {
        let mut o = Obj::new();
        o.str("_tag", "UnsupportedModelError").str("providerID", provider).str("modelID", id).str("capability", capability);
        o.str(
            "message",
            &format!("{provider}/{id} has no {capability} API opencode can call (package {})", package.unwrap_or("unknown")),
        );
        return Err(Failure::new(422, o.done()));
    }
    let settings = json::get(&model, "settings").cloned().unwrap_or_else(json::object);
    let mut options = json::object();
    if let Json::Object(fields) = &settings {
        for (key, value) in fields.iter().filter(|(key, _)| !NOT_OPTIONS.contains(&key.as_str())) {
            json::set(&mut options, key, value.clone());
        }
    }
    let headers = match json::get(&model, "headers") {
        Some(Json::Object(headers)) => headers.iter().filter_map(|(k, v)| json::as_str(v).map(|v| (k.clone(), v.to_owned()))).collect(),
        _ => vec![],
    };
    Ok(Model {
        provider: provider.to_owned(),
        model_id: json::get_str(&model, "modelID").unwrap_or(id).to_owned(),
        base_url: json::get_str(&settings, "baseURL").unwrap_or("https://openrouter.ai/api/v1").to_owned(),
        api_key: crate::credential::api_key(inventory, provider, json::get_str(&settings, "apiKey")),
        headers,
        options,
        body: json::get(&model, "body").filter(|b| matches!(b, Json::Object(o) if !o.is_empty())).cloned(),
        media_input: json::get(&model, "compatibility").and_then(|c| json::get_str(c, "mediaInput")).map(str::to_owned),
    })
}

/// `mergeJsonRecords`: records merge key by key; any other overlay value replaces.
fn merge(base: &mut Json, overlay: &Json) {
    let Json::Object(fields) = overlay else { return };
    for (key, value) in fields {
        match (json::get_mut(base, key), value) {
            (Some(existing @ Json::Object(_)), Json::Object(_)) => merge(existing, value),
            _ => json::set(base, key, value.clone()),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------------------------------------------------

/// What a media exchange answered: the response's HTTP context and its body.
struct Answer {
    context: HttpContext,
    body: Vec<u8>,
}

async fn read(mut response: http::Response, url: &str) -> std::result::Result<Vec<u8>, AIError> {
    let mut out = Vec::new();
    while let Some(chunk) = response.body.next().await {
        match chunk {
            Ok(bytes) => out.extend_from_slice(&bytes),
            Err(failure) => return Err(error::read_failure(&failure, Some(response.status), url)),
        }
    }
    Ok(out)
}

/// One request through the executor: a status of 400 or more is the provider's error.
async fn exchange(method: &str, url: &str, headers: &[(String, String)], body: String) -> std::result::Result<Answer, AIError> {
    let response = http::send(method, url, headers, body).await.map_err(|failure| error::transport(&failure, "request", None, url))?;
    let status = response.status;
    let response_headers = response.headers.clone();
    let body = read(response, url).await?;
    if status >= 400 {
        let text = String::from_utf8_lossy(&body).into_owned();
        return Err(error::http_status(status, &response_headers, Some(&text), url));
    }
    Ok(Answer { context: HttpContext { url: url.to_owned(), status, headers: response_headers }, body })
}

/// POSTs a JSON body to the route's endpoint, with the deployment's headers and bearer auth.
async fn post(model: &Model, path: &str, body: &Json) -> std::result::Result<Answer, AIError> {
    let url = format!("{}{path}", model.base_url.trim_end_matches('/'));
    let mut headers: Vec<(String, String)> = Vec::new();
    headers.extend(model.headers.iter().cloned());
    if let Some(key) = &model.api_key {
        headers.push(("Authorization".into(), format!("Bearer {key}")));
    }
    headers.push(("Content-Type".into(), "application/json".into()));
    exchange("POST", &url, &headers, json::text(body)).await
}

/// An invalid answer (`frameError`): the provider's output, with the response's HTTP context.
fn output_error(message: impl Into<String>, context: &HttpContext) -> AIError {
    AIError { http: Some(context.clone()), ..AIError::output(message, None) }
}

/// `retryOneShot`: a failure the provider never ran is retried, at most three times.
async fn retried<T, F, Fut>(mut attempt: F) -> std::result::Result<T, AIError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, AIError>>,
{
    let mut retry = Retry::default();
    let mut retries = 0;
    loop {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(error) => match retry.decide(&error, super::not_run(&error)) {
                Some(decision) if decision.retry && retries < 3 => {
                    retries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(decision.delay as u64)).await;
                }
                _ => return Err(error),
            },
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Media
// ---------------------------------------------------------------------------------------------------------------------

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Effect's `Base64.decode`: line breaks are dropped, the length must be a multiple of 4, padding may only end it.
fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let stripped: Vec<u8> = input.bytes().filter(|b| *b != b'\n' && *b != b'\r').collect();
    let length = stripped.len();
    if length % 4 != 0 {
        return None;
    }
    if let Some(index) = stripped.iter().position(|b| *b == b'=') {
        if index + 2 < length || (index + 2 == length && stripped[length - 1] != b'=') {
            return None;
        }
    }
    let code = |byte: u8| -> Option<u32> {
        if byte == b'=' {
            return Some(0);
        }
        ALPHABET.iter().position(|c| *c == byte).map(|p| p as u32)
    };
    let missing = if stripped.ends_with(b"==") { 2 } else if stripped.ends_with(b"=") { 1 } else { 0 };
    let mut out = Vec::with_capacity(3 * (length / 4));
    for quad in stripped.chunks(4) {
        let buffer = code(quad[0])? << 18 | code(quad[1])? << 12 | code(quad[2])? << 6 | code(quad[3])?;
        out.extend_from_slice(&[(buffer >> 16) as u8, (buffer >> 8) as u8, buffer as u8]);
    }
    out.truncate(3 * (length / 4) - missing);
    Some(out)
}

fn encode_base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `detectMediaType` (ai/src/utils/media-type.ts): a type from leading magic bytes, of the containers media routes
/// commonly return.
fn detect(bytes: &[u8]) -> Option<&'static str> {
    let starts = |prefix: &[u8]| bytes.starts_with(prefix);
    let ascii = |start: usize, end: usize| bytes.get(start..end).map(|b| b.iter().map(|c| *c as char).collect::<String>());
    if starts(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if starts(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if starts(&[0x47, 0x49, 0x46, 0x38]) {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && ascii(0, 4).as_deref() == Some("RIFF") {
        match ascii(8, 12).as_deref() {
            Some("WEBP") => return Some("image/webp"),
            Some("WAVE") => return Some("audio/wav"),
            _ => {}
        }
    }
    if starts(&[0x25, 0x50, 0x44, 0x46]) {
        return Some("application/pdf");
    }
    if bytes.len() >= 12 && ascii(4, 8).as_deref() == Some("ftyp") {
        let brand = ascii(8, 12).unwrap_or_default();
        let box_end = (u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize).min(bytes.len());
        let count = box_end.saturating_sub(16) / 4;
        let compatible: Vec<String> = (0..count).filter_map(|i| ascii(16 + i * 4, 20 + i * 4)).collect();
        let generic = brand == "mif1" || brand == "msf1";
        if brand == "avif" || brand == "avis" || (generic && compatible.iter().any(|b| b == "avif" || b == "avis")) {
            return Some("image/avif");
        }
        if generic || ["heic", "heix", "hevc", "hevx"].contains(&brand.as_str()) {
            return Some("image/heic");
        }
        return Some(match brand.as_str() {
            "qt  " => "video/quicktime",
            "M4A " => "audio/mp4",
            _ => "video/mp4",
        });
    }
    if starts(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return Some("video/webm");
    }
    if starts(&[0x49, 0x44, 0x33]) {
        return Some("audio/mpeg");
    }
    // An 11-bit frame sync; layer bits `00` mark AAC ADTS, any other layer is MPEG audio.
    if bytes.len() >= 2 && bytes[0] == 0xff && (bytes[1] & 0xe0) == 0xe0 {
        return Some(if bytes[1] & 0x06 == 0 { "audio/aac" } else { "audio/mpeg" });
    }
    if starts(&[0x4f, 0x67, 0x67, 0x53]) {
        return Some("audio/ogg");
    }
    if starts(&[0x66, 0x4c, 0x61, 0x43]) {
        return Some("audio/flac");
    }
    None
}

/// `kindOf`.
fn kind(mime: &str) -> &'static str {
    let lower = mime.to_lowercase();
    if lower.starts_with("image/") {
        "image"
    } else if lower.starts_with("video/") {
        "video"
    } else if lower.starts_with("audio/") {
        "audio"
    } else if lower == "application/pdf" || lower.starts_with("text/") {
        "document"
    } else {
        "other"
    }
}

fn data_url(mime: &str, base64: &str) -> String {
    format!("data:{};base64,{base64}", mime.to_lowercase())
}

/// An image result before materialization (`Media.Asset`).
enum Asset {
    /// Decoded bytes, typed as declared, else by their bytes.
    Bytes { data: Vec<u8>, mime: String },
    /// A data URL's payload, kept as written.
    Base64 { data: String, mime: String },
    /// A URL to download, with the declared type.
    Url { url: String, mime: Option<String> },
}

/// `Media.parseDataUrl`: `data:<mime>(;param)*;base64,<data>`, case-insensitively.
fn parse_data_url(value: &str) -> Option<(String, String)> {
    let rest = value.get(..5).filter(|p| p.eq_ignore_ascii_case("data:")).map(|_| &value[5..])?;
    let comma = rest.find(',')?;
    let (head, data) = (&rest[..comma], &rest[comma + 1..]);
    let mime_end = head.find(';')?;
    let mime = &head[..mime_end];
    if mime.is_empty() || mime.contains(',') {
        return None;
    }
    let params = &head[mime_end..];
    if !params.to_ascii_lowercase().ends_with(";base64") {
        return None;
    }
    Some((mime.to_owned(), data.to_owned()))
}

// ---------------------------------------------------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------------------------------------------------

const RESOLUTIONS: &[&str] = &["512", "768", "1K", "1.5K", "2K", "4K"];

/// The `/images/generations` body (`OpenAIImages.fromRequest` with OpenRouter's `references` input and sizing).
fn image_body(model: &Model, payload: &Json) -> Json {
    let mut body = Obj::new();
    body.str("model", &model.model_id).str("prompt", json::get_str(payload, "prompt").unwrap_or_default());
    if let Some(Json::Array(images)) = json::get(payload, "images").filter(|i| matches!(i, Json::Array(a) if !a.is_empty())) {
        let references = images.iter().map(|file| {
            let mut url = Obj::new();
            url.str("url", &data_url(json::get_str(file, "mime").unwrap_or_default(), json::get_str(file, "data").unwrap_or_default()));
            let mut o = Obj::new();
            o.str("type", "image_url").set("image_url", url.done());
            o.done()
        });
        body.set("input_references", Json::Array(references.collect()));
    }
    let mut body = body.done();
    let mut fields = Obj::new();
    fields.opt("n", json::get(payload, "n").cloned());
    fields.opt("aspect_ratio", json::get(payload, "aspect").cloned());
    let resolution = json::get_str(payload, "resolution").map(|r| match r.to_uppercase().as_str() {
        "0.5K" => "512".to_owned(),
        "0.75K" => "768".to_owned(),
        other => other.to_owned(),
    });
    fields.opt_str("resolution", resolution.as_deref().filter(|r| RESOLUTIONS.contains(r)));
    let mut fields = fields.done();
    // `nativeOptions`: camel-cased output options renamed, preview options dropped.
    let mut native = Obj::new();
    native.opt("output_compression", json::get(&model.options, "outputCompression").cloned());
    native.opt("output_format", json::get(&model.options, "outputFormat").cloned());
    let mut native = native.done();
    if let Json::Object(options) = &model.options {
        for (key, value) in options {
            if !["outputCompression", "outputFormat", "partialImages", "mask", "response_format"].contains(&key.as_str()) {
                json::set(&mut native, key, value.clone());
            }
        }
        if let Some(format) = options.get("response_format").filter(|v| !matches!(v, Json::Null)) {
            json::set(&mut native, "response_format", format.clone());
        }
    }
    merge(&mut fields, &native);
    if let Some(overlay) = &model.body {
        merge(&mut fields, overlay);
    }
    merge(&mut body, &fields);
    body
}

fn optional_string(value: &Json, key: &str) -> bool {
    matches!(json::get(value, key), None | Some(Json::String(_)))
}

fn nullable_string(value: &Json, key: &str) -> bool {
    matches!(json::get(value, key), None | Some(Json::Null) | Some(Json::String(_)))
}

/// `OpenAIImageResponse`'s schema.
fn valid_images(value: &Json) -> bool {
    let Some(Json::Array(data)) = json::get(value, "data") else { return false };
    let usage_valid = match json::get(value, "usage") {
        None => true,
        Some(usage @ Json::Object(_)) => {
            ["input_tokens", "output_tokens", "total_tokens"].iter().all(|k| matches!(json::get(usage, k), None | Some(Json::Number(_))))
                && ["input_tokens_details", "output_tokens_details"].iter().all(|k| matches!(json::get(usage, k), None | Some(Json::Object(_))))
        }
        Some(_) => false,
    };
    matches!(value, Json::Object(_))
        && data.iter().all(|item| matches!(item, Json::Object(_)) && nullable_string(item, "b64_json") && nullable_string(item, "url"))
        && ["output_format", "size", "quality", "background"].iter().all(|k| optional_string(value, k))
        && usage_valid
}

/// `onDocument`: the images of a JSON answer and what the provider says it rendered.
fn image_answer(answer: &Answer) -> std::result::Result<(Vec<Asset>, Json), AIError> {
    let invalid = |message: String| output_error(message, &answer.context);
    let decoded = std::str::from_utf8(&answer.body).ok().and_then(crate::json::parse).filter(valid_images);
    let Some(decoded) = decoded else { return Err(invalid(format!("{IMAGES} returned an invalid response"))) };
    let format = json::get_str(&decoded, "output_format");
    let declared = format.map(|f| format!("image/{f}"));
    let mut assets = Vec::new();
    for (index, item) in json::array(json::get(&decoded, "data").unwrap_or(&Json::Null)).unwrap_or_default().iter().enumerate() {
        let label = format!("{IMAGES} result {index}");
        let b64 = json::get_str(item, "b64_json").filter(|s| !s.is_empty());
        let url = json::get_str(item, "url").filter(|s| !s.is_empty());
        let asset = if let Some(b64) = b64 {
            let data = decode_base64(b64).ok_or_else(|| invalid(format!("{label} contains invalid base64 data")))?;
            let mime = declared.clone().or_else(|| detect(&data).map(str::to_owned)).unwrap_or_else(|| "application/octet-stream".into());
            Asset::Bytes { data, mime }
        } else if let Some(url) = url.filter(|u| u.starts_with("data:")) {
            let (mime, data) = parse_data_url(url).ok_or_else(|| invalid(format!("{label} has an invalid data URL")))?;
            Asset::Base64 { data, mime }
        } else if let Some(url) = url {
            Asset::Url { url: url.to_owned(), mime: declared.clone() }
        } else {
            return Err(invalid(format!("{label} has neither image data nor a URL")));
        };
        assets.push(asset);
    }
    if assets.is_empty() {
        return Err(invalid(format!("{IMAGES} returned no images")));
    }
    let mut metadata = Obj::new();
    for (key, field) in [("outputFormat", "output_format"), ("size", "size"), ("quality", "quality"), ("background", "background")] {
        metadata.opt_str(key, json::get_str(&decoded, field));
    }
    let mut usage = Obj::new();
    if let Some(reported) = json::get(&decoded, "usage") {
        usage.opt("input", json::get(reported, "input_tokens").cloned());
        usage.opt("output", json::get(reported, "output_tokens").cloned());
    }
    let mut result = Obj::new();
    result.set("tokens", usage.done()).set("native", metadata.done());
    Ok((assets, result.done()))
}

/// `asset.materialize()` then `base64()`: the image inline, a URL downloaded (typed as declared, else by its bytes).
async fn materialize(asset: Asset) -> std::result::Result<Json, AIError> {
    let (data, mime) = match asset {
        Asset::Bytes { data, mime } => (encode_base64(&data), mime),
        Asset::Base64 { data, mime } => (data, mime),
        Asset::Url { url, mime } => {
            let downloaded = exchange("GET", &url, &[], String::new()).await?;
            let mime = mime.or_else(|| detect(&downloaded.body).map(str::to_owned)).unwrap_or_else(|| "application/octet-stream".into());
            (encode_base64(&downloaded.body), mime)
        }
    };
    let mut o = Obj::new();
    o.str("type", "image").str("data", &data).str("mime", &mime);
    Ok(o.done())
}

/// `Generate.image`.
pub async fn image(model: &Model, payload: &Json) -> Result<Json> {
    let body = image_body(model, payload);
    let answer = retried(|| async { post(model, "/images/generations", &body).await.and_then(|answer| image_answer(&answer)) })
        .await
        .map_err(|error| super::provider_error(&model.provider, &error))?;
    let (assets, details) = answer;
    let mut images = Vec::with_capacity(assets.len());
    for asset in assets {
        images.push(materialize(asset).await.map_err(|error| super::provider_error(&model.provider, &error))?);
    }
    let mut result = Obj::new();
    result.set("images", Json::Array(images));
    if let Some(tokens) = json::get(&details, "tokens").filter(|t| matches!(t, Json::Object(o) if !o.is_empty())) {
        result.set("tokens", tokens.clone());
    }
    let mut native = Obj::new();
    native.set(&model.provider, json::get(&details, "native").cloned().unwrap_or_else(json::object));
    result.set("native", native.done());
    Ok(result.done())
}

// ---------------------------------------------------------------------------------------------------------------------
// Decisions
// ---------------------------------------------------------------------------------------------------------------------

fn invalid_request(message: String) -> AIError {
    AIError::new(Kind::InvalidRequest, message)
}

/// `String(value)` of a choice value.
fn choice_key(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        Json::Bool(flag) => flag.to_string(),
        other => json::text(other),
    }
}

/// `encodeQuestions`: questions keyed by name; choice options are map keys, so a boolean and a string with the same
/// text cannot both be one.
fn encode_questions(questions: &[Json]) -> std::result::Result<Json, AIError> {
    let mut out = json::object();
    for question in questions {
        let name = json::get_str(question, "name").unwrap_or_default();
        let instructions = json::get_str(question, "instructions").unwrap_or_default();
        let mut o = Obj::new();
        match json::get_str(question, "type") {
            Some("predicate") => {
                o.str("type", "noul").str("instructions", instructions);
            }
            Some("score") => {
                let criteria = json::array(json::get(question, "levels").unwrap_or(&Json::Null)).unwrap_or_default().iter().map(|level| {
                    let label = json::get_str(level, "label").unwrap_or_default();
                    match json::get_str(level, "description") {
                        Some(description) => json::string(&format!("{label}: {description}")),
                        None => json::string(label),
                    }
                });
                o.str("type", "score").str("instructions", instructions).set("criteria", Json::Array(criteria.collect()));
            }
            _ => {
                let choices = json::array(json::get(question, "choices").unwrap_or(&Json::Null)).unwrap_or_default();
                let keys: Vec<String> = choices.iter().map(|c| choice_key(json::get(c, "value").unwrap_or(&Json::Null))).collect();
                let unique: std::collections::HashSet<&String> = keys.iter().collect();
                if unique.len() != keys.len() {
                    return Err(invalid_request(format!(
                        "{SYSTEM_ONE} choice options are strings; \"{name}\" has a boolean and a string with the same text"
                    )));
                }
                let mut criteria = Obj::new();
                for (key, choice) in keys.iter().zip(choices.iter()) {
                    criteria.set(key, json::get(choice, "description").cloned().unwrap_or(Json::Null));
                }
                o.str("type", "choice").str("instructions", instructions).set("criteria", criteria.done());
            }
        }
        json::set(&mut out, name, o.done());
    }
    Ok(out)
}

fn plain(part: &Json) -> Json {
    match json::get_str(part, "type") {
        Some("text") => json::string(json::get_str(part, "text").unwrap_or_default()),
        _ => json::get(part, "value").cloned().unwrap_or(Json::Null),
    }
}

/// `UnsupportedOperationError` (`decision.media`): the API answers it as an invalid request with its message.
fn unsupported(_provider: &str, mime: &str, accepted: &str) -> AIError {
    AIError::new(Kind::UnsupportedOperation, format!("{SYSTEM_ONE} takes {accepted} here; got {mime}"))
}

/// A media part as `state` takes it: images and video as URL parts, audio as `input_audio`.
fn state_item(provider: &str, part: &Json) -> std::result::Result<Json, AIError> {
    if json::get_str(part, "type") != Some("file") {
        return Ok(plain(part));
    }
    let mime = json::get_str(part, "mime").unwrap_or_default();
    let data = json::get_str(part, "data").unwrap_or_default();
    let mut o = Obj::new();
    let mut inner = Obj::new();
    match kind(mime) {
        "image" => {
            inner.str("url", &data_url(mime, data));
            o.str("type", "image_url").set("image_url", inner.done());
        }
        "video" => {
            inner.str("url", &data_url(mime, data));
            o.str("type", "video_url").set("video_url", inner.done());
        }
        "audio" => {
            inner.str("data", data).str("format", &mime.to_lowercase()["audio/".len()..]);
            o.str("type", "input_audio").set("input_audio", inner.done());
        }
        _ => return Err(unsupported(provider, mime, "images, audio and video")),
    }
    Ok(o.done())
}

/// A lone text or JSON part is the state itself; several are a list.
fn lone(mut items: Vec<Json>) -> Json {
    if items.len() == 1 { items.remove(0) } else { Json::Array(items) }
}

/// `placeMedia`: where the input goes, by the model's `mediaInput` (OpenRouter's decisions take media in `state`).
fn place_media(model: &Model, input: &[Json]) -> std::result::Result<Obj, AIError> {
    let media: Vec<&Json> = input.iter().filter(|p| json::get_str(p, "type") == Some("file")).collect();
    let mut placed = Obj::new();
    if media.is_empty() {
        placed.set("state", lone(input.iter().map(plain).collect()));
        return Ok(placed);
    }
    match model.media_input.as_deref().unwrap_or("state") {
        "state" => {
            let items = input.iter().map(|part| state_item(&model.provider, part)).collect::<std::result::Result<Vec<_>, _>>()?;
            placed.set("state", Json::Array(items));
        }
        "root" => {
            let lists = [("image", "images"), ("audio", "audio"), ("video", "videos")];
            if let Some(unplaced) = media.iter().find(|m| !lists.iter().any(|(k, _)| *k == kind(json::get_str(m, "mime").unwrap_or_default()))) {
                return Err(unsupported(&model.provider, json::get_str(unplaced, "mime").unwrap_or_default(), "images, audio and video"));
            }
            placed.set("state", lone(input.iter().filter(|p| json::get_str(p, "type") != Some("file")).map(plain).collect()));
            for (k, field) in lists {
                let items: Vec<Json> = media
                    .iter()
                    .filter(|m| kind(json::get_str(m, "mime").unwrap_or_default()) == k)
                    .map(|m| json::string(&data_url(json::get_str(m, "mime").unwrap_or_default(), json::get_str(m, "data").unwrap_or_default())))
                    .collect();
                if !items.is_empty() {
                    placed.set(field, Json::Array(items));
                }
            }
        }
        _ => {
            if let Some(unplaced) = media.iter().find(|m| kind(json::get_str(m, "mime").unwrap_or_default()) != "image") {
                return Err(unsupported(&model.provider, json::get_str(unplaced, "mime").unwrap_or_default(), "images"));
            }
            // Every occurrence of a reference string in the body is replaced with its image.
            let mut next = 0;
            let mut assets = Obj::new();
            let items: Vec<Json> = input
                .iter()
                .map(|part| {
                    if json::get_str(part, "type") != Some("file") {
                        return plain(part);
                    }
                    next += 1;
                    let reference = format!("<image-{next}>");
                    let mime = json::get_str(part, "mime").unwrap_or_default().to_lowercase();
                    let mut asset = Obj::new();
                    asset.str("type", "image").str("format", &mime["image/".len()..]).str("data_base64", json::get_str(part, "data").unwrap_or_default());
                    assets.set(&reference, asset.done());
                    json::string(&reference)
                })
                .collect();
            placed.set("state", Json::Array(items)).set("assets", assets.done());
        }
    }
    Ok(placed)
}

fn probability(value: Option<&Json>) -> bool {
    matches!(value, Some(Json::Number(n)) if (0.0..=1.0).contains(n))
}

fn probabilities(value: Option<&Json>) -> bool {
    match value {
        Some(Json::Object(fields)) => fields.values().all(|v| probability(Some(v))),
        _ => false,
    }
}

/// `SystemOne.Response`'s schema.
fn valid_decision(value: &Json) -> bool {
    let answers = match json::get(value, "answers") {
        Some(Json::Object(answers)) => answers.values().all(|answer| match json::get_str(answer, "type") {
            Some("noul") => probability(json::get(answer, "noul")),
            Some("choice") => {
                matches!(json::get(answer, "choice"), Some(Json::String(_)))
                    && probabilities(json::get(answer, "probabilities"))
                    && probability(json::get(answer, "confidence"))
            }
            Some("score") => {
                matches!(json::get(answer, "score"), Some(Json::Number(_)))
                    && probabilities(json::get(answer, "probabilities"))
                    && probability(json::get(answer, "confidence"))
            }
            _ => false,
        }),
        _ => false,
    };
    let usage = match json::get(value, "usage") {
        None => true,
        Some(usage @ Json::Object(_)) => {
            ["input_tokens", "output_tokens", "cost"].iter().all(|k| matches!(json::get(usage, k), None | Some(Json::Number(_))))
        }
        Some(_) => false,
    };
    let metadata = match json::get(value, "provider_metadata") {
        None => true,
        Some(Json::Object(fields)) => fields.values().all(|v| matches!(v, Json::Object(_))),
        Some(_) => false,
    };
    matches!(json::get(value, "model"), Some(Json::String(_)))
        && answers
        && usage
        && optional_string(value, "id")
        && optional_string(value, "provider")
        && metadata
}

/// A JS object's own keys in enumeration order: array-index keys ascending, then the rest as inserted.
fn js_entries(fields: &opencode_protocol::wire::Object) -> Vec<(&String, &Json)> {
    let index = |key: &str| -> Option<u32> {
        if key.is_empty() || (key.len() > 1 && key.starts_with('0')) || !key.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        key.parse::<u32>().ok().filter(|n| *n != u32::MAX)
    };
    let mut indexed: Vec<(u32, (&String, &Json))> = fields.iter().filter_map(|(k, v)| index(k).map(|i| (i, (k, v)))).collect();
    indexed.sort_by_key(|(i, _)| *i);
    let mut out: Vec<(&String, &Json)> = indexed.into_iter().map(|(_, entry)| entry).collect();
    out.extend(fields.iter().filter(|(k, _)| index(k).is_none()));
    out
}

/// `toAnswer`: an answer in the question's terms (choice values typed again, score levels labelled).
fn to_answer(question: &Json, answer: &Json) -> Json {
    let name = json::get_str(question, "name").unwrap_or_default();
    let mut o = Obj::new();
    o.str("name", name);
    let entries = match json::get(answer, "probabilities") {
        Some(Json::Object(fields)) => js_entries(fields),
        _ => vec![],
    };
    match json::get_str(answer, "type") {
        Some("noul") => {
            o.str("type", "predicate").set("probability", json::get(answer, "noul").cloned().unwrap_or(Json::Null));
        }
        Some("choice") => {
            let choices = if json::get_str(question, "type") == Some("choice") {
                json::array(json::get(question, "choices").unwrap_or(&Json::Null)).unwrap_or_default()
            } else {
                &[]
            };
            let value = |key: &str| {
                choices
                    .iter()
                    .find(|c| choice_key(json::get(c, "value").unwrap_or(&Json::Null)) == key)
                    .and_then(|c| json::get(c, "value").cloned())
                    .unwrap_or_else(|| json::string(key))
            };
            let rank = |key: &str| {
                choices.iter().position(|c| choice_key(json::get(c, "value").unwrap_or(&Json::Null)) == key).unwrap_or(choices.len())
            };
            let mut sorted = entries.clone();
            sorted.sort_by_key(|(key, _)| rank(key));
            let probabilities = sorted.iter().map(|(key, probability)| {
                let mut p = Obj::new();
                p.set("value", value(key)).set("probability", (*probability).clone());
                p.done()
            });
            o.str("type", "choice")
                .set("choice", value(json::get_str(answer, "choice").unwrap_or_default()))
                .set("probabilities", Json::Array(probabilities.collect()))
                .set("confidence", json::get(answer, "confidence").cloned().unwrap_or(Json::Null));
        }
        _ => {
            let levels = if json::get_str(question, "type") == Some("score") {
                json::array(json::get(question, "levels").unwrap_or(&Json::Null)).unwrap_or_default()
            } else {
                &[]
            };
            let mut scored: Vec<(f64, Json)> = entries
                .iter()
                .map(|(key, probability)| {
                    // `Number(key)`: level indices in practice.
                    let value = if key.trim().is_empty() { 0.0 } else { key.trim().parse::<f64>().unwrap_or(f64::NAN) };
                    let label = (value.fract() == 0.0 && value >= 0.0)
                        .then(|| levels.get(value as usize))
                        .flatten()
                        .and_then(|level| json::get_str(level, "label"))
                        .unwrap_or(key.as_str());
                    let mut p = Obj::new();
                    p.set("value", Json::Number(value)).str("label", label).set("probability", (*probability).clone());
                    (value, p.done())
                })
                .collect();
            scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            o.str("type", "score")
                .set("score", json::get(answer, "score").cloned().unwrap_or(Json::Null))
                .set("probabilities", Json::Array(scored.into_iter().map(|(_, p)| p).collect()))
                .set("confidence", json::get(answer, "confidence").cloned().unwrap_or(Json::Null));
        }
    }
    o.done()
}

/// `SystemOne.toResponse` and `Generate.decisions`' result.
fn decision_answer(model: &Model, questions: &[Json], answer: &Answer) -> std::result::Result<Json, AIError> {
    let invalid = |message: String| output_error(message, &answer.context);
    let decoded = std::str::from_utf8(&answer.body).ok().and_then(crate::json::parse).filter(valid_decision);
    let Some(decoded) = decoded else { return Err(invalid(format!("{SYSTEM_ONE} returned an invalid response"))) };
    let Some(Json::Object(answers)) = json::get(&decoded, "answers") else { unreachable!("validated") };
    let names: Vec<&str> = questions.iter().filter_map(|q| json::get_str(q, "name")).collect();
    if answers.len() != names.len() || !names.iter().all(|name| answers.contains_key(*name)) {
        return Err(invalid(format!("{SYSTEM_ONE} answers do not match the requested questions")));
    }
    let mapped = questions.iter().map(|q| to_answer(q, &answers[json::get_str(q, "name").unwrap_or_default()]));
    let mut result = Obj::new();
    result.set("answers", Json::Array(mapped.collect()));
    let usage = json::get(&decoded, "usage");
    result.opt("cost", usage.and_then(|u| json::get(u, "cost")).cloned());
    let input = usage.and_then(|u| json::get(u, "input_tokens")).cloned();
    let output = usage.and_then(|u| json::get(u, "output_tokens")).cloned();
    if input.is_some() || output.is_some() {
        let mut tokens = Obj::new();
        tokens.opt("input", input).opt("output", output);
        result.set("tokens", tokens.done());
    }
    let mut metadata = Obj::new();
    metadata.opt("model", json::get(&decoded, "model").cloned());
    metadata.opt("usage", usage.cloned());
    metadata.opt("responseId", json::get(&decoded, "id").cloned());
    metadata.opt("provider", json::get(&decoded, "provider").cloned());
    let mut metadata = metadata.done();
    if let Some(own) = json::get(&decoded, "provider_metadata").and_then(|m| json::get(m, &model.provider)) {
        if let Json::Object(fields) = own {
            for (key, value) in fields {
                json::set(&mut metadata, key, value.clone());
            }
        }
    }
    let mut native = Obj::new();
    native.set(&model.provider, metadata);
    result.set("native", native.done());
    Ok(result.done())
}

/// `Generate.decisions`.
pub async fn decisions(model: &Model, payload: &Json) -> Result<Json> {
    let questions = json::array(json::get(payload, "questions").unwrap_or(&Json::Null)).unwrap_or_default();
    let input = json::array(json::get(payload, "input").unwrap_or(&Json::Null)).unwrap_or_default();
    let body = (|| {
        let encoded = encode_questions(&questions)?;
        let mut placed = place_media(model, &input)?;
        let mut body = model.options.clone();
        if !matches!(body, Json::Object(_)) {
            body = json::object();
        }
        if let Some(overlay) = &model.body {
            merge(&mut body, overlay);
        }
        if let Json::Object(fields) = placed.done() {
            for (key, value) in fields {
                json::set(&mut body, &key, value);
            }
        }
        json::set(&mut body, "questions", encoded);
        json::set(&mut body, "model", json::string(&model.model_id));
        Ok::<Json, AIError>(body)
    })()
    .map_err(|error| super::provider_error(&model.provider, &error))?;
    retried(|| async { post(model, "/systemone", &body).await.and_then(|answer| decision_answer(model, &questions, &answer)) })
        .await
        .map_err(|error| super::provider_error(&model.provider, &error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_as_effect_decodes() {
        assert_eq!(decode_base64("aGk="), Some(b"hi".to_vec()));
        assert_eq!(decode_base64("aGk"), None);
        assert_eq!(decode_base64("a=Gk"), None);
        assert_eq!(decode_base64("%%%%"), None);
        assert_eq!(decode_base64("aG\nk="), Some(b"hi".to_vec()));
        assert_eq!(parse_data_url("data:image/png;base64,AAAA"), Some(("image/png".into(), "AAAA".into())));
        assert_eq!(parse_data_url("data:image/png,AAAA"), None);
    }
}
