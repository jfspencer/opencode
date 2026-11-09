//! The parts of the WHATWG URL parser (JS `new URL(input, base)`) the tools and discovery need: absolute and relative
//! parsing, `href`, `origin`, the credentials and protocol checks. Special schemes (`http`, `https`, `ws`, `wss`,
//! `ftp`, `file`) get their authority, host, port, dot-segment and percent-encoding rules; other schemes keep an
//! opaque path or a path with an optional authority.
//!
//! TODO(port): international domain names (`ToASCII` with UTS #46 mapping and Punycode; a non-ASCII host is rejected
//! here) and IPv6 address compression (an IPv6 host is kept as written, lower-cased).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// Lower-cased, without the `:`.
    pub scheme: String,
    pub username: String,
    pub password: String,
    /// `None` for a URL without an authority (`mailto:x`).
    pub host: Option<String>,
    /// `None` for the scheme's default port.
    pub port: Option<u16>,
    /// The serialized path: `/a/b` (segments), or an opaque path.
    pub path: String,
    opaque: bool,
    pub query: Option<String>,
    pub fragment: Option<String>,
}

fn special_port(scheme: &str) -> Option<Option<u16>> {
    match scheme {
        "http" | "ws" => Some(Some(80)),
        "https" | "wss" => Some(Some(443)),
        "ftp" => Some(Some(21)),
        "file" => Some(None),
        _ => None,
    }
}

fn is_special(scheme: &str) -> bool {
    special_port(scheme).is_some()
}

// Percent-encode sets (https://url.spec.whatwg.org/#percent-encoded-bytes).
fn c0(b: u8) -> bool {
    b < 0x20 || b > 0x7e
}
fn fragment_set(b: u8) -> bool {
    c0(b) || matches!(b, b' ' | b'"' | b'<' | b'>' | b'`')
}
fn query_set(b: u8) -> bool {
    c0(b) || matches!(b, b' ' | b'"' | b'#' | b'<' | b'>')
}
fn special_query_set(b: u8) -> bool {
    query_set(b) || b == b'\''
}
fn path_set(b: u8) -> bool {
    query_set(b) || matches!(b, b'?' | b'^' | b'`' | b'{' | b'}')
}
fn userinfo_set(b: u8) -> bool {
    path_set(b) || matches!(b, b'/' | b':' | b';' | b'=' | b'@' | b'[' | b'\\' | b']' | b'|')
}

fn encode(text: &str, set: fn(u8) -> bool, out: &mut String) {
    for &b in text.as_bytes() {
        if set(b) {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push(b as char);
        }
    }
}

fn encoded(text: &str, set: fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(text.len());
    encode(text, set, &mut out);
    out
}

/// Percent-decodes into bytes (`%` not followed by two hex digits stays as is).
pub fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() && bytes[i + 1].is_ascii_hexdigit() && bytes[i + 2].is_ascii_hexdigit() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
            out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn single_dot(segment: &str) -> bool {
    segment == "." || segment.eq_ignore_ascii_case("%2e")
}

fn double_dot(segment: &str) -> bool {
    matches!(segment.to_ascii_lowercase().as_str(), ".." | ".%2e" | "%2e." | "%2e%2e")
}

/// Parses an IPv4 number (decimal, `0x` hex, `0` octal); `None` when it is not one.
fn ipv4_number(part: &str) -> Option<u64> {
    if part.is_empty() {
        return None;
    }
    let (digits, radix) = if let Some(rest) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
        (rest, 16)
    } else if part.len() > 1 && part.starts_with('0') {
        (&part[1..], 8)
    } else {
        (part, 10)
    };
    if digits.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(digits, radix).ok().or_else(|| {
        // Too large to fit is still a number (it fails the range check).
        digits.chars().all(|c| c.is_digit(radix)).then_some(u64::MAX)
    })
}

/// The IPv4 parser's verdict on a host that ends in a number: `Some(Ok(address))`, `Some(Err)` (failure) or `None`
/// (not an IPv4 address: a domain).
fn ipv4(host: &str) -> Option<Result<String, ()>> {
    let mut parts: Vec<&str> = host.split('.').collect();
    if parts.last() == Some(&"") && parts.len() > 1 {
        parts.pop();
    }
    let last = parts.last()?;
    let ends_in_number = !last.is_empty()
        && (last.chars().all(|c| c.is_ascii_digit())
            || ((last.starts_with("0x") || last.starts_with("0X")) && last[2..].chars().all(|c| c.is_ascii_hexdigit())));
    if !ends_in_number {
        return None;
    }
    if parts.len() > 4 {
        return Some(Err(()));
    }
    let mut numbers = Vec::new();
    for part in &parts {
        match ipv4_number(part) {
            Some(n) => numbers.push(n),
            None => return Some(Err(())),
        }
    }
    let (last, rest) = numbers.split_last()?;
    if rest.iter().any(|n| *n > 255) || *last >= 256u64.pow(5 - numbers.len() as u32) {
        return Some(Err(()));
    }
    let mut address = *last;
    for (i, n) in rest.iter().enumerate() {
        address += n * 256u64.pow(3 - i as u32);
    }
    Some(Ok(format!("{}.{}.{}.{}", address >> 24, (address >> 16) & 255, (address >> 8) & 255, address & 255)))
}

fn forbidden_host(c: char) -> bool {
    matches!(c, '\0' | '\t' | '\n' | '\r' | ' ' | '#' | '/' | ':' | '<' | '>' | '?' | '@' | '[' | '\\' | ']' | '^' | '|')
}

fn forbidden_domain(c: char) -> bool {
    forbidden_host(c) || (c as u32) < 0x20 || c == '%' || c == '\u{7f}'
}

/// The host parser.
fn host(input: &str, special: bool) -> Result<String, ()> {
    if let Some(inner) = input.strip_prefix('[') {
        let inner = inner.strip_suffix(']').ok_or(())?;
        if inner.is_empty() || !inner.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.') {
            return Err(());
        }
        return Ok(format!("[{}]", inner.to_ascii_lowercase()));
    }
    if !special {
        if input.chars().any(forbidden_host) {
            return Err(());
        }
        return Ok(encoded(input, c0));
    }
    let decoded = String::from_utf8(percent_decode(input)).map_err(|_| ())?;
    if !decoded.is_ascii() {
        // TODO(port): domain to ASCII (IDNA).
        return Err(());
    }
    let domain = decoded.to_ascii_lowercase();
    if domain.is_empty() || domain.chars().any(forbidden_domain) {
        return Err(());
    }
    match ipv4(&domain) {
        Some(Ok(address)) => Ok(address),
        Some(Err(())) => Err(()),
        None => Ok(domain),
    }
}

impl Url {
    /// `new URL(input)`.
    pub fn parse(input: &str) -> Option<Url> {
        parse(input, None).ok()
    }

    /// `new URL(input, base)`.
    pub fn join(&self, input: &str) -> Option<Url> {
        parse(input, Some(self)).ok()
    }

    pub fn is_special(&self) -> bool {
        is_special(&self.scheme)
    }

    /// `url.protocol`.
    pub fn protocol(&self) -> String {
        format!("{}:", self.scheme)
    }

    /// `url.host` (with the port).
    pub fn authority(&self) -> String {
        let host = self.host.clone().unwrap_or_default();
        match self.port {
            Some(port) => format!("{host}:{port}"),
            None => host,
        }
    }

    /// `url.origin` (`null` for schemes without a tuple origin).
    pub fn origin(&self) -> String {
        if !matches!(self.scheme.as_str(), "http" | "https" | "ws" | "wss" | "ftp") {
            return "null".to_owned();
        }
        format!("{}://{}", self.scheme, self.authority())
    }

    /// `url.href`.
    pub fn href(&self) -> String {
        let mut out = format!("{}:", self.scheme);
        if let Some(host) = &self.host {
            out.push_str("//");
            if !self.username.is_empty() || !self.password.is_empty() {
                out.push_str(&self.username);
                if !self.password.is_empty() {
                    out.push(':');
                    out.push_str(&self.password);
                }
                out.push('@');
            }
            out.push_str(host);
            if let Some(port) = self.port {
                out.push_str(&format!(":{port}"));
            }
        } else if !self.opaque && self.path.starts_with("//") {
            out.push_str("/.");
        }
        out.push_str(&self.path);
        if let Some(query) = &self.query {
            out.push('?');
            out.push_str(query);
        }
        if let Some(fragment) = &self.fragment {
            out.push('#');
            out.push_str(fragment);
        }
        out
    }
}

/// Splits a raw path into segments and resolves dot segments onto `segments`.
fn push_path(segments: &mut Vec<String>, raw: &str, special: bool) {
    let parts: Vec<&str> = if special { raw.split(['/', '\\']).collect() } else { raw.split('/').collect() };
    let count = parts.len();
    for (i, part) in parts.into_iter().enumerate() {
        let last = i + 1 == count;
        if double_dot(part) {
            segments.pop();
            if last {
                segments.push(String::new());
            }
        } else if single_dot(part) {
            if last {
                segments.push(String::new());
            }
        } else {
            segments.push(encoded(part, path_set));
        }
    }
}

fn serialize_path(segments: &[String]) -> String {
    segments.iter().map(|s| format!("/{s}")).collect()
}

/// Splits off `?query` and `#fragment` from what follows the authority (or a relative reference).
fn split_rest(rest: &str) -> (&str, Option<&str>, Option<&str>) {
    let (before_fragment, fragment) = match rest.find('#') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    let (path, query) = match before_fragment.find('?') {
        Some(i) => (&before_fragment[..i], Some(&before_fragment[i + 1..])),
        None => (before_fragment, None),
    };
    (path, query, fragment)
}

fn finish(mut url: Url, query: Option<&str>, fragment: Option<&str>) -> Url {
    let set = if url.is_special() { special_query_set } else { query_set };
    url.query = query.map(|q| encoded(q, set));
    url.fragment = fragment.map(|f| encoded(f, fragment_set));
    url
}

/// Parses an authority and what follows it (`rest` starts right after the slashes).
fn authority(scheme: &str, rest: &str) -> Result<Url, ()> {
    let special = is_special(scheme);
    let end = rest.find(|c| c == '/' || c == '?' || c == '#' || (special && c == '\\')).unwrap_or(rest.len());
    let (authority, after) = rest.split_at(end);
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    let (username, password) = match userinfo {
        Some(info) => match info.find(':') {
            Some(i) => (encoded(&info[..i], userinfo_set), encoded(&info[i + 1..], userinfo_set)),
            None => (encoded(info, userinfo_set), String::new()),
        },
        None => (String::new(), String::new()),
    };
    if userinfo.is_some() && hostport.is_empty() {
        return Err(());
    }
    let (host_text, port_text) = match hostport.rfind(':') {
        Some(i) if !hostport[i..].contains(']') => (&hostport[..i], Some(&hostport[i + 1..])),
        _ => (hostport, None),
    };
    let default = special_port(scheme).flatten();
    let port = match port_text {
        None | Some("") => None,
        Some(digits) => {
            if !digits.chars().all(|c| c.is_ascii_digit()) {
                return Err(());
            }
            let value: u32 = digits.trim_start_matches('0').parse().unwrap_or(0);
            if value > 65535 {
                return Err(());
            }
            let value = value as u16;
            (Some(value) != default).then_some(value)
        }
    };
    let host = if host_text.is_empty() {
        if special && scheme != "file" {
            return Err(());
        }
        if port.is_some() || userinfo.is_some() {
            return Err(());
        }
        String::new()
    } else {
        let parsed = host(host_text, special)?;
        if scheme == "file" && parsed == "localhost" { String::new() } else { parsed }
    };
    let (path, query, fragment) = split_rest(after);
    let mut segments = Vec::new();
    if !path.is_empty() {
        push_path(&mut segments, &path[1..], special);
    }
    let path = if special && segments.is_empty() { "/".to_owned() } else { serialize_path(&segments) };
    let url = Url {
        scheme: scheme.to_owned(),
        username,
        password,
        host: Some(host),
        port,
        path,
        opaque: false,
        query: None,
        fragment: None,
    };
    Ok(finish(url, query, fragment))
}

fn segments_of(path: &str) -> Vec<String> {
    if path.is_empty() {
        return vec![];
    }
    path[1..].split('/').map(str::to_owned).collect()
}

/// Resolves a relative reference against a base.
fn relative(base: &Url, input: &str) -> Result<Url, ()> {
    let special = base.is_special();
    let slash = |c: char| c == '/' || (special && c == '\\');
    let mut chars = input.chars();
    let first = chars.next();
    let second = chars.next();
    if first.is_some_and(slash) && second.is_some_and(slash) {
        return authority(&base.scheme, &input[2..]);
    }
    if base.opaque {
        // Only a fragment may follow an opaque path.
        let Some(fragment) = input.strip_prefix('#') else { return Err(()) };
        let mut url = base.clone();
        url.fragment = Some(encoded(fragment, fragment_set));
        return Ok(url);
    }
    let mut url = Url { query: None, fragment: None, ..base.clone() };
    let (path, query, fragment) = split_rest(input);
    if path.is_empty() {
        url.query = base.query.clone();
        let set = if special { special_query_set } else { query_set };
        if let Some(query) = query {
            url.query = Some(encoded(query, set));
        }
        url.fragment = fragment.map(|f| encoded(f, fragment_set));
        return Ok(url);
    }
    let mut segments = if path.starts_with(slash) {
        let mut segments = Vec::new();
        push_path(&mut segments, &path[1..], special);
        segments
    } else {
        let mut segments = segments_of(&base.path);
        segments.pop();
        push_path(&mut segments, path, special);
        segments
    };
    if special && segments.is_empty() {
        segments.push(String::new());
    }
    url.path = serialize_path(&segments);
    Ok(finish(url, query, fragment))
}

fn parse(input: &str, base: Option<&Url>) -> Result<Url, ()> {
    let trimmed = input.trim_matches(|c: char| (c as u32) <= 0x20);
    let cleaned: String = trimmed.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
    let input = cleaned.as_str();
    let scheme_end = input.find(':').filter(|&i| {
        let name = &input[..i];
        name.starts_with(|c: char| c.is_ascii_alphabetic())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    let Some(end) = scheme_end else {
        return match base {
            Some(base) => relative(base, input),
            None => Err(()),
        };
    };
    let scheme = input[..end].to_ascii_lowercase();
    let rest = &input[end + 1..];
    if is_special(&scheme) {
        if let Some(base) = base.filter(|b| b.scheme == scheme) {
            if !rest.starts_with(['/', '\\']) {
                return relative(base, rest);
            }
        }
        if scheme == "file" {
            // TODO(port): file URLs' drive letters and base-relative forms.
            let slashes = rest.chars().take(2).filter(|c| *c == '/' || *c == '\\').count();
            if slashes == 2 {
                return authority(&scheme, &rest[2..]);
            }
            return authority(&scheme, &format!("/{}", rest.trim_start_matches(['/', '\\'])));
        }
        // Special schemes ignore any number of slashes before the authority.
        return authority(&scheme, rest.trim_start_matches(['/', '\\']));
    }
    if let Some(after) = rest.strip_prefix("//") {
        return authority(&scheme, after);
    }
    let (path, query, fragment) = split_rest(rest);
    if path.starts_with('/') {
        let mut segments = Vec::new();
        push_path(&mut segments, &path[1..], false);
        let url = Url {
            scheme,
            username: String::new(),
            password: String::new(),
            host: None,
            port: None,
            path: serialize_path(&segments),
            opaque: false,
            query: None,
            fragment: None,
        };
        return Ok(finish(url, query, fragment));
    }
    let url = Url {
        scheme,
        username: String::new(),
        password: String::new(),
        host: None,
        port: None,
        path: encoded(path, c0),
        opaque: true,
        query: None,
        fragment: None,
    };
    Ok(finish(url, query, fragment))
}

/// `URL.canParse(value) && /^https?:$/.test(new URL(value).protocol)`.
pub fn is_http(value: &str) -> bool {
    Url::parse(value).is_some_and(|url| url.scheme == "http" || url.scheme == "https")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn href(input: &str) -> Option<String> {
        Url::parse(input).map(|u| u.href())
    }

    #[test]
    fn absolute() {
        assert_eq!(href("https://Docs.Example.com/release#section-2").as_deref(), Some("https://docs.example.com/release#section-2"));
        assert_eq!(href("http://unanswered.example.com").as_deref(), Some("http://unanswered.example.com/"));
        assert_eq!(href("https://a.example:443/x/../y/./z?q=a b#f g").as_deref(), Some("https://a.example/y/z?q=a%20b#f%20g"));
        assert_eq!(href("not a url"), None);
        assert_eq!(href("ftp://files.example.com/file.txt").as_deref(), Some("ftp://files.example.com/file.txt"));
        assert_eq!(href("file:///etc/passwd").as_deref(), Some("file:///etc/passwd"));
        assert_eq!(href("mailto:someone@example.com").as_deref(), Some("mailto:someone@example.com"));
        assert_eq!(href("http://127.1/").as_deref(), Some("http://127.0.0.1/"));
        assert_eq!(href("http://exa mple.com/"), None);
        assert_eq!(href("https://"), None);
        let url = Url::parse("https://user:secret@errors.example.com/private").unwrap();
        assert_eq!((url.username.as_str(), url.password.as_str()), ("user", "secret"));
    }

    #[test]
    fn relative_references() {
        let base = Url::parse("https://old.example.com/a/b?x#y").unwrap();
        let join = |input: &str| base.join(input).map(|u| u.href());
        assert_eq!(join("/final").as_deref(), Some("https://old.example.com/final"));
        assert_eq!(join("c").as_deref(), Some("https://old.example.com/a/c"));
        assert_eq!(join("../c").as_deref(), Some("https://old.example.com/c"));
        assert_eq!(join("?z").as_deref(), Some("https://old.example.com/a/b?z"));
        assert_eq!(join("#z").as_deref(), Some("https://old.example.com/a/b?x#z"));
        assert_eq!(join("//new.example.com/landing").as_deref(), Some("https://new.example.com/landing"));
        assert_eq!(join("https://new.example.com/landing#frag").as_deref(), Some("https://new.example.com/landing#frag"));
        assert_eq!(join("ftp://old.example.com/file").as_deref(), Some("ftp://old.example.com/file"));
        let index = Url::parse("https://skills.example.com/catalog/").unwrap();
        assert_eq!(index.join("index.json").map(|u| u.href()).as_deref(), Some("https://skills.example.com/catalog/index.json"));
        assert_eq!(index.join("bad%E0%A4%A.md").map(|u| u.href()).as_deref(), Some("https://skills.example.com/catalog/bad%E0%A4%A.md"));
        assert_eq!(index.origin(), "https://skills.example.com");
    }
}
