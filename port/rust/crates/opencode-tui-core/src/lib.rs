//! OpenTUI 0.8.1 (`@opentui/core`, its Zig core and `@opentui/solid`) for the opencode TUI.
//!
//! - [`color`], [`border`], [`buffer`]: `lib/RGBA.ts`, `lib/border.ts` and `buffer.zig` (cells, alpha blending,
//!   text, borders, scissor rects, opacity).
//! - [`output`]: `renderer.zig`, `renderer-output.zig`, `stdout-writer.zig` (diffed frames, cursor, hit grid).
//! - [`terminal`]: `session.zig`, `terminal.zig`, `lib/terminal-capability-detection.ts` (setup, queries and their
//!   replies, modes, teardown), without I/O: the renderer writes what it returns.
//! - [`reactive`]: Solid 1.9's reactive core (signals, memos, effects, owners, contexts, batch, stores).
pub mod border;
pub mod buffer;
pub mod layout;
pub mod color;
pub mod output;
pub mod pool;
pub mod reactive;
pub mod terminal;
pub mod unicode;
