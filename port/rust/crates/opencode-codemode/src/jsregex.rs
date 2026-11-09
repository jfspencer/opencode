//! TEMPORARY STUB (replaced by the jsregex work): JS RegExp semantics over regress.

use std::cell::Cell;

pub enum RegExpError {
    Flags(String),
    Pattern(String),
}

pub struct JsRegExp {
    regex: regress::Regex,
    pattern: Vec<u16>,
    flags: String,
    last_index: Cell<f64>,
}

pub struct Match {
    pub captures: Vec<Option<(usize, usize)>>,
    pub groups: Option<Vec<(Vec<u16>, Option<(usize, usize)>)>>,
}

impl JsRegExp {
    pub fn new(pattern: &[u16], flags: &[u16]) -> Result<JsRegExp, RegExpError> {
        let flags_text = String::from_utf16_lossy(flags);
        let mut canonical = String::new();
        for f in "dgimsuvy".chars() {
            if flags_text.contains(f) {
                canonical.push(f);
            }
        }
        if canonical.len() != flags_text.len() {
            return Err(RegExpError::Flags(format!("Invalid flags supplied to RegExp constructor '{flags_text}'")));
        }
        let engine_flags: String = canonical.chars().filter(|c| "imsuvy".contains(*c)).collect();
        let source = String::from_utf16_lossy(pattern);
        let regex = regress::Regex::with_flags(&source, engine_flags.as_str())
            .map_err(|e| RegExpError::Pattern(e.to_string()))?;
        Ok(JsRegExp { regex, pattern: pattern.to_vec(), flags: canonical, last_index: Cell::new(0.0) })
    }
    pub fn source(&self) -> Vec<u16> {
        if self.pattern.is_empty() { "(?:)".encode_utf16().collect() } else { self.pattern.clone() }
    }
    pub fn flags(&self) -> String {
        self.flags.clone()
    }
    fn has(&self, f: char) -> bool {
        self.flags.contains(f)
    }
    pub fn global(&self) -> bool {
        self.has('g')
    }
    pub fn ignore_case(&self) -> bool {
        self.has('i')
    }
    pub fn multiline(&self) -> bool {
        self.has('m')
    }
    pub fn dot_all(&self) -> bool {
        self.has('s')
    }
    pub fn unicode(&self) -> bool {
        self.has('u')
    }
    pub fn unicode_sets(&self) -> bool {
        self.has('v')
    }
    pub fn sticky(&self) -> bool {
        self.has('y')
    }
    pub fn has_indices(&self) -> bool {
        self.has('d')
    }
    pub fn last_index(&self) -> f64 {
        self.last_index.get()
    }
    pub fn set_last_index(&self, value: f64) {
        self.last_index.set(value)
    }
    fn find_at(&self, text: &[u16], start: usize) -> Option<Match> {
        let found = if self.unicode() || self.unicode_sets() {
            self.regex.find_from_utf16(text, start).next()
        } else {
            self.regex.find_from_ucs2(text, start).next()
        }?;
        if self.sticky() && found.start() != start {
            return None;
        }
        let mut captures = vec![Some((found.start(), found.end()))];
        for group in found.groups().skip(1) {
            captures.push(group.map(|r| (r.start, r.end)));
        }
        let names: Vec<(Vec<u16>, Option<(usize, usize)>)> = found
            .named_groups()
            .map(|(name, range)| (name.encode_utf16().collect(), range.map(|r| (r.start, r.end))))
            .collect();
        Some(Match { captures, groups: if names.is_empty() { None } else { Some(names) } })
    }
    pub fn exec(&self, text: &[u16]) -> Option<Match> {
        let tracks = self.global() || self.sticky();
        let mut start = if tracks { self.last_index.get().max(0.0) as usize } else { 0 };
        if start > text.len() {
            self.last_index.set(0.0);
            return None;
        }
        if !tracks {
            start = 0;
        }
        match self.find_at(text, start) {
            None => {
                if tracks {
                    self.last_index.set(0.0);
                }
                None
            }
            Some(found) => {
                if tracks {
                    self.last_index.set(found.captures[0].unwrap().1 as f64);
                }
                Some(found)
            }
        }
    }
    pub fn clone_regexp(&self) -> JsRegExp {
        let flags: Vec<u16> = self.flags.encode_utf16().collect();
        JsRegExp::new(&self.pattern, &flags).ok().unwrap()
    }
}

fn all_matches(re: &JsRegExp, text: &[u16]) -> Vec<Match> {
    let mut out = Vec::new();
    if !re.global() {
        if let Some(m) = re.exec(text) {
            out.push(m);
        }
        return out;
    }
    re.set_last_index(0.0);
    while let Some(m) = re.exec(text) {
        let (s, e) = m.captures[0].unwrap();
        if s == e {
            re.set_last_index(e as f64 + 1.0);
        }
        out.push(m);
    }
    re.set_last_index(0.0);
    out
}

pub fn get_substitution(
    matched: &[u16],
    text: &[u16],
    position: usize,
    captures: &[Option<Vec<u16>>],
    _groups: Option<&[(Vec<u16>, Option<Vec<u16>>)]>,
    replacement: &[u16],
) -> Vec<u16> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < replacement.len() {
        let c = replacement[i];
        if c == b'$' as u16 && i + 1 < replacement.len() {
            let n = replacement[i + 1];
            match n {
                0x24 => {
                    out.push(0x24);
                    i += 2;
                    continue;
                }
                0x26 => {
                    out.extend_from_slice(matched);
                    i += 2;
                    continue;
                }
                0x60 => {
                    out.extend_from_slice(&text[..position]);
                    i += 2;
                    continue;
                }
                0x27 => {
                    let end = (position + matched.len()).min(text.len());
                    out.extend_from_slice(&text[end..]);
                    i += 2;
                    continue;
                }
                d if (b'1' as u16..=b'9' as u16).contains(&d) => {
                    let index = (d - b'0' as u16) as usize;
                    if index <= captures.len() {
                        if let Some(Some(cap)) = captures.get(index - 1) {
                            out.extend_from_slice(cap);
                        }
                        i += 2;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn substitute(m: &Match, text: &[u16], replacement: &[u16]) -> Vec<u16> {
    let (s, e) = m.captures[0].unwrap();
    let captures: Vec<Option<Vec<u16>>> = m.captures[1..].iter().map(|c| c.map(|(a, b)| text[a..b].to_vec())).collect();
    get_substitution(&text[s..e], text, s, &captures, None, replacement)
}

pub fn replace(re: &JsRegExp, text: &[u16], replacement: &[u16]) -> Vec<u16> {
    let matches = all_matches(re, text);
    let mut out = Vec::new();
    let mut end = 0;
    for m in &matches {
        let (s, e) = m.captures[0].unwrap();
        out.extend_from_slice(&text[end..s]);
        out.extend(substitute(m, text, replacement));
        end = e;
    }
    out.extend_from_slice(&text[end..]);
    out
}

pub fn replace_matches(re: &JsRegExp, text: &[u16]) -> Vec<Match> {
    all_matches(re, text)
}

pub fn split(re: &JsRegExp, text: &[u16], limit: Option<u32>) -> Vec<Option<Vec<u16>>> {
    let limit = limit.unwrap_or(u32::MAX) as usize;
    let mut out = Vec::new();
    if limit == 0 {
        return out;
    }
    let copy = re.clone_regexp();
    let mut last = 0;
    let mut q = 0;
    if text.is_empty() {
        if copy.find_at(text, 0).is_none() {
            out.push(Some(Vec::new()));
        }
        return out;
    }
    while q < text.len() {
        match copy.find_at(text, q) {
            Some(m) if m.captures[0].unwrap().0 == q => {
                let e = m.captures[0].unwrap().1;
                if e == last || e == q && e >= text.len() {
                    q += 1;
                    continue;
                }
                if e == q {
                    q += 1;
                    continue;
                }
                out.push(Some(text[last..q].to_vec()));
                if out.len() == limit {
                    return out;
                }
                for cap in &m.captures[1..] {
                    out.push(cap.map(|(a, b)| text[a..b].to_vec()));
                    if out.len() == limit {
                        return out;
                    }
                }
                last = e;
                q = e;
            }
            _ => q += 1,
        }
    }
    out.push(Some(text[last..].to_vec()));
    out
}

pub fn match_global(re: &JsRegExp, text: &[u16]) -> Option<Vec<Vec<u16>>> {
    let matches = all_matches(re, text);
    if matches.is_empty() {
        return None;
    }
    Some(matches.iter().map(|m| { let (s, e) = m.captures[0].unwrap(); text[s..e].to_vec() }).collect())
}

pub fn match_all(re: &JsRegExp, text: &[u16]) -> Vec<Match> {
    let copy = re.clone_regexp();
    copy.set_last_index(re.last_index());
    let mut out = Vec::new();
    while let Some(m) = copy.exec(text) {
        let (s, e) = m.captures[0].unwrap();
        if s == e {
            copy.set_last_index(e as f64 + 1.0);
        }
        out.push(m);
    }
    out
}

pub fn search(re: &JsRegExp, text: &[u16]) -> i64 {
    let saved = re.last_index();
    re.set_last_index(0.0);
    let found = re.find_at(text, 0);
    re.set_last_index(saved);
    found.map(|m| m.captures[0].unwrap().0 as i64).unwrap_or(-1)
}

pub fn string_replace_positions(text: &[u16], search: &[u16], all: bool) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    loop {
        let found = if search.is_empty() {
            if from <= text.len() { Some(from) } else { None }
        } else {
            (from..=text.len().saturating_sub(search.len())).find(|&i| i + search.len() <= text.len() && &text[i..i + search.len()] == search)
        };
        match found {
            None => break,
            Some(at) => {
                out.push(at);
                if !all {
                    break;
                }
                from = at + search.len().max(1);
                if from > text.len() {
                    break;
                }
            }
        }
    }
    out
}

pub fn string_replace(text: &[u16], search: &[u16], replacement: &[u16], all: bool) -> Vec<u16> {
    let positions = string_replace_positions(text, search, all);
    let mut out = Vec::new();
    let mut end = 0;
    for at in positions {
        out.extend_from_slice(&text[end..at]);
        out.extend(get_substitution(search, text, at, &[], None, replacement));
        end = at + search.len();
    }
    out.extend_from_slice(&text[end..]);
    out
}

pub fn escape(text: &[u16]) -> Vec<u16> {
    let mut out = Vec::new();
    for &u in text {
        if (u as u8 as u16 == u) && "^$\\.*+?()[]{}|/".contains(u as u8 as char) {
            out.push(b'\\' as u16);
        }
        out.push(u);
    }
    out
}
