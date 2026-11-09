//! Union decoding as Effect does it (`SchemaAST.Union.getParser`).
//!
//! Effect does not try every member. It first selects candidates by the input's runtime type and by the members'
//! discriminating literal fields ("sentinels"), see `getCandidateIndex` in effect/src/SchemaAST.ts. With no
//! candidate the union fails with its own expected text (`Expected A | B`, or its identifier); otherwise the
//! candidates are tried in order and the first that decodes wins (`anyOf`). When every candidate fails, the issue
//! lists each candidate's issue. Which members are candidates therefore decides both acceptance and the message, so
//! [`Site::candidates`] reproduces that selection exactly. The generator describes each TS union as a [`Site`] with
//! one [`Member`] per TS member, computed from the member's wire-most AST like Effect's `toCandidate`.

use super::{Issue, Json, Lit, is_lit};

/// JS runtime types of JSON values, as a bit set (`getCandidateTypes`).
pub mod types {
    pub const NULL: u8 = 1;
    pub const BOOLEAN: u8 = 2;
    pub const NUMBER: u8 = 4;
    pub const STRING: u8 = 8;
    pub const ARRAY: u8 = 16;
    pub const OBJECT: u8 = 32;
    pub const ALL: u8 = 63;
}

/// One TS union member, as the candidate index sees it.
#[derive(Debug, Clone, Copy)]
pub struct Member {
    /// `Never` members take no part in the selection.
    pub never: bool,
    /// The member is a literal (`isLiteral` of its wire-most AST).
    pub literal: Option<Lit>,
    /// Required literal fields (`collectSentinels`): `(key, literal)`.
    pub sentinels: &'static [(&'static str, Lit)],
    /// The runtime types the member can take ([`types`]); used when it has no sentinels.
    pub types: u8,
}

/// A TS union: its expected text (the `AnyOf` message when no member is a candidate) and its members.
#[derive(Debug, Clone, Copy)]
pub struct Site {
    pub expected: &'static str,
    pub members: &'static [Member],
}

impl Site {
    /// The indices of the members Effect tries for `v`, in order.
    pub fn candidates(&self, v: &Json) -> Vec<usize> {
        let live = || self.members.iter().enumerate().filter(|(_, m)| !m.never);
        let only_literals = live().all(|(_, m)| m.literal.is_some());
        if only_literals && live().next().is_some() {
            return live().filter(|(_, m)| m.literal.is_some_and(|l| is_lit(v, l))).map(|(i, _)| i).collect();
        }
        let ty = runtime_type(v);
        let fallback =
            || live().filter(|(_, m)| m.sentinels.is_empty() && m.types & ty != 0).map(|(i, _)| i).collect::<Vec<_>>();
        // Sentinel keys in order of first appearance.
        let mut keys: Vec<&'static str> = Vec::new();
        for (_, m) in live() {
            for (key, _) in m.sentinels {
                if !keys.contains(key) {
                    keys.push(key);
                }
            }
        }
        if keys.is_empty() {
            return live()
                .filter(|(_, m)| m.types & ty != 0 && m.literal.is_none_or(|l| is_lit(v, l)))
                .map(|(i, _)| i)
                .collect();
        }
        let discriminated = || live().filter(|(_, m)| !m.sentinels.is_empty());
        let owns = |i: usize, key: &str| self.members[i].sentinels.iter().any(|(k, _)| *k == key);
        let matches = |i: usize, key: &str, value: &Value| {
            self.members[i].sentinels.iter().any(|(k, l)| *k == key && value.is(*l))
        };
        let others = live().any(|(_, m)| m.sentinels.is_empty());
        if keys.len() == 1 && !others {
            return match lookup(v, keys[0]) {
                Some(value) => discriminated().filter(|(i, _)| matches(*i, keys[0], &value)).map(|(i, _)| i).collect(),
                None => Vec::new(),
            };
        }
        if !is_object_keyword(v) {
            return fallback();
        }
        // A key every discriminated member owns, with the most distinct values (the first on ties).
        let count = discriminated().count();
        let mut common: Option<(&str, usize)> = None;
        for key in &keys {
            if discriminated().filter(|(i, _)| owns(*i, key)).count() != count {
                continue;
            }
            let mut values: Vec<Lit> = Vec::new();
            for (_, m) in discriminated() {
                for (k, l) in m.sentinels {
                    if k == key && !values.iter().any(|x| same_lit(*x, *l)) {
                        values.push(*l);
                    }
                }
            }
            if common.is_none_or(|(_, n)| values.len() > n) {
                common = Some((key, values.len()));
            }
        }
        let mut selected = vec![false; self.members.len()];
        for i in fallback() {
            selected[i] = true;
        }
        let mut direct = None;
        if let Some((key, _)) = common
            && let Some(value) = lookup(v, key)
        {
            let found: Vec<usize> = discriminated().filter(|(i, _)| matches(*i, key, &value)).map(|(i, _)| i).collect();
            if found.is_empty() {
                return fallback();
            }
            for i in found {
                selected[i] = true;
            }
            direct = Some(key);
        }
        if direct.is_none() {
            for key in &keys {
                if let Some(value) = lookup(v, key) {
                    for (i, _) in discriminated() {
                        if matches(i, key, &value) {
                            selected[i] = true;
                        }
                    }
                }
            }
        }
        for key in &keys {
            if direct == Some(*key) {
                continue;
            }
            if let Some(value) = lookup(v, key) {
                for (i, chosen) in selected.iter_mut().enumerate() {
                    if *chosen && owns(i, key) && !matches(i, key, &value) {
                        *chosen = false;
                    }
                }
            }
        }
        selected.iter().enumerate().filter(|(_, s)| **s).map(|(i, _)| i).collect()
    }
}

/// `anyOf`: the first candidate that decodes wins; with no candidate the union's expected text, otherwise every
/// candidate's issue.
pub fn any_of<T>(v: &Json, site: &Site, mut member: impl FnMut(usize) -> Result<T, Issue>) -> Result<T, Issue> {
    let candidates = site.candidates(v);
    if candidates.is_empty() {
        return Err(Issue::new(site.expected));
    }
    let mut issue = Issue::none();
    for i in candidates {
        match member(i) {
            Ok(x) => return Ok(x),
            Err(e) => issue.extend(e),
        }
    }
    Err(issue)
}

/// `oneOf`: exactly one candidate may decode; a second success fails with `ambiguous`.
pub fn one_of<T>(
    v: &Json,
    site: &Site,
    ambiguous: &'static str,
    mut member: impl FnMut(usize) -> Result<T, Issue>,
) -> Result<T, Issue> {
    let candidates = site.candidates(v);
    if candidates.is_empty() {
        return Err(Issue::new(site.expected));
    }
    let mut found = None;
    let mut issue = Issue::none();
    for i in candidates {
        match member(i) {
            Ok(_) if found.is_some() => return Err(Issue::new(ambiguous)),
            Ok(x) => found = Some(x),
            Err(e) => issue.extend(e),
        }
    }
    found.ok_or(issue)
}

fn runtime_type(v: &Json) -> u8 {
    match v {
        Json::Null => types::NULL,
        Json::Bool(_) => types::BOOLEAN,
        Json::Number(_) => types::NUMBER,
        Json::String(_) => types::STRING,
        Json::Array(_) => types::ARRAY,
        Json::Object(_) => types::OBJECT,
    }
}

// `Predicate.isObjectKeyword`: objects and arrays.
fn is_object_keyword(v: &Json) -> bool {
    matches!(v, Json::Array(_) | Json::Object(_))
}

/// The value of `key in input ? input[key] : undefined`, for own properties (and an array's indices and length;
/// properties inherited from the prototypes are not looked up).
enum Value<'a> {
    Json(&'a Json),
    Length(usize),
}

impl Value<'_> {
    fn is(&self, lit: Lit) -> bool {
        match self {
            Value::Json(v) => is_lit(v, lit),
            Value::Length(n) => is_lit(&Json::Number(*n as f64), lit),
        }
    }
}

fn lookup<'a>(v: &'a Json, key: &str) -> Option<Value<'a>> {
    match v {
        Json::Object(o) => o.get(key).map(Value::Json),
        Json::Array(items) if key == "length" => Some(Value::Length(items.len())),
        Json::Array(items) => super::out::array_index(key).and_then(|i| items.get(i as usize)).map(Value::Json),
        _ => None,
    }
}

// SameValueZero of two literals (`Map` keys).
fn same_lit(a: Lit, b: Lit) -> bool {
    match (a, b) {
        (Lit::Str(a), Lit::Str(b)) => a == b,
        (Lit::Num(a), Lit::Num(b)) => a == b,
        (Lit::Bool(a), Lit::Bool(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::parse;

    const TAGGED: &[Member] = &[
        Member { never: false, literal: None, sentinels: &[("type", Lit::Str("a"))], types: types::OBJECT },
        Member { never: false, literal: None, sentinels: &[("type", Lit::Str("b"))], types: types::OBJECT },
    ];

    #[test]
    fn selects_by_sentinel() {
        let site = Site { expected: "Expected A | B", members: TAGGED };
        assert_eq!(site.candidates(&parse(r#"{"type":"b"}"#).unwrap()), vec![1]);
        assert!(site.candidates(&parse(r#"{"type":"c"}"#).unwrap()).is_empty());
        assert!(site.candidates(&parse(r#"{}"#).unwrap()).is_empty());
        assert!(site.candidates(&parse(r#""a""#).unwrap()).is_empty());
    }

    #[test]
    fn selects_by_type_and_literal() {
        const MEMBERS: &[Member] = &[
            Member { never: false, literal: None, sentinels: &[], types: types::STRING },
            Member { never: false, literal: None, sentinels: &[], types: types::NULL },
        ];
        let site = Site { expected: "Expected string | null", members: MEMBERS };
        assert_eq!(site.candidates(&Json::Null), vec![1]);
        assert!(site.candidates(&Json::Number(1.0)).is_empty());
        let r: Result<(), Issue> = any_of(&Json::Number(1.0), &site, |_| Ok(()));
        assert_eq!(r.unwrap_err().to_string(), "Expected string | null");
    }
}
