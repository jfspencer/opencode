//! `lib/tree-sitter/types.ts`'s highlight shapes.

use crate::marked::Nullable;
use std::rc::Rc;

/// `HighlightMeta`. `conceal` and `concealLines` are the `#set!` values: a string, `null` (no value given) or
/// absent.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HighlightMeta {
    pub is_injection: Option<bool>,
    pub injection_lang: Option<Rc<str>>,
    pub contains_injection: Option<bool>,
    pub conceal: Nullable,
    pub conceal_lines: Nullable,
}

impl HighlightMeta {
    /// Whether no field is set (`Object.keys(meta).length === 0`).
    pub fn is_empty(&self) -> bool {
        self.is_injection.is_none()
            && self.injection_lang.is_none()
            && self.contains_injection.is_none()
            && self.conceal == Nullable::Undefined
            && self.conceal_lines == Nullable::Undefined
    }
}

/// `SimpleHighlight`: `[start, end, group, meta?]`, offsets in UTF-16 code units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimpleHighlight {
    pub start: usize,
    pub end: usize,
    pub group: Rc<str>,
    pub meta: Option<HighlightMeta>,
}

impl SimpleHighlight {
    pub fn new(start: usize, end: usize, group: impl Into<Rc<str>>, meta: Option<HighlightMeta>) -> SimpleHighlight {
        SimpleHighlight { start, end, group: group.into(), meta }
    }

    pub fn conceal(&self) -> &Nullable {
        self.meta.as_ref().map_or(&Nullable::Undefined, |m| &m.conceal)
    }

    pub fn conceal_lines(&self) -> &Nullable {
        self.meta.as_ref().map_or(&Nullable::Undefined, |m| &m.conceal_lines)
    }

    pub fn is_injection(&self) -> bool {
        self.meta.as_ref().and_then(|m| m.is_injection) == Some(true)
    }

    pub fn contains_injection(&self) -> bool {
        self.meta.as_ref().and_then(|m| m.contains_injection) == Some(true)
    }
}

/// A source range `{ start, end }` of a chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceRange {
    pub start: usize,
    pub end: usize,
}
