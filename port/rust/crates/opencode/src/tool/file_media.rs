//! Images in tool results and prompt attachments (packages/core/src/tool.ts `normalizeImages`, image.ts,
//! image/photon-normalize.ts): an inline image within the limits (2000×2000, 5 MiB of base64; a PNG also small:
//! 384 KiB or 256×256 pixels) passes unchanged; a PNG that fits becomes a JPEG when that is smaller (unless it has
//! transparency); a larger image is resized (Lanczos3, to fit, then by quarters) and encoded as the first of PNG and
//! JPEG at qualities 80, 70, 55, 40 that fits (PNG first for transparent or small images). A tool's undecodable
//! images are dropped, and those that cannot be made to fit, each with a note.
//!
//! TS runs photon-node 0.3.4: image 0.24.9 compiled to WebAssembly without SIMD. The port runs the same image 0.24.9
//! with the same formats and codec versions (Cargo.toml), jpeg-decoder's scalar paths, and its Lanczos3 resampling
//! re-implemented with the wasm target's `sinf` (libm), so decoding, resizing and encoding give photon's bytes. What
//! can still differ is other floating-point math the platform's libm does (HDR images' `powf`).

use base64::Engine;
use image::{DynamicImage, ImageBuffer, ImageOutputFormat, Rgba, RgbaImage};
use opencode_protocol::Json;

use crate::json::{self, Obj};

/// `Image.Limits` (their defaults; `Image.configure` is for plugins).
const MAX_WIDTH: u32 = 2_000;
const MAX_HEIGHT: u32 = 2_000;
const MAX_BASE64_BYTES: usize = 5 * 1024 * 1024;
const PNG_PASSTHROUGH_MAX_BASE64_BYTES: usize = 384 * 1024;
const SMALL_PNG_MAX_PIXELS: u64 = 256 * 256;
const JPEG_QUALITIES: [u8; 4] = [80, 70, 55, 40];

/// What normalizing one image gave (`PhotonNormalize.Result`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Normalized {
    Unchanged,
    /// Re-encoded: the base64 data and its type.
    Content { data: String, mime: &'static str },
    DecodeError,
    /// Nothing fits: the image's size, for `Image.SizeError`.
    SizeError { width: u32, height: u32, bytes: usize },
}

/// `Image.SizeError`'s message.
pub fn size_error(resource: &str, width: u32, height: u32, bytes: usize) -> String {
    format!(
        "Image {resource} is {width}x{height} with base64 size {bytes}, exceeding configured limits {MAX_WIDTH}x{MAX_HEIGHT}/{MAX_BASE64_BYTES} bytes"
    )
}

/// `Buffer.from(text, "base64")`: characters outside the alphabets skipped, up to the padding.
fn decode_base64(text: &str) -> Vec<u8> {
    let clean: String = text
        .chars()
        .take_while(|c| *c != '=')
        .filter_map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '/' => Some(c),
            '-' => Some('+'),
            '_' => Some('/'),
            _ => None,
        })
        .collect();
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    // A lone trailing character carries no whole byte.
    let usable = clean.len() - if clean.len() % 4 == 1 { 1 } else { 0 };
    engine.decode(&clean[..usable]).unwrap_or_default()
}

/// Base64 uses four bytes per three input bytes, including padding.
fn base64_length(bytes: &[u8]) -> usize {
    bytes.len().div_ceil(3) * 4
}

/// `hasTransparency`: more than 5% of an evenly spaced grid of up to 64×64 pixels is not (nearly) opaque.
fn has_transparency(pixels: &RgbaImage) -> bool {
    let (width, height) = pixels.dimensions();
    let columns = 64.min(width);
    let rows = 64.min(height);
    let mut transparent = 0;
    for row in 0..rows {
        let y = (((row as f64 + 0.5) * height as f64) / rows as f64).floor() as u32;
        for column in 0..columns {
            let x = (((column as f64 + 0.5) * width as f64) / columns as f64).floor() as u32;
            if pixels.get_pixel(x, y).0[3] < 250 {
                transparent += 1;
            }
        }
    }
    transparent as f64 / (columns * rows) as f64 > 0.05
}

/// `get_bytes_jpeg(quality)`: photon's RGBA image written as a JPEG.
fn jpeg(image: &RgbaImage, quality: u8) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    let _ = DynamicImage::ImageRgba8(image.clone()).write_to(&mut buffer, ImageOutputFormat::Jpeg(quality));
    buffer.into_inner()
}

/// `get_bytes`: photon's RGBA image written as a PNG.
fn png(image: &RgbaImage) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    let _ = DynamicImage::ImageRgba8(image.clone()).write_to(&mut buffer, ImageOutputFormat::Png);
    buffer.into_inner()
}

/// `PhotonNormalize.normalize` of one base64 image of the declared type `mime`.
pub fn normalize(mime: &str, data: &str) -> Normalized {
    let bytes = decode_base64(data);
    // photon unwraps the decoder's result: an error (or a panic) is a trap the caller sees as a decode error.
    let decoded = std::panic::catch_unwind(|| image::load_from_memory(&bytes).map(|image| image.to_rgba8()));
    let Ok(Ok(decoded)) = decoded else { return Normalized::DecodeError };
    drop(bytes);
    let (width, height) = decoded.dimensions();
    let size = data.len();
    let fits = width <= MAX_WIDTH && height <= MAX_HEIGHT && size <= MAX_BASE64_BYTES;
    let small_png = size <= PNG_PASSTHROUGH_MAX_BASE64_BYTES || width as u64 * height as u64 <= SMALL_PNG_MAX_PIXELS;
    if fits && (mime != "image/png" || small_png) {
        return Normalized::Unchanged;
    }
    let transparent = mime != "image/jpeg" && has_transparency(&decoded);
    // An image that already fits only changes when JPEG makes it smaller.
    if fits {
        if transparent {
            return Normalized::Unchanged;
        }
        let encoded = jpeg(&decoded, JPEG_QUALITIES[0]);
        if base64_length(&encoded) < size {
            return Normalized::Content { data: base64::engine::general_purpose::STANDARD.encode(encoded), mime: "image/jpeg" };
        }
        return Normalized::Unchanged;
    }
    let scale = 1f64.min(MAX_WIDTH as f64 / width as f64).min(MAX_HEIGHT as f64 / height as f64);
    let mut sizes: Vec<(u32, u32)> = Vec::new();
    for _ in 0..32 {
        let next = match sizes.last() {
            None => (((width as f64 * scale).round() as u32).max(1), ((height as f64 * scale).round() as u32).max(1)),
            Some(&(w, h)) => (
                if w == 1 { 1 } else { ((w as f64 * 0.75).floor() as u32).max(1) },
                if h == 1 { 1 } else { ((h as f64 * 0.75).floor() as u32).max(1) },
            ),
        };
        if !sizes.contains(&next) {
            sizes.push(next);
        }
    }
    for (w, h) in sizes {
        let resized = if (w, h) == (width, height) { decoded.clone() } else { lanczos3(&decoded, w, h) };
        let png_first = transparent || w as u64 * h as u64 <= SMALL_PNG_MAX_PIXELS;
        let mut order: Vec<Option<u8>> = JPEG_QUALITIES.iter().map(|q| Some(*q)).collect();
        if png_first { order.insert(0, None) } else { order.push(None) }
        for quality in order {
            let (candidate, mime) = match quality {
                None => (png(&resized), "image/png"),
                Some(quality) => (jpeg(&resized, quality), "image/jpeg"),
            };
            if base64_length(&candidate) <= MAX_BASE64_BYTES {
                return Normalized::Content { data: base64::engine::general_purpose::STANDARD.encode(candidate), mime };
            }
        }
    }
    Normalized::SizeError { width, height, bytes: size }
}

/// `imageops::resize(image, width, height, Lanczos3)` of image 0.24.9 (`vertical_sample`, then `horizontal_sample`),
/// with the kernel's `sin` as the wasm target computes it.
fn lanczos3(image: &RgbaImage, new_width: u32, new_height: u32) -> RgbaImage {
    const SUPPORT: f32 = 3.0;
    fn sinc(t: f32) -> f32 {
        let a = t * std::f32::consts::PI;
        if t == 0.0 { 1.0 } else { libm::sinf(a) / a }
    }
    fn kernel(x: f32) -> f32 {
        if x.abs() < SUPPORT { sinc(x) * sinc(x / SUPPORT) } else { 0.0 }
    }
    /// The bounds and normalized weights of one output position.
    fn weights(out: u32, size: u32, new_size: u32, ws: &mut Vec<f32>) -> (u32, u32) {
        let ratio = size as f32 / new_size as f32;
        let sratio = if ratio < 1.0 { 1.0 } else { ratio };
        let src_support = SUPPORT * sratio;
        let input = (out as f32 + 0.5) * ratio;
        let left = ((input - src_support).floor() as i64).clamp(0, size as i64 - 1) as u32;
        let right = ((input + src_support).ceil() as i64).clamp(left as i64 + 1, size as i64) as u32;
        let input = input - 0.5;
        ws.clear();
        let mut sum = 0.0;
        for i in left..right {
            let w = kernel((i as f32 - input) / sratio);
            ws.push(w);
            sum += w;
        }
        ws.iter_mut().for_each(|w| *w /= sum);
        (left, right)
    }
    let (width, height) = image.dimensions();
    let mut ws = Vec::new();
    // Columns first, into floats.
    let mut tmp: ImageBuffer<Rgba<f32>, Vec<f32>> = ImageBuffer::new(width, new_height);
    for outy in 0..new_height {
        let (left, _) = weights(outy, height, new_height, &mut ws);
        for x in 0..width {
            let mut t = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for (i, w) in ws.iter().enumerate() {
                let p = image.get_pixel(x, left + i as u32).0;
                t.0 += p[0] as f32 * w;
                t.1 += p[1] as f32 * w;
                t.2 += p[2] as f32 * w;
                t.3 += p[3] as f32 * w;
            }
            tmp.put_pixel(x, outy, Rgba([t.0, t.1, t.2, t.3]));
        }
    }
    // Then rows, rounded back to bytes.
    let mut out: RgbaImage = ImageBuffer::new(new_width, new_height);
    let byte = |v: f32| v.clamp(0.0, 255.0).round() as u8;
    for outx in 0..new_width {
        let (left, _) = weights(outx, width, new_width, &mut ws);
        for y in 0..new_height {
            let mut t = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for (i, w) in ws.iter().enumerate() {
                let p = tmp.get_pixel(left + i as u32, y).0;
                t.0 += p[0] * w;
                t.1 += p[1] * w;
                t.2 += p[2] * w;
                t.3 += p[3] * w;
            }
            out.put_pixel(outx, y, Rgba([byte(t.0), byte(t.1), byte(t.2), byte(t.3)]));
        }
    }
    out
}

/// [`normalize`] off the runtime's thread (decoding and resizing are CPU work).
pub async fn normalize_async(mime: String, data: String) -> Normalized {
    tokio::task::spawn_blocking(move || normalize(&mime, &data)).await.unwrap_or(Normalized::DecodeError)
}

/// `normalizeImages`: a tool result's inline images normalized; the undecodable ones and those that cannot fit are
/// dropped, with a note for each kind.
pub async fn normalize_content(content: Vec<Json>) -> Vec<Json> {
    let (mut decode_errors, mut size_errors) = (0, 0);
    let mut out = Vec::with_capacity(content.len());
    for mut item in content {
        let mime = json::get_str(&item, "mime").unwrap_or_default().to_owned();
        let image = json::get_str(&item, "type") == Some("file") && mime.starts_with("image/");
        let data = json::get_str(&item, "uri")
            .and_then(|uri| uri.strip_prefix("data:"))
            .and_then(|rest| rest.split_once(','))
            .filter(|(head, _)| head.ends_with(";base64"))
            .map(|(_, data)| data.to_owned());
        let Some(data) = data.filter(|_| image) else {
            out.push(item);
            continue;
        };
        match normalize_async(mime, data).await {
            Normalized::Unchanged => out.push(item),
            Normalized::Content { data, mime } => {
                json::set(&mut item, "uri", json::string(&format!("data:{mime};base64,{data}")));
                json::set(&mut item, "mime", json::string(mime));
                out.push(item);
            }
            Normalized::DecodeError => decode_errors += 1,
            Normalized::SizeError { .. } => size_errors += 1,
        }
    }
    for (count, text) in [(decode_errors, "could not be decoded."), (size_errors, "could not be resized below the image size limit.")] {
        if count > 0 {
            let mut note = Obj::new();
            let plural = if count == 1 { "" } else { "s" };
            note.str("type", "text").str("text", &format!("[{count} image{plural} omitted: {text}]"));
            out.push(note.done());
        }
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
    }

    /// Images recorded through the reference's photon (`IMAGE_CASES=<file>`, see harness/images/run.sh):
    /// `[{mime, data, result: {_tag: "Unchanged" | "DecodeError"} | {_tag: "Content", mime, data} | {_tag:
    /// "SizeError", width, height, bytes}}]`.
    #[test]
    #[ignore]
    fn recorded_images() {
        let path = std::env::var("IMAGE_CASES").expect("IMAGE_CASES names the recorded cases");
        let cases = json::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
        let cases = json::array(&cases).unwrap();
        let mut failures = 0;
        for (index, case) in cases.iter().enumerate() {
            let mime = json::get_str(case, "mime").unwrap();
            let data = json::get_str(case, "data").unwrap();
            let expected = json::get(case, "result").unwrap();
            let actual = normalize(mime, data);
            let same = match (&actual, json::get_str(expected, "_tag").unwrap()) {
                (Normalized::Unchanged, "Unchanged") | (Normalized::DecodeError, "DecodeError") => true,
                (Normalized::Content { data, mime }, "Content") => {
                    Some(*mime) == json::get_str(expected, "mime") && Some(data.as_str()) == json::get_str(expected, "data")
                }
                (Normalized::SizeError { width, height, bytes }, "SizeError") => {
                    json::get_num(expected, "width") == Some(*width as f64)
                        && json::get_num(expected, "height") == Some(*height as f64)
                        && json::get_num(expected, "bytes") == Some(*bytes as f64)
                }
                _ => false,
            };
            if !same {
                failures += 1;
                let shown = match &actual {
                    Normalized::Content { data, mime } => format!("Content {mime} ({} bytes)", data.len()),
                    other => format!("{other:?}"),
                };
                let expected_data = json::get_str(expected, "data").map(|d| d.len());
                eprintln!("case {index} ({mime}, {} bytes): expected {} {:?} ({expected_data:?}), got {shown}", data.len(), json::get_str(expected, "_tag").unwrap(), json::get_str(expected, "mime"));
            }
        }
        eprintln!("{} image cases, {failures} differences", cases.len());
        assert_eq!(failures, 0);
    }

    #[test]
    fn resizes_large_images() {
        let image: RgbaImage = ImageBuffer::from_fn(2500, 100, |x, y| Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]));
        let data = encoded(&png(&image));
        let Normalized::Content { data, mime } = normalize("image/png", &data) else { panic!("resized") };
        assert_eq!(mime, "image/jpeg");
        let back = image::load_from_memory(&decode_base64(&data)).unwrap();
        assert_eq!((back.width(), back.height()), (2000, 80));
    }
}
