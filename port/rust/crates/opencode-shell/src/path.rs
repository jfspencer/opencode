//! Node's `path.posix` (`normalize`, `join`, `resolve`, `dirname`, `basename`) over UTF-16 units, as the TS code
//! calls them on Linux and macOS.

use crate::js::{self, JsString};

const SLASH: u16 = b'/' as u16;
const DOT: u16 = b'.' as u16;

fn normalize_string(path: &[u16], allow_above_root: bool) -> JsString {
    let mut res = JsString::new();
    let mut last_segment_length: isize = 0;
    let mut last_slash: isize = -1;
    let mut dots: isize = 0;
    let mut code: u16 = 0;
    let n = path.len() as isize;
    let mut i: isize = 0;
    while i <= n {
        if i < n {
            code = path[i as usize];
        } else if code == SLASH {
            break;
        } else {
            code = SLASH;
        }
        if code == SLASH {
            if last_slash == i - 1 || dots == 1 {
                // NOOP
            } else if dots == 2 {
                let dotted = res.len() >= 2
                    && last_segment_length == 2
                    && res[res.len() - 1] == DOT
                    && res[res.len() - 2] == DOT;
                if !dotted {
                    if res.len() > 2 {
                        match res.iter().rposition(|&c| c == SLASH) {
                            None => {
                                res.clear();
                                last_segment_length = 0;
                            }
                            Some(last_slash_index) => {
                                res.truncate(last_slash_index);
                                last_segment_length = res.len() as isize
                                    - 1
                                    - res.iter().rposition(|&c| c == SLASH).map_or(-1, |p| p as isize);
                            }
                        }
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if !res.is_empty() {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if !res.is_empty() {
                        res.push(SLASH);
                    }
                    res.extend_from_slice(&[DOT, DOT]);
                    last_segment_length = 2;
                }
            } else {
                if !res.is_empty() {
                    res.push(SLASH);
                }
                res.extend_from_slice(js::slice(path, last_slash + 1, i));
                last_segment_length = i - last_slash - 1;
            }
            last_slash = i;
            dots = 0;
        } else if code == DOT && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }
    res
}

/// `path.posix.normalize(path)`
pub fn normalize(path: &[u16]) -> JsString {
    if path.is_empty() {
        return vec![DOT];
    }
    let absolute = path[0] == SLASH;
    let trailing = path[path.len() - 1] == SLASH;
    let mut normalized = normalize_string(path, !absolute);
    if normalized.is_empty() {
        if absolute {
            return vec![SLASH];
        }
        return if trailing { vec![DOT, SLASH] } else { vec![DOT] };
    }
    if trailing {
        normalized.push(SLASH);
    }
    if absolute {
        normalized.insert(0, SLASH);
    }
    normalized
}

/// `path.posix.join(...parts)`
pub fn join(parts: &[&[u16]]) -> JsString {
    let present: Vec<JsString> = parts.iter().filter(|p| !p.is_empty()).map(|p| p.to_vec()).collect();
    if present.is_empty() {
        return vec![DOT];
    }
    normalize(&js::join(&present, &[SLASH]))
}

/// `path.posix.resolve(...parts)`, with `cwd` standing in for `process.cwd()`.
pub fn resolve(parts: &[&[u16]], cwd: &[u16]) -> JsString {
    let mut resolved = JsString::new();
    let mut absolute = false;
    for part in parts.iter().rev().copied().chain(std::iter::once(cwd)) {
        if absolute {
            break;
        }
        if part.is_empty() {
            continue;
        }
        let mut next = part.to_vec();
        next.push(SLASH);
        next.extend_from_slice(&resolved);
        resolved = next;
        absolute = part[0] == SLASH;
    }
    let normalized = normalize_string(&resolved, !absolute);
    if absolute {
        let mut out = vec![SLASH];
        out.extend_from_slice(&normalized);
        return out;
    }
    if normalized.is_empty() { vec![DOT] } else { normalized }
}

/// `path.posix.dirname(path)`
pub fn dirname(path: &[u16]) -> JsString {
    if path.is_empty() {
        return vec![DOT];
    }
    let root = path[0] == SLASH;
    let mut end: isize = -1;
    let mut matched_slash = true;
    for i in (1..path.len()).rev() {
        if path[i] == SLASH {
            if !matched_slash {
                end = i as isize;
                break;
            }
        } else {
            matched_slash = false;
        }
    }
    if end == -1 {
        return if root { vec![SLASH] } else { vec![DOT] };
    }
    if root && end == 1 {
        return vec![SLASH, SLASH];
    }
    path[..end as usize].to_vec()
}

/// `path.posix.basename(path)`
pub fn basename(path: &[u16]) -> JsString {
    let mut start = 0;
    let mut end: Option<usize> = None;
    let mut matched_slash = true;
    for i in (0..path.len()).rev() {
        if path[i] == SLASH {
            if !matched_slash {
                start = i + 1;
                break;
            }
        } else if end.is_none() {
            matched_slash = false;
            end = Some(i + 1);
        }
    }
    match end {
        None => JsString::new(),
        Some(end) => path[start..end].to_vec(),
    }
}
