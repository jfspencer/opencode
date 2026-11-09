//! `util/locale.ts` truncation as JS measures it (UTF-16 code units): `truncate`, `truncateLeft`,
//! `truncateMiddle`.

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn from(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// `truncate(str, len)`.
pub fn truncate(text: &str, len: usize) -> String {
    let u = units(text);
    if u.len() <= len {
        return text.to_string();
    }
    from(&u[..len.saturating_sub(1)]) + "…"
}

/// `truncateLeft(str, len)`.
pub fn truncate_left(text: &str, len: usize) -> String {
    let u = units(text);
    if u.len() <= len {
        return text.to_string();
    }
    let keep = len.saturating_sub(1);
    // `str.slice(-0)` is the whole string.
    let start = if keep == 0 { 0 } else { u.len() - keep };
    "…".to_string() + &from(&u[start..])
}

/// `truncateMiddle(str, maxLength)`.
pub fn truncate_middle(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let u = units(text);
    if u.len() <= max {
        return text.to_string();
    }
    let keep_start = (max - 1).div_ceil(2);
    let keep_end = (max - 1) / 2;
    if keep_end == 0 {
        return from(&u[..keep_start]) + "…";
    }
    from(&u[..keep_start]) + "…" + &from(&u[u.len() - keep_end..])
}
