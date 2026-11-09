//! The edit tool (packages/core/src/tool/plugin/edit.ts): exact text replaced in a file. `oldString` is matched
//! against the LF view the read tool shows (CRLF and mixed endings neither block a match nor leak into the
//! replacement); without an exact match, again ignoring typographic quote, dash and space variants, then trailing
//! whitespace on each line. The replacement's lines take their endings from the region they replace. Strings that
//! differ only in `\r` change the endings themselves, on the exact bytes. A miss hints at the closest lines (when the
//! file is readable).
//!
//! Offsets are UTF-8 byte offsets where TS has UTF-16 ones; the normalizations map characters one to one, so the
//! normalized tier maps its offsets back through the characters it shortened.

use indexmap::IndexMap;
use opencode_protocol::Json;

use super::file_diff::file_diff;
use super::file_write::{self, ReadError};
use super::{Context, Definition, Execution, Failure, Output, Tool, ToolError};
use crate::json::{self, Obj};
use crate::permission::Ask;

pub struct Edit {
    pub definition: Definition,
}

const CRLF: &str = "\r\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Match {
    start: usize,
    end: usize,
}

/// `normalizeForMatch` of one character.
fn normalize_char(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
        '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => '-',
        '\u{a0}' | '\u{2002}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' => ' ',
        other => other,
    }
}

fn normalize(value: &str) -> String {
    value.chars().map(normalize_char).collect()
}

/// The normalized text with the offset deltas it introduced: `(normalized offset after a shortened character,
/// bytes removed so far)`.
fn normalize_mapped(value: &str) -> (String, Vec<(usize, usize)>) {
    let mut out = String::with_capacity(value.len());
    let mut deltas = Vec::new();
    let mut removed = 0;
    for c in value.chars() {
        let n = normalize_char(c);
        out.push(n);
        if n.len_utf8() != c.len_utf8() {
            removed += c.len_utf8() - n.len_utf8();
            deltas.push((out.len(), removed));
        }
    }
    (out, deltas)
}

fn unmap(deltas: &[(usize, usize)], offset: usize) -> usize {
    let index = deltas.partition_point(|(at, _)| *at <= offset);
    offset + if index == 0 { 0 } else { deltas[index - 1].1 }
}

/// `findOccurrences`: non-overlapping, left to right.
fn find_occurrences(content: &str, search: &str) -> Vec<Match> {
    if search.is_empty() {
        return vec![];
    }
    content.match_indices(search).map(|(start, found)| Match { start, end: start + found.len() }).collect()
}

/// `String.prototype.trimEnd`.
fn trim_end(text: &str) -> &str {
    text.trim_end_matches(crate::text::is_js_whitespace)
}

/// `findLineOccurrences`: line runs equal to the search lines ignoring trailing whitespace and the typographic
/// variants.
fn find_line_occurrences(content: &str, search: &str) -> Vec<Match> {
    let trailing_newline = search.ends_with('\n');
    let mut expected: Vec<&str> = search.split('\n').collect();
    if trailing_newline {
        expected.pop();
    }
    struct Line<'a> {
        start: usize,
        end: usize,
        text: &'a str,
        content_end: usize,
        newline: bool,
    }
    let mut lines: Vec<Line> = Vec::new();
    let mut start = 0;
    while start < content.len() {
        let (end, newline) = match content[start..].find('\n') {
            Some(index) => (start + index + 1, true),
            None => (content.len(), false),
        };
        let text = &content[start..if newline { end - 1 } else { end }];
        let content_end = start + text.len() - usize::from(text.ends_with('\r'));
        lines.push(Line { start, end, text, content_end, newline });
        start = end;
    }
    let keys: Vec<String> = expected.iter().map(|line| normalize(trim_end(line))).collect();
    let mut result: Vec<Match> = Vec::new();
    for index in 0..lines.len() {
        if index + expected.len() > lines.len() {
            break;
        }
        let actual = &lines[index..index + expected.len()];
        if !actual.iter().zip(&keys).all(|(item, key)| normalize(trim_end(item.text)) == *key) {
            continue;
        }
        let Some(last) = actual.last() else { continue };
        if trailing_newline && !last.newline {
            continue;
        }
        let candidate = Match { start: lines[index].start, end: if trailing_newline { last.end } else { last.content_end } };
        if result.iter().any(|m| m.end > candidate.start && m.start < candidate.end) {
            continue;
        }
        result.push(candidate);
    }
    result
}

/// `lineEnding`: the dominant line ending of the text, or `fallback` when there is none.
fn line_ending<'a>(text: &str, fallback: &'a str) -> &'a str {
    let crlf = text.matches(CRLF).count();
    let lf = text.matches('\n').count() - crlf;
    if crlf == lf {
        fallback
    } else if crlf > lf {
        CRLF
    } else {
        "\n"
    }
}

/// `alignLines`: for each `after` line, the `before` line it keeps (longest common subsequence) or, between kept
/// lines, the one it takes the place of.
fn align_lines(before: &[String], after: &[String]) -> Vec<Option<usize>> {
    let width = after.len() + 1;
    let mut table = vec![0u32; (before.len() + 1) * width];
    for i in (0..before.len()).rev() {
        for j in (0..after.len()).rev() {
            table[i * width + j] = if before[i] == after[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let mut result: Vec<Option<usize>> = vec![None; after.len()];
    let (mut gap_before, mut gap_after): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
    let flush = |result: &mut Vec<Option<usize>>, gap_before: &mut Vec<usize>, gap_after: &mut Vec<usize>| {
        for (k, j) in gap_after.iter().enumerate() {
            result[*j] = gap_before.get(k).copied();
        }
        gap_before.clear();
        gap_after.clear();
    };
    let (mut i, mut j) = (0, 0);
    while i < before.len() && j < after.len() {
        if before[i] == after[j] {
            flush(&mut result, &mut gap_before, &mut gap_after);
            result[j] = Some(i);
            i += 1;
            j += 1;
            continue;
        }
        if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            gap_before.push(i);
            i += 1;
        } else {
            gap_after.push(j);
            j += 1;
        }
    }
    gap_before.extend(i..before.len());
    gap_after.extend(j..after.len());
    flush(&mut result, &mut gap_before, &mut gap_after);
    result
}

/// `region.split(/\r?\n/)` and the endings it split on.
fn split_endings(region: &str) -> (Vec<&str>, Vec<&'static str>) {
    let mut lines = Vec::new();
    let mut endings = Vec::new();
    let mut start = 0;
    for (index, _) in region.match_indices('\n') {
        let crlf = index > start && region.as_bytes()[index - 1] == b'\r';
        lines.push(&region[start..if crlf { index - 1 } else { index }]);
        endings.push(if crlf { CRLF } else { "\n" });
        start = index + 1;
    }
    lines.push(&region[start..]);
    (lines, endings)
}

/// `withRegionEndings`.
fn with_region_endings(region: &str, replacement: &str, fallback: &str) -> String {
    let (before, endings) = split_endings(region);
    let after: Vec<&str> = replacement.split('\n').collect();
    let key = |line: &str| normalize(trim_end(line));
    let source = align_lines(&before.iter().map(|l| key(l)).collect::<Vec<_>>(), &after.iter().map(|l| key(l)).collect::<Vec<_>>());
    let mut out = String::with_capacity(replacement.len() + after.len());
    for (index, line) in after.iter().enumerate() {
        out.push_str(line);
        if index + 1 < after.len() {
            out.push_str(source[index].and_then(|s| endings.get(s).copied()).unwrap_or(fallback));
        }
    }
    out
}

/// `countBefore`: the sorted positions strictly before `offset`.
fn count_before(positions: &[usize], offset: usize) -> usize {
    positions.partition_point(|p| *p < offset)
}

/// `matchView`: the matches in the source, and the replacement of each.
fn match_view(source: &str, old: &str, new: &str) -> (Vec<Match>, String) {
    let view = source.replace(CRLF, "\n");
    let removed: Vec<usize> = source.match_indices(CRLF).enumerate().map(|(index, (at, _))| at - index).collect();
    let search = old.replace(CRLF, "\n");
    let exact = find_occurrences(&view, &search);
    let found = if !exact.is_empty() {
        exact
    } else {
        let (normalized, deltas) = normalize_mapped(&view);
        let unicode: Vec<Match> = find_occurrences(&normalized, &normalize(&search))
            .into_iter()
            .map(|m| Match { start: unmap(&deltas, m.start), end: unmap(&deltas, m.end) })
            .collect();
        if !unicode.is_empty() { unicode } else { find_line_occurrences(&view, &search) }
    };
    let matches = found
        .into_iter()
        .map(|m| Match { start: m.start + count_before(&removed, m.start), end: m.end + count_before(&removed, m.end) })
        .collect();
    (matches, new.replace(CRLF, "\n"))
}

const HINT_LINES: usize = 12;
const HINT_LINE_LENGTH: usize = 200;

/// JS `\s+` → `" "`, then `trim`.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if crate::text::is_js_whitespace(c) {
            space = true;
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    crate::text::trim(&out).to_owned()
}

/// `/[\p{L}\p{N}]/u`: a letter or a number. Alphabetic also covers the `Other_Alphabetic` marks, which only differ
/// for a line of marks without any letter.
fn distinct(text: &str) -> bool {
    text.chars().any(|c| c.is_alphabetic() || c.is_numeric())
}

/// `missHint`: newString already present, or the lines closest to oldString, numbered like the read tool's.
fn miss_hint(source: &str, old: &str, new: &str) -> Option<String> {
    let view = source.replace(CRLF, "\n");
    let replacement = new.replace(CRLF, "\n");
    if crate::text::trim(&replacement).encode_utf16().count() >= 20 && normalize(&view).contains(&normalize(&replacement)) {
        return Some("newString is already in the file, so this edit may already be applied.".into());
    }
    let key = |line: &str| collapse_whitespace(&normalize(line));
    let lines: Vec<&str> = view.split('\n').collect();
    let search_text = old.replace(CRLF, "\n");
    let search: Vec<&str> = search_text.split('\n').collect();
    let mut positions: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
    for (index, line) in lines.iter().enumerate() {
        let value = key(line);
        if distinct(&value) {
            positions.entry(value).or_default().push(index);
        }
    }
    let mut votes: IndexMap<i64, usize> = IndexMap::new();
    let mut wanted = 0;
    for (offset, line) in search.iter().enumerate() {
        let value = key(line);
        if !distinct(&value) {
            continue;
        }
        for index in positions.get(&value).map(Vec::as_slice).unwrap_or(&[]) {
            *votes.entry(*index as i64 - offset as i64).or_default() += 1;
        }
        wanted += 1;
    }
    let (start, count) = votes.iter().fold((0i64, 0usize), |best, (start, count)| if *count > best.1 { (*start, *count) } else { best });
    if count == 0 || count * 2 < wanted {
        return None;
    }
    let matched: Vec<usize> = search
        .iter()
        .enumerate()
        .filter_map(|(offset, line)| {
            let value = key(line);
            let index = start + offset as i64;
            (!value.is_empty() && index >= 0 && (index as usize) < lines.len() && key(lines[index as usize]) == value)
                .then_some(index as usize)
        })
        .collect();
    let from = *matched.first()?;
    let to = (matched.last()? + 1).min(from + HINT_LINES);
    let mut out = vec!["The closest lines in the file:".to_owned()];
    for (index, line) in lines[from..to].iter().enumerate() {
        let units: Vec<u16> = line.encode_utf16().collect();
        let shown = if units.len() > HINT_LINE_LENGTH {
            format!("{}…", String::from_utf16_lossy(&units[..HINT_LINE_LENGTH]))
        } else {
            (*line).to_owned()
        };
        out.push(format!("{}: {shown}", from + index + 1));
    }
    Some(out.join("\n"))
}

/// The edit applied to the source: how many matches there are, and the text with the first (or every) one
/// replaced.
fn apply(source: &str, old: &str, new: &str, replace_all: bool) -> (usize, String) {
    let endings_only = old.replace('\r', "") == new.replace('\r', "");
    let (matches, replacement) =
        if endings_only { (find_occurrences(source, old), None) } else { let (m, r) = match_view(source, old, new); (m, Some(r)) };
    let file_ending = line_ending(source, "\n");
    let replace = |m: &Match| match &replacement {
        None => new.to_owned(),
        Some(replacement) => {
            let region = &source[m.start..m.end];
            with_region_endings(region, replacement, line_ending(region, file_ending))
        }
    };
    let applied: &[Match] = if replace_all { &matches } else { &matches[..matches.len().min(1)] };
    let mut replaced = source.to_owned();
    for m in applied.iter().rev() {
        replaced = format!("{}{}{}", &replaced[..m.start], replace(m), &replaced[m.end..]);
    }
    (matches.len(), replaced)
}

fn failure(message: impl Into<String>) -> Failure {
    Failure::Error(ToolError::new(message))
}

impl Edit {
    async fn run(&self, context: Context, input: Json) -> Result<Output, Failure> {
        let input = super::file_input::decode(&self.definition, &input, &[])?;
        let path = json::get_str(&input, "path").unwrap_or_default().to_owned();
        let old = json::get_str(&input, "oldString").unwrap_or_default().to_owned();
        let new = json::get_str(&input, "newString").unwrap_or_default().to_owned();
        let replace_all = json::get_bool(&input, "replaceAll") == Some(true);
        let _lock = file_write::lock(&[crate::file_access::resolve_path(&context.location.directory, &path)]).await;
        let unable = || failure(format!("Unable to edit {path}"));
        if old == new {
            return Err(failure("No changes to apply: oldString and newString are identical."));
        }
        if old.is_empty() {
            return Err(failure("oldString must not be empty. Use write to create or overwrite a file."));
        }
        let target = file_write::target(&context, "edit", &path).await?;
        let source = match file_write::read_text(&target.absolute) {
            Ok((_, text)) => text,
            Err(ReadError::NotFound) => return Err(failure(format!("File not found: {path}"))),
            Err(ReadError::WrongKind("directory")) => return Err(failure(format!("Path is a directory, not a file: {path}"))),
            Err(_) => return Err(unable()),
        };
        let endings_only = old.replace('\r', "") == new.replace('\r', "");
        let (replacements, replaced) = apply(&source, &old, &new, replace_all);
        let preview = (replacements > 0 && (replacements == 1 || replace_all)).then(|| file_diff(&target.resource, &source, &replaced, "modified"));
        file_write::authorize(&context, "edit", &target, preview.as_ref()).await?;
        if replacements == 0 {
            let ask = Ask { action: "read".into(), resources: vec![target.resource.clone()], save: Some(vec!["*".into()]), metadata: None };
            let readable = context.allows(ask)?;
            let hint = if readable { miss_hint(&source, &old, &new) } else { None };
            return Err(failure(format!(
                "Could not find oldString in {path}. It must match exactly, including whitespace and indentation.{}",
                hint.map(|hint| format!("\n\n{hint}")).unwrap_or_default()
            )));
        }
        if replacements > 1 && !replace_all {
            return Err(failure(format!(
                "Found {replacements} matches for oldString, but expected exactly one. Add more surrounding context to make oldString unique, or set replaceAll to true to replace every occurrence."
            )));
        }
        if replaced == source {
            return Err(failure(if endings_only {
                format!("No changes to apply: {path} already has those line endings.")
            } else {
                format!("No changes to apply: the edit leaves {path} unchanged. Line endings follow the file; to change them, write \\r explicitly in oldString or newString.")
            }));
        }
        let written = file_write::commit(&context, &target, &replaced).await.map_err(|_| unable())?;
        let formatted = match written.formatted {
            Some(formatted) => formatted,
            None => file_write::read_text(&target.absolute).map_err(|_| unable())?.1,
        };
        let diff = file_diff(&target.resource, &source, &formatted, "modified");
        let mut text = Obj::new();
        let plural = if replacements == 1 { "" } else { "s" };
        text.str("type", "text").str("text", &format!("Edited {} ({replacements} replacement{plural})", diff.file));
        let mut metadata = Obj::new();
        metadata.set("files", Json::Array(vec![diff.to_json()]));
        Ok(Output { content: vec![text.done()], metadata: Some(metadata.done()) })
    }
}

impl Tool for Edit {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn permission(&self) -> &str {
        "edit"
    }

    fn execute<'a>(&'a self, context: Context, input: Json) -> Execution<'a> {
        Box::pin(self.run(context, input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(source: &str, old: &str, new: &str) -> String {
        let (matches, replacement) = match_view(source, old, new);
        let m = matches[0];
        let region = &source[m.start..m.end];
        format!("{}{}{}", &source[..m.start], with_region_endings(region, &replacement, line_ending(region, line_ending(source, "\n"))), &source[m.end..])
    }

    #[test]
    fn crlf_view() {
        assert_eq!(apply("a\r\nb\r\nc\r\n", "a\nb", "x\ny"), "x\r\ny\r\nc\r\n");
        assert_eq!(apply("one\r\ntwo\r\n", "two\n", "2\n3\n"), "one\r\n2\r\n3\r\n");
    }

    #[test]
    fn tiers() {
        assert_eq!(apply("say \u{201c}hi\u{201d} now", "say \"hi\"", "say 'yo'"), "say 'yo' now");
        assert_eq!(apply("  value = 1   \nnext\n", "  value = 1\nnext", "  value = 2\nnext"), "  value = 2\nnext\n");
        let (matches, _) = match_view("caf\u{e9} \u{2014} x", "caf\u{e9} - x", "y");
        assert_eq!(matches, vec![Match { start: 0, end: "caf\u{e9} \u{2014} x".len() }]);
    }

    /// Cases recorded from edit.ts's matching (`EDIT_CASES=<file>`: `[{source, old, new, replaceAll, count, replaced,
    /// hint}]`).
    #[test]
    #[ignore]
    fn recorded() {
        let path = std::env::var("EDIT_CASES").expect("EDIT_CASES names the recorded cases");
        let cases = json::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut failures = 0;
        for case in json::array(&cases).unwrap() {
            let get = |key: &str| json::get_str(case, key).unwrap_or_default();
            let (count, replaced) = super::apply(get("source"), get("old"), get("new"), json::get_bool(case, "replaceAll") == Some(true));
            let hint = if count == 0 { miss_hint(get("source"), get("old"), get("new")) } else { None };
            let want = (json::get_num(case, "count").unwrap() as usize, get("replaced").to_owned(), json::get_str(case, "hint").map(str::to_owned));
            if (count, replaced.clone(), hint.clone()) != want {
                failures += 1;
                if failures < 4 {
                    eprintln!("case {}\nwant {want:?}\ngot  {:?}", json::text(case), (count, replaced, hint));
                }
            }
        }
        assert_eq!(failures, 0);
    }

    #[test]
    fn hints() {
        let source = "export function add(a: number, b: number) {\n  return a + b\n}\n";
        assert_eq!(miss_hint(source, "return a - b", "return b"), None);
        assert_eq!(
            miss_hint(source, "export function add(a: number, b: number) {\n  return a - b\n}", "x").as_deref(),
            Some("The closest lines in the file:\n1: export function add(a: number, b: number) {\n2:   return a + b\n3: }")
        );
    }
}
