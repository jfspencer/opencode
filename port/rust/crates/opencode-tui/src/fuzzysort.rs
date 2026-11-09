//! fuzzysort 3.1 for the TUI: `fuzzysort.go(search, targets, options)` with `keys`, `threshold`, `limit` and
//! `scoreFn`, as autocomplete and the dialogs' select call it.
//!
//! The matching core is the server's port (`crates/opencode/src/fuzzysort.rs`, operation for operation over
//! JavaScript's UTF-16 code units, the same priority queue), copied here because the TUI crate cannot depend on the
//! server crate, with the partial matches of keys searches (`allowPartialMatch`) added. Targets are prepared per
//! search instead of cached (the cache changes no score).

use unicode_normalization::UnicodeNormalization;

/// JS `\s`.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}'
            | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// `String.prototype.trim`.
fn trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// `Script=Latin` (Unicode 15.0).
const LATIN: &[(u32, u32)] = &[
    (0x41, 0x5A), (0x61, 0x7A), (0xAA, 0xAA), (0xBA, 0xBA), (0xC0, 0xD6), (0xD8, 0xF6), (0xF8, 0x2B8), (0x2E0, 0x2E4),
    (0x1D00, 0x1D25), (0x1D2C, 0x1D5C), (0x1D62, 0x1D65), (0x1D6B, 0x1D77), (0x1D79, 0x1DBE), (0x1E00, 0x1EFF),
    (0x2071, 0x2071), (0x207F, 0x207F), (0x2090, 0x209C), (0x212A, 0x212B), (0x2132, 0x2132), (0x214E, 0x214E),
    (0x2160, 0x2188), (0x2C60, 0x2C7F), (0xA722, 0xA787), (0xA78B, 0xA7CA), (0xA7D0, 0xA7D1), (0xA7D3, 0xA7D3),
    (0xA7D5, 0xA7D9), (0xA7F2, 0xA7FF), (0xAB30, 0xAB5A), (0xAB5C, 0xAB64), (0xAB66, 0xAB69), (0xFB00, 0xFB06),
    (0xFF21, 0xFF3A), (0xFF41, 0xFF5A), (0x10780, 0x10785), (0x10787, 0x107B0), (0x107B2, 0x107BA), (0x1DF00, 0x1DF1E),
    (0x1DF25, 0x1DF2A),
];

fn is_latin(c: char) -> bool {
    let cp = c as u32;
    LATIN.iter().any(|(first, last)| (*first..=*last).contains(&cp))
}

/// `remove_accents`: Latin runs decomposed, then the combining diacritics (U+0300-U+036F) dropped.
fn remove_accents(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        out.extend(run.nfd());
        run.clear();
    };
    for c in text.chars() {
        if is_latin(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out.chars().filter(|c| !('\u{300}'..='\u{36f}').contains(c)).collect()
}

/// `prepareLowerInfo`.
struct LowerInfo {
    lower: Vec<u16>,
    codes: Vec<u16>,
    bitflags: u32,
    contains_space: bool,
}

fn lower_info(text: &str) -> LowerInfo {
    let text = remove_accents(text);
    let length = text.encode_utf16().count();
    let lower: Vec<u16> = text.to_lowercase().encode_utf16().collect();
    let codes: Vec<u16> = lower.iter().copied().take(length).collect();
    let mut bitflags = 0u32;
    let mut contains_space = false;
    for &code in &codes {
        if code == 32 {
            contains_space = true;
            continue;
        }
        let bit = match code {
            97..=122 => code - 97,
            48..=57 => 26,
            0..=127 => 30,
            _ => 31,
        };
        bitflags |= 1 << bit;
    }
    LowerInfo { lower, codes, bitflags, contains_space }
}

struct Search {
    lower: Vec<u16>,
    codes: Vec<u16>,
    bitflags: u32,
    contains_space: bool,
    spaces: Vec<Search>,
}

fn prepare_search(search: &str) -> Search {
    let search = trim(search);
    let info = lower_info(search);
    let mut spaces = vec![];
    if info.contains_space {
        let mut seen: Vec<&str> = vec![];
        for part in search.split(is_js_whitespace) {
            if seen.contains(&part) {
                continue;
            }
            seen.push(part);
            if part.is_empty() {
                continue;
            }
            let part_info = lower_info(part);
            // The part's own text is only lower-cased (its codes have their accents removed).
            spaces.push(Search {
                lower: part.to_lowercase().encode_utf16().collect(),
                codes: part_info.codes,
                bitflags: 0,
                contains_space: false,
                spaces: vec![],
            });
        }
    }
    Search { lower: info.lower, codes: info.codes, bitflags: info.bitflags, contains_space: info.contains_space, spaces }
}

struct Target {
    /// The target's text with accents removed (what beginnings are computed on).
    plain: Vec<u16>,
    lower: Vec<u16>,
    codes: Vec<u16>,
    bitflags: u32,
    next_beginnings: Option<Vec<usize>>,
    indexes: Vec<usize>,
}

fn prepare(target: &str) -> Target {
    let info = lower_info(target);
    Target {
        plain: remove_accents(target).encode_utf16().collect(),
        lower: info.lower,
        codes: info.codes,
        bitflags: info.bitflags,
        next_beginnings: None,
        indexes: vec![],
    }
}

fn beginning_indexes(target: &[u16]) -> Vec<usize> {
    let mut out = vec![];
    let (mut was_upper, mut was_alphanum) = (false, false);
    for (i, &code) in target.iter().enumerate() {
        let is_upper = (65..=90).contains(&code);
        let is_alphanum = is_upper || (97..=122).contains(&code) || (48..=57).contains(&code);
        let is_beginning = is_upper && !was_upper || !was_alphanum || !is_alphanum;
        was_upper = is_upper;
        was_alphanum = is_alphanum;
        if is_beginning {
            out.push(i);
        }
    }
    out
}

fn next_beginning_indexes(target: &[u16]) -> Vec<usize> {
    let len = target.len();
    let beginnings = beginning_indexes(target);
    let mut out = Vec::with_capacity(len);
    let mut last = beginnings.first().copied();
    let mut last_index = 0;
    for i in 0..len {
        if last.is_some_and(|l| l > i) {
            out.push(last.unwrap());
        } else {
            last_index += 1;
            last = beginnings.get(last_index).copied();
            out.push(last.unwrap_or(len));
        }
    }
    out
}

/// `indexOf` over code units.
fn index_of(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(haystack.len()));
    }
    (from..haystack.len().saturating_sub(needle.len() - 1)).find(|&i| haystack[i..i + needle.len()] == *needle)
}

struct State {
    simple: Vec<usize>,
    strict: Vec<usize>,
    /// `allowPartialMatchScores`: each space-separated part's share of the score (keys searches with spaces).
    partial: Vec<f64>,
}

fn algorithm(search: &Search, target: &mut Target, state: &mut State, allow_spaces: bool, allow_partial: bool) -> Option<f64> {
    if !allow_spaces && search.contains_space {
        return algorithm_spaces(search, target, state, allow_partial);
    }
    let codes = &search.codes;
    let search_len = codes.len();
    let target_len = target.codes.len();
    if search_len == 0 {
        return None;
    }
    state.simple.clear();
    let mut search_i = 0;
    let mut target_i = 0;
    loop {
        if target.codes.get(target_i) == Some(&codes[search_i]) {
            state.simple.push(target_i);
            search_i += 1;
            if search_i == search_len {
                break;
            }
        }
        target_i += 1;
        if target_i >= target_len {
            return None;
        }
    }

    let mut search_i = 0;
    let mut success_strict = false;
    state.strict.clear();
    if target.next_beginnings.is_none() {
        target.next_beginnings = Some(next_beginning_indexes(&target.plain));
    }
    let next = target.next_beginnings.as_ref().unwrap();
    let at = |i: usize| next.get(i).copied().unwrap_or(usize::MAX);
    let mut target_i = if state.simple[0] == 0 { 0 } else { at(state.simple[0] - 1) };
    let mut backtrack = 0;
    if target_i != target_len {
        loop {
            if target_i >= target_len {
                if search_i == 0 {
                    break;
                }
                backtrack += 1;
                if backtrack > 200 {
                    break;
                }
                search_i -= 1;
                let last = state.strict.pop().unwrap();
                target_i = at(last);
            } else if codes[search_i] == target.codes[target_i] {
                state.strict.push(target_i);
                search_i += 1;
                if search_i == search_len {
                    success_strict = true;
                    break;
                }
                target_i += 1;
            } else {
                target_i = at(target_i);
            }
        }
    }

    let mut substring = if search_len <= 1 { None } else { index_of(&target.lower, &search.lower, state.simple[0]) };
    let mut substring_beginning = match substring {
        None => false,
        Some(index) => index == 0 || at(index - 1) == index,
    };
    if let Some(index) = substring.filter(|_| !substring_beginning) {
        let mut i = 0;
        while i < next.len() {
            if i > index {
                let mut s = 0;
                while s < search_len && Some(&codes[s]) == target.codes.get(i + s) {
                    s += 1;
                }
                if s == search_len {
                    substring = Some(i);
                    substring_beginning = true;
                    break;
                }
            }
            i = next[i];
        }
    }

    let is_substring = substring.is_some();
    let score_of = |matches: &[usize]| -> f64 {
        let mut score = 0f64;
        let mut extra_groups = 0f64;
        for i in 1..search_len {
            if matches[i] as i64 - matches[i - 1] as i64 != 1 {
                score -= matches[i] as f64;
                extra_groups += 1.0;
            }
        }
        let unmatched = matches[search_len - 1] as f64 - matches[0] as f64 - (search_len as f64 - 1.0);
        score -= (12.0 + unmatched) * extra_groups;
        if matches[0] != 0 {
            score -= matches[0] as f64 * matches[0] as f64 * 0.2;
        }
        if !success_strict {
            score *= 1000.0;
        } else {
            let mut unique = 1usize;
            let mut i = next.first().copied().unwrap_or(target_len);
            while i < target_len {
                unique += 1;
                i = next[i];
            }
            if unique > 24 {
                score *= (unique as f64 - 24.0) * 10.0;
            }
        }
        score -= (target_len as f64 - search_len as f64) / 2.0;
        if is_substring {
            score /= 1.0 + search_len as f64 * search_len as f64;
        }
        if substring_beginning {
            score /= 1.0 + search_len as f64 * search_len as f64;
        }
        score -= (target_len as f64 - search_len as f64) / 2.0;
        score
    };

    let substring_matches = |index: usize| (0..search_len).map(|i| index + i).collect::<Vec<_>>();
    let best: Vec<usize> = if !success_strict {
        match substring {
            Some(index) => substring_matches(index),
            None => state.simple.clone(),
        }
    } else if substring_beginning {
        substring_matches(substring.unwrap())
    } else {
        state.strict.clone()
    };
    let score = score_of(&best);
    target.indexes = best;
    Some(score)
}

fn algorithm_spaces(search: &Search, target: &mut Target, state: &mut State, allow_partial: bool) -> Option<f64> {
    let mut seen: Vec<usize> = vec![];
    let mut score = 0f64;
    let mut first_seen_last = 0usize;
    let count = search.spaces.len();
    let mut changes: Vec<(usize, usize)> = vec![];
    let mut matched = false;
    state.partial.clear();
    state.partial.resize(count, f64::NEG_INFINITY);
    let reset = |target: &mut Target, changes: &[(usize, usize)]| {
        if let Some(next) = target.next_beginnings.as_mut() {
            for (index, value) in changes.iter().rev() {
                next[*index] = *value;
            }
        }
    };
    for (i, part) in search.spaces.iter().enumerate() {
        let Some(result) = algorithm(part, target, state, false, false) else {
            if allow_partial {
                continue;
            }
            reset(target, &changes);
            return None;
        };
        matched = true;
        let indexes = target.indexes.clone();
        if i != count - 1 {
            let consecutive = indexes.windows(2).all(|w| w[1] as i64 - w[0] as i64 == 1);
            if consecutive {
                let begin = indexes[indexes.len() - 1] + 1;
                let next = target.next_beginnings.as_mut().unwrap();
                let replaced = next.get(begin - 1).copied();
                let mut j = begin as i64 - 1;
                while j >= 0 {
                    let k = j as usize;
                    if Some(next[k]) != replaced {
                        break;
                    }
                    next[k] = begin;
                    changes.push((k, replaced.unwrap()));
                    j -= 1;
                }
            }
        }
        score += result / count as f64;
        state.partial[i] = result / count as f64;
        // Dock points based on order, otherwise "c man" returns Manifest.cpp instead of CheatManager.h.
        if indexes[0] < first_seen_last {
            score -= (first_seen_last - indexes[0]) as f64 * 2.0;
        }
        first_seen_last = indexes[0];
        for index in indexes {
            if !seen.contains(&index) {
                seen.push(index);
            }
        }
    }
    if allow_partial && !matched {
        return None;
    }
    reset(target, &changes);
    // A search with spaces that is an exact substring scores well.
    if let Some(whole) = algorithm(search, target, state, true, false)
        && whole > score
    {
        if allow_partial {
            for share in state.partial.iter_mut() {
                *share = whole / count as f64;
            }
        }
        return Some(whole);
    }
    target.indexes = seen;
    Some(score)
}

/// fuzzysort's priority queue (a hacked FastPriorityQueue): a min-heap on the score.
struct Queue {
    items: Vec<(f64, usize)>,
    len: usize,
}

impl Queue {
    fn set(&mut self, index: usize, value: (f64, usize)) {
        if index < self.items.len() {
            self.items[index] = value;
        } else {
            self.items.push(value);
        }
    }

    fn add(&mut self, value: (f64, usize)) {
        let mut a = self.len;
        self.len += 1;
        self.set(a, value);
        let mut v = (a as i64 - 1) >> 1;
        while a > 0 && value.0 < self.items[v as usize].0 {
            self.items[a] = self.items[v as usize];
            a = v as usize;
            v = (a as i64 - 1) >> 1;
        }
        self.items[a] = value;
    }

    fn percolate(&mut self) {
        let mut a = 0usize;
        let v = self.items[0];
        let mut c = 1usize;
        while c < self.len {
            let s = c + 1;
            a = c;
            if s < self.len && self.items[s].0 < self.items[c].0 {
                a = s;
            }
            self.items[(a - 1) >> 1] = self.items[a];
            c = 1 + (a << 1);
        }
        let mut f = (a as i64 - 1) >> 1;
        while a > 0 && v.0 < self.items[f as usize].0 {
            self.items[a] = self.items[f as usize];
            a = f as usize;
            f = (a as i64 - 1) >> 1;
        }
        self.items[a] = v;
    }

    fn poll(&mut self) -> Option<(f64, usize)> {
        if self.len == 0 {
            return None;
        }
        let top = self.items[0];
        self.len -= 1;
        self.items[0] = self.items[self.len];
        self.percolate();
        Some(top)
    }

    fn replace_top(&mut self, value: (f64, usize)) {
        self.items[0] = value;
        self.percolate();
    }
}

/// `fuzzysort.go(search, targets, { limit })`: the indexes of the matching targets, best first.
#[allow(dead_code)]
pub fn go(search: &str, targets: &[&str], limit: Option<usize>) -> Vec<usize> {
    scored(search, targets, limit).into_iter().map(|(index, _)| index).collect()
}

fn scored(search: &str, targets: &[&str], limit: Option<usize>) -> Vec<(usize, f64)> {
    if search.is_empty() {
        return vec![];
    }
    let prepared = prepare_search(search);
    let limit = limit.filter(|l| *l > 0).unwrap_or(usize::MAX);
    let mut state = State { simple: vec![], strict: vec![], partial: vec![] };
    let mut queue = Queue { items: vec![], len: 0 };
    let mut results = 0usize;
    for (index, text) in targets.iter().enumerate() {
        if text.is_empty() {
            continue;
        }
        let mut target = prepare(text);
        if prepared.bitflags & target.bitflags != prepared.bitflags {
            continue;
        }
        let Some(score) = algorithm(&prepared, &mut target, &mut state, false, false) else { continue };
        if results < limit {
            queue.add((score, index));
            results += 1;
        } else if score > queue.items[0].0 {
            queue.replace_top((score, index));
        }
    }
    let mut out = vec![(0, 0.0); results];
    for slot in (0..results).rev() {
        let (score, index) = queue.poll().unwrap();
        out[slot] = (index, score);
    }
    out
}


/// `normalizeScore`: a raw score as the 0..1 `score` getters report it.
pub fn normalize_score(score: f64) -> f64 {
    if score == f64::NEG_INFINITY {
        return 0.0;
    }
    if score > 1.0 {
        return score;
    }
    std::f64::consts::E.powf(((-score + 1.0).powf(0.04307) - 1.0) * -2.0)
}

/// `denormalizeScore`.
pub fn denormalize_score(normalized: f64) -> f64 {
    if normalized == 0.0 {
        return f64::NEG_INFINITY;
    }
    if normalized > 1.0 {
        return normalized;
    }
    1.0 - (normalized.ln() / -2.0 + 1.0).powf(1.0 / 0.04307)
}

/// One key's result in a [`KeysResult`] (`Result`, or `noTarget` for a key without a value or a match).
#[derive(Clone, Debug, PartialEq)]
pub struct KeyResult {
    /// `target`: the key's value ("" for no target).
    pub target: String,
    /// `_score` (raw: `f64::NEG_INFINITY` when the key did not match).
    pub raw: f64,
}

impl KeyResult {
    /// `score` (normalized).
    pub fn score(&self) -> f64 {
        normalize_score(self.raw)
    }
}

/// `KeysResult`: an item's per-key results, what `scoreFn` sees.
#[derive(Clone, Debug, PartialEq)]
pub struct KeysResult {
    /// The item's index in the targets (`obj`).
    pub index: usize,
    pub keys: Vec<KeyResult>,
    /// `_score` (raw).
    pub raw: f64,
}

impl KeysResult {
    /// `score` (normalized).
    pub fn score(&self) -> f64 {
        normalize_score(self.raw)
    }
}

/// A key: the item's value for it (`None` or "" is no target).
pub type Key<'a, T> = &'a dyn Fn(&T) -> Option<String>;

/// The options of a keys search.
pub struct Options<'a> {
    /// `threshold` (normalized; 0 keeps everything).
    pub threshold: f64,
    /// `limit` (`None` or 0: no limit).
    pub limit: Option<usize>,
    /// `scoreFn`: the item's normalized score (0 drops it).
    pub score_fn: Option<&'a dyn Fn(&KeysResult) -> f64>,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options { threshold: 0.0, limit: None, score_fn: None }
    }
}

/// `fuzzysort.go(search, items, { keys, threshold, limit, scoreFn })`: the matching items' results, best first. An
/// empty search returns nothing (`noResults`; the TUI never passes `all`).
pub fn go_keys<T>(search: &str, items: &[T], keys: &[Key<'_, T>], options: &Options<'_>) -> Vec<KeysResult> {
    if search.is_empty() {
        return vec![];
    }
    let prepared = prepare_search(search);
    let threshold = denormalize_score(options.threshold);
    let limit = options.limit.filter(|l| *l > 0).unwrap_or(usize::MAX);
    let mut state = State { simple: vec![], strict: vec![], partial: vec![] };
    let mut queue = Queue { items: vec![], len: 0 };
    let mut found: Vec<Option<KeysResult>> = vec![];
    let mut results = 0usize;
    let spaces = prepared.spaces.len();
    'outer: for (index, item) in items.iter().enumerate() {
        let mut targets: Vec<Option<(String, Target)>> = Vec::with_capacity(keys.len());
        let mut bitflags = 0u32;
        for key in keys {
            match key(item).filter(|value| !value.is_empty()) {
                Some(value) => {
                    let target = prepare(&value);
                    bitflags |= target.bitflags;
                    targets.push(Some((value, target)));
                }
                None => targets.push(None),
            }
        }
        if prepared.bitflags & bitflags != prepared.bitflags {
            continue;
        }
        let mut best_spaces = vec![f64::NEG_INFINITY; spaces];
        let mut key_results: Vec<KeyResult> = Vec::with_capacity(keys.len());
        for target in targets.iter_mut() {
            let Some((value, target)) = target else {
                key_results.push(KeyResult { target: String::new(), raw: f64::NEG_INFINITY });
                continue;
            };
            match algorithm(&prepared, target, &mut state, false, prepared.contains_space) {
                Some(raw) => key_results.push(KeyResult { target: value.clone(), raw }),
                None => {
                    key_results.push(KeyResult { target: String::new(), raw: f64::NEG_INFINITY });
                    continue;
                }
            }
            if prepared.contains_space {
                for (i, best) in best_spaces.iter_mut().enumerate() {
                    let share = state.partial.get(i).copied().unwrap_or(f64::NEG_INFINITY);
                    if share > -1000.0 && *best > f64::NEG_INFINITY {
                        let bonus = (*best + share) / 4.0;
                        if bonus > *best {
                            *best = bonus;
                        }
                    }
                    if share > *best {
                        *best = share;
                    }
                }
            }
        }
        if prepared.contains_space {
            if best_spaces.contains(&f64::NEG_INFINITY) {
                continue 'outer;
            }
        } else if !key_results.iter().any(|r| r.raw != f64::NEG_INFINITY) {
            continue;
        }
        let mut score = if prepared.contains_space {
            best_spaces.iter().sum::<f64>()
        } else {
            // Matching several keys earns a bonus.
            let mut score = f64::NEG_INFINITY;
            for result in &key_results {
                if result.raw > -1000.0 && score > f64::NEG_INFINITY {
                    let bonus = (score + result.raw) / 4.0;
                    if bonus > score {
                        score = bonus;
                    }
                }
                if result.raw > score {
                    score = result.raw;
                }
            }
            score
        };
        let mut result = KeysResult { index, keys: key_results, raw: score };
        if let Some(score_fn) = options.score_fn {
            let normalized = score_fn(&result);
            if normalized == 0.0 || normalized.is_nan() {
                continue;
            }
            score = denormalize_score(normalized);
            result.raw = score;
        }
        if score < threshold {
            continue;
        }
        let slot = found.len();
        found.push(Some(result));
        if results < limit {
            queue.add((score, slot));
            results += 1;
        } else if score > queue.items[0].0 {
            queue.replace_top((score, slot));
        }
    }
    let mut order = vec![0usize; results];
    for slot in (0..results).rev() {
        order[slot] = queue.poll().unwrap().1;
    }
    order.into_iter().map(|slot| found[slot].take().unwrap()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Results and scores recorded from fuzzysort 3.1.0 (`fuzzysort.go(q, targets.map(fuzzysort.prepare), { limit: 50 })`).
    #[test]
    fn ranks_like_fuzzysort() {
        let targets = [
            "README.md", "src/math.ts", "src/strings.ts", "src/nested/deep/value.ts", "docs/guide.md", "docs/with space.md",
            "assets/pixel.png", "data/empty.txt", ".gitignore", "src/", "src/nested/", "src/nested/deep/", "docs/", "assets/",
            "data/",
        ];
        let cases: &[(&str, &[(&str, f64)])] = &[
            ("math", &[("src/math.ts", -3.5231833910034602)]),
            ("s t", &[("src/math.ts", -18.1), ("src/strings.ts", -27.4), ("src/nested/deep/value.ts", -71.4), ("assets/", -1706.0), ("assets/pixel.png", -1715.0), ("src/nested/", -4910.0), ("src/nested/deep/", -4915.0), ("docs/with space.md", -4933.0)]),
            ("doc md", &[("docs/guide.md", -5.869), ("docs/with space.md", -8.9715)]),
        ];
        for (query, expected) in cases {
            let found: Vec<(&str, f64)> = scored(query, &targets, Some(50)).into_iter().map(|(i, s)| (targets[i], s)).collect();
            assert_eq!(&found, expected, "query {query:?}");
        }
    }

    /// Recorded from fuzzysort 3.1.0: `go(q, items, { keys: [o => o.d.trimEnd(), "desc", () => ""], threshold,
    /// limit: 10, scoreFn: r => r[0].target.startsWith("/" + q) ? r.score * 2 : r.score })`, as `[index, _score]`.
    #[test]
    fn keys_rank_like_fuzzysort() {
        let items = [
            ("/sessions", "Switch session"),
            ("/new", "New session"),
            ("/status", "View status"),
            ("/greet", "Greets someone by name"),
            ("/share", "Share session"),
            ("/help", "Help"),
            ("@explore", ""),
            ("@general", ""),
            ("src/util/strings.ts", ""),
        ];
        let cases: &[(&str, f64, &[(usize, f64)])] = &[
            ("se", 0.0, &[(0, 1.7868756818209766), (1, -4.808000000000008), (4, -6.007999999999987), (3, -31019.999999999985)]),
            ("sess", 0.0, &[(0, 1.8213988008140933), (1, -3.523183391003455), (4, -4.540484429065744)]),
            ("s s", 0.0, &[(4, -4.299999999999994), (2, -5.299999999999991), (0, -5.299999999999991), (1, -13.200000000000022), (8, -17.999999999999975), (3, -30.800000000000068)]),
            ("gr", 0.5, &[(3, 1.8105403577409063)]),
            ("str", 0.5, &[(8, -8.24199999999998)]),
            ("ne se", 0.0, &[(1, -2.9280000000000013), (3, -18395.999999999975)]),
        ];
        let display = |item: &(&str, &str)| Some(item.0.trim_end().to_string());
        let description = |item: &(&str, &str)| Some(item.1.to_string());
        let none = |_: &(&str, &str)| Some(String::new());
        for (query, threshold, expected) in cases {
            let score_fn = |r: &KeysResult| {
                let score = r.score();
                if r.keys[0].target.starts_with(&format!("/{query}")) { score * 2.0 } else { score }
            };
            let options = Options { threshold: *threshold, limit: Some(10), score_fn: Some(&score_fn) };
            let found: Vec<(usize, f64)> =
                go_keys(query, &items, &[&display, &description, &none], &options).into_iter().map(|r| (r.index, r.raw)).collect();
            assert_eq!(found.len(), expected.len(), "query {query:?}: {found:?}");
            for ((index, raw), (want_index, want_raw)) in found.iter().zip(expected.iter()) {
                assert_eq!(index, want_index, "query {query:?}: {found:?}");
                assert!((raw - want_raw).abs() < 1e-9, "query {query:?}: {found:?}");
            }
        }
    }

    #[test]
    fn a_key_scores_like_its_target() {
        let items = ["src/math.ts", "README.md"];
        let key = |item: &&str| Some(item.to_string());
        let found = go_keys("math", &items, &[&key], &Options::default());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].index, 0);
        assert_eq!(found[0].raw, -3.5231833910034602);
        assert!((denormalize_score(normalize_score(-3.5)) + 3.5).abs() < 1e-9);
    }
}
