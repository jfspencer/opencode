//! Small helpers the components share: `util/locale.ts` (titlecase, takeWidth, truncateWidth), `util/string-width`,
//! `ui/file-path.tsx` (`truncateFilePath`), `abbreviateHome`, and theme colors as core colors.
use opencode_tui_core::color::RGBA;
use opencode_tui_core::unicode::{self, WidthMethod};

use crate::theme::Color;

/// A theme color as a renderable color (both are RGBA.ts's packed channels and meta bytes).
pub fn rgba(color: Color) -> RGBA {
    let c = color.rgba;
    RGBA([0, 1, 2, 3].map(|i| c.channels[i] as u16 | (c.meta[i] as u16) << 8))
}

/// `RGBA.fromValues(color.r, color.g, color.b, color.a * alpha)`.
pub fn fade(color: RGBA, alpha: f64) -> RGBA {
    RGBA::from_values(color.r(), color.g(), color.b(), color.a() * alpha)
}

/// `stringWidth`.
pub fn string_width(text: &str) -> u32 {
    unicode::text_width(text, 2, WidthMethod::Unicode)
}

fn grapheme_width(g: &str) -> u32 {
    string_width(g)
}

/// Grapheme clusters (`Intl.Segmenter` graphemes).
pub fn graphemes(text: &str) -> Vec<&str> {
    unicode::clusters(text, 2, WidthMethod::Unicode)
        .into_iter()
        .map(|c| &text[c.byte_start..c.byte_start + c.byte_len])
        .collect()
}

/// `Locale.titlecase`: the first word character after a word boundary upper-cased.
pub fn titlecase(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous_word = false;
    for c in text.chars() {
        let word = c.is_ascii_alphanumeric() || c == '_';
        if word && !previous_word { out.extend(c.to_uppercase()) } else { out.push(c) }
        previous_word = word;
    }
    out
}

/// `Locale.takeWidth`.
pub fn take_width(text: &str, width: i64) -> String {
    if width <= 0 {
        return String::new();
    }
    if string_width(text) as i64 <= width {
        return text.to_string();
    }
    let mut used = 0i64;
    let mut out = String::new();
    for g in graphemes(text) {
        let next = grapheme_width(g) as i64;
        if used + next > width {
            break;
        }
        out.push_str(g);
        used += next;
    }
    out
}

/// `Locale.truncateWidth`.
pub fn truncate_width(text: &str, width: i64) -> String {
    if width <= 0 {
        return String::new();
    }
    if string_width(text) as i64 <= width {
        return text.to_string();
    }
    if width == 1 {
        return "…".to_string();
    }
    take_width(text, width - 1) + "…"
}

fn take_start(text: &str, width: i64) -> String {
    take_width(text, width)
}

fn take_end(text: &str, width: i64) -> String {
    if width <= 0 {
        return String::new();
    }
    let parts = graphemes(text);
    let mut used = 0i64;
    let mut start = parts.len();
    for (i, g) in parts.iter().enumerate().rev() {
        let next = grapheme_width(g) as i64;
        if used + next > width {
            break;
        }
        used += next;
        start = i;
    }
    parts[start..].concat()
}

fn truncate_basename(value: &str, max_width: i64) -> String {
    if string_width(value) as i64 <= max_width {
        return value.to_string();
    }
    if max_width <= 1 {
        return take_start("…", max_width);
    }
    let dot = value.rfind('.').filter(|d| *d > 0);
    let extension = dot.map(|d| &value[d..]).unwrap_or("");
    let extension_width = string_width(extension) as i64;
    if extension_width >= max_width {
        return "…".to_string() + &take_end(extension, max_width - 1);
    }
    let stem = dot.map(|d| &value[..d]).unwrap_or(value);
    take_start(stem, max_width - extension_width - 1) + "…" + extension
}

/// `truncateFilePath(value, maxWidth)` for POSIX paths (Windows forms are dropped with Windows).
pub fn truncate_file_path(value: &str, max_width: i64) -> String {
    if max_width <= 0 {
        return String::new();
    }
    if string_width(value) as i64 <= max_width {
        return value.to_string();
    }
    let root = if value.starts_with('/') { "/" } else { "" };
    let source = &value[root.len()..];
    let segments: Vec<&str> = source.split('/').filter(|s| !s.is_empty()).collect();
    let basename = segments.last().copied().unwrap_or(value);
    if segments.len() < 2 {
        let root_width = string_width(root) as i64;
        if root_width >= max_width {
            return take_start(root, max_width);
        }
        return root.to_string() + &truncate_basename(basename, max_width - root_width);
    }
    let prefix = format!("{root}…/");
    let basename_width = max_width - string_width(&prefix) as i64;
    if basename_width <= 0 {
        return take_start(&prefix, max_width);
    }
    let compact = truncate_basename(basename, basename_width);
    if compact != basename {
        return prefix + &compact;
    }
    let mut selected = vec![basename.to_string()];
    let mut width = string_width(&prefix) as i64 + string_width(basename) as i64;
    for index in (0..segments.len() - 1).rev() {
        let segment = segments[index];
        let next = string_width(segment) as i64 + 1;
        if width + next > max_width {
            let available = max_width - width - 1;
            if available > 1 {
                selected.insert(0, take_start(segment, available - 1) + "…");
            }
            break;
        }
        selected.insert(0, segment.to_string());
        width += next;
    }
    prefix + &selected.join("/")
}

/// `abbreviateHome(directory, home)`: `~` for the home directory.
pub fn abbreviate_home(directory: &str, home: &str) -> String {
    if home.is_empty() || home == "/" {
        return directory.to_string();
    }
    if directory == home {
        return "~".to_string();
    }
    match directory.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => directory.to_string(),
    }
}

/// `tint(base, overlay, alpha)` (theme/color.ts) on renderable colors.
pub fn tint(base: RGBA, overlay: RGBA, alpha: f64) -> RGBA {
    RGBA::from_values(
        base.r() + (overlay.r() - base.r()) * alpha,
        base.g() + (overlay.g() - base.g()) * alpha,
        base.b() + (overlay.b() - base.b()) * alpha,
        base.a() + (overlay.a() - base.a()) * alpha,
    )
}
