//! Images in tool results (packages/core/src/tool.ts `normalizeImages`, image.ts, image/photon-normalize.ts): every
//! inline image a tool returns is normalized before it is recorded. One that cannot be decoded is dropped, and a
//! note says how many were; one within the limits (2000×2000, 5 MiB of base64; a PNG also small: 384 KiB or 256×256
//! pixels) passes unchanged.
//!
//! TS decodes with photon (image 0.24.9 compiled to WebAssembly). The port checks each format's structure instead:
//! PNG chunks and their CRCs, JPEG markers up to a frame and a scan, GIF blocks, WebP's RIFF chunks and bitstream
//! headers; that is what tells the undecodable files in the standard apart, and it gives the dimensions the limits
//! need. TODO(port): decoding the compressed data itself, and resizing and re-encoding images over the limits (TS
//! returns a JPEG or PNG re-encoded by photon, or "could not be resized" when nothing fits; here they pass
//! unchanged).

use base64::Engine;
use opencode_protocol::Json;

use crate::json::{self, Obj};

const MAX_WIDTH: u64 = 2_000;
const MAX_HEIGHT: u64 = 2_000;
const MAX_BASE64_BYTES: usize = 5 * 1024 * 1024;
const PNG_PASSTHROUGH_MAX_BASE64_BYTES: usize = 384 * 1024;
const SMALL_PNG_MAX_PIXELS: u64 = 256 * 256;

/// What normalizing one image gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Normalized {
    Unchanged,
    DecodeError,
}

fn crc32(bytes: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (n, slot) in table.iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        table
    });
    !bytes.iter().fold(!0u32, |c, b| table[((c ^ *b as u32) & 0xff) as usize] ^ (c >> 8))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A PNG's dimensions when its chunks are sound: IHDR first and valid, every CRC right, image data, IEND.
fn png(bytes: &[u8]) -> Option<(u64, u64)> {
    let mut at = 8;
    let mut size = None;
    let mut data = false;
    loop {
        let length = be32(bytes, at)? as usize;
        let kind = bytes.get(at + 4..at + 8)?;
        let body = bytes.get(at + 8..at + 8 + length)?;
        let crc = be32(bytes, at + 8 + length)?;
        if crc32(&bytes[at + 4..at + 8 + length]) != crc {
            return None;
        }
        match kind {
            b"IHDR" => {
                if size.is_some() || length != 13 {
                    return None;
                }
                let (width, height) = (be32(body, 0)? as u64, be32(body, 4)? as u64);
                let (depth, color) = (body[8], body[9]);
                let valid = match color {
                    0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
                    3 => matches!(depth, 1 | 2 | 4 | 8),
                    2 | 4 | 6 => matches!(depth, 8 | 16),
                    _ => false,
                };
                if width == 0 || height == 0 || !valid || body[10] != 0 || body[11] != 0 || body[12] > 1 {
                    return None;
                }
                size = Some((width, height));
            }
            _ if size.is_none() => return None,
            b"IDAT" => data = true,
            b"IEND" => return data.then_some(size?),
            _ => {}
        }
        at += 12 + length;
    }
}

/// A JPEG's dimensions when its markers reach a frame and a scan.
fn jpeg(bytes: &[u8]) -> Option<(u64, u64)> {
    let mut at = 2;
    let mut size = None;
    loop {
        if *bytes.get(at)? != 0xff {
            return None;
        }
        while bytes.get(at) == Some(&0xff) {
            at += 1;
        }
        let marker = *bytes.get(at)?;
        at += 1;
        match marker {
            0xd9 => return None,
            0xd0..=0xd7 | 0x01 => continue,
            _ => {}
        }
        let length = u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]) as usize;
        if length < 2 {
            return None;
        }
        let segment = bytes.get(at + 2..at + length)?;
        match marker {
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                let height = u16::from_be_bytes([*segment.get(1)?, *segment.get(2)?]) as u64;
                let width = u16::from_be_bytes([*segment.get(3)?, *segment.get(4)?]) as u64;
                let components = *segment.get(5)? as usize;
                if width == 0 || components == 0 || segment.len() < 6 + components * 3 {
                    return None;
                }
                size = Some((width, height));
            }
            // The scan: a frame must come first; its entropy-coded data follows.
            0xda => return size,
            _ => {}
        }
        at += length;
    }
}

/// A GIF's dimensions when its blocks are sound and it has an image.
fn gif(bytes: &[u8]) -> Option<(u64, u64)> {
    if !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return None;
    }
    let (width, height) = (le16(bytes, 6)? as u64, le16(bytes, 8)? as u64);
    let flags = *bytes.get(10)?;
    let mut at = 13 + if flags & 0x80 != 0 { 3 << ((flags & 7) + 1) } else { 0 };
    let sub_blocks = |mut at: usize| -> Option<usize> {
        loop {
            let size = *bytes.get(at)? as usize;
            at += 1;
            if size == 0 {
                return Some(at);
            }
            bytes.get(at..at + size)?;
            at += size;
        }
    };
    let mut images = 0;
    loop {
        match *bytes.get(at)? {
            0x3b => return (images > 0).then_some((width, height)),
            0x21 => at = sub_blocks(at + 2)?,
            0x2c => {
                let flags = *bytes.get(at + 9)?;
                at += 10 + if flags & 0x80 != 0 { 3 << ((flags & 7) + 1) } else { 0 };
                let minimum = *bytes.get(at)?;
                if !(1..=11).contains(&minimum) {
                    return None;
                }
                at = sub_blocks(at + 1)?;
                images += 1;
            }
            _ => return None,
        }
    }
}

/// A WebP's dimensions when its RIFF chunks and bitstream header are sound.
fn webp(bytes: &[u8]) -> Option<(u64, u64)> {
    let riff = le32(bytes, 4)? as usize;
    if riff + 8 > bytes.len() || bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    let kind = bytes.get(12..16)?;
    let length = le32(bytes, 16)? as usize;
    let chunk = bytes.get(20..20 + length)?;
    match kind {
        b"VP8L" => {
            if *chunk.first()? != 0x2f {
                return None;
            }
            let bits = le32(chunk, 1)?;
            if bits >> 29 != 0 {
                return None;
            }
            Some(((bits & 0x3fff) as u64 + 1, ((bits >> 14) & 0x3fff) as u64 + 1))
        }
        b"VP8 " => {
            if chunk.get(3..6)? != [0x9d, 0x01, 0x2a] || chunk[0] & 1 != 0 {
                return None;
            }
            let (width, height) = (le16(chunk, 6)? & 0x3fff, le16(chunk, 8)? & 0x3fff);
            (width > 0 && height > 0).then_some((width as u64, height as u64))
        }
        b"VP8X" => {
            let width = u32::from_le_bytes([*chunk.get(4)?, *chunk.get(5)?, *chunk.get(6)?, 0]) as u64 + 1;
            let height = u32::from_le_bytes([*chunk.get(7)?, *chunk.get(8)?, *chunk.get(9)?, 0]) as u64 + 1;
            Some((width, height))
        }
        _ => None,
    }
}

/// `Image.normalize` of one base64 image.
pub fn normalize(mime: &str, data: &str) -> Normalized {
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data.trim_end()) else { return Normalized::DecodeError };
    let size = match crate::session::attachment::detect(&bytes) {
        "image/png" => png(&bytes),
        "image/jpeg" => jpeg(&bytes),
        "image/gif" => gif(&bytes),
        "image/webp" => webp(&bytes),
        _ => None,
    };
    let Some((width, height)) = size else { return Normalized::DecodeError };
    let fits = width <= MAX_WIDTH && height <= MAX_HEIGHT && data.len() <= MAX_BASE64_BYTES;
    let small = mime != "image/png" || data.len() <= PNG_PASSTHROUGH_MAX_BASE64_BYTES || width * height <= SMALL_PNG_MAX_PIXELS;
    if !(fits && small) {
        // TODO(port): resizing and re-encoding (see the module doc); the image passes unchanged.
    }
    Normalized::Unchanged
}

/// `normalizeImages`: a tool result's inline images normalized; undecodable ones dropped, with a note.
pub fn normalize_content(content: Vec<Json>) -> Vec<Json> {
    let mut decode_errors = 0;
    let mut out = Vec::with_capacity(content.len());
    for item in content {
        let image = json::get_str(&item, "type") == Some("file") && json::get_str(&item, "mime").is_some_and(|m| m.starts_with("image/"));
        let data = json::get_str(&item, "uri")
            .and_then(|uri| uri.strip_prefix("data:"))
            .and_then(|rest| rest.split_once(','))
            .filter(|(head, _)| head.ends_with(";base64"))
            .map(|(_, data)| data.to_owned());
        match data.filter(|_| image) {
            Some(data) if normalize(json::get_str(&item, "mime").unwrap_or_default(), &data) == Normalized::DecodeError => {
                decode_errors += 1;
            }
            _ => out.push(item),
        }
    }
    if decode_errors > 0 {
        let mut note = Obj::new();
        let plural = if decode_errors == 1 { "" } else { "s" };
        note.str("type", "text").str("text", &format!("[{decode_errors} image{plural} omitted: could not be decoded.]"));
        out.push(note.done());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
    }

    fn encoded(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn decodable() {
        let valid = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        assert_eq!(normalize("image/png", valid), Normalized::Unchanged);
        let bad_crc = hex("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d4944415478da63f8cfc0f01f0005000201a5f6a6c70000000049454e44ae426082");
        assert_eq!(normalize("image/png", &encoded(&bad_crc)), Normalized::DecodeError);
        let no_frame = hex("ffd8ffe000104a46494600010100000100010000ffd9");
        assert_eq!(normalize("image/jpeg", &encoded(&no_frame)), Normalized::DecodeError);
        let gif = hex("47494638396101000100800000000000ffffff21f90401000000002c00000000010001000002024401003b");
        assert_eq!(normalize("image/gif", &encoded(&gif)), Normalized::Unchanged);
        let mut webp = b"RIFF".to_vec();
        webp.extend([26, 0, 0, 0]);
        webp.extend(b"WEBPVP8L");
        webp.extend([1u8; 14]);
        assert_eq!(normalize("image/webp", &encoded(&webp)), Normalized::DecodeError);
    }
}
