//! Wire types of opencode's public interface: every HTTP API endpoint (with a route table), the SSE event stream
//! and the v2 config file (`opencode.json`).
//!
//! The types in [`types`], [`routes`] and [`events`] are generated from the TS reference's Effect schemas by
//! `port/rust/gen` (run `port/scripts/gen-types.sh`); never edit them by hand. [`wire`] is the hand-written runtime
//! they build on. Read JSON with [`wire::from_str`] and write it with [`wire::to_string`]: the output is
//! byte-identical to `JSON.stringify` of the TS value.

pub mod wire;

#[path = "generated/types.rs"]
pub mod types;

#[path = "generated/routes.rs"]
pub mod routes;

#[path = "generated/events.rs"]
pub mod events;

#[path = "generated/roots.rs"]
pub mod roots;

pub use wire::{Json, Wire, from_str, to_string};
