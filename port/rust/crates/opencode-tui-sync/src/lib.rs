//! The TUI's server client and data layer, ported from the TS reference (`packages/client/src` and the data contexts
//! of `packages/tui/src/context`).
//!
//! | module | ports |
//! |---|---|
//! | [`client`] | `promise/generated/client.ts`, `promise/client.ts`, `promise/media.ts`: every operation by name, errors, SSE |
//! | [`shared_events`] | `shared-events.ts`: one event stream shared by the client's subscribers |
//! | [`connection`] | `solid/connection.ts`: reconnects, timeouts, backoff and 10 ms event batches |
//! | [`data`] | `solid/data.ts`: the normalized store, events reconciled into it, cached reads, optimistic writes |
//! | [`retention`], [`usage`], [`pty`] | `solid/retention.ts`, `solid/usage.ts`, `solid/pty.ts` |
//! | [`transport`] | Bun's `fetch` (and the seam the harness scripts) |
//! | [`event`], [`bus`] | the event value; `createGlobalEmitter` (the client context's `on`/`listen`) |
//! | [`reactive`] | change notification for the TUI's reactive layer: keys, `track`/`trigger`/`batch` |
//! | [`abort`], [`runtime`], [`js`], [`json`] | `AbortController`, timers and `Date.now`, JS string semantics, JSON values |
//!
//! Everything runs on the TUI's current-thread tokio runtime, inside a `LocalSet` (state is `Rc`, background work is
//! local tasks).
//!
//! The differential harness (`harness/run.sh`) drives the TS originals (in bun, under the TUI's Solid build) and this
//! crate with the same scripted responses, events and virtual time, and compares the results.

pub mod abort;
pub mod bus;
pub mod client;
pub mod connection;
pub mod data;
pub mod event;
mod generated;
pub mod js;
pub mod pty;
pub mod json;
pub mod reactive;
pub mod retention;
pub mod runtime;
pub mod shared_events;
pub mod transport;
pub mod usage;
