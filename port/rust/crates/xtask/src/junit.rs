//! A small reader for the JUnit XML bun writes (`--reporter=junit`). It only needs the testcase tags and
//! whether each contains failure/error/skipped, which keeps the runner dependency-free while matching what the
//! Python ElementTree reader in port/scripts/rust-conformance.sh sees.

use std::path::Path;

use crate::process::Result;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Pass,
    Fail,
    Skip,
}

#[derive(Debug)]
pub struct Case {
    pub file: String,
    pub name: String,
    pub kind: Kind,
}

pub fn parse(path: &Path, conformance: &Path) -> Result<Vec<Case>> {
    let text = crate::process::read(path)?;
    let mut cases = Vec::new();
    let mut cursor = 0;
    while let Some(start) = text[cursor..].find("<testcase") {
        let start = cursor + start;
        let Some(tag_end) = tag_end(&text, start) else {
            break;
        };
        let tag = &text[start..=tag_end];
        let file = attribute(tag, "file").map(|value| relative(&value, conformance));
        let name = attribute(tag, "name").unwrap_or_default();
        let self_closing = tag.trim_end().ends_with("/>");
        let (body, next) = if self_closing {
            ("", tag_end + 1)
        } else {
            let Some(close) = text[tag_end + 1..].find("</testcase>") else {
                break;
            };
            let close = tag_end + 1 + close;
            (&text[tag_end + 1..close], close + "</testcase>".len())
        };
        if let Some(file) = file {
            let kind = if body.contains("<failure") || body.contains("<error") {
                Kind::Fail
            } else if body.contains("<skipped") {
                Kind::Skip
            } else {
                Kind::Pass
            };
            cases.push(Case { file, name, kind });
        }
        cursor = next;
    }
    Ok(cases)
}

fn tag_end(text: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, character) in text[start..].char_indices() {
        match (quote, character) {
            (None, '"' | '\'') => quote = Some(character),
            (Some(open), character) if open == character => quote = None,
            (None, '>') => return Some(start + offset),
            _ => {}
        }
    }
    None
}

fn attribute(tag: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')?;
    Some(unescape(&tag[start..start + end]))
}

fn relative(file: &str, conformance: &Path) -> String {
    Path::new(file)
        .strip_prefix(conformance)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file.to_owned())
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        let Some(end) = rest[index..].find(';') else {
            out.push_str(&rest[index..]);
            return out;
        };
        let entity = &rest[index + 1..index + end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => {
                entity[1..].parse::<u32>().ok().and_then(char::from_u32)
            }
            _ => None,
        };
        match decoded {
            Some(character) => out.push(character),
            None => out.push_str(&rest[index..=index + end]),
        }
        rest = &rest[index + end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_passes_failures_and_skips() {
        let xml = r#"<?xml version="1.0"?>
<testsuites>
  <testsuite file="/repo/port/conformance/scenarios/a.test.ts">
    <testcase name="one" file="/repo/port/conformance/scenarios/a.test.ts" time="1" />
    <testcase name="two &amp; more" file="/repo/port/conformance/scenarios/a.test.ts">
      <failure message="no">bad</failure>
    </testcase>
    <testcase name="three" file="/repo/port/conformance/scenarios/a.test.ts">
      <skipped />
    </testcase>
    <testcase name="four" file="/repo/port/conformance/scenarios/a.test.ts">
      <error message="oops" />
    </testcase>
  </testsuite>
</testsuites>"#;
        let temporary = crate::process::TempDir::new("xtask-junit-test").unwrap();
        let path = temporary.path().join("shard-0.xml");
        crate::process::write(&path, xml).unwrap();
        let cases = parse(&path, Path::new("/repo/port/conformance")).unwrap();
        assert_eq!(cases.len(), 4);
        assert_eq!(cases[0].file, "scenarios/a.test.ts");
        assert_eq!(cases[0].kind, Kind::Pass);
        assert_eq!(cases[1].name, "two & more");
        assert_eq!(cases[1].kind, Kind::Fail);
        assert_eq!(cases[2].kind, Kind::Skip);
        assert_eq!(cases[3].kind, Kind::Fail);
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(unescape("a&amp;b&#10;c&#x27;d&apos;e"), "a&b\nc'd'e");
    }
}
