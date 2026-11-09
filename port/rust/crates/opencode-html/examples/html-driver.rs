//! The Rust side of harness/diff.ts. Reads records `[kind: u8][length: u32 LE][bytes]` from the file named by the
//! first argument and writes one `[length: u32 LE][bytes]` answer per record to the file named by the second:
//!
//! - `m`: `html_to_markdown(input)`;
//! - `t`: `html_to_text(input)`;
//! - `c`: a content type: `mime\nisImageAttachment\nisTextual\ncheck` (check is `ok` or the error message);
//! - `v`: `convert` of an HTML body: the input is `contentType\0format\0content`;
//! - `d`: `decode_body(bytes)`;
//! - `b`: the body limit: the input is `max\0contentLength\0chunk sizes` (`-` for no header, sizes comma-separated),
//!   the answer `ok:<size>` or `too-large`.

use opencode_html::webfetch::{self, Format};
use std::io::{Read, Write};

fn answer(kind: u8, input: &[u8]) -> Vec<u8> {
    let text = || std::str::from_utf8(input).expect("UTF-8 input");
    match kind {
        b'm' => opencode_html::html_to_markdown(text()).into_bytes(),
        b't' => opencode_html::html_to_text(text()).into_bytes(),
        b'c' => {
            let mime = webfetch::mime_from(text());
            let check = match webfetch::check_content_type(text()) {
                Ok(()) => "ok".to_owned(),
                Err(e) => e,
            };
            format!("{mime}\n{}\n{}\n{check}", webfetch::is_image_attachment(&mime), webfetch::is_textual(&mime))
                .into_bytes()
        }
        b'v' => {
            let mut parts = text().splitn(3, '\0');
            let (content_type, format, content) =
                (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
            let format = Format::parse(format).expect("a format");
            webfetch::convert(content, content_type, format).into_owned().into_bytes()
        }
        b'd' => webfetch::decode_body(input).into_owned().into_bytes(),
        b'b' => {
            let mut parts = text().split('\0');
            let (max, content_length, chunks) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
            let content_length = (content_length != "-").then_some(content_length);
            let outcome = webfetch::BoundedBody::new(content_length, max.parse().unwrap()).and_then(|mut body| {
                for size in chunks.split(',').filter(|s| !s.is_empty()) {
                    body.push(&vec![0u8; size.parse().unwrap()])?;
                }
                Ok(body.finish())
            });
            match outcome {
                Ok(body) => format!("ok:{}", body.len()).into_bytes(),
                Err(webfetch::TooLarge) => b"too-large".to_vec(),
            }
        }
        _ => panic!("unknown record kind {kind}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // `html-driver --m <file>` (or `--t`): one conversion of a file, to stdout.
    if let Some(kind) = args[1].strip_prefix("--") {
        let input = std::fs::read(&args[2]).expect("input file");
        std::io::stdout().write_all(&answer(kind.as_bytes()[0], &input)).unwrap();
        return;
    }
    let mut input = Vec::new();
    std::fs::File::open(&args[1]).expect("records file").read_to_end(&mut input).expect("read records");
    let mut out = std::io::BufWriter::new(std::fs::File::create(&args[2]).expect("answers file"));
    let mut i = 0;
    while i < input.len() {
        let kind = input[i];
        let len = u32::from_le_bytes(input[i + 1..i + 5].try_into().unwrap()) as usize;
        let record = &input[i + 5..i + 5 + len];
        i += 5 + len;
        let result = std::panic::catch_unwind(|| answer(kind, record));
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(e) => {
                let message = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                format!("PANIC: {message}").into_bytes()
            }
        };
        out.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
        out.write_all(&bytes).unwrap();
    }
    out.flush().unwrap();
}
