//! `util/path-format.ts` (`formatPath`, `abbreviateHome`) and `context/path-format.tsx` (`usePathFormatter`):
//! paths shown relative to the location, or with the home abbreviated outside it. Windows-style inputs (from a
//! model or a config) keep TS's handling; the host is POSIX.

/// `/^[A-Za-z]:[\\/]/.test(input) || input.startsWith("\\\\")`.
fn windows_path(input: &str) -> bool {
    let b = input.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')) || input.starts_with("\\\\")
}

/// `path.posix.normalize` of an absolute path's segments.
fn normalize_segments(path: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `path.posix.resolve(base, input)` for an absolute base.
pub fn resolve(base: &str, input: &str) -> String {
    let joined = if input.starts_with('/') { input.to_string() } else { format!("{base}/{input}") };
    format!("/{}", normalize_segments(&joined).join("/"))
}

/// `path.posix.relative(from, to)` for absolute paths.
pub fn relative(from: &str, to: &str) -> String {
    let from = normalize_segments(from);
    let to = normalize_segments(to);
    let common = from.iter().zip(to.iter()).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = vec![".."; from.len() - common];
    parts.extend(&to[common..]);
    parts.join("/")
}

/// `abbreviateHome(input, home)`.
pub fn abbreviate_home(input: &str, home: &str) -> String {
    if home.is_empty() {
        return input.to_string();
    }
    let relative = relative(home, input);
    if relative.is_empty() {
        return "~".into();
    }
    if relative == ".." || relative.starts_with("../") || relative.starts_with('/') {
        return input.to_string();
    }
    format!("~/{relative}")
}

/// `formatPath(input, { base, home })`.
pub fn format_path(input: Option<&str>, base: &str, home: Option<&str>) -> String {
    let Some(input) = input.filter(|i| !i.is_empty()) else { return String::new() };
    if !windows_path(base) && windows_path(input) {
        return input.to_string();
    }
    let absolute = if input.starts_with('/') { input.to_string() } else { resolve(base, input) };
    let relative = relative(base, &absolute);
    if relative.is_empty() {
        return ".".into();
    }
    if relative != ".." && !relative.starts_with("../") && !relative.starts_with('/') {
        return relative;
    }
    match home.filter(|h| !h.is_empty()) {
        Some(home) => abbreviate_home(&absolute, home),
        None => absolute,
    }
}

/// `usePathFormatter()`: formats against the location's directory (or the working directory).
#[derive(Clone)]
pub struct PathFormatter {
    t: crate::ctx::Tui,
}

pub fn use_path_formatter() -> PathFormatter {
    PathFormatter { t: crate::ctx::tui() }
}

impl PathFormatter {
    /// `path()`.
    pub fn path(&self) -> String {
        use opencode_tui_sync::json::JsonExt;
        self.t
            .data
            .location
            .current()
            .and_then(|l| l.str("directory").filter(|d| !d.is_empty()).map(str::to_string))
            .unwrap_or_else(|| self.t.paths.cwd.clone())
    }

    /// `format(input)`.
    pub fn format(&self, input: Option<&str>) -> String {
        format_path(input, &self.path(), Some(&self.t.paths.home))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_path(Some("src/math.ts"), "/p", Some("/home/u")), "src/math.ts");
        assert_eq!(format_path(Some("/p/src/a.ts"), "/p", Some("/home/u")), "src/a.ts");
        assert_eq!(format_path(Some("/p"), "/p", None), ".");
        assert_eq!(format_path(Some("../elsewhere/notes.txt"), "/a/p", Some("/home/u")), "/a/elsewhere/notes.txt");
        assert_eq!(format_path(Some("/home/u/x"), "/p", Some("/home/u")), "~/x");
        assert_eq!(format_path(Some("/dev/null/impossible.txt"), "/p", Some("/home/u")), "/dev/null/impossible.txt");
        assert_eq!(format_path(None, "/p", None), "");
    }
}
