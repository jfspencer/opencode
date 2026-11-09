//! node-semver 7.8.5, the version arborist, npm-pick-manifest and npm-install-checks resolve: `SemVer`,
//! `Comparator` and `Range` as classes/*.js define them, over the "safe" regular expressions of internal/re.js
//! (whitespace runs capped at one, digit runs at 256, identifier runs at 250), translated to the `regex` crate with
//! JS's `\s` and `\d`. Range parsing is the same chain of string rewrites (hyphen ranges, comparator, tilde and
//! caret trims, carets, tildes, x-ranges, stars, `>=0.0.0`), so whatever text node-semver produces, accepts or
//! rejects, this does too; numbers are JS numbers (`+M + 1` of a 30-digit major is `1e+30`, which then fails to
//! parse as node-semver's does). Checked against the reference by `harness/npm/run.sh`.


use std::sync::OnceLock;

use opencode_protocol::wire::number::{is_js_whitespace, js_number_string, js_trim};
use regex::{Captures, Regex, RegexBuilder};

const MAX_LENGTH: usize = 256;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `{ loose, includePrerelease }` (parse-options.js).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    pub loose: bool,
    pub include_prerelease: bool,
}

impl Options {
    pub const LOOSE: Options = Options { loose: true, include_prerelease: false };
    pub const STRICT: Options = Options { loose: false, include_prerelease: false };
}

/// The compiled tokens of internal/re.js (`safeRe`), plus `BUILDSTRIPRE` (the unsafe BUILD source, global).
struct Tokens {
    full: Regex,
    loose: Regex,
    build: Regex,
    build_strip: Regex,
    xrange: Regex,
    xrange_loose: Regex,
    tilde_trim: Regex,
    tilde: Regex,
    tilde_loose: Regex,
    caret_trim: Regex,
    caret: Regex,
    caret_loose: Regex,
    comparator: Regex,
    comparator_loose: Regex,
    comparator_trim: Regex,
    hyphen: Regex,
    hyphen_loose: Regex,
    star: Regex,
    gte0: Regex,
    gte0_pre: Regex,
}

/// JS `\s` (WhiteSpace and LineTerminator) as the members of a character class.
const JS_SPACE: &str = r"\t\n\x0B\x0C\r \xA0\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

/// A JS pattern in the `regex` crate's syntax: `\s` and `\d` get JS's meaning (ASCII digits only).
fn translate(js: &str) -> String {
    let mut out = String::new();
    let mut in_class = false;
    let mut chars = js.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let next = chars.next().expect("escape");
                match (next, in_class) {
                    ('s', false) => out.push_str(&format!("[{JS_SPACE}]")),
                    ('s', true) => out.push_str(JS_SPACE),
                    ('d', false) => out.push_str("[0-9]"),
                    ('d', true) => out.push_str("0-9"),
                    (other, _) => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            '[' if !in_class => {
                in_class = true;
                out.push(c);
            }
            ']' if in_class => {
                in_class = false;
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// `makeSafeRegex`: `\s*`/`\s+` capped at one, `\d` at MAX_LENGTH, `[a-zA-Z0-9-]` at MAX_SAFE_BUILD_LENGTH.
fn make_safe(value: &str) -> String {
    let mut value = value.to_owned();
    for (token, max) in [(r"\s", 1usize), (r"\d", MAX_LENGTH), ("[a-zA-Z0-9-]", MAX_LENGTH - 6)] {
        value = value.split(&format!("{token}*")).collect::<Vec<_>>().join(&format!("{token}{{0,{max}}}"));
        value = value.split(&format!("{token}+")).collect::<Vec<_>>().join(&format!("{token}{{1,{max}}}"));
    }
    value
}

fn compile(js: &str) -> Regex {
    RegexBuilder::new(&translate(js)).size_limit(1 << 26).build().expect("semver token")
}

fn tokens() -> &'static Tokens {
    static TOKENS: OnceLock<Tokens> = OnceLock::new();
    TOKENS.get_or_init(|| {
        let ldn = "[a-zA-Z0-9-]";
        let numeric = "0|[1-9]\\d*";
        let numeric_loose = "\\d+";
        let non_numeric = format!("\\d*[a-zA-Z-]{ldn}*");
        let main = format!("({numeric})\\.({numeric})\\.({numeric})");
        let main_loose = format!("({numeric_loose})\\.({numeric_loose})\\.({numeric_loose})");
        let pre_id = format!("(?:{non_numeric}|{numeric})");
        let pre_id_loose = format!("(?:{non_numeric}|{numeric_loose})");
        let pre = format!("(?:-({pre_id}(?:\\.{pre_id})*))");
        let pre_loose = format!("(?:-?({pre_id_loose}(?:\\.{pre_id_loose})*))");
        let build_id = format!("{ldn}+");
        let build = format!("(?:\\+({build_id}(?:\\.{build_id})*))");
        let full_plain = format!("v?{main}{pre}?{build}?");
        let loose_plain = format!("[v=\\s]*{main_loose}{pre_loose}?{build}?");
        let gtlt = "((?:<|>)?=?)";
        let xr_id_loose = format!("{numeric_loose}|x|X|\\*");
        let xr_id = format!("{numeric}|x|X|\\*");
        let xr_plain = format!("[v=\\s]*({xr_id})(?:\\.({xr_id})(?:\\.({xr_id})(?:{pre})?{build}?)?)?");
        let xr_plain_loose =
            format!("[v=\\s]*({xr_id_loose})(?:\\.({xr_id_loose})(?:\\.({xr_id_loose})(?:{pre_loose})?{build}?)?)?");
        let lone_tilde = "(?:~>?)";
        let lone_caret = "(?:\\^)";
        let safe = |source: &str| compile(&make_safe(source));
        Tokens {
            full: safe(&format!("^{full_plain}$")),
            loose: safe(&format!("^{loose_plain}$")),
            build: safe(&build),
            build_strip: compile(&build),
            xrange: safe(&format!("^{gtlt}\\s*{xr_plain}$")),
            xrange_loose: safe(&format!("^{gtlt}\\s*{xr_plain_loose}$")),
            tilde_trim: safe(&format!("(\\s*){lone_tilde}\\s+")),
            tilde: safe(&format!("^{lone_tilde}{xr_plain}$")),
            tilde_loose: safe(&format!("^{lone_tilde}{xr_plain_loose}$")),
            caret_trim: safe(&format!("(\\s*){lone_caret}\\s+")),
            caret: safe(&format!("^{lone_caret}{xr_plain}$")),
            caret_loose: safe(&format!("^{lone_caret}{xr_plain_loose}$")),
            comparator: safe(&format!("^{gtlt}\\s*({full_plain})$|^$")),
            comparator_loose: safe(&format!("^{gtlt}\\s*({loose_plain})$|^$")),
            comparator_trim: safe(&format!("(\\s*){gtlt}\\s*({loose_plain}|{xr_plain})")),
            hyphen: safe(&format!("^\\s*({xr_plain})\\s+-\\s+({xr_plain})\\s*$")),
            hyphen_loose: safe(&format!("^\\s*({xr_plain_loose})\\s+-\\s+({xr_plain_loose})\\s*$")),
            star: safe("(<|>)?=?\\s*\\*"),
            gte0: safe("^\\s*>=\\s*0\\.0\\.0\\s*$"),
            gte0_pre: safe("^\\s*>=\\s*0\\.0\\.0-0\\s*$"),
        }
    })
}

/// JS `str.split(/\s+/)`.
fn split_space(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if is_js_whitespace(c) {
            let mut end = index + c.len_utf8();
            while let Some(&(next, d)) = chars.peek() {
                if !is_js_whitespace(d) {
                    break;
                }
                end = next + d.len_utf8();
                chars.next();
            }
            out.push(&text[start..index]);
            start = end;
        }
    }
    out.push(&text[start..]);
    out
}

/// JS `str.replace(/\s+/g, " ")`.
fn collapse_space(text: &str) -> String {
    split_space(text).join(" ")
}

/// `+digits` then `+ 1`, as the string a template literal makes of it.
fn plus_one(digits: &str) -> String {
    js_number_string(to_number(digits) + 1.0)
}

/// `+digits` (a string of ASCII digits).
fn to_number(digits: &str) -> f64 {
    digits.parse::<f64>().unwrap_or(f64::NAN)
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// A prerelease identifier: a number below MAX_SAFE_INTEGER, or the text.
#[derive(Debug, Clone, PartialEq)]
pub enum Ident {
    Num(f64),
    Str(String),
}

impl Ident {
    fn text(&self) -> String {
        match self {
            Ident::Num(n) => js_number_string(*n),
            Ident::Str(s) => s.clone(),
        }
    }
}

/// `compareIdentifiers` (internal/identifiers.js).
fn compare_identifiers(a: &Ident, b: &Ident) -> i32 {
    if let (Ident::Num(x), Ident::Num(y)) = (a, b) {
        return if x == y { 0 } else if x < y { -1 } else { 1 };
    }
    let numeric = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let (at, bt) = (a.text(), b.text());
    let (anum, bnum) = (numeric(&at), numeric(&bt));
    if anum && bnum {
        let (x, y) = (to_number(&at), to_number(&bt));
        return if x == y { 0 } else if x < y { -1 } else { 1 };
    }
    // Strict equality of the original values (a number never equals a string).
    let equal = match (a, b) {
        (Ident::Str(x), Ident::Str(y)) => x == y,
        _ => false,
    };
    if equal {
        0
    } else if anum && !bnum {
        -1
    } else if bnum && !anum {
        1
    } else if at.encode_utf16().lt(bt.encode_utf16()) {
        -1
    } else {
        1
    }
}

/// `class SemVer`.
#[derive(Debug, Clone)]
pub struct SemVer {
    pub options: Options,
    pub major: f64,
    pub minor: f64,
    pub patch: f64,
    pub prerelease: Vec<Ident>,
    /// The formatted version (`format()`).
    pub version: String,
}

impl SemVer {
    /// `new SemVer(version, options)`; `Err` is the TypeError node-semver throws.
    pub fn parse(version: &str, options: Options) -> Result<SemVer, String> {
        if utf16_len(version) > MAX_LENGTH {
            return Err(format!("version is longer than {MAX_LENGTH} characters"));
        }
        let t = tokens();
        let re = if options.loose { &t.loose } else { &t.full };
        let trimmed = js_trim(version);
        let m = re.captures(trimmed).ok_or_else(|| format!("Invalid Version: {version}"))?;
        let number = |i: usize| to_number(m.get(i).map(|g| g.as_str()).unwrap_or(""));
        let (major, minor, patch) = (number(1), number(2), number(3));
        for (value, name) in [(major, "major"), (minor, "minor"), (patch, "patch")] {
            if value > MAX_SAFE_INTEGER || value < 0.0 {
                return Err(format!("Invalid {name} version"));
            }
        }
        let prerelease = match m.get(4).map(|g| g.as_str()).filter(|s| !s.is_empty()) {
            None => Vec::new(),
            Some(text) => text
                .split('.')
                .map(|id| {
                    if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) {
                        let num = to_number(id);
                        if (0.0..MAX_SAFE_INTEGER).contains(&num) {
                            return Ident::Num(num);
                        }
                    }
                    Ident::Str(id.to_owned())
                })
                .collect(),
        };
        // Build metadata (group 5) takes no part in comparisons and is not kept.
        let mut semver = SemVer { options, major, minor, patch, prerelease, version: String::new() };
        semver.format();
        Ok(semver)
    }

    fn format(&mut self) {
        let mut version =
            format!("{}.{}.{}", js_number_string(self.major), js_number_string(self.minor), js_number_string(self.patch));
        if !self.prerelease.is_empty() {
            version.push('-');
            version.push_str(&self.prerelease.iter().map(Ident::text).collect::<Vec<_>>().join("."));
        }
        self.version = version;
    }

    /// `new SemVer(semver, options)` for an existing one: itself when its flags match, else reparsed.
    fn with(&self, options: Options) -> Result<SemVer, String> {
        if self.options.loose == options.loose && self.options.include_prerelease == options.include_prerelease {
            Ok(self.clone())
        } else {
            SemVer::parse(&self.version, options)
        }
    }

    /// `compare(other)` (other already a SemVer of the same options).
    pub fn compare(&self, other: &SemVer) -> i32 {
        if other.version == self.version {
            return 0;
        }
        let main = self.compare_main(other);
        if main != 0 { main } else { self.compare_pre(other) }
    }

    fn compare_main(&self, other: &SemVer) -> i32 {
        for (a, b) in [(self.major, other.major), (self.minor, other.minor), (self.patch, other.patch)] {
            if a < b {
                return -1;
            }
            if a > b {
                return 1;
            }
        }
        0
    }

    fn compare_pre(&self, other: &SemVer) -> i32 {
        match (self.prerelease.is_empty(), other.prerelease.is_empty()) {
            (false, true) => return -1,
            (true, false) => return 1,
            (true, true) => return 0,
            _ => {}
        }
        let mut i = 0;
        loop {
            match (self.prerelease.get(i), other.prerelease.get(i)) {
                (None, None) => return 0,
                (Some(_), None) => return 1,
                (None, Some(_)) => return -1,
                (Some(a), Some(b)) if a == b => {}
                (Some(a), Some(b)) => return compare_identifiers(a, b),
            }
            i += 1;
        }
    }
}

/// `compare(a, b, options)`: both made SemVers of `options` (reparsing when their flags differ).
pub fn compare_with(a: &SemVer, b: &SemVer, options: Options) -> Result<i32, String> {
    Ok(a.with(options)?.compare(&b.with(options)?))
}

/// `semver.compare(a, b, options)` of two strings.
pub fn compare(a: &str, b: &str, options: Options) -> Result<i32, String> {
    Ok(SemVer::parse(a, options)?.compare(&SemVer::parse(b, options)?))
}

/// `semver.rcompare`.
pub fn rcompare(a: &str, b: &str, options: Options) -> Result<i32, String> {
    compare(b, a, options)
}

/// `semver.valid(version, options)`.
pub fn valid(version: &str, options: Options) -> Option<String> {
    SemVer::parse(version, options).ok().map(|v| v.version)
}

/// `semver.clean(version, options)`.
pub fn clean(version: &str, options: Options) -> Option<String> {
    let trimmed = js_trim(version).trim_start_matches(['=', 'v']);
    valid(trimmed, options)
}

/// `class Comparator`.
#[derive(Debug, Clone)]
pub struct Comparator {
    pub operator: String,
    /// `None` is `ANY`.
    pub semver: Option<SemVer>,
    pub value: String,
    options: Options,
}

impl Comparator {
    pub fn parse(comp: &str, options: Options) -> Result<Comparator, String> {
        let comp = split_space(js_trim(comp)).join(" ");
        let t = tokens();
        let re = if options.loose { &t.comparator_loose } else { &t.comparator };
        let m = re.captures(&comp).ok_or_else(|| format!("Invalid comparator: {comp}"))?;
        let mut operator = m.get(1).map(|g| g.as_str().to_owned()).unwrap_or_default();
        if operator == "=" {
            operator.clear();
        }
        // `new SemVer(m[2], this.options.loose)`: a boolean option is `{ loose: true }` or `{}`.
        let semver = match m.get(2).map(|g| g.as_str()).filter(|s| !s.is_empty()) {
            None => None,
            Some(text) => Some(SemVer::parse(text, Options { loose: options.loose, include_prerelease: false })?),
        };
        let value = match &semver {
            None => String::new(),
            Some(v) => format!("{operator}{}", v.version),
        };
        Ok(Comparator { operator, semver, value, options })
    }

    /// `test(version)` for a parsed version; `Err` is a reparse that throws (see [`SemVer::with`]).
    pub fn test(&self, version: &SemVer) -> Result<bool, String> {
        let Some(semver) = &self.semver else { return Ok(true) };
        let order = compare_with(version, semver, self.options)?;
        Ok(match self.operator.as_str() {
            "" | "=" | "==" => order == 0,
            ">" => order > 0,
            ">=" => order >= 0,
            "<" => order < 0,
            "<=" => order <= 0,
            other => return Err(format!("Invalid operator: {other}")),
        })
    }
}

/// `class Range`.
#[derive(Debug, Clone)]
pub struct Range {
    pub set: Vec<Vec<Comparator>>,
    options: Options,
}

impl Range {
    /// `new Range(range, options)`; `Err` is the TypeError node-semver throws.
    pub fn parse(range: &str, options: Options) -> Result<Range, String> {
        let raw = collapse_space(js_trim(range));
        let mut set = Vec::new();
        for part in raw.split("||") {
            let comparators = parse_range(js_trim(part), options)?;
            if !comparators.is_empty() {
                set.push(comparators);
            }
        }
        if set.is_empty() {
            return Err(format!("Invalid SemVer Range: {raw}"));
        }
        if set.len() > 1 {
            let first = set[0].clone();
            set.retain(|c| c[0].value != "<0.0.0-0");
            if set.is_empty() {
                set = vec![first];
            } else if set.len() > 1 {
                if let Some(any) = set.iter().find(|c| c.len() == 1 && c[0].value.is_empty()).cloned() {
                    set = vec![any];
                }
            }
        }
        Ok(Range { set, options })
    }

    /// The `range` getter: comparators joined by spaces, sets by `||`.
    pub fn text(&self) -> String {
        self.set
            .iter()
            .map(|comps| comps.iter().map(|c| js_trim(&c.value).to_owned()).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join("||")
    }

    /// `test(version)` of a version string: `false` when it does not parse; `Err` where node-semver throws.
    pub fn test_str(&self, version: &str) -> Result<bool, String> {
        if version.is_empty() {
            return Ok(false);
        }
        match SemVer::parse(version, self.options) {
            Ok(v) => self.test(&v),
            Err(_) => Ok(false),
        }
    }

    pub fn test(&self, version: &SemVer) -> Result<bool, String> {
        for set in &self.set {
            if test_set(set, version, self.options)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn test_set(set: &[Comparator], version: &SemVer, options: Options) -> Result<bool, String> {
    for comparator in set {
        if !comparator.test(version)? {
            return Ok(false);
        }
    }
    if !version.prerelease.is_empty() && !options.include_prerelease {
        for comparator in set {
            let Some(allowed) = &comparator.semver else { continue };
            if !allowed.prerelease.is_empty()
                && allowed.major == version.major
                && allowed.minor == version.minor
                && allowed.patch == version.patch
            {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    Ok(true)
}

fn group<'a>(caps: &'a Captures, index: usize) -> Option<&'a str> {
    caps.get(index).map(|g| g.as_str())
}

/// `isX`.
fn is_x(id: Option<&str>) -> bool {
    match id {
        None | Some("") => true,
        Some(id) => id.eq_ignore_ascii_case("x") || id == "*",
    }
}

fn parse_range(range: &str, options: Options) -> Result<Vec<Comparator>, String> {
    let t = tokens();
    let range = t.build_strip.replace_all(range, "").into_owned();
    let hyphen = if options.loose { &t.hyphen_loose } else { &t.hyphen };
    let range = hyphen.replace(&range, |caps: &Captures| hyphen_replace(caps, options.include_prerelease)).into_owned();
    let range = t
        .comparator_trim
        .replace_all(&range, |caps: &Captures| {
            format!("{}{}{}", group(caps, 1).unwrap_or(""), group(caps, 2).unwrap_or(""), group(caps, 3).unwrap_or(""))
        })
        .into_owned();
    let range =
        t.tilde_trim.replace_all(&range, |caps: &Captures| format!("{}~", group(caps, 1).unwrap_or(""))).into_owned();
    let range =
        t.caret_trim.replace_all(&range, |caps: &Captures| format!("{}^", group(caps, 1).unwrap_or(""))).into_owned();
    let joined = range.split(' ').map(|comp| parse_comparator(comp, options)).collect::<Vec<_>>().join(" ");
    let gte0 = if options.include_prerelease { &t.gte0_pre } else { &t.gte0 };
    let mut list: Vec<String> = split_space(&joined).into_iter().map(|comp| gte0.replace(js_trim(comp), "").into_owned()).collect();
    if options.loose {
        list.retain(|comp| t.comparator_loose.is_match(comp));
    }
    let mut map: Vec<Comparator> = Vec::new();
    for text in &list {
        let comp = Comparator::parse(text, options)?;
        if comp.value == "<0.0.0-0" {
            return Ok(vec![comp]);
        }
        match map.iter_mut().find(|c| c.value == comp.value) {
            Some(existing) => *existing = comp,
            None => map.push(comp),
        }
    }
    if map.len() > 1 {
        map.retain(|c| !c.value.is_empty());
    }
    Ok(map)
}

fn parse_comparator(comp: &str, options: Options) -> String {
    let t = tokens();
    let comp = t.build.replace(comp, "").into_owned();
    let comp = split_space(js_trim(&comp)).into_iter().map(|c| replace_caret(c, options)).collect::<Vec<_>>().join(" ");
    let comp = split_space(js_trim(&comp)).into_iter().map(|c| replace_tilde(c, options)).collect::<Vec<_>>().join(" ");
    let comp = split_space(&comp).into_iter().map(|c| replace_xrange(c, options)).collect::<Vec<_>>().join(" ");
    t.star.replace(js_trim(&comp), "").into_owned()
}

fn replace_tilde(comp: &str, options: Options) -> String {
    let t = tokens();
    let re = if options.loose { &t.tilde_loose } else { &t.tilde };
    let z = if options.include_prerelease { "-0" } else { "" };
    re.replace(comp, |caps: &Captures| {
        let (major, minor, patch, pre) = (group(caps, 1), group(caps, 2), group(caps, 3), group(caps, 4));
        let (mj, mn, pt) = (major.unwrap_or("undefined"), minor.unwrap_or("undefined"), patch.unwrap_or("undefined"));
        if is_x(major) {
            String::new()
        } else if is_x(minor) {
            format!(">={mj}.0.0{z} <{}.0.0-0", plus_one(mj))
        } else if is_x(patch) {
            format!(">={mj}.{mn}.0{z} <{mj}.{}.0-0", plus_one(mn))
        } else if let Some(pr) = pre.filter(|p| !p.is_empty()) {
            format!(">={mj}.{mn}.{pt}-{pr} <{mj}.{}.0-0", plus_one(mn))
        } else {
            format!(">={mj}.{mn}.{pt} <{mj}.{}.0-0", plus_one(mn))
        }
    })
    .into_owned()
}

fn replace_caret(comp: &str, options: Options) -> String {
    let t = tokens();
    let re = if options.loose { &t.caret_loose } else { &t.caret };
    let z = if options.include_prerelease { "-0" } else { "" };
    re.replace(comp, |caps: &Captures| {
        let (major, minor, patch, pre) = (group(caps, 1), group(caps, 2), group(caps, 3), group(caps, 4));
        let (mj, mn, pt) = (major.unwrap_or("undefined"), minor.unwrap_or("undefined"), patch.unwrap_or("undefined"));
        if is_x(major) {
            String::new()
        } else if is_x(minor) {
            format!(">={mj}.0.0{z} <{}.0.0-0", plus_one(mj))
        } else if is_x(patch) {
            if mj == "0" {
                format!(">={mj}.{mn}.0{z} <{mj}.{}.0-0", plus_one(mn))
            } else {
                format!(">={mj}.{mn}.0{z} <{}.0.0-0", plus_one(mj))
            }
        } else if let Some(pr) = pre.filter(|p| !p.is_empty()) {
            if mj == "0" {
                if mn == "0" {
                    format!(">={mj}.{mn}.{pt}-{pr} <{mj}.{mn}.{}-0", plus_one(pt))
                } else {
                    format!(">={mj}.{mn}.{pt}-{pr} <{mj}.{}.0-0", plus_one(mn))
                }
            } else {
                format!(">={mj}.{mn}.{pt}-{pr} <{}.0.0-0", plus_one(mj))
            }
        } else if mj == "0" {
            if mn == "0" {
                format!(">={mj}.{mn}.{pt} <{mj}.{mn}.{}-0", plus_one(pt))
            } else {
                format!(">={mj}.{mn}.{pt} <{mj}.{}.0-0", plus_one(mn))
            }
        } else {
            format!(">={mj}.{mn}.{pt} <{}.0.0-0", plus_one(mj))
        }
    })
    .into_owned()
}

fn replace_xrange(comp: &str, options: Options) -> String {
    let t = tokens();
    let comp = js_trim(comp);
    let re = if options.loose { &t.xrange_loose } else { &t.xrange };
    re.replace(comp, |caps: &Captures| {
        let whole = group(caps, 0).unwrap_or("");
        let mut gtlt = group(caps, 1).unwrap_or("").to_owned();
        let (major, minor, patch) = (group(caps, 2), group(caps, 3), group(caps, 4));
        // invalidXRangeOrder
        if (is_x(major) && !is_x(minor)) || (is_x(minor) && patch.is_some_and(|p| !p.is_empty()) && !is_x(patch)) {
            return comp.to_owned();
        }
        let xmajor = is_x(major);
        let xminor = xmajor || is_x(minor);
        let xpatch = xminor || is_x(patch);
        let any_x = xpatch;
        if gtlt == "=" && any_x {
            gtlt.clear();
        }
        let mut pr = if options.include_prerelease { "-0" } else { "" };
        let mut mj = major.unwrap_or("undefined").to_owned();
        let mut mn = minor.unwrap_or("undefined").to_owned();
        let mut pt;
        if xmajor {
            if gtlt == ">" || gtlt == "<" { "<0.0.0-0".to_owned() } else { "*".to_owned() }
        } else if !gtlt.is_empty() && any_x {
            if xminor {
                mn = "0".into();
            }
            pt = "0".to_owned();
            if gtlt == ">" {
                gtlt = ">=".into();
                if xminor {
                    mj = plus_one(&mj);
                    mn = "0".into();
                    pt = "0".into();
                } else {
                    mn = plus_one(&mn);
                    pt = "0".into();
                }
            } else if gtlt == "<=" {
                gtlt = "<".into();
                if xminor {
                    mj = plus_one(&mj);
                } else {
                    mn = plus_one(&mn);
                }
            }
            if gtlt == "<" {
                pr = "-0";
            }
            format!("{gtlt}{mj}.{mn}.{pt}{pr}")
        } else if xminor {
            format!(">={mj}.0.0{pr} <{}.0.0-0", plus_one(&mj))
        } else if xpatch {
            format!(">={mj}.{mn}.0{pr} <{mj}.{}.0-0", plus_one(&mn))
        } else {
            whole.to_owned()
        }
    })
    .into_owned()
}

/// `hyphenReplace(incPr)`: groups are (from, fM, fm, fp, fpr, fb, to, tM, tm, tp, tpr).
fn hyphen_replace(caps: &Captures, inc_pr: bool) -> String {
    let g = |i| group(caps, i);
    let (from_text, fmj, fmn, fpt, fpr) = (g(1).unwrap_or("undefined"), g(2), g(3), g(4), g(5));
    let (to_text, tmj, tmn, tpt, tpr) = (g(7).unwrap_or("undefined"), g(8), g(9), g(10), g(11));
    let z = if inc_pr { "-0" } else { "" };
    let u = |s: Option<&str>| s.unwrap_or("undefined").to_owned();
    let from = if is_x(fmj) {
        String::new()
    } else if is_x(fmn) {
        format!(">={}.0.0{z}", u(fmj))
    } else if is_x(fpt) {
        format!(">={}.{}.0{z}", u(fmj), u(fmn))
    } else if fpr.is_some_and(|p| !p.is_empty()) {
        format!(">={from_text}")
    } else {
        format!(">={from_text}{z}")
    };
    let to = if is_x(tmj) {
        String::new()
    } else if is_x(tmn) {
        format!("<{}.0.0-0", plus_one(&u(tmj)))
    } else if is_x(tpt) {
        format!("<{}.{}.0-0", u(tmj), plus_one(&u(tmn)))
    } else if let Some(pr) = tpr.filter(|p| !p.is_empty()) {
        format!("<={}.{}.{}-{pr}", u(tmj), u(tmn), u(tpt))
    } else if inc_pr {
        format!("<{}.{}.{}-0", u(tmj), u(tmn), plus_one(&u(tpt)))
    } else {
        format!("<={to_text}")
    };
    js_trim(&format!("{from} {to}")).to_owned()
}

/// `semver.satisfies(version, range, options)`: `false` for an invalid range; `Err` where node-semver throws.
pub fn satisfies(version: &str, range: &str, options: Options) -> Result<bool, String> {
    match Range::parse(range, options) {
        Ok(range) => range.test_str(version),
        Err(_) => Ok(false),
    }
}

/// `semver.validRange(range, options)`.
pub fn valid_range(range: &str, options: Options) -> Option<String> {
    Range::parse(range, options).ok().map(|r| {
        let text = r.text();
        if text.is_empty() { "*".to_owned() } else { text }
    })
}
