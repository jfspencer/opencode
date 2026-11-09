//! jsdiff 8.0.4 (npm `diff`): `diffLines` (its Myers variant, with the same path choices, so ties resolve as in TS)
//! and `createPatch` / `structuredPatch` / `formatPatch` with their hunk building and `\ No newline at end of file`
//! markers.

/// One change (`ChangeObject`): `count` tokens, added, removed or common.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub value: String,
    pub count: usize,
    pub added: bool,
    pub removed: bool,
}

/// `tokenize` of `LineDiff`: lines with their terminators (`\n` or `\r\n`).
fn tokenize(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = value.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            out.push(&value[start..=i]);
            start = i + 1;
        }
    }
    if start < value.len() {
        out.push(&value[start..]);
    }
    out
}

struct Component {
    count: usize,
    added: bool,
    removed: bool,
    previous: Option<usize>,
}

#[derive(Clone, Copy)]
struct Path {
    old_pos: i64,
    last: Option<usize>,
}

struct Builder {
    components: Vec<Component>,
}

impl Builder {
    fn add_to_path(&mut self, path: Path, added: bool, removed: bool, inc: i64) -> Path {
        let last = path.last.map(|i| (i, &self.components[i]));
        if let Some((_, component)) = last.filter(|(_, c)| c.added == added && c.removed == removed) {
            let replacement = Component { count: component.count + 1, added, removed, previous: component.previous };
            self.components.push(replacement);
        } else {
            self.components.push(Component { count: 1, added, removed, previous: path.last });
        }
        Path { old_pos: path.old_pos + inc, last: Some(self.components.len() - 1) }
    }

    fn extract_common(&mut self, path: &mut Path, new: &[&str], old: &[&str], diagonal: i64) -> i64 {
        let (new_len, old_len) = (new.len() as i64, old.len() as i64);
        let mut old_pos = path.old_pos;
        let mut new_pos = old_pos - diagonal;
        let mut common = 0;
        while new_pos + 1 < new_len && old_pos + 1 < old_len && old[(old_pos + 1) as usize] == new[(new_pos + 1) as usize] {
            new_pos += 1;
            old_pos += 1;
            common += 1;
        }
        if common > 0 {
            self.components.push(Component { count: common, added: false, removed: false, previous: path.last });
            path.last = Some(self.components.len() - 1);
        }
        path.old_pos = old_pos;
        new_pos
    }

    fn values(&self, last: Option<usize>, new: &[&str], old: &[&str]) -> Vec<Change> {
        let mut chain = Vec::new();
        let mut next = last;
        while let Some(i) = next {
            chain.push(i);
            next = self.components[i].previous;
        }
        chain.reverse();
        let (mut new_pos, mut old_pos) = (0, 0);
        chain
            .into_iter()
            .map(|i| {
                let c = &self.components[i];
                let value = if !c.removed {
                    let value = new[new_pos..new_pos + c.count].concat();
                    new_pos += c.count;
                    if !c.added {
                        old_pos += c.count;
                    }
                    value
                } else {
                    let value = old[old_pos..old_pos + c.count].concat();
                    old_pos += c.count;
                    value
                };
                Change { value, count: c.count, added: c.added, removed: c.removed }
            })
            .collect()
    }
}

/// `diffLines(old, new)`.
pub fn diff_lines(old: &str, new: &str) -> Vec<Change> {
    let old_tokens = tokenize(old);
    let new_tokens = tokenize(new);
    let (old_len, new_len) = (old_tokens.len() as i64, new_tokens.len() as i64);
    let mut builder = Builder { components: Vec::new() };
    let offset = (old_len + new_len + 1) as usize;
    let mut best: Vec<Option<Path>> = vec![None; offset * 2 + 1];
    let at = |d: i64| (d + offset as i64) as usize;
    let mut first = Path { old_pos: -1, last: None };
    let new_pos = builder.extract_common(&mut first, &new_tokens, &old_tokens, 0);
    if first.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
        return builder.values(first.last, &new_tokens, &old_tokens);
    }
    best[at(0)] = Some(first);
    let (mut min_diagonal, mut max_diagonal) = (i64::MIN, i64::MAX);
    let max_edit = old_len + new_len;
    let mut edit = 1;
    while edit <= max_edit {
        let mut diagonal = min_diagonal.max(-edit);
        while diagonal <= max_diagonal.min(edit) {
            let remove = best[at(diagonal - 1)];
            let add = best[at(diagonal + 1)];
            if remove.is_some() {
                best[at(diagonal - 1)] = None;
            }
            let can_add = add.is_some_and(|p| {
                let pos = p.old_pos - diagonal;
                0 <= pos && pos < new_len
            });
            let can_remove = remove.is_some_and(|p| p.old_pos + 1 < old_len);
            if !can_add && !can_remove {
                best[at(diagonal)] = None;
                diagonal += 2;
                continue;
            }
            let mut path = if !can_remove || (can_add && remove.unwrap().old_pos < add.unwrap().old_pos) {
                builder.add_to_path(add.unwrap(), true, false, 0)
            } else {
                builder.add_to_path(remove.unwrap(), false, true, 1)
            };
            let new_pos = builder.extract_common(&mut path, &new_tokens, &old_tokens, diagonal);
            if path.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                return builder.values(path.last, &new_tokens, &old_tokens);
            }
            best[at(diagonal)] = Some(path);
            if path.old_pos + 1 >= old_len {
                max_diagonal = max_diagonal.min(diagonal - 1);
            }
            if new_pos + 1 >= new_len {
                min_diagonal = min_diagonal.max(diagonal + 1);
            }
            diagonal += 2;
        }
        edit += 1;
    }
    vec![]
}

/// `splitLines` of patch/create.js.
fn split_lines(text: &str) -> Vec<String> {
    let mut result: Vec<String> = text.split('\n').map(|line| format!("{line}\n")).collect();
    if text.ends_with('\n') {
        result.pop();
    } else if let Some(last) = result.pop() {
        result.push(last[..last.len() - 1].to_owned());
    }
    result
}

pub struct Hunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<String>,
}

/// `structuredPatch(...).hunks` with `context` lines around each change.
pub fn hunks(old: &str, new: &str, context: usize) -> Vec<Hunk> {
    let mut diff: Vec<(Vec<String>, bool, bool)> =
        diff_lines(old, new).into_iter().map(|c| (split_lines(&c.value), c.added, c.removed)).collect();
    diff.push((vec![], false, false));
    let context_lines = |lines: &[String]| lines.iter().map(|l| format!(" {l}")).collect::<Vec<_>>();
    let mut hunks = Vec::new();
    let (mut old_range, mut new_range) = (0usize, 0usize);
    let mut current: Vec<String> = Vec::new();
    let (mut old_line, mut new_line) = (1usize, 1usize);
    for i in 0..diff.len() {
        let (lines, added, removed) = &diff[i];
        if *added || *removed {
            if old_range == 0 {
                old_range = old_line;
                new_range = new_line;
                if i > 0 {
                    let previous = &diff[i - 1].0;
                    current = if context > 0 { context_lines(&previous[previous.len().saturating_sub(context)..]) } else { vec![] };
                    old_range -= current.len();
                    new_range -= current.len();
                }
            }
            current.extend(lines.iter().map(|l| format!("{}{l}", if *added { '+' } else { '-' })));
            if *added {
                new_line += lines.len();
            } else {
                old_line += lines.len();
            }
        } else {
            if old_range != 0 {
                if lines.len() <= context * 2 && i + 2 < diff.len() {
                    current.extend(context_lines(lines));
                } else {
                    let size = lines.len().min(context);
                    current.extend(context_lines(&lines[..size]));
                    hunks.push(Hunk {
                        old_start: old_range,
                        old_lines: old_line - old_range + size,
                        new_start: new_range,
                        new_lines: new_line - new_range + size,
                        lines: std::mem::take(&mut current),
                    });
                    old_range = 0;
                    new_range = 0;
                }
            }
            old_line += lines.len();
            new_line += lines.len();
        }
    }
    for hunk in &mut hunks {
        let mut i = 0;
        while i < hunk.lines.len() {
            if hunk.lines[i].ends_with('\n') {
                hunk.lines[i].pop();
            } else {
                hunk.lines.insert(i + 1, "\\ No newline at end of file".to_owned());
                i += 1;
            }
            i += 1;
        }
    }
    hunks
}

/// `createPatch(file, old, new, oldHeader, newHeader, { context })` (`formatPatch` with every header).
pub fn create_patch(file: &str, old: &str, new: &str, old_header: &str, new_header: &str, context: usize) -> String {
    format_patch(file, old, new, Some((old_header, new_header)), context)
}

/// `createTwoFilesPatch(file, file, old, new)`: without file headers, so no tab follows the names.
pub fn create_patch_without_headers(file: &str, old: &str, new: &str, context: usize) -> String {
    format_patch(file, old, new, None, context)
}

fn format_patch(file: &str, old: &str, new: &str, headers: Option<(&str, &str)>, context: usize) -> String {
    let header = |header: Option<&str>| header.map(|h| format!("\t{h}")).unwrap_or_default();
    let mut out = vec![
        format!("Index: {file}"),
        "===================================================================".to_owned(),
        format!("--- {file}{}", header(headers.map(|h| h.0))),
        format!("+++ {file}{}", header(headers.map(|h| h.1))),
    ];
    for hunk in hunks(old, new, context) {
        let old_start = if hunk.old_lines == 0 { hunk.old_start - 1 } else { hunk.old_start };
        let new_start = if hunk.new_lines == 0 { hunk.new_start - 1 } else { hunk.new_start };
        out.push(format!("@@ -{old_start},{} +{new_start},{} @@", hunk.old_lines, hunk.new_lines));
        out.extend(hunk.lines);
    }
    out.join("\n") + "\n"
}

/// One hunk of a parsed patch (`StructuredPatchHunk`): its lines keep their `+`, `-`, ` ` or `\` prefix.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchHunk {
    pub old_start: i64,
    pub old_lines: i64,
    pub new_start: i64,
    pub new_lines: i64,
    pub lines: Vec<String>,
}

/// `parsePatch(uniDiff)`: each file's hunks; `Err` is the thrown message.
pub fn parse_patch(uni_diff: &str) -> Result<Vec<Vec<PatchHunk>>, String> {
    let lines: Vec<&str> = uni_diff.split('\n').collect();
    let starts_ws = |line: &str, prefixes: &[&str]| {
        prefixes.iter().any(|p| line.strip_prefix(p).and_then(|rest| rest.chars().next()).is_some_and(crate::text::is_js_whitespace))
    };
    let mut list = vec![];
    let mut i = 0;
    while i < lines.len() {
        while i < lines.len() && !starts_ws(lines[i], &["---", "+++", "@@"]) {
            i += 1;
        }
        for _ in 0..2 {
            if i < lines.len() && starts_ws(lines[i], &["---", "+++"]) {
                i += 1;
            }
        }
        let mut hunks = vec![];
        while i < lines.len() {
            let line = lines[i];
            if starts_ws(line, &["Index:", "diff", "---", "+++"]) || line.starts_with("===================================================================") {
                break;
            } else if line.starts_with("@@") {
                hunks.push(parse_hunk(&lines, &mut i)?);
            } else if !line.is_empty() {
                return Err(format!("Unknown line {} {}", i + 1, crate::json::text(&crate::json::string(line))));
            } else {
                i += 1;
            }
        }
        list.push(hunks);
    }
    Ok(list)
}

fn parse_hunk(lines: &[&str], i: &mut usize) -> Result<PatchHunk, String> {
    let header_index = *i;
    let header = lines[*i];
    *i += 1;
    let numbers = parse_hunk_header(header).ok_or_else(|| format!("Hunk at line {} contained invalid line {header}", header_index + 1))?;
    let mut hunk = PatchHunk {
        old_start: numbers.0,
        old_lines: numbers.1.unwrap_or(1),
        new_start: numbers.2,
        new_lines: numbers.3.unwrap_or(1),
        lines: vec![],
    };
    if hunk.old_lines == 0 {
        hunk.old_start += 1;
    }
    if hunk.new_lines == 0 {
        hunk.new_start += 1;
    }
    let (mut added, mut removed) = (0i64, 0i64);
    while *i < lines.len() && (removed < hunk.old_lines || added < hunk.new_lines || lines[*i].starts_with('\\')) {
        let line = lines[*i];
        let operation = if line.is_empty() && *i != lines.len() - 1 { Some(' ') } else { line.chars().next() };
        match operation {
            Some('+') => added += 1,
            Some('-') => removed += 1,
            Some(' ') => {
                added += 1;
                removed += 1;
            }
            Some('\\') => {}
            _ => return Err(format!("Hunk at line {} contained invalid line {line}", header_index + 1)),
        }
        hunk.lines.push(line.to_owned());
        *i += 1;
    }
    if added == 0 && hunk.new_lines == 1 {
        hunk.new_lines = 0;
    }
    if removed == 0 && hunk.old_lines == 1 {
        hunk.old_lines = 0;
    }
    if added != hunk.new_lines {
        return Err(format!("Added line count did not match for hunk at line {}", header_index + 1));
    }
    if removed != hunk.old_lines {
        return Err(format!("Removed line count did not match for hunk at line {}", header_index + 1));
    }
    Ok(hunk)
}

/// `@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@` anywhere in the line.
fn parse_hunk_header(line: &str) -> Option<(i64, Option<i64>, i64, Option<i64>)> {
    let digits = |text: &str| -> Option<(i64, usize)> {
        let count = text.bytes().take_while(u8::is_ascii_digit).count();
        (count > 0).then(|| (text[..count].parse().unwrap_or(i64::MAX), count))
    };
    for (at, _) in line.match_indices("@@ -") {
        let rest = &line[at + 4..];
        let Some((old_start, n)) = digits(rest) else { continue };
        let mut rest = &rest[n..];
        let mut old_lines = None;
        if let Some(after) = rest.strip_prefix(',') {
            let Some((value, n)) = digits(after) else { continue };
            old_lines = Some(value);
            rest = &after[n..];
        }
        let Some(rest) = rest.strip_prefix(" +") else { continue };
        let Some((new_start, n)) = digits(rest) else { continue };
        let mut rest = &rest[n..];
        let mut new_lines = None;
        if let Some(after) = rest.strip_prefix(',') {
            let Some((value, n)) = digits(after) else { continue };
            new_lines = Some(value);
            rest = &after[n..];
        }
        if rest.starts_with(" @@") {
            return Some((old_start, old_lines, new_start, new_lines));
        }
    }
    None
}

/// `reversePatch(patch)` of one file's hunks.
pub fn reverse_patch(hunks: &[PatchHunk]) -> Vec<PatchHunk> {
    hunks
        .iter()
        .map(|hunk| PatchHunk {
            old_start: hunk.new_start,
            old_lines: hunk.new_lines,
            new_start: hunk.old_start,
            new_lines: hunk.old_lines,
            lines: hunk
                .lines
                .iter()
                .map(|line| match line.chars().next() {
                    Some('-') => format!("+{}", &line[1..]),
                    Some('+') => format!("-{}", &line[1..]),
                    _ => line.clone(),
                })
                .collect(),
        })
        .collect()
}

/// `applyPatch(source, patch)` with the default options (no fuzz, line endings converted to the source's);
/// `None` when a hunk does not fit (`false`).
pub fn apply_patch(source: &str, hunks: &[PatchHunk]) -> Option<String> {
    let mut hunks = hunks.to_vec();
    let only_win = source.contains("\r\n") && !source.starts_with('\n') && !source.as_bytes().windows(2).any(|w| w[0] != b'\r' && w[1] == b'\n');
    let only_unix = !source.contains("\r\n") && source.contains('\n');
    let is_unix = !hunks.iter().any(|h| h.lines.iter().any(|l| !l.starts_with('\\') && l.ends_with('\r')));
    let win_line = |h: &PatchHunk, i: usize| h.lines[i].starts_with('\\') || h.lines[i].ends_with('\r') || h.lines.get(i + 1).is_some_and(|n| n.starts_with('\\'));
    let is_win = hunks.iter().any(|h| h.lines.iter().any(|l| l.ends_with('\r'))) && hunks.iter().all(|h| (0..h.lines.len()).all(|i| win_line(h, i)));
    if only_win && is_unix {
        for hunk in &mut hunks {
            let converted: Vec<String> = (0..hunk.lines.len()).map(|i| if win_line(hunk, i) { hunk.lines[i].clone() } else { format!("{}\r", hunk.lines[i]) }).collect();
            hunk.lines = converted;
        }
    } else if only_unix && is_win {
        for hunk in &mut hunks {
            for line in &mut hunk.lines {
                if line.ends_with('\r') {
                    line.pop();
                }
            }
        }
    }
    let mut lines: Vec<String> = source.split('\n').map(String::from).collect();
    let Some(last_hunk) = hunks.last() else { return Some(source.to_owned()) };
    let (mut remove_eofnl, mut add_eofnl) = (false, false);
    let mut previous = "";
    for line in &last_hunk.lines {
        if line.starts_with('\\') {
            if previous.starts_with('+') {
                remove_eofnl = true;
            } else if previous.starts_with('-') {
                add_eofnl = true;
            }
        }
        previous = line;
    }
    let last_empty = lines.last().is_some_and(|l| l.is_empty());
    if remove_eofnl {
        if add_eofnl {
            if last_empty {
                return None;
            }
        } else if last_empty {
            lines.pop();
        } else {
            return None;
        }
    } else if add_eofnl {
        if !last_empty {
            lines.push(String::new());
        } else {
            return None;
        }
    }
    let line_at = |at: i64| if at >= 0 { lines.get(at as usize).map(String::as_str) } else { None };
    // `applyHunk` without fuzz: the patched lines and the last old line used, or nothing.
    let apply_hunk = |hunk_lines: &[String], mut to: i64| -> Option<(Vec<String>, i64)> {
        let mut patched: Vec<String> = vec![];
        let mut trailing_context = 0i64;
        for hunk_line in hunk_lines {
            let operation = hunk_line.chars().next().unwrap_or(' ');
            let content = if hunk_line.is_empty() { "" } else { &hunk_line[operation.len_utf8()..] };
            match operation {
                '-' => {
                    if line_at(to) != Some(content) {
                        return None;
                    }
                    to += 1;
                    trailing_context = 0;
                }
                '+' => {
                    patched.push(content.to_owned());
                    trailing_context = 0;
                }
                ' ' => {
                    trailing_context += 1;
                    if line_at(to) != Some(content) {
                        return None;
                    }
                    patched.push(content.to_owned());
                    to += 1;
                }
                _ => {}
            }
        }
        patched.truncate(patched.len() - trailing_context as usize);
        Some((patched, to - trailing_context - 1))
    };
    let mut result: Vec<String> = vec![];
    let mut min_line = 0i64;
    let mut previous_offset = 0i64;
    for hunk in &hunks {
        let max_line = lines.len() as i64 - hunk.old_lines;
        let start = hunk.old_start + previous_offset - 1;
        let mut iterator = Distance::new(start, min_line, max_line);
        let mut to = Some(start);
        let mut fitted = None;
        while let Some(position) = to {
            if let Some(found) = apply_hunk(&hunk.lines, position) {
                fitted = Some((position, found));
                break;
            }
            to = iterator.next();
        }
        let (position, (patched, last)) = fitted?;
        for i in min_line..position {
            result.push(line_at(i).unwrap_or_default().to_owned());
        }
        result.extend(patched);
        min_line = last + 1;
        previous_offset = position + 1 - hunk.old_start;
    }
    for i in min_line.max(0)..lines.len() as i64 {
        result.push(lines[i as usize].clone());
    }
    Some(result.join("\n"))
}

/// jsdiff's `distanceIterator`: positions in `[min, max]` by distance from `start`, forward first.
struct Distance {
    start: i64,
    min: i64,
    max: i64,
    want_forward: bool,
    backward_done: bool,
    forward_done: bool,
    offset: i64,
}

impl Distance {
    fn new(start: i64, min: i64, max: i64) -> Self {
        Self { start, min, max, want_forward: true, backward_done: false, forward_done: false, offset: 1 }
    }

    fn next(&mut self) -> Option<i64> {
        loop {
            if self.want_forward && !self.forward_done {
                if self.backward_done {
                    self.offset += 1;
                } else {
                    self.want_forward = false;
                }
                if self.start + self.offset <= self.max {
                    return Some(self.start + self.offset);
                }
                self.forward_done = true;
            }
            if !self.backward_done {
                if !self.forward_done {
                    self.want_forward = true;
                }
                if self.min <= self.start - self.offset {
                    let at = self.start - self.offset;
                    self.offset += 1;
                    return Some(at);
                }
                self.backward_done = true;
                continue;
            }
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_diff() {
        let changes = diff_lines("a\nb\nc\n", "a\nx\nc\n");
        let shape: Vec<(String, bool, bool)> = changes.into_iter().map(|c| (c.value, c.added, c.removed)).collect();
        assert_eq!(shape, [
            ("a\n".to_owned(), false, false),
            ("b\n".to_owned(), false, true),
            ("x\n".to_owned(), true, false),
            ("c\n".to_owned(), false, false)
        ]);
    }

    #[test]
    fn patch() {
        let rules = |changed: Option<usize>| {
            (1..=30)
                .map(|i| if Some(i) == changed { format!("Rule {i}: changed.") } else { format!("Rule {i}: keep it simple.") })
                .collect::<Vec<_>>()
                .join("\n")
                + "\n"
        };
        let patch = create_patch("/p/AGENTS.md", &rules(None), &rules(Some(12)), "", "", 3);
        let body = &patch[patch.find("@@").unwrap()..];
        assert_eq!(
            body.trim_end(),
            "@@ -9,7 +9,7 @@\n Rule 9: keep it simple.\n Rule 10: keep it simple.\n Rule 11: keep it simple.\n-Rule 12: keep it simple.\n+Rule 12: changed.\n Rule 13: keep it simple.\n Rule 14: keep it simple.\n Rule 15: keep it simple."
        );
        let eof = create_patch("f", "a\nb", "a\nc", "", "", 3);
        assert!(eof.ends_with("@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+c\n\\ No newline at end of file\n"));
        let added = create_patch("f", "", "x\n", "", "", 3);
        assert!(added.ends_with("@@ -0,0 +1,1 @@\n+x\n"));
    }

    #[test]
    fn apply_and_reverse() {
        let cases = [
            ("alpha\nbeta\ngamma\n", "alpha\nBETA\ngamma\n"),
            ("", "brand new\n"),
            ("one\ntwo\n", "one\ntwo\nthree\n"),
            ("a\nb", "a\nc"),
            ("a\nb\n", "a\nb"),
            ("a\nb", "a\nb\n"),
            ("x\r\ny\r\n", "x\r\nz\r\n"),
        ];
        for (old, new) in cases {
            let patch = create_patch_without_headers("f.txt", old, new, 4);
            let hunks = parse_patch(&patch).unwrap().remove(0);
            assert_eq!(apply_patch(old, &hunks).as_deref(), Some(new), "{patch}");
            assert_eq!(apply_patch(new, &reverse_patch(&hunks)).as_deref(), Some(old), "{patch}");
        }
        // A hunk that fits nowhere.
        let hunks = parse_patch(&create_patch_without_headers("f", "a\nb\n", "a\nc\n", 4)).unwrap().remove(0);
        assert_eq!(apply_patch("x\ny\n", &hunks), None);
        // A hunk found away from where its header says.
        assert_eq!(apply_patch("top\na\nb\n", &hunks).as_deref(), Some("top\na\nc\n"));
        assert!(parse_patch("@@ -1,2 +1,1 @@\n-a\n").is_err());
    }
}
