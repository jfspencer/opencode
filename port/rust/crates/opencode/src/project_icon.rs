//! Project icons (packages/core/src/project/icon.ts): an image found in the project's canonical directory (favicons,
//! app icons, Icon Composer bundles, asset catalogs, Windows resources), stored as a data URL of at most 64 KiB.

use std::path::Path;

use base64::Engine;
use opencode_protocol::Json;
use opencode_protocol::wire::number::{js_number_string, js_string_to_number};
use rusqlite::{Connection, OptionalExtension, params};

use crate::bus::{Bus, LocationRef, PublishOptions};
use crate::project::{GLOBAL, contains, info_from_row};
use crate::text::extname;

const LIMIT: usize = 64 * 1024;
const IMAGES: &[&str] = &[".svg", ".png", ".ico", ".jpg", ".jpeg", ".webp"];

fn candidates() -> Vec<String> {
    let mut out: Vec<String> = vec!["Assets.xcassets/AppIcon.appiconset".into(), "ios/App/Assets.xcassets/AppIcon.appiconset".into()];
    for base in ["assets/app-icon", "build/icon", "src-tauri/icons/icon", "resources/icon", "icons/icon", "extension/icons/icon"] {
        out.push(format!("{base}.svg"));
        out.push(format!("{base}.png"));
    }
    for file in ["favicon.svg", "icon.svg", "favicon.png", "apple-touch-icon.png", "icon.png", "favicon.ico"] {
        for dir in ["public/", "app/", "src/app/", "site/public/", ""] {
            out.push(format!("{dir}{file}"));
        }
    }
    for base in ["assets/icon", "assets/logo", "public/logo", "logo"] {
        out.push(format!("{base}.svg"));
        out.push(format!("{base}.png"));
    }
    out
}

/// `ProjectIcon.refresh`: discovers the project's icon and stores it when it changed; the updated project, announced.
pub fn refresh(db: &Connection, bus: &Bus, id: &str, location: Option<LocationRef>) -> rusqlite::Result<()> {
    if id == GLOBAL {
        return Ok(());
    }
    let row: Option<(String, Option<String>)> = db
        .query_row("SELECT worktree, icon_url FROM project WHERE id = ?1", params![id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let Some((worktree, current)) = row else { return Ok(()) };
    let Some(url) = discover(&worktree) else { return Ok(()) };
    if current.is_some_and(|current| current == url) {
        return Ok(());
    }
    let now = crate::id::now();
    let info = db
        .query_row("UPDATE project SET icon_url = ?1, time_updated = ?2 WHERE id = ?3 RETURNING *", params![url, now, id], info_from_row)
        .optional()?;
    if let Some(info) = info {
        bus.emit("project.updated", info, PublishOptions { id: None, location });
    }
    Ok(())
}

fn real(path: &Path) -> Option<String> {
    std::fs::canonicalize(path).ok().map(|p| p.to_string_lossy().into_owned())
}

/// `readDirectoryEntries`: names with their own type (symlinks are neither files nor directories).
fn entries(directory: &str) -> Vec<(String, bool, bool)> {
    let Ok(read) = std::fs::read_dir(directory) else { return vec![] };
    read.filter_map(|entry| entry.ok())
        .map(|entry| {
            let kind = entry.file_type().ok();
            (
                entry.file_name().to_string_lossy().into_owned(),
                kind.is_some_and(|k| k.is_file()),
                kind.is_some_and(|k| k.is_dir()),
            )
        })
        .collect()
}

fn utf16_sorted(mut names: Vec<String>) -> Vec<String> {
    names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    names
}

/// `ProjectIcon.discover`: the data URL of the first usable image.
pub fn discover(directory: &str) -> Option<String> {
    let root = real(Path::new(directory))?;
    let catalogs = utf16_sorted(entries(&root).into_iter().filter(|(name, _, dir)| *dir && !name.starts_with('.')).map(|(n, _, _)| n).collect());
    let mut names = candidates();
    names.extend(["AppIcon.icon", "app/AppIcon.icon", "assets/AppIcon.icon"].map(String::from));
    names.extend(catalogs.into_iter().take(128).map(|name| format!("{name}/Assets.xcassets/AppIcon.appiconset")));
    names.push("resources/windows".into());
    for name in names {
        let Some(candidate) = real(&Path::new(&root).join(&name)) else { continue };
        if candidate == root || !contains(&root, &candidate) {
            continue;
        }
        if name.ends_with(".icon") {
            if let Some(url) = composer_icon(&root, &candidate) {
                return Some(url);
            }
            continue;
        }
        let windows = name == "resources/windows";
        let files: Vec<String> = if name.ends_with(".appiconset") || windows {
            let rank = |file: &str| -> f64 {
                if windows {
                    let digits = file.get(5..file.len().saturating_sub(4)).unwrap_or("");
                    (js_string_to_number(digits) - 128.0).abs()
                } else if file.contains("128") || file.contains("256") {
                    0.0
                } else {
                    1.0
                }
            };
            let mut files = utf16_sorted(
                entries(&candidate)
                    .into_iter()
                    .filter(|(file, is_file, _)| {
                        *is_file && if windows { windows_icon(file) } else { IMAGES.contains(&extname(file).to_lowercase().as_str()) }
                    })
                    .map(|(file, _, _)| file)
                    .collect(),
            );
            files.sort_by(|a, b| rank(a).partial_cmp(&rank(b)).unwrap_or(std::cmp::Ordering::Equal));
            files.into_iter().take(128).map(|file| format!("{candidate}/{file}")).collect()
        } else {
            vec![candidate]
        };
        for file in files {
            let Some((file, bytes)) = read(&root, &file, IMAGES) else { continue };
            let url = data_url(&file, &bytes);
            if url.len() <= LIMIT {
                return Some(url);
            }
        }
    }
    None
}

/// `/^icon-\d+\.png$/i`
fn windows_icon(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    lower.strip_prefix("icon-").and_then(|rest| rest.strip_suffix(".png")).is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// An Icon Composer bundle: the first layer image, over its fill when the image is an SVG.
fn composer_icon(root: &str, candidate: &str) -> Option<String> {
    let (_, manifest) = read(root, &format!("{candidate}/icon.json"), &[".json"])?;
    let config = crate::json::parse(&String::from_utf8_lossy(&manifest)).and_then(composer)?;
    for image in &config.images {
        let Some((file, bytes)) = read(root, &format!("{candidate}/Assets/{image}"), &[".svg", ".png"]) else { continue };
        let plain = data_url(&file, &bytes);
        let url = if file.ends_with(".svg") { backdrop(&plain, &config.fill) } else { plain.clone() };
        if url.len() <= LIMIT {
            return Some(url);
        }
        if plain.len() <= LIMIT {
            return Some(plain);
        }
    }
    None
}

struct Composer {
    /// Every layer's `image-name`, in order.
    images: Vec<String>,
    /// The fill's colors: `solid`, else `linear-gradient`.
    fill: Vec<String>,
}

/// The manifest's schema (icon.ts `composer`): anything else in it rejects the whole manifest.
fn composer(json: Json) -> Option<Composer> {
    let object = json.as_object()?;
    let mut images = vec![];
    for group in crate::json::array(object.get("groups")?)? {
        for layer in crate::json::array(group.as_object()?.get("layers")?)? {
            match layer.as_object()?.get("image-name") {
                None => {}
                Some(Json::String(name)) => {
                    if !name.is_empty() {
                        images.push(name.clone());
                    }
                }
                Some(_) => return None,
            }
        }
    }
    let strings = |value: &Json| -> Option<Vec<String>> {
        crate::json::array(value)?.iter().map(|v| match v {
            Json::String(s) => Some(s.clone()),
            _ => None,
        }).collect()
    };
    let fill = match object.get("fill") {
        None => vec![],
        Some(fill) => {
            let fill = fill.as_object()?;
            let solid = match fill.get("solid") {
                None => None,
                Some(Json::String(s)) => Some(s.clone()),
                Some(_) => return None,
            };
            let gradient = match fill.get("linear-gradient") {
                None => None,
                Some(value) => Some(strings(value)?),
            };
            match solid.filter(|s| !s.is_empty()) {
                Some(solid) => vec![solid],
                None => gradient.unwrap_or_default(),
            }
        }
    };
    Some(Composer { images, fill })
}

fn read(root: &str, name: &str, extensions: &[&str]) -> Option<(String, Vec<u8>)> {
    let file = real(Path::new(name))?;
    if file == root || !contains(root, &file) || !extensions.contains(&extname(&file).to_lowercase().as_str()) {
        return None;
    }
    let info = std::fs::metadata(&file).ok()?;
    if !info.is_file() || info.len() == 0 || info.len() as usize > LIMIT {
        return None;
    }
    let bytes = std::fs::read(&file).ok()?;
    (!bytes.is_empty() && bytes.len() <= LIMIT).then_some((file, bytes))
}

fn data_url(file: &str, bytes: &[u8]) -> String {
    let mime = if extname(file).eq_ignore_ascii_case(".ico") { "image/x-icon" } else { crate::fs::mime_type(file) };
    format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// The layer over a rounded square of the bundle's fill (one color, or a vertical gradient).
fn backdrop(url: &str, fill: &[String]) -> String {
    let colors: Vec<Option<String>> = fill.iter().map(|color| color_of(color)).collect();
    if colors.is_empty() || colors.iter().any(Option::is_none) {
        return url.to_owned();
    }
    let colors: Vec<String> = colors.into_iter().flatten().collect();
    let gradient = if colors.len() > 1 {
        let stops: String = colors
            .iter()
            .enumerate()
            .map(|(i, color)| format!("<stop offset=\"{}\" stop-color=\"{color}\"/>", js_number_string(i as f64 / (colors.len() - 1) as f64)))
            .collect();
        format!("<defs><linearGradient id=\"bg\" x2=\"0\" y2=\"1\">{stops}</linearGradient></defs>")
    } else {
        String::new()
    };
    let fill = if colors.len() > 1 { "url(#bg)".to_owned() } else { colors[0].clone() };
    // Embed the layer as an image, never as SVG markup: its content stays in an inert image document.
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 128 128\">{gradient}<rect width=\"128\" height=\"128\" rx=\"28\" fill=\"{fill}\"/><image width=\"128\" height=\"128\" href=\"{url}\"/></svg>"
    );
    data_url("icon.svg", svg.as_bytes())
}

/// `/^(gray|srgb|display-p3):([\d.,]+)$/` as a CSS `color()`.
fn color_of(color: &str) -> Option<String> {
    let (space, list) = color.split_once(':')?;
    if !["gray", "srgb", "display-p3"].contains(&space) || list.is_empty() || !list.bytes().all(|b| b.is_ascii_digit() || b == b'.' || b == b',') {
        return None;
    }
    let values: Vec<f64> = list.split(',').map(js_string_to_number).collect();
    if values.len() != if space == "gray" { 2 } else { 4 } || values.iter().any(|n| !n.is_finite() || *n < 0.0 || *n > 1.0) {
        return None;
    }
    let rgb: Vec<f64> = if space == "gray" { vec![values[0]; 3] } else { values[..3].to_vec() };
    let rgb: Vec<String> = rgb.into_iter().map(js_number_string).collect();
    let alpha = js_number_string(*values.last().unwrap());
    Some(format!("color({} {} / {alpha})", if space == "display-p3" { "display-p3" } else { "srgb" }, rgb.join(" ")))
}
