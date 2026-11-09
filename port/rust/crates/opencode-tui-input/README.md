# opencode-tui-input

The TUI's input layer, ported from OpenTUI 0.8.1 (`packages/core/src/lib`). No dependencies.

| Module | OpenTUI 0.8.1 |
|---|---|
| `stdin` | `stdin-parser.ts`: `StdinParser`, stdin bytes to `StdinEvent`s (key, mouse, paste, response), the ESC timeout, the protocol context the renderer updates (Kitty keyboard, pending capability replies, CPRs, pixel-resolution reply), `abortPendingStartupCursorCpr`, paused timeouts, the pending-byte cap |
| `keypress` | `parse.keypress.ts`: `parseKeypress`, `ParsedKey`, `nonAlphanumericKeys`, `terminalNamedSingleStrokeKeys` |
| `kitty` | `parse.keypress-kitty.ts`: the Kitty keyboard protocol, `kittyKeyMap`, `printableKeypadText` |
| `mouse` | `parse.mouse.ts`: `MouseParser` (SGR and X10, drag tracking) |
| `paste` | `paste.ts`: `PasteMetadata`, `decodePasteBytes`, `stripAnsiSequences` |
| `events` | `KeyHandler.ts`: `KeyEvent`, `PasteEvent`, the renderer's `InternalKeyHandler` dispatch |
| `js` | the JS semantics the TS relies on: UTF-16 lengths, `parseInt`, `ToInt32`, `TextDecoder` (BOM dropped), latin1 |

The renderer uses this TS parser: the native `input-parser.zig` is behind `OTUI_NATIVE_INPUT_PARSER` /
`experimental_nativeInput`, which the TUI does not set. The TUI makes it with `timeoutMs: 20`,
`useKittyKeyboard: true` and the Kitty context on.

## Using it

```rust
use opencode_tui_input::{StdinEvent, StdinParser, StdinParserOptions};

let mut parser = StdinParser::new(StdinParserOptions { timeout_ms: Some(20.0), ..Default::default() });
parser.push(&bytes)?;                       // each stdin read
while let Some(event) = parser.read()? {    // the renderer's drain
    match event {
        StdinEvent::Key { key, .. } => { /* KeyHandler::process_parsed_key(key) */ }
        StdinEvent::Mouse { event, .. } => { /* hit testing */ }
        StdinEvent::Paste { bytes, .. } => { /* KeyHandler::process_paste */ }
        StdinEvent::Response { protocol, sequence } => { /* focus, capability replies, OSC subscribers */ }
    }
}
// Timer: after any call, parser.timeout_deadline() is when OpenTUI's setTimeout would fire (on the parser's
// Clock, ms). When it passes, call parser.on_timeout() and drain again (the renderer's onTimeoutFlush).
```

Focus reports, DA, DECRQM, the Kitty flags reply, color-scheme reports, CPRs, window and pixel size reports,
XTVERSION, OSC colors and Kitty graphics replies are `StdinEvent::Response`s with the protocol the framing found, as in
TS; the renderer (`renderer.ts`, `terminal-capability-detection.ts`) interprets them.

Divergences: a call on a destroyed parser returns `Destroyed` where TS throws the same message; a lone surrogate
that a Kitty or modifyOtherKeys sequence names becomes U+FFFD (a Rust `String` cannot hold one).

## Differential check

```sh
port/rust/crates/opencode-tui-input/harness/run.sh                         # unit tests, 148 hand-picked + 20000 random programs
port/rust/crates/opencode-tui-input/harness/run.sh --count 40000 --seed 2  # a deeper run
```

`harness/oracle.ts` builds the reference's `StdinParser` as the renderer does, on a manual clock, and feeds it
random terminal byte streams (keys in every encoding, Kitty, modifyOtherKeys, SGR and X10 mouse, bracketed paste,
focus, terminal replies, invalid UTF-8, junk) cut at random points, with time passing between reads (around the
20 ms timeout) and the protocol context changing; plus direct calls of `parseKeypress`, `MouseParser`,
`stripAnsiSequences` and `decodePasteBytes`. The hand-picked programs are OpenTUI's own conformance vectors
(`stdin-parser.conformance.json`, whole and one byte per read) and timer and context cases. `examples/input-driver.rs`
replays them; every event (all `ParsedKey` fields, mouse fields as f64 bits, paste bytes, response sequences), timer
firing time, error and query answer is compared.

## Licence

OpenTUI: MIT, Copyright (c) 2025 opentui (`port/.vendor/opentui/LICENSE`).
