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
}
