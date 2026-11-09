//! A file change as the file tools report it (packages/core/src/tool/plugin/file-diff.ts `fileDiff`): jsdiff 8.0.4's
//! `diffLines` (Myers' diff over line tokens, with jsdiff's diagonal pruning and tie-breaking, so the same edit script
//! comes out) for the counts, and `createTwoFilesPatch` (`structuredPatch` with 4 lines of context, `formatPatch` with
//! every header) for the patch.

use opencode_protocol::Json;

use crate::json::Obj;

/// One component of a diff (`ChangeObject`): `count` tokens, added, removed or common.
#[derive(Debug, Clone)]
pub struct Change {
    pub count: usize,
    pub added: bool,
    pub removed: bool,
    pub value: String,
}

/// `lineDiff.tokenize`: lines with their terminators (`\n` or `\r\n`); a final line without one is kept.
fn tokenize(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            tokens.push(&text[start..=index]);
            start = index + 1;
        }
    }
    if start < text.len() {
        tokens.push(&text[start..]);
    }
    tokens
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

/// `diffLines(old, new)`.
pub fn diff_lines(old: &str, new: &str) -> Vec<Change> {
    let old_tokens = tokenize(old);
    let new_tokens = tokenize(new);
    let (old_len, new_len) = (old_tokens.len() as i64, new_tokens.len() as i64);
    let mut components: Vec<Component> = Vec::new();
    let max_edit_length = new_len + old_len;
    let offset = max_edit_length + 1;
    let mut best: Vec<Option<Path>> = vec![None; (2 * offset + 1) as usize];
    let slot = |diagonal: i64| (diagonal + offset) as usize;

    // `extractCommon`.
    let extract = |path: &mut Path, diagonal: i64, components: &mut Vec<Component>| -> i64 {
        let mut old_pos = path.old_pos;
        let mut new_pos = old_pos - diagonal;
        let mut common = 0;
        while new_pos + 1 < new_len && old_pos + 1 < old_len && old_tokens[(old_pos + 1) as usize] == new_tokens[(new_pos + 1) as usize] {
            new_pos += 1;
            old_pos += 1;
            common += 1;
        }
        if common > 0 {
            components.push(Component { count: common, added: false, removed: false, previous: path.last });
            path.last = Some(components.len() - 1);
        }
        path.old_pos = old_pos;
        new_pos
    };
    // `addToPath`.
    let add = |path: &Path, added: bool, removed: bool, old_pos_inc: i64, components: &mut Vec<Component>| -> Path {
        let next = match path.last.map(|index| &components[index]) {
            Some(last) if last.added == added && last.removed == removed => {
                Component { count: last.count + 1, added, removed, previous: last.previous }
            }
            _ => Component { count: 1, added, removed, previous: path.last },
        };
        components.push(next);
        Path { old_pos: path.old_pos + old_pos_inc, last: Some(components.len() - 1) }
    };

    let mut seed = Path { old_pos: -1, last: None };
    let new_pos = extract(&mut seed, 0, &mut components);
    best[slot(0)] = Some(seed);
    let finished = if seed.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
        Some(seed.last)
    } else {
        let mut min_diagonal = i64::MIN;
        let mut max_diagonal = i64::MAX;
        let mut edit_length = 1;
        let mut done = None;
        'outer: while edit_length <= max_edit_length {
            let mut diagonal = min_diagonal.max(-edit_length);
            while diagonal <= max_diagonal.min(edit_length) {
                let remove_path = best[slot(diagonal - 1)];
                let add_path = if diagonal + 1 <= offset { best[slot(diagonal + 1)] } else { None };
                if remove_path.is_some() {
                    best[slot(diagonal - 1)] = None;
                }
                let can_add = add_path.is_some_and(|p| {
                    let add_new_pos = p.old_pos - diagonal;
                    0 <= add_new_pos && add_new_pos < new_len
                });
                let can_remove = remove_path.is_some_and(|p| p.old_pos + 1 < old_len);
                if !can_add && !can_remove {
                    best[slot(diagonal)] = None;
                    diagonal += 2;
                    continue;
                }
                let mut base = if !can_remove || (can_add && remove_path.unwrap().old_pos < add_path.unwrap().old_pos) {
                    add(&add_path.unwrap(), true, false, 0, &mut components)
                } else {
                    add(&remove_path.unwrap(), false, true, 1, &mut components)
                };
                let new_pos = extract(&mut base, diagonal, &mut components);
                if base.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                    done = Some(base.last);
                    break 'outer;
                }
                best[slot(diagonal)] = Some(base);
                if base.old_pos + 1 >= old_len {
                    max_diagonal = max_diagonal.min(diagonal - 1);
                }
                if new_pos + 1 >= new_len {
                    min_diagonal = min_diagonal.max(diagonal + 1);
                }
                diagonal += 2;
            }
            edit_length += 1;
        }
        done
    };
    let Some(last) = finished else { return vec![] };
    // `buildValues`.
    let mut chain = Vec::new();
    let mut next = last;
    while let Some(index) = next {
        chain.push(index);
        next = components[index].previous;
    }
    chain.reverse();
    let (mut new_index, mut old_index) = (0usize, 0usize);
    chain
        .into_iter()
        .map(|index| {
            let component = &components[index];
            let value = if !component.removed {
                let value = new_tokens[new_index..new_index + component.count].concat();
                new_index += component.count;
                if !component.added {
                    old_index += component.count;
                }
                value
            } else {
                let value = old_tokens[old_index..old_index + component.count].concat();
                old_index += component.count;
                value
            };
            Change { count: component.count, added: component.added, removed: component.removed, value }
        })
        .collect()
}

/// `splitLines` of patch/create.js: lines with their trailing `\n` where present.
fn split_lines(text: &str) -> Vec<String> {
    let mut result: Vec<String> = text.split('\n').map(|line| format!("{line}\n")).collect();
    if text.ends_with('\n') {
        result.pop();
    } else if let Some(last) = result.last_mut() {
        last.pop();
    }
    result
}

struct Hunk {
    old_start: i64,
    old_lines: i64,
    new_start: i64,
    new_lines: i64,
    lines: Vec<String>,
}

const CONTEXT: usize = 4;

/// `createTwoFilesPatch(file, file, old, new)` over an already computed `diffLines` result.
fn two_files_patch(file: &str, diff: &[Change]) -> String {
    let mut entries: Vec<(bool, bool, Vec<String>)> =
        diff.iter().map(|change| (change.added, change.removed, split_lines(&change.value))).collect();
    entries.push((false, false, Vec::new()));
    let context_lines = |lines: &[String]| lines.iter().map(|line| format!(" {line}")).collect::<Vec<_>>();
    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut old_range_start, mut new_range_start) = (0i64, 0i64);
    let mut range: Vec<String> = Vec::new();
    let (mut old_line, mut new_line) = (1i64, 1i64);
    for i in 0..entries.len() {
        let (added, removed, lines) = (&entries[i].0, &entries[i].1, &entries[i].2);
        if *added || *removed {
            if old_range_start == 0 {
                old_range_start = old_line;
                new_range_start = new_line;
                if i > 0 {
                    let previous = &entries[i - 1].2;
                    range = context_lines(&previous[previous.len().saturating_sub(CONTEXT)..]);
                    old_range_start -= range.len() as i64;
                    new_range_start -= range.len() as i64;
                }
            }
            for line in lines {
                range.push(format!("{}{line}", if *added { '+' } else { '-' }));
            }
            if *added {
                new_line += lines.len() as i64;
            } else {
                old_line += lines.len() as i64;
            }
        } else {
            if old_range_start != 0 {
                if lines.len() <= CONTEXT * 2 && i + 2 < entries.len() {
                    range.extend(context_lines(lines));
                } else {
                    let size = lines.len().min(CONTEXT);
                    range.extend(context_lines(&lines[..size]));
                    hunks.push(Hunk {
                        old_start: old_range_start,
                        old_lines: old_line - old_range_start + size as i64,
                        new_start: new_range_start,
                        new_lines: new_line - new_range_start + size as i64,
                        lines: std::mem::take(&mut range),
                    });
                    old_range_start = 0;
                    new_range_start = 0;
                }
            }
            old_line += lines.len() as i64;
            new_line += lines.len() as i64;
        }
    }
    let mut out = vec![
        format!("Index: {file}"),
        "===================================================================".to_owned(),
        format!("--- {file}"),
        format!("+++ {file}"),
    ];
    for hunk in &mut hunks {
        let mut lines = Vec::with_capacity(hunk.lines.len());
        for line in hunk.lines.drain(..) {
            match line.strip_suffix('\n') {
                Some(stripped) => lines.push(stripped.to_owned()),
                None => {
                    lines.push(line);
                    lines.push("\\ No newline at end of file".to_owned());
                }
            }
        }
        if hunk.old_lines == 0 {
            hunk.old_start -= 1;
        }
        if hunk.new_lines == 0 {
            hunk.new_start -= 1;
        }
        out.push(format!("@@ -{},{} +{},{} @@", hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines));
        out.extend(lines);
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// `FileDiff.Info`.
#[derive(Debug, Clone)]
pub struct FileDiff {
    pub file: String,
    pub patch: String,
    pub status: &'static str,
    pub additions: usize,
    pub deletions: usize,
}

impl FileDiff {
    pub fn to_json(&self) -> Json {
        let mut o = Obj::new();
        o.str("file", &self.file).str("patch", &self.patch).str("status", self.status);
        o.int("additions", self.additions as i64).int("deletions", self.deletions as i64);
        o.done()
    }
}

/// `fileDiff(file, before, after, status)`.
pub fn file_diff(file: &str, before: &str, after: &str, status: &'static str) -> FileDiff {
    let diff = diff_lines(before, after);
    let additions = diff.iter().filter(|c| c.added).map(|c| c.count).sum();
    let deletions = diff.iter().filter(|c| c.removed).map(|c| c.count).sum();
    FileDiff { file: file.to_owned(), patch: two_files_patch(file, &diff), status, additions, deletions }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modified() {
        let before = "export function add(a: number, b: number) {\n  return a + b\n}\n\nexport const twice = (n: number) => add(n, n)\n";
        let after = before.replace("return a + b", "return a + b + 0");
        let diff = file_diff("src/math.ts", before, &after, "modified");
        assert_eq!(
            diff.patch,
            "Index: src/math.ts\n===================================================================\n--- src/math.ts\n+++ src/math.ts\n@@ -1,5 +1,5 @@\n export function add(a: number, b: number) {\n-  return a + b\n+  return a + b + 0\n }\n \n export const twice = (n: number) => add(n, n)\n"
        );
        assert_eq!((diff.additions, diff.deletions), (1, 1));
    }

    #[test]
    fn replaced_all() {
        let diff = file_diff("dup.txt", "x\nx\nx\n", "y\ny\ny\n", "modified");
        assert_eq!(
            diff.patch,
            "Index: dup.txt\n===================================================================\n--- dup.txt\n+++ dup.txt\n@@ -1,3 +1,3 @@\n-x\n-x\n-x\n+y\n+y\n+y\n"
        );
    }

    #[test]
    fn added_and_unchanged() {
        let diff = file_diff("a.txt", "", "one\ntwo", "added");
        assert_eq!(
            diff.patch,
            "Index: a.txt\n===================================================================\n--- a.txt\n+++ a.txt\n@@ -0,0 +1,2 @@\n+one\n+two\n\\ No newline at end of file\n"
        );
        assert_eq!((diff.additions, diff.deletions), (2, 0));
        let same = file_diff("a.txt", "x\n", "x\n", "modified");
        assert_eq!(same.patch, "Index: a.txt\n===================================================================\n--- a.txt\n+++ a.txt\n");
    }
}
