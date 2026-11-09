//! webfetch.ts `extractTextFromHTML`.

use crate::js::{self, JsString};
use crate::parser::{Attributes, Handler, Parser, Writes};

struct Text {
    text: JsString,
    skip_depth: u64,
}

impl Handler for Text {
    fn open_tag(&mut self, name: &str, _attributes: &Attributes) {
        if self.skip_depth > 0 || matches!(name, "script" | "style" | "noscript" | "iframe" | "object" | "embed") {
            self.skip_depth += 1;
        }
    }

    fn text(&mut self, text: &[u8]) {
        if self.skip_depth == 0 {
            js::push(&mut self.text, text);
        }
    }

    fn close_tag(&mut self, _name: &str) {
        if self.skip_depth > 0 {
            self.skip_depth -= 1;
        }
    }
}

pub(crate) fn extract(html: &str) -> String {
    let text = Parser::new(html, Writes::Whole, Text { text: Vec::new(), skip_depth: 0 }).run().text;
    js::into_string(js::trim(&text).to_vec())
}
