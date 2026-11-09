# opencode-tui-input

The TUI's input layer, ported from OpenTUI 0.8.1 (`packages/core/src/lib`) and `@opentui/keymap` 0.8.1
(`packages/keymap/src`). No dependencies.

| Module | OpenTUI 0.8.1 |
|---|---|
| `keymap` | `@opentui/keymap`: `Keymap` (layers, bindings, commands, sequences, tokens, precedence, focus targets, conditions, key intercepts, command dispatch, the pending sequence, active keys, command queries, layer slots as `useBindings` uses them); `keymap::parser` the default binding syntax; `keymap::addons` the TUI's addons (default keys, `enabled` and metadata fields, comma bindings, Escape/Backspace on a pending sequence, the base-layout fallback, the timed leader); `keymap::format` `formatKeySequence`/`formatCommandBindings` |
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

## The keymap

`Keymap::create_default(host)` is `createDefaultOpenTuiKeymap`; then register what `context/keymap.tsx` registers:
`addons::register_comma_bindings`, the TUI's own alias expander (`append_binding_expander`),
`addons::register_base_layout_fallback`, `register_escape_clears_pending_sequence(&keymap, true, 0)`,
`register_backspace_pops_pending_sequence(&keymap, true, 0)`, `register_timed_leader(&keymap, &leader, Some("leader"),
Some(timeout))`, and a `mode` layer field (`register_layer_fields`) that calls `ctx.require("opencode.mode", value)`,
with `set_data("opencode.mode", ...)`. `Keymap.createLayer`/`useBindings` is one `create_layer_slot()` per component
and `replace_layer(&slot, Some(layer))` on each reactive run: a layer whose data did not change keeps its place and a
pending sequence through it. The host does what OpenTUI's host does with listeners: `handle_key_event` from the
renderer's key listener (first), `handle_focus_change`, `handle_target_destroyed`, and timers through
`KeymapHost::set_timeout` for the timed leader.

Queries: `get_pending_sequence`, `get_active_keys`, `get_commands`, `get_command_entries`, `get_command_bindings`,
`dispatch_command`, `run_command`, `create_key_matcher`, `parse_key_sequence`, `on_state` (what `useKeymapSelector`
subscribes to), `on_pending_sequence`. Not ported (the TUI does not use them): sequence patterns, disambiguation and
command resolvers, transformers, layer analyzers, `key:after`/`raw` intercepts, reactive matchers, query filters, and
`registerManagedTextareaLayer` (it belongs with the textarea renderable). A destroyed host makes queries empty where TS
throws; a matcher, field compiler, expander or parser may not mutate the keymap.

`harness/keymap-oracle.ts` configures the reference's `@opentui/keymap` as the TUI does on a test host (a target tree,
fake timers) and generates random layers (binding strings from valid and invalid syntax, commands whose handlers
handle, reject, return results, clear the sequence, change the mode or dispatch, targets and target modes, priorities,
`enabled` conditions, modes, metadata) and random presses and releases (with base-layout codes), focus changes,
condition and data changes, slot replacements (kept and not), unregistrations, target destruction, timer advances,
command dispatch and queries; `examples/keymap-driver.rs` replays them. Every command run (with its input, target,
focus and event), event flag, pending sequence, pending-sequence event, active key, command and binding query,
formatted shortcut, error and warning is compared.

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
