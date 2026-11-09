//! Decode failures, rendered exactly as Effect's default schema issue formatter renders them
//! (`SchemaIssue.makeFormatterDefault`, which is what `SchemaError.message` returns and what the TS server puts in
//! `InvalidRequestError.message`).
//!
//! Effect builds a tree of issues; its formatter flattens the tree into leaves, each a message and the path to the
//! offending value, and writes every leaf as `<message>` or `<message>\n  at <path>`, joined by newlines. An
//! [`Issue`] is that flattened list. Messages come from the schemas (the generator computes them from the TS AST:
//! identifiers, `expected` and `message` annotations), so `issue.to_string()` is the TS text byte for byte.
//!
//! Decoding stops at the first problem (Effect's default `errors: "first"`), so there is usually one entry. A union
//! whose candidate members all fail reports one entry per member, in member order, as Effect's `AnyOf` does.

use std::borrow::Cow;

/// One step of the path to a rejected value: an object key or an array index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Key(String),
    Index(usize),
}

impl From<&str> for Segment {
    fn from(key: &str) -> Self {
        Self::Key(key.to_owned())
    }
}

impl From<&String> for Segment {
    fn from(key: &String) -> Self {
        Self::Key(key.clone())
    }
}

impl From<String> for Segment {
    fn from(key: String) -> Self {
        Self::Key(key)
    }
}

impl From<usize> for Segment {
    fn from(index: usize) -> Self {
        Self::Index(index)
    }
}

/// One leaf of the issue tree: what was expected, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: Vec<Segment>,
    pub message: Cow<'static, str>,
}

/// Why a value was rejected. `Display` (and [`Issue::message`]) writes Effect's formatted message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    entries: Vec<Entry>,
}

impl Issue {
    /// A failure of the value itself (an empty path).
    pub fn new(message: impl Into<Cow<'static, str>>) -> Self {
        Self { entries: vec![Entry { path: Vec::new(), message: message.into() }] }
    }

    /// A required key is absent (Effect's `MissingKey` without a `messageMissingKey` annotation).
    pub fn missing() -> Self {
        Self::new("Missing key")
    }

    /// Prefixes every entry's path with a key or index.
    pub fn at(mut self, segment: impl Into<Segment>) -> Self {
        let segment = segment.into();
        for entry in &mut self.entries {
            entry.path.insert(0, segment.clone());
        }
        self
    }

    /// The flattened leaves, in Effect's order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Effect's formatted message (`SchemaError.message`), the same text as `Display`.
    pub fn message(&self) -> String {
        self.to_string()
    }

    /// Collects the issues of several failed union members.
    pub(crate) fn none() -> Self {
        Self { entries: Vec::new() }
    }

    pub(crate) fn extend(&mut self, other: Issue) {
        self.entries.extend(other.entries)
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, entry) in self.entries.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            f.write_str(&entry.message)?;
            if !entry.path.is_empty() {
                // Formatter.formatPath: `[${JSON.stringify(key)}]` for keys, `[${index}]` for indices.
                f.write_str("\n  at ")?;
                for segment in &entry.path {
                    match segment {
                        Segment::Key(key) => {
                            let mut s = String::with_capacity(key.len() + 2);
                            super::out::write_json_string(&mut s, key);
                            write!(f, "[{s}]")?;
                        }
                        Segment::Index(i) => write!(f, "[{i}]")?,
                    }
                }
            }
        }
        Ok(())
    }
}

impl std::error::Error for Issue {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_effect() {
        assert_eq!(Issue::new("Expected string").to_string(), "Expected string");
        let issue = Issue::missing().at("id").at("model");
        assert_eq!(issue.to_string(), "Missing key\n  at [\"model\"][\"id\"]");
        let mut both = Issue::none();
        both.extend(Issue::new("Expected never").at("every"));
        both.extend(Issue::missing().at("at"));
        assert_eq!(both.at(0usize).to_string(), "Expected never\n  at [0][\"every\"]\nMissing key\n  at [0][\"at\"]");
        assert_eq!(Issue::new("x").at("a\"b\n").to_string(), "x\n  at [\"a\\\"b\\n\"]");
    }
}
