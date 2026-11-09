//! `util/diff.ts`: a patch split into one patch per hunk (each with the file header and the row count its split
//! view takes), and a new-file patch split into fixed-size chunks.
use std::sync::LazyLock;

use regex::Regex;

/// `PatchHunk`.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchHunk {
    pub patch: String,
    pub header: Option<String>,
    pub rows: Option<usize>,
}

/// `AddedPatchChunk`.
#[derive(Clone, Debug, PartialEq)]
pub struct AddedPatchChunk {
    pub patch: String,
    pub lines: Vec<String>,
    pub rows: usize,
}

static HUNK_HEADER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^@@ -[0-9]+(?:,[0-9]+)? \+[0-9]+(?:,[0-9]+)? @@.*$").unwrap());
static ADDED_HEADER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^@@ -0,0 \+1,([0-9]+) @@[^\n]*\n").unwrap());

/// `splitAddedPatch(patch, size)`: only a new file's patch (one hunk of `+` lines) splits.
pub fn split_added_patch(patch: &str, size: usize) -> Option<Vec<AddedPatchChunk>> {
    let header = ADDED_HEADER.captures(patch)?;
    let whole = header.get(0)?;
    let count: usize = header[1].parse().ok()?;
    let rest = &patch[whole.end()..];
    let rest = rest.strip_suffix('\n').unwrap_or(rest);
    let mut lines: Vec<String> = rest.split('\n').map(str::to_string).collect();
    let marker = if lines.last().is_some_and(|l| l.starts_with("\\ No newline at end of file")) { lines.pop() } else { None };
    if lines.len() != count || lines.iter().any(|l| !l.starts_with('+')) {
        return None;
    }
    let prefix = &patch[..whole.start()];
    let chunks = count.div_ceil(size);
    Some(
        (0..chunks)
            .map(|index| {
                let start = index * size;
                let slice: Vec<String> = lines[start..(start + size).min(count)].to_vec();
                let tail = match &marker {
                    Some(marker) if start + size >= count => format!("\n{marker}"),
                    _ => String::new(),
                };
                AddedPatchChunk {
                    patch: format!("{prefix}@@ -0,0 +{},{} @@\n{}{tail}", start + 1, slice.len(), slice.join("\n")),
                    rows: slice.len(),
                    lines: slice,
                }
            })
            .collect(),
    )
}

/// `splitPatchHunks(patch)`.
pub fn split_patch_hunks(patch: &str) -> Vec<PatchHunk> {
    let starts: Vec<usize> = HUNK_HEADER.find_iter(patch).map(|m| m.start()).collect();
    if starts.len() <= 1 {
        return vec![PatchHunk { patch: patch.to_string(), header: None, rows: None }];
    }
    let prefix = &patch[..starts[0]];
    starts
        .iter()
        .enumerate()
        .map(|(index, &start)| {
            let end = starts.get(index + 1).copied().unwrap_or(patch.len());
            let line_end = patch[start..].find('\n').map(|i| start + i);
            let header_end = line_end.unwrap_or(end);
            PatchHunk {
                header: Some(patch[start..header_end].to_string()),
                patch: format!("{prefix}{}", &patch[start..end]),
                rows: Some(split_rows(&patch[start..end])),
            }
        })
        .collect()
}

/// `splitRows(hunk)`: the rows a hunk takes when removes and adds pair up.
fn split_rows(hunk: &str) -> usize {
    let hunk = hunk.strip_suffix('\n').unwrap_or(hunk);
    let lines: Vec<&str> = hunk.split('\n').skip(1).collect();
    let mut rows = 0;
    let mut index = 0;
    while index < lines.len() {
        let prefix = lines[index].chars().next();
        if prefix == Some(' ') || prefix.is_none() {
            rows += 1;
            index += 1;
            continue;
        }
        if prefix == Some('\\') {
            index += 1;
            continue;
        }
        let mut additions = 0;
        let mut deletions = 0;
        while index < lines.len() && matches!(lines[index].chars().next(), Some('+') | Some('-')) {
            if lines[index].starts_with('+') {
                additions += 1;
            }
            if lines[index].starts_with('-') {
                deletions += 1;
            }
            index += 1;
        }
        rows += additions.max(deletions);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunks() {
        let patch = "--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-a\n+b\n c\n@@ -10,1 +10,1 @@\n-d\n+e\n";
        let hunks = split_patch_hunks(patch);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].header.as_deref(), Some("@@ -1,2 +1,2 @@"));
        assert_eq!(hunks[0].patch, "--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-a\n+b\n c\n");
        assert_eq!(hunks[0].rows, Some(2));
        assert_eq!(hunks[1].rows, Some(1));
        assert_eq!(split_patch_hunks("@@ -1 +1 @@\n-a\n+b\n").len(), 1);
    }
}
