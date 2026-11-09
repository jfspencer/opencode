//! OpenTUI 0.8.1's input layer, ported from `packages/core/src/lib`:
//!
//! - [`stdin`] (`stdin-parser.ts`): stdin bytes to [`StdinEvent`]s (keys, mouse reports, pastes, terminal
//!   replies), with OpenTUI's framing, split-read handling and ESC timeout.
//! - [`keypress`] (`parse.keypress.ts`) and [`kitty`] (`parse.keypress-kitty.ts`): a key's bytes to a
//!   [`ParsedKey`], legacy, modifyOtherKeys and Kitty keyboard protocol forms.
//! - [`mouse`] (`parse.mouse.ts`): SGR and X10 mouse reports.
//! - [`paste`] (`paste.ts`): paste metadata, decoding and ANSI stripping.
//! - [`events`] (`KeyHandler.ts`): `KeyEvent`, `PasteEvent` and the renderer's key dispatch.
//!
//! Strings follow JavaScript where the TS depends on it (UTF-16 lengths, `parseInt`, `ToInt32`; see [`js`]). The
//! one divergence: a JS string can hold a lone surrogate and a Rust one cannot, so a Kitty or modifyOtherKeys
//! sequence naming a surrogate code point gives U+FFFD where TS gives the surrogate.
//!
//! OpenTUI is MIT-licensed, Copyright (c) 2025 opentui.

pub mod events;
pub mod js;
pub mod keypress;
pub mod kitty;
pub mod mouse;
pub mod paste;
pub mod stdin;

pub use events::{KeyEvent, KeyHandler, KeyHandlerEvent, KeyHandlerPayload, ListenerId, PasteEvent};
pub use keypress::{KeyEventType, KeySource, ParsedKey, parse_keypress};
pub use mouse::{MouseEventType, MouseModifiers, MouseParser, RawMouseEvent, ScrollDirection, ScrollInfo};
pub use paste::{PasteKind, PasteMetadata, decode_paste_bytes, strip_ansi_sequences};
pub use stdin::{
    Clock, Destroyed, MouseEncoding, ProtocolContext, ProtocolContextPatch, ResponseProtocol, StdinEvent, StdinParser,
    StdinParserOptions, SystemClock,
};
