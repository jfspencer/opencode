//! The patch format the `patch` tool applies (packages/util/src/patch.ts): `*** Begin Patch` ... `*** End Patch`
//! around `*** Add File:`, `*** Delete File:` and `*** Update File:` hunks (an update may `*** Move to:` and holds
//! `@@` chunks of context, removed and added lines, optionally pinned to the end of the file). [`parse`] reads a
//! patch, with TS's error texts; [`derive`] applies an update's chunks to a file's text, seeking each chunk's lines
//! exactly, then ignoring trailing whitespace, then surrounding whitespace, then typographic quote, dash and space
//! variants.
//!
//! Strings compare as TS's do: every slice is at an ASCII prefix, and `trim` is JavaScript's. `harness/patch/run.sh`
//! checks both functions against the reference on generated patches.

use crate::text::is_js_whitespace;

/// `Patch.Hunk`.
#[derive(Debug, Clone, PartialEq)]
pub enum Hunk {
    Add { path: String, contents: String },
    Delete { path: String },
    Update { path: String, move_path: Option<String>, chunks: Vec<Chunk> },
}

impl Hunk {
    pub fn path(&self) -> &str {
        match self {
            Hunk::Add { path, .. } | Hunk::Delete { path } | Hunk::Update { path, .. } => path,
        }
    }
}

/// `Patch.UpdateFileChunk`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chunk {
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
    pub change_context: Option<String>,
    pub end_of_file: bool,
}

/// `Patch.ParseError`.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// `BoundaryError`: `first` or `last`.
    Boundary(&'static str),
    /// `InvalidHunkError`.
    InvalidHunk { line: String, line_number: usize, reason: Option<String> },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Boundary(boundary) => write!(
                f,
                "The {boundary} line of the patch must be '{}'",
                if *boundary == "first" { "*** Begin Patch" } else { "*** End Patch" }
            ),
            ParseError::InvalidHunk { line_number, reason: Some(reason), .. } => {
                write!(f, "Invalid hunk at line {line_number}: {reason}")
            }
            ParseError::InvalidHunk { line, line_number, reason: None } => write!(
                f,
                "Invalid hunk at line {line_number}: '{line}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
            ),
        }
    }
}

fn trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

fn trim_end(text: &str) -> &str {
    text.trim_end_matches(is_js_whitespace)
}

fn invalid(line: &str, line_number: usize, reason: Option<String>) -> ParseError {
    ParseError::InvalidHunk { line: line.to_owned(), line_number, reason }
}

/// `Patch.parse`.
pub fn parse(patch_text: &str) -> Result<Vec<Hunk>, ParseError> {
    let stripped = strip_heredoc(trim(patch_text));
    let lines: Vec<&str> = stripped.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)).collect();
    if lines.first().map(|l| trim(l)) != Some("*** Begin Patch") {
        return Err(ParseError::Boundary("first"));
    }
    let begin = 0;
    if lines.last().map(|l| trim(l)) != Some("*** End Patch") || lines.len() == 1 {
        return Err(ParseError::Boundary("last"));
    }
    let end = lines.len() - 1;
    let mut hunks = Vec::new();
    let mut index = begin + 1;
    while index < end {
        let header = trim(lines[index]);
        if index == begin + 1 {
            if let Some(id) = header.strip_prefix("*** Environment ID:") {
                if !trim(id).is_empty() {
                    index += 1;
                    continue;
                }
            }
        }
        if let Some(rest) = header.strip_prefix("*** Add File: ") {
            let path = trim(rest).to_owned();
            let (contents, next) = parse_add(&lines, index + 1, end, &path)?;
            hunks.push(Hunk::Add { path, contents });
            index = next;
            continue;
        }
        if let Some(rest) = header.strip_prefix("*** Delete File: ") {
            let path = trim(rest).to_owned();
            if index + 1 < end {
                let next = trim(lines[index + 1]);
                if !is_boundary(next) {
                    if next.starts_with("*** ") {
                        return Err(invalid(next, index + 2, None));
                    }
                    let reason = format!("Unexpected line after Delete File '{path}': '{next}'. Delete hunks do not contain body lines");
                    return Err(invalid(next, index + 2, Some(reason)));
                }
            }
            hunks.push(Hunk::Delete { path });
            index += 1;
            continue;
        }
        if let Some(rest) = header.strip_prefix("*** Update File: ") {
            let path = trim(rest).to_owned();
            let mut next = index + 1;
            while lines.get(next).is_some_and(|l| trim_end(l) == "*** End of File") {
                next += 1;
            }
            let mut move_path = None;
            if let Some(line) = lines.get(next) {
                let candidate = trim_end(line);
                if candidate == "*** Move to:" || candidate.starts_with("*** Move to: ") {
                    let destination = trim(candidate.get("*** Move to: ".len()..).unwrap_or(""));
                    if destination.is_empty() {
                        let reason = format!("Move destination for '{path}' must not be empty");
                        return Err(invalid(trim(line), next + 1, Some(reason)));
                    }
                    move_path = Some(destination.to_owned());
                    next += 1;
                }
            }
            let (chunks, after) = parse_update(&lines, next, end, &path, index)?;
            hunks.push(Hunk::Update { path, move_path, chunks });
            index = after;
            continue;
        }
        return Err(invalid(header, index + 1, None));
    }
    Ok(hunks)
}

fn parse_add(lines: &[&str], start: usize, end: usize, path: &str) -> Result<(String, usize), ParseError> {
    let mut content: Vec<&str> = Vec::new();
    let mut index = start;
    while index < end && !is_boundary(trim(lines[index])) {
        let Some(rest) = lines[index].strip_prefix('+') else {
            let line = trim(lines[index]);
            let reason = format!("Invalid Add File line for '{path}': expected a line starting with '+', got '{line}'");
            return Err(invalid(line, index + 1, Some(reason)));
        };
        content.push(rest);
        index += 1;
    }
    Ok((content.join("\n"), index))
}

const UNEXPECTED: &str = "Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)";

fn is_marker(line: &str) -> bool {
    line == "@@" || line.starts_with("@@ ")
}

fn empty(chunk: &Chunk) -> bool {
    chunk.old_lines.is_empty() && chunk.new_lines.is_empty()
}

fn parse_update(lines: &[&str], start: usize, end: usize, path: &str, hunk: usize) -> Result<(Vec<Chunk>, usize), ParseError> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut index = start;
    let mut after_end_of_file = false;
    while index < end {
        let line = lines[index];
        let update_line = trim_end(line);
        if after_end_of_file {
            if update_line.is_empty() {
                index += 1;
                continue;
            }
            if is_marker(update_line) {
                after_end_of_file = false;
            } else if is_boundary(update_line) {
                break;
            } else {
                let reason = format!("Expected update hunk to start with a @@ context marker, got: '{line}'");
                return Err(invalid(line, index + 1, Some(reason)));
            }
        }
        if update_line == "*** End of File" {
            if let Some(chunk) = chunks.last_mut() {
                if empty(chunk) {
                    return Err(invalid(update_line, index + 1, Some("Update hunk does not contain any lines".into())));
                }
                chunk.end_of_file = true;
                after_end_of_file = true;
            }
            index += 1;
            continue;
        }
        if is_boundary(update_line) {
            break;
        }
        if is_marker(update_line) {
            if chunks.last().is_some_and(empty) {
                let reason = format!("Unexpected line found in update hunk: '{line}'. {UNEXPECTED}");
                return Err(invalid(line, index + 1, Some(reason)));
            }
            let context = if update_line == "@@" { None } else { Some(update_line["@@ ".len()..].to_owned()) };
            chunks.push(Chunk { change_context: context, ..Chunk::default() });
            index += 1;
            continue;
        }
        if chunks.is_empty() {
            chunks.push(Chunk::default());
        }
        let chunk = chunks.last_mut().expect("a chunk");
        if line.is_empty() {
            chunk.old_lines.push(String::new());
            chunk.new_lines.push(String::new());
        } else if let Some(rest) = line.strip_prefix(' ') {
            chunk.old_lines.push(rest.to_owned());
            chunk.new_lines.push(rest.to_owned());
        } else if let Some(rest) = line.strip_prefix('-') {
            chunk.old_lines.push(rest.to_owned());
        } else if let Some(rest) = line.strip_prefix('+') {
            chunk.new_lines.push(rest.to_owned());
        } else {
            let reason = if empty(chunk) {
                format!("Unexpected line found in update hunk: '{line}'. {UNEXPECTED}")
            } else {
                format!("Expected update hunk to start with a @@ context marker, got: '{line}'")
            };
            return Err(invalid(line, index + 1, Some(reason)));
        }
        index += 1;
    }
    if chunks.is_empty() {
        let reason = format!("Update file hunk for path '{path}' is empty");
        return Err(invalid(trim(lines[hunk]), hunk + 1, Some(reason)));
    }
    if chunks.last().is_some_and(empty) {
        let line = trim(lines[index]);
        let reason = if line == "*** End Patch" {
            "Update hunk does not contain any lines".to_owned()
        } else {
            format!("Unexpected line found in update hunk: '{line}'. {UNEXPECTED}")
        };
        return Err(invalid(line, index + 1, Some(reason)));
    }
    Ok((chunks, index))
}

fn is_boundary(line: &str) -> bool {
    line == "*** End Patch"
        || line.starts_with("*** Add File: ")
        || line.starts_with("*** Delete File: ")
        || line.starts_with("*** Update File: ")
}

/// `stripHeredoc`: `/^(?:cat\s+)?<<(['"]?)(\w+)\1\s*\n([\s\S]*?)\n\2\s*$/`'s body, else the input.
fn strip_heredoc(input: &str) -> &str {
    heredoc(input).unwrap_or(input)
}

fn heredoc(input: &str) -> Option<&str> {
    // `(?:cat\s+)?`: tried with the prefix first.
    let after_cat = input.strip_prefix("cat").and_then(|rest| {
        let trimmed = rest.trim_start_matches(is_js_whitespace);
        (trimmed.len() < rest.len()).then_some(trimmed)
    });
    after_cat.and_then(heredoc_body).or_else(|| heredoc_body(input))
}

fn heredoc_body(input: &str) -> Option<&str> {
    let rest = input.strip_prefix("<<")?;
    // `(['"]?)`: a quote when there is one (a quote not closed after the word cannot match without it either).
    let (quote, rest) = match rest.chars().next() {
        Some(q @ ('\'' | '"')) => (Some(q), &rest[1..]),
        _ => (None, rest),
    };
    let word_len = rest.bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'_').count();
    if word_len == 0 {
        return None;
    }
    let (word, mut rest) = rest.split_at(word_len);
    if let Some(q) = quote {
        rest = rest.strip_prefix(q)?;
    }
    // `\s*\n`: the whitespace run is greedy, so the body starts after its last newline first, then earlier ones.
    let run = rest.len() - rest.trim_start_matches(is_js_whitespace).len();
    let newlines: Vec<usize> = rest[..run].match_indices('\n').map(|(i, _)| i).collect();
    for newline in newlines.into_iter().rev() {
        let body = &rest[newline + 1..];
        // `([\s\S]*?)\n\2\s*$`: the shortest body followed by a newline, the word and only whitespace.
        for (at, _) in body.match_indices('\n') {
            let tail = &body[at + 1..];
            if let Some(after) = tail.strip_prefix(word) {
                if after.chars().all(is_js_whitespace) {
                    return Some(&body[..at]);
                }
            }
        }
    }
    None
}

/// `Patch.FileUpdate`: the text without a BOM, and whether the file keeps one.
#[derive(Debug, Clone, PartialEq)]
pub struct FileUpdate {
    pub content: String,
    pub bom: bool,
}

const BOM: char = '\u{feff}';

/// `Patch.derive`: an update's chunks applied to a file's text (its BOM split off); `Err` is the thrown message.
pub fn derive(path: &str, chunks: &[Chunk], original: &str) -> Result<FileUpdate, String> {
    let source = original.trim_start_matches(BOM);
    let source_bom = source.len() != original.len();
    let mut lines: Vec<String> = source.split('\n').map(str::to_owned).collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let replacements = compute_replacements(&lines, path, chunks)?;
    for (start, remove, insert) in replacements.into_iter().rev() {
        // `Array.prototype.splice` clamps its range.
        let start = start.min(lines.len());
        let end = (start + remove).min(lines.len());
        lines.splice(start..end, insert);
    }
    if lines.last().is_none_or(|l| !l.is_empty()) {
        lines.push(String::new());
    }
    let joined = lines.join("\n");
    let next = joined.trim_start_matches(BOM);
    let next_bom = next.len() != joined.len();
    Ok(FileUpdate { content: next.to_owned(), bom: source_bom || next_bom })
}

type Replacement = (usize, usize, Vec<String>);

fn compute_replacements(lines: &[String], path: &str, chunks: &[Chunk]) -> Result<Vec<Replacement>, String> {
    let mut replacements: Vec<Replacement> = Vec::new();
    let mut line_index = 0;
    for chunk in chunks {
        if let Some(context) = chunk.change_context.as_deref().filter(|c| !c.is_empty()) {
            let found = seek(lines, &[context.to_owned()], line_index, false)
                .ok_or_else(|| format!("Failed to find context '{context}' in {path}"))?;
            line_index = found + 1;
        }
        if chunk.old_lines.is_empty() {
            replacements.push((lines.len(), 0, chunk.new_lines.clone()));
            continue;
        }
        let mut old_lines: &[String] = &chunk.old_lines;
        let mut new_lines: &[String] = &chunk.new_lines;
        let mut found = seek(lines, old_lines, line_index, chunk.end_of_file);
        if found.is_none() && old_lines.last().is_some_and(|l| l.is_empty()) {
            old_lines = &old_lines[..old_lines.len() - 1];
            if new_lines.last().is_some_and(|l| l.is_empty()) {
                new_lines = &new_lines[..new_lines.len() - 1];
            }
            found = seek(lines, old_lines, line_index, chunk.end_of_file);
        }
        let Some(found) = found else {
            if chunk.old_lines.iter().all(|l| l.is_empty()) {
                let expected = if chunk.old_lines.len() == 1 {
                    "an expected blank line".to_owned()
                } else {
                    format!("{} consecutive blank lines", chunk.old_lines.len())
                };
                return Err(format!("Failed to find {expected} in {path}"));
            }
            return Err(format!("Failed to find expected lines in {path}:\n{}", chunk.old_lines.join("\n")));
        };
        replacements.push((found, old_lines.len(), new_lines.to_vec()));
        line_index = found + old_lines.len();
    }
    // A stable sort by start.
    replacements.sort_by_key(|(start, _, _)| *start);
    Ok(replacements)
}

type Compare = fn(&str, &str) -> bool;

const COMPARES: [Compare; 4] = [
    |left, right| left == right,
    |left, right| trim_end(left) == trim_end(right),
    |left, right| trim(left) == trim(right),
    |left, right| normalize(trim(left)) == normalize(trim(right)),
];

fn seek(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() || pattern.len() > lines.len() {
        return None;
    }
    let last = lines.len() - pattern.len();
    if eof {
        if last < start {
            return None;
        }
        return COMPARES.iter().any(|compare| matches(lines, pattern, last, *compare)).then_some(last);
    }
    COMPARES.iter().find_map(|compare| (start..=last).find(|offset| matches(lines, pattern, *offset, *compare)))
}

fn matches(lines: &[String], pattern: &[String], offset: usize, compare: Compare) -> bool {
    pattern.iter().enumerate().all(|(index, line)| compare(&lines[offset + index], line))
}

/// `normalize`: typographic quotes, dashes and spaces as their ASCII forms.
fn normalize(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => '-',
            '\u{a0}' | '\u{2002}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_derives() {
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n one\n-two\n+TWO\n*** End Patch";
        let hunks = parse(patch).unwrap();
        let Hunk::Update { chunks, .. } = &hunks[0] else { panic!() };
        assert_eq!(derive("a.txt", chunks, "one\ntwo\nthree\n").unwrap().content, "one\nTWO\nthree\n");
        assert_eq!(parse("nope").unwrap_err().to_string(), "The first line of the patch must be '*** Begin Patch'");
        assert_eq!(strip_heredoc("cat <<'EOF'\nbody\nEOF"), "body");
        assert_eq!(strip_heredoc("<<EOF \n\nEOF"), "");
    }

    /// Cases recorded from the reference's `Patch.parse` and `Patch.derive` (`PATCH_CASES=<file>`, see
    /// harness/patch/run.sh): `[{patch, parsed: {hunks} | {error}, files: [{path, original, derived: {content, bom} |
    /// {error}}]}]`.
    #[test]
    #[ignore]
    fn recorded_patches() {
        use crate::json;
        use opencode_protocol::Json;
        let path = std::env::var("PATCH_CASES").expect("PATCH_CASES names the recorded cases");
        let cases = json::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut failures = 0;
        let cases = json::array(&cases).unwrap();
        for (index, case) in cases.iter().enumerate() {
            let patch = json::get_str(case, "patch").unwrap();
            let expected = json::get(case, "parsed").unwrap();
            let actual = match parse(patch) {
                Ok(hunks) => {
                    let mut o = json::Obj::new();
                    o.set("hunks", Json::Array(hunks.iter().map(hunk_json).collect()));
                    o.done()
                }
                Err(error) => {
                    let mut o = json::Obj::new();
                    o.str("error", &error.to_string());
                    o.done()
                }
            };
            if json::text(&actual) != json::text(expected) {
                failures += 1;
                if failures <= 5 {
                    eprintln!("parse case {index}: {patch:?}\n  expected {}\n  actual   {}", json::text(expected), json::text(&actual));
                }
                continue;
            }
            let Ok(hunks) = parse(patch) else { continue };
            for file in json::array(json::get(case, "files").unwrap_or(&Json::Null)).unwrap_or_default() {
                let at = json::get_num(file, "hunk").unwrap() as usize;
                let Hunk::Update { chunks, .. } = &hunks[at] else { continue };
                let original = json::get_str(file, "original").unwrap();
                let actual = match derive("f.txt", chunks, original) {
                    Ok(update) => {
                        let mut o = json::Obj::new();
                        o.str("content", &update.content).set("bom", Json::Bool(update.bom));
                        o.done()
                    }
                    Err(error) => {
                        let mut o = json::Obj::new();
                        o.str("error", &error);
                        o.done()
                    }
                };
                let expected = json::get(file, "derived").unwrap();
                if json::text(&actual) != json::text(expected) {
                    failures += 1;
                    if failures <= 5 {
                        eprintln!("derive case {index}: {patch:?} on {original:?}\n  expected {}\n  actual   {}", json::text(expected), json::text(&actual));
                    }
                }
            }
        }
        eprintln!("{} patch cases, {failures} differences", cases.len());
        assert_eq!(failures, 0);
    }

    #[cfg(test)]
    fn hunk_json(hunk: &Hunk) -> opencode_protocol::Json {
        use crate::json;
        use opencode_protocol::Json;
        let strings = |items: &[String]| Json::Array(items.iter().map(|s| json::string(s)).collect());
        let mut o = json::Obj::new();
        match hunk {
            Hunk::Add { path, contents } => {
                o.str("type", "add").str("path", path).str("contents", contents);
            }
            Hunk::Delete { path } => {
                o.str("type", "delete").str("path", path);
            }
            Hunk::Update { path, move_path, chunks } => {
                o.str("type", "update").str("path", path);
                o.opt_str("movePath", move_path.as_deref());
                let chunks = chunks
                    .iter()
                    .map(|chunk| {
                        let mut c = json::Obj::new();
                        c.set("oldLines", strings(&chunk.old_lines)).set("newLines", strings(&chunk.new_lines));
                        c.opt_str("changeContext", chunk.change_context.as_deref());
                        if chunk.end_of_file {
                            c.set("endOfFile", Json::Bool(true));
                        }
                        c.done()
                    })
                    .collect();
                o.set("chunks", Json::Array(chunks));
            }
        }
        o.done()
    }
}
