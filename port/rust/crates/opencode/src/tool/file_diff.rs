//! A file change as the file tools report it (packages/core/src/tool/plugin/file-diff.ts `fileDiff`): jsdiff's
//! `diffLines` counts and `createTwoFilesPatch` (4 lines of context, no file headers), from [`crate::jsdiff`].

use opencode_protocol::Json;

use crate::json::Obj;

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
    let diff = crate::jsdiff::diff_lines(before, after);
    let additions = diff.iter().filter(|c| c.added).map(|c| c.count).sum();
    let deletions = diff.iter().filter(|c| c.removed).map(|c| c.count).sum();
    let patch = crate::jsdiff::create_patch_without_headers(file, before, after, 4);
    FileDiff { file: file.to_owned(), patch, status, additions, deletions }
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

    /// Cases recorded from jsdiff (`DIFF_CASES=<file>`: `[{before, after, patch, additions, deletions}]`).
    #[test]
    #[ignore]
    fn recorded() {
        let path = std::env::var("DIFF_CASES").expect("DIFF_CASES names the recorded cases");
        let cases = crate::json::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut failures = 0;
        for case in crate::json::array(&cases).unwrap() {
            let get = |key: &str| crate::json::get_str(case, key).unwrap_or_default();
            let diff = file_diff("f.txt", get("before"), get("after"), "modified");
            let counts = (crate::json::get_num(case, "additions").unwrap() as usize, crate::json::get_num(case, "deletions").unwrap() as usize);
            if diff.patch != get("patch") || (diff.additions, diff.deletions) != counts {
                failures += 1;
                if failures < 4 {
                    eprintln!("before {:?}\nafter {:?}\nwant {:?}\ngot  {:?}", get("before"), get("after"), get("patch"), diff.patch);
                }
            }
        }
        assert_eq!(failures, 0);
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
