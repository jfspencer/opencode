//! `Diff.ts`'s content side: jsdiff 9.0.0's `parsePatch` (the version `@opentui/core` 0.8.1 depends on) and the
//! line model `buildUnifiedView` / `buildSplitView` hand to the code panes and line-number gutters.
//!
//! Divergences: the renderables (panes, gutters, error text) are the renderer's. [`DiffModel::split_view`] takes
//! the panes' visual line sources when TS would align wrapped lines (`width > 0`, a wrapping mode, and no
//! highlight pending when conceal could change wrapping); the renderer computes them as TS reads `lineInfo`.

use crate::js::{JsRegex, JsStr, JsString};
use crate::style::Rgba;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

/// A hunk of a `StructuredPatch`.
#[derive(Clone, Debug, PartialEq)]
pub struct Hunk {
    pub old_start: f64,
    pub old_lines: f64,
    pub new_start: f64,
    pub new_lines: f64,
    pub lines: Vec<JsString>,
}

/// A parsed patch (`StructuredPatch` with jsdiff's optional Git fields). `None` is undefined.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct StructuredPatch {
    pub index: Option<JsString>,
    pub old_file_name: Option<JsString>,
    pub new_file_name: Option<JsString>,
    pub old_header: Option<JsString>,
    pub new_header: Option<JsString>,
    pub hunks: Vec<Hunk>,
    pub is_git: Option<bool>,
    pub is_rename: Option<bool>,
    pub is_copy: Option<bool>,
    pub is_create: Option<bool>,
    pub is_delete: Option<bool>,
    pub is_binary: Option<bool>,
    pub old_mode: Option<JsString>,
    pub new_mode: Option<JsString>,
}

macro_rules! re {
    ($name:ident, $src:expr) => {
        static $name: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::compile($src, ""));
    };
}
re!(GIT_DIFF_HEADER, r"^diff --git ");
re!(INDEX_HEADER, r"^Index:\s");
re!(DIFF_R_HEADER, r"^diff(?: -r \w+)+\s");
re!(FILE_HEADER, r"^(---|\+\+\+)\s");
re!(HUNK_HEADER, r"^@@\s");
re!(RENAME_FROM, r"^rename from (.*)");
re!(RENAME_TO, r"^rename to (.*)");
re!(COPY_FROM, r"^copy from (.*)");
re!(COPY_TO, r"^copy to (.*)");
re!(NEW_FILE_MODE, r"^new file mode (\d+)");
re!(DELETED_FILE_MODE, r"^deleted file mode (\d+)");
re!(OLD_MODE, r"^old mode (\d+)");
re!(NEW_MODE, r"^new mode (\d+)");
re!(BINARY_FILES, r"^Binary files ");
re!(HEADER_MATCH, r"^(?:Index:|diff(?: -r \w+)+)\s+");
re!(FILE_HEADER_MATCH, r"^(---|\+\+\+)\s+");
re!(SEPARATOR, r"^===================================================================");
re!(CHUNK_HEADER, r"@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@");
re!(BODY_LINE, r"^[+ -]");
static NEWLINE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::compile(r"\n", ""));
static DOUBLE_BACKSLASH: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::compile(r"\\\\", "g"));

fn is_git_diff_header(line: &[u16]) -> bool {
    GIT_DIFF_HEADER.test(line)
}

fn is_diff_header(line: &[u16]) -> bool {
    is_git_diff_header(line) || INDEX_HEADER.test(line) || DIFF_R_HEADER.test(line)
}

fn is_file_header(line: &[u16]) -> bool {
    FILE_HEADER.test(line)
}

fn is_hunk_header(line: &[u16]) -> bool {
    HUNK_HEADER.test(line)
}

fn lit(s: &str) -> JsString {
    JsString::from(s)
}

/// `parseQuotedFileName(s)`: the unescaped name and the raw length consumed.
fn parse_quoted_file_name(s: &[u16]) -> Option<(JsString, usize)> {
    if s.first() != Some(&(b'"' as u16)) {
        return None;
    }
    let mut result: Vec<u16> = Vec::new();
    let mut j = 1;
    let is_octal = |c: Option<&u16>| c.is_some_and(|&c| (b'0' as u16..=b'7' as u16).contains(&c));
    while j < s.len() {
        if s[j] == b'"' as u16 {
            return Some((JsString(result), j + 1));
        }
        if s[j] == b'\\' as u16 && j + 1 < s.len() {
            j += 1;
            let c = s[j];
            let simple = match c {
                0x61 => Some(0x07),
                0x62 => Some(0x08),
                0x66 => Some(0x0c),
                0x6e => Some(0x0a),
                0x72 => Some(0x0d),
                0x74 => Some(0x09),
                0x76 => Some(0x0b),
                0x5c => Some(0x5c),
                0x22 => Some(0x22),
                _ => None,
            };
            if let Some(v) = simple {
                result.push(v);
            } else if is_octal(Some(&c)) {
                if j + 2 >= s.len() || !is_octal(s.get(j + 1)) || !is_octal(s.get(j + 2)) {
                    return None;
                }
                // `parseInt(digits, 8)` stored in a Uint8Array: values above 255 wrap.
                let octal = |a: usize| -> u8 {
                    let digits = String::from_utf16_lossy(&s[a..a + 3]);
                    u32::from_str_radix(&digits, 8).unwrap_or(0) as u8
                };
                let mut bytes = vec![octal(j)];
                j += 3;
                while s.get(j) == Some(&(b'\\' as u16)) && is_octal(s.get(j + 1)) {
                    if j + 3 >= s.len() || !is_octal(s.get(j + 2)) || !is_octal(s.get(j + 3)) {
                        return None;
                    }
                    bytes.push(octal(j + 1));
                    j += 4;
                }
                // TextDecoder('utf-8') replaces invalid sequences with U+FFFD.
                result.extend(String::from_utf8_lossy(&bytes).encode_utf16());
                continue;
            } else {
                return None;
            }
        } else {
            result.push(s[j]);
        }
        j += 1;
    }
    None
}

fn unquote_if_quoted(s: &JsString) -> JsString {
    if s.first() == Some(&(b'"' as u16)) {
        if let Some((name, _)) = parse_quoted_file_name(s) {
            return name;
        }
    }
    s.clone()
}

/// `parseGitDiffHeader(line)`.
fn parse_git_diff_header(line: &[u16]) -> Option<(JsString, JsString)> {
    let rest = line.js_substring("diff --git ".len() as isize, None);
    if rest.first() == Some(&(b'"' as u16)) {
        let (old, raw_len) = parse_quoted_file_name(&rest)?;
        let after = rest.js_substring(raw_len as isize + 1, None);
        let new = if after.first() == Some(&(b'"' as u16)) { parse_quoted_file_name(&after)?.0 } else { after };
        return Some((old, new));
    }
    let quote = rest.index_of(&[b'"' as u16], 0);
    if quote > 0 {
        let old = rest.js_substring(0, Some(quote - 1));
        let (new, _) = parse_quoted_file_name(&rest.js_substring(quote, None))?;
        return Some((old, new));
    }
    if rest.js_starts_with(&lit("a/")) {
        let mut splits = Vec::new();
        let mut idx: isize = 0;
        loop {
            idx = rest.index_of(&lit(" b/"), (idx + 1) as usize);
            if idx == -1 {
                break;
            }
            splits.push(idx);
        }
        if !splits.is_empty() {
            let mid = splits[splits.len() / 2];
            return Some((rest.js_substring(0, Some(mid)), rest.js_substring(mid + 1, None)));
        }
    }
    None
}

/// `parsePatch(uniDiff)`: the patches, or the message jsdiff throws.
pub fn parse_patch(uni_diff: &[u16]) -> Result<Vec<StructuredPatch>, String> {
    let diffstr: Vec<JsString> = NEWLINE.split(uni_diff).into_iter().map(|p| p.unwrap_or_default()).collect();
    let mut list: Vec<StructuredPatch> = Vec::new();
    let mut i = 0usize;
    while i < diffstr.len() {
        parse_index(&diffstr, &mut i, &mut list)?;
    }
    Ok(list)
}

fn parse_index(diffstr: &[JsString], i: &mut usize, list: &mut Vec<StructuredPatch>) -> Result<(), String> {
    list.push(StructuredPatch::default());
    let index = list.len() - 1;
    let mut seen_diff_header = false;
    while *i < diffstr.len() {
        let line = &diffstr[*i];
        if is_file_header(line) || is_hunk_header(line) {
            break;
        }
        if is_git_diff_header(line) {
            if seen_diff_header {
                return Ok(());
            }
            seen_diff_header = true;
            let p = &mut list[index];
            p.is_git = Some(true);
            if let Some((old, new)) = parse_git_diff_header(line) {
                p.old_file_name = Some(old);
                p.new_file_name = Some(new);
            }
            *i += 1;
            while *i < diffstr.len() {
                let ext = &diffstr[*i];
                if is_file_header(ext) || is_hunk_header(ext) || is_diff_header(ext) {
                    break;
                }
                if let Some(m) = RENAME_FROM.exec(ext) {
                    p.old_file_name = Some(lit("a/").concat(&unquote_if_quoted(&m.get_or_empty(ext, 1))));
                    p.is_rename = Some(true);
                }
                if let Some(m) = RENAME_TO.exec(ext) {
                    p.new_file_name = Some(lit("b/").concat(&unquote_if_quoted(&m.get_or_empty(ext, 1))));
                    p.is_rename = Some(true);
                }
                if let Some(m) = COPY_FROM.exec(ext) {
                    p.old_file_name = Some(lit("a/").concat(&unquote_if_quoted(&m.get_or_empty(ext, 1))));
                    p.is_copy = Some(true);
                }
                if let Some(m) = COPY_TO.exec(ext) {
                    p.new_file_name = Some(lit("b/").concat(&unquote_if_quoted(&m.get_or_empty(ext, 1))));
                    p.is_copy = Some(true);
                }
                if let Some(m) = NEW_FILE_MODE.exec(ext) {
                    p.is_create = Some(true);
                    p.new_mode = Some(m.get_or_empty(ext, 1));
                }
                if let Some(m) = DELETED_FILE_MODE.exec(ext) {
                    p.is_delete = Some(true);
                    p.old_mode = Some(m.get_or_empty(ext, 1));
                }
                if let Some(m) = OLD_MODE.exec(ext) {
                    p.old_mode = Some(m.get_or_empty(ext, 1));
                }
                if let Some(m) = NEW_MODE.exec(ext) {
                    p.new_mode = Some(m.get_or_empty(ext, 1));
                }
                if BINARY_FILES.test(ext) {
                    p.is_binary = Some(true);
                }
                *i += 1;
            }
            continue;
        } else if is_diff_header(line) {
            if seen_diff_header {
                return Ok(());
            }
            seen_diff_header = true;
            if let Some(m) = HEADER_MATCH.exec(line) {
                list[index].index = Some(line.js_substring(m.len() as isize, None).js_trim());
            }
        }
        *i += 1;
    }
    parse_file_header(diffstr, i, &mut list[index]);
    parse_file_header(diffstr, i, &mut list[index]);
    let p = &list[index];
    if p.old_file_name.is_some() != p.new_file_name.is_some() {
        let missing = if p.old_file_name.is_some() { "\"+++ ...\"" } else { "\"--- ...\"" };
        let name = p.old_file_name.clone().or_else(|| p.new_file_name.clone()).unwrap_or_default();
        return Err(format!("Missing {missing} file header for {name}"));
    }
    while *i < diffstr.len() {
        let line = &diffstr[*i];
        if is_diff_header(line) || is_file_header(line) || SEPARATOR.test(line) {
            break;
        } else if is_hunk_header(line) {
            let hunk = parse_hunk(diffstr, i)?;
            list[index].hunks.push(hunk);
        } else {
            *i += 1;
        }
    }
    Ok(())
}

fn parse_file_header(diffstr: &[JsString], i: &mut usize, p: &mut StructuredPatch) {
    let Some(line) = diffstr.get(*i) else { return };
    let Some(m) = FILE_HEADER_MATCH.exec(line) else { return };
    let prefix = m.get_or_empty(line, 1);
    let data = line.js_substring(3, None).js_trim().split_str(&[b'\t' as u16]);
    let header = data.get(1).cloned().unwrap_or_default().js_trim();
    let mut file_name = data[0].clone();
    if file_name.first() == Some(&(b'"' as u16)) {
        file_name = unquote_if_quoted(&file_name);
    } else {
        file_name = DOUBLE_BACKSLASH.replace_str(&file_name, "\\");
    }
    if prefix == "---" {
        p.old_file_name = Some(file_name);
        p.old_header = Some(header);
    } else {
        p.new_file_name = Some(file_name);
        p.new_header = Some(header);
    }
    *i += 1;
}

fn parse_hunk(diffstr: &[JsString], i: &mut usize) -> Result<Hunk, String> {
    let chunk_header_index = *i;
    let header_line = &diffstr[*i];
    *i += 1;
    let parts = CHUNK_HEADER.split(header_line);
    let num = |k: usize| -> Option<f64> { parts.get(k).cloned().flatten().map(|s| crate::js::parse_decimal(&s)) };
    // `+chunkHeader[1]` of a line the regex did not split is `+undefined`: NaN.
    let mut hunk = Hunk {
        old_start: num(1).unwrap_or(f64::NAN),
        old_lines: if parts.len() > 2 { num(2).unwrap_or(1.0) } else { 1.0 },
        new_start: num(3).unwrap_or(f64::NAN),
        new_lines: if parts.len() > 4 { num(4).unwrap_or(1.0) } else { 1.0 },
        lines: Vec::new(),
    };
    if hunk.old_lines == 0.0 {
        hunk.old_start += 1.0;
    }
    if hunk.new_lines == 0.0 {
        hunk.new_start += 1.0;
    }
    let mut add_count = 0f64;
    let mut remove_count = 0f64;
    while *i < diffstr.len()
        && (remove_count < hunk.old_lines
            || add_count < hunk.new_lines
            || diffstr.get(*i).is_some_and(|l| l.js_starts_with(&[b'\\' as u16])))
    {
        let line = &diffstr[*i];
        let operation = if line.is_empty() && *i != diffstr.len() - 1 { Some(b' ' as u16) } else { line.first().copied() };
        match operation {
            Some(c) if c == b'+' as u16 || c == b'-' as u16 || c == b' ' as u16 || c == b'\\' as u16 => {
                hunk.lines.push(line.clone());
                if c == b'+' as u16 {
                    add_count += 1.0;
                } else if c == b'-' as u16 {
                    remove_count += 1.0;
                } else if c == b' ' as u16 {
                    add_count += 1.0;
                    remove_count += 1.0;
                }
            }
            _ => {
                return Err(format!("Hunk at line {} contained invalid line {}", chunk_header_index + 1, line));
            }
        }
        *i += 1;
    }
    if add_count == 0.0 && hunk.new_lines == 1.0 {
        hunk.new_lines = 0.0;
    }
    if remove_count == 0.0 && hunk.old_lines == 1.0 {
        hunk.old_lines = 0.0;
    }
    if add_count != hunk.new_lines {
        return Err(format!("Added line count did not match for hunk at line {}", chunk_header_index + 1));
    }
    if remove_count != hunk.old_lines {
        return Err(format!("Removed line count did not match for hunk at line {}", chunk_header_index + 1));
    }
    if let Some(next) = diffstr.get(*i) {
        if !next.is_empty() && BODY_LINE.test(next) && !is_file_header(next) {
            return Err(format!(
                "Hunk at line {} has more lines than expected (expected {} old lines and {} new lines)",
                chunk_header_index + 1,
                js_num(hunk.old_lines),
                js_num(hunk.new_lines)
            ));
        }
    }
    Ok(hunk)
}

fn js_num(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// `LineSign`: the text after (and before) a line number, with its colour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineSign {
    pub after: &'static str,
    pub after_color: Rgba,
}

/// `LineColorConfig`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineColorConfig {
    pub gutter: Rgba,
    pub content: Rgba,
}

/// What one pane gets: its code content and its gutter's maps (`createOrUpdateSide`'s arguments).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SideModel {
    pub content: JsString,
    pub line_colors: BTreeMap<usize, LineColorConfig>,
    pub line_signs: BTreeMap<usize, LineSign>,
    pub line_numbers: BTreeMap<usize, f64>,
    pub hide_line_numbers: BTreeSet<usize>,
}

/// The colours of a `DiffRenderable` (its props after defaults).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiffColors {
    pub line_number_bg: Rgba,
    pub added_bg: Rgba,
    pub removed_bg: Rgba,
    pub context_bg: Rgba,
    pub added_content_bg: Option<Rgba>,
    pub removed_content_bg: Option<Rgba>,
    pub context_content_bg: Option<Rgba>,
    pub added_sign_color: Rgba,
    pub removed_sign_color: Rgba,
    pub added_line_number_bg: Rgba,
    pub removed_line_number_bg: Rgba,
}

impl Default for DiffColors {
    /// `defaultColors`.
    fn default() -> Self {
        DiffColors {
            line_number_bg: Rgba::from_values(0.0, 0.0, 0.0, 0.0),
            added_bg: Rgba::from_hex("#1a4d1a"),
            removed_bg: Rgba::from_hex("#4d1a1a"),
            context_bg: Rgba::from_values(0.0, 0.0, 0.0, 0.0),
            added_content_bg: None,
            removed_content_bg: None,
            context_content_bg: None,
            added_sign_color: Rgba::from_hex("#22c55e"),
            removed_sign_color: Rgba::from_hex("#ef4444"),
            added_line_number_bg: Rgba::from_values(0.0, 0.0, 0.0, 0.0),
            removed_line_number_bg: Rgba::from_values(0.0, 0.0, 0.0, 0.0),
        }
    }
}

/// What `parseDiff` leaves: the first patch, or the error text `buildErrorView` shows.
#[derive(Clone, Debug, PartialEq)]
pub enum ParsedDiff {
    /// No diff text, or no patches.
    None,
    Patch(StructuredPatch),
    /// `Error parsing diff: <message>\n`, shown above the raw diff in a diff-highlighted code pane.
    Error(String),
}

/// `parseDiff()`.
pub fn parse_diff(diff: &JsString) -> ParsedDiff {
    if diff.is_empty() {
        return ParsedDiff::None;
    }
    match parse_patch(diff) {
        Ok(mut patches) if !patches.is_empty() => ParsedDiff::Patch(patches.remove(0)),
        Ok(_) => ParsedDiff::None,
        Err(message) => ParsedDiff::Error(format!(
            "Error parsing diff: {}\n",
            if message.is_empty() { "Unknown error".to_owned() } else { message }
        )),
    }
}

#[derive(Clone, Debug)]
struct LogicalLine {
    content: JsString,
    line_num: Option<f64>,
    hide_line_number: bool,
    sign: Option<LineSign>,
    kind: LineKind,
    hunk_start: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineKind {
    Context,
    Add,
    Remove,
    Empty,
}

fn empty_line() -> LogicalLine {
    LogicalLine { content: JsString::new(), line_num: None, hide_line_number: true, sign: None, kind: LineKind::Empty, hunk_start: false }
}

fn join_lines(lines: impl Iterator<Item = JsString>) -> JsString {
    let mut out = JsString::new();
    for (i, l) in lines.enumerate() {
        if i > 0 {
            out.push_literal("\n");
        }
        out.push_str(&l);
    }
    out
}

/// The split view's line model before and after alignment.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SplitView {
    /// The panes' content before alignment (`preLeftContent`, `preRightContent`): what the panes hold when TS
    /// reads their `lineInfo`.
    pub pre_left: JsString,
    pub pre_right: JsString,
    pub left: SideModel,
    pub right: SideModel,
    pub hunk_start_lines: Vec<usize>,
}

/// The line model of one patch.
pub struct DiffModel<'a> {
    pub patch: &'a StructuredPatch,
    pub colors: DiffColors,
}

impl DiffModel<'_> {
    fn color(&self, kind: LineKind) -> LineColorConfig {
        let c = &self.colors;
        match kind {
            LineKind::Add => LineColorConfig { gutter: c.added_line_number_bg, content: c.added_content_bg.unwrap_or(c.added_bg) },
            LineKind::Remove => {
                LineColorConfig { gutter: c.removed_line_number_bg, content: c.removed_content_bg.unwrap_or(c.removed_bg) }
            }
            _ => LineColorConfig { gutter: c.line_number_bg, content: c.context_content_bg.unwrap_or(c.context_bg) },
        }
    }

    /// `buildUnifiedView()`: the left pane's model and the hunk start lines.
    pub fn unified_view(&self) -> (SideModel, Vec<usize>) {
        let mut side = SideModel::default();
        let mut lines: Vec<JsString> = Vec::new();
        let mut hunk_starts = Vec::new();
        let mut index = 0usize;
        for hunk in &self.patch.hunks {
            hunk_starts.push(index);
            let mut old = hunk.old_start;
            let mut new = hunk.new_start;
            for line in &hunk.lines {
                let first = line.first().copied();
                let content = line.js_slice(1, None);
                match first {
                    Some(0x2b) => {
                        lines.push(content);
                        side.line_colors.insert(index, self.color(LineKind::Add));
                        side.line_signs.insert(index, LineSign { after: " +", after_color: self.colors.added_sign_color });
                        side.line_numbers.insert(index, new);
                        new += 1.0;
                        index += 1;
                    }
                    Some(0x2d) => {
                        lines.push(content);
                        side.line_colors.insert(index, self.color(LineKind::Remove));
                        side.line_signs.insert(index, LineSign { after: " -", after_color: self.colors.removed_sign_color });
                        side.line_numbers.insert(index, old);
                        old += 1.0;
                        index += 1;
                    }
                    Some(0x20) => {
                        lines.push(content);
                        side.line_colors.insert(index, self.color(LineKind::Context));
                        side.line_numbers.insert(index, new);
                        old += 1.0;
                        new += 1.0;
                        index += 1;
                    }
                    _ => {}
                }
            }
        }
        side.content = join_lines(lines.into_iter());
        (side, hunk_starts)
    }

    /// `buildSplitView()`'s logical lines, before alignment.
    fn split_logical_lines(&self) -> (Vec<LogicalLine>, Vec<LogicalLine>) {
        let mut left: Vec<LogicalLine> = Vec::new();
        let mut right: Vec<LogicalLine> = Vec::new();
        let mut hunk_first_left = Vec::new();
        for hunk in &self.patch.hunks {
            hunk_first_left.push(left.len());
            let mut old = hunk.old_start;
            let mut new = hunk.new_start;
            let mut i = 0;
            while i < hunk.lines.len() {
                let line = &hunk.lines[i];
                let first = line.first().copied();
                if first == Some(0x20) {
                    let content = line.js_slice(1, None);
                    left.push(LogicalLine { content: content.clone(), line_num: Some(old), hide_line_number: false, sign: None, kind: LineKind::Context, hunk_start: false });
                    right.push(LogicalLine { content, line_num: Some(new), hide_line_number: false, sign: None, kind: LineKind::Context, hunk_start: false });
                    old += 1.0;
                    new += 1.0;
                    i += 1;
                } else if first == Some(0x5c) {
                    i += 1;
                } else {
                    let mut removes: Vec<(JsString, f64)> = Vec::new();
                    let mut adds: Vec<(JsString, f64)> = Vec::new();
                    while i < hunk.lines.len() {
                        let current = &hunk.lines[i];
                        let c = current.first().copied();
                        if c == Some(0x20) || c == Some(0x5c) {
                            break;
                        }
                        let content = current.js_slice(1, None);
                        if c == Some(0x2d) {
                            removes.push((content, old));
                            old += 1.0;
                        } else if c == Some(0x2b) {
                            adds.push((content, new));
                            new += 1.0;
                        }
                        i += 1;
                    }
                    for j in 0..removes.len().max(adds.len()) {
                        left.push(match removes.get(j) {
                            Some((content, num)) => LogicalLine {
                                content: content.clone(),
                                line_num: Some(*num),
                                hide_line_number: false,
                                sign: Some(LineSign { after: " -", after_color: self.colors.removed_sign_color }),
                                kind: LineKind::Remove,
                                hunk_start: false,
                            },
                            None => empty_line(),
                        });
                        right.push(match adds.get(j) {
                            Some((content, num)) => LogicalLine {
                                content: content.clone(),
                                line_num: Some(*num),
                                hide_line_number: false,
                                sign: Some(LineSign { after: " +", after_color: self.colors.added_sign_color }),
                                kind: LineKind::Add,
                                hunk_start: false,
                            },
                            None => empty_line(),
                        });
                    }
                }
            }
        }
        for start in hunk_first_left {
            if let Some(line) = left.get_mut(start) {
                line.hunk_start = true;
            }
        }
        (left, right)
    }

    /// `buildSplitView()`. `alignment` is the left and right panes' `lineInfo.lineSources` for their
    /// `pre_left`/`pre_right` content when TS aligns wrapped lines, else `None`.
    pub fn split_view(&self, alignment: Option<(&[usize], &[usize])>) -> SplitView {
        let (left_lines, right_lines) = self.split_logical_lines();
        let pre_left = join_lines(left_lines.iter().map(|l| l.content.clone()));
        let pre_right = join_lines(right_lines.iter().map(|l| l.content.clone()));
        let (final_left, final_right) = match alignment {
            None => (left_lines, right_lines),
            Some((left_sources, right_sources)) => {
                let count = |sources: &[usize]| {
                    let mut m: BTreeMap<usize, usize> = BTreeMap::new();
                    for &s in sources {
                        *m.entry(s).or_insert(0) += 1;
                    }
                    m
                };
                let (lc, rc) = (count(left_sources), count(right_sources));
                let mut fl = Vec::new();
                let mut fr = Vec::new();
                let (mut lpos, mut rpos) = (0usize, 0usize);
                for i in 0..left_lines.len() {
                    if lpos < rpos {
                        for _ in 0..rpos - lpos {
                            fl.push(empty_line());
                        }
                        lpos = rpos;
                    } else if rpos < lpos {
                        for _ in 0..lpos - rpos {
                            fr.push(empty_line());
                        }
                        rpos = lpos;
                    }
                    fl.push(left_lines[i].clone());
                    // Split views pair lines one to one, so the right side has a line at every left index.
                    if let Some(r) = right_lines.get(i) {
                        fr.push(r.clone());
                    }
                    lpos += lc.get(&i).copied().unwrap_or(0);
                    rpos += rc.get(&i).copied().unwrap_or(0);
                }
                if lpos < rpos {
                    for _ in 0..rpos - lpos {
                        fl.push(empty_line());
                    }
                } else if rpos < lpos {
                    for _ in 0..lpos - rpos {
                        fr.push(empty_line());
                    }
                }
                (fl, fr)
            }
        };
        let mut view = SplitView { pre_left, pre_right, ..Default::default() };
        for (index, line) in final_left.iter().enumerate() {
            if line.hunk_start {
                view.hunk_start_lines.push(index);
            }
            if let Some(n) = line.line_num {
                view.left.line_numbers.insert(index, n);
            }
            if line.hide_line_number {
                view.left.hide_line_numbers.insert(index);
            }
            if matches!(line.kind, LineKind::Remove | LineKind::Context) {
                view.left.line_colors.insert(index, self.color(line.kind));
            }
            if let Some(sign) = &line.sign {
                view.left.line_signs.insert(index, sign.clone());
            }
        }
        for (index, line) in final_right.iter().enumerate() {
            if let Some(n) = line.line_num {
                view.right.line_numbers.insert(index, n);
            }
            if line.hide_line_number {
                view.right.hide_line_numbers.insert(index);
            }
            if matches!(line.kind, LineKind::Add | LineKind::Context) {
                view.right.line_colors.insert(index, self.color(line.kind));
            }
            if let Some(sign) = &line.sign {
                view.right.line_signs.insert(index, sign.clone());
            }
        }
        view.left.content = join_lines(final_left.into_iter().map(|l| l.content));
        view.right.content = join_lines(final_right.into_iter().map(|l| l.content));
        view
    }
}
