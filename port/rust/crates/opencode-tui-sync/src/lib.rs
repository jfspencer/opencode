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
//! | [`context`] | the TUI's data contexts: `client.tsx`, `data.tsx`, `event.ts`, `location.tsx`, `args.tsx` |
//! | [`transport`] | Bun's `fetch` (and the seam the harness scripts) |
//! | [`event`], [`bus`] | the event value; `createGlobalEmitter` (the client context's `on`/`listen`) |
//! | [`reactive`] | change notification for the TUI's reactive layer: keys, `track`/`trigger`/`batch`/`untrack` |
//! | [`abort`], [`runtime`], [`js`], [`json`] | `AbortController`, timers and `Date.now`, JS string semantics, JSON values |
//!
//! # Using it from the app
//!
//! Everything runs on the TUI's current-thread tokio runtime, inside a `LocalSet` (state is `Rc`, background work is
//! local tasks).
//!
//! **Providers.** The app builds the contexts in the TS provider order and keeps them where its components find
//! them: `ClientContext::new(ClientInput { api: Client::make(..), url, headers, service, log, reactivity })`, then
//! `DataContext::new(&client, directory)`, `LocationContext::new(&client, &data)`, `EventContext::new(&client)`,
//! `Args::new(..)`. `reactivity` is the app's reactive layer (an implementation of [`reactive::Reactivity`]).
//!
//! **Reading state.** Accessors keep the TS paths: `data.session.get(id)`, `data.session.message.list(id)`,
//! `data.session.status(id)`, `data.location.model.list(ref)`, `location.current()`, `client.connection().status()`.
//! Records are the server's JSON (`Rc<Json>`, TS field names): `info.str("title")`, `info.at("time.updated")`,
//! `message.arr("content")` ([`json::JsonExt`]). A read reports its [`reactive::Key`] to `Reactivity::track`, so a
//! computation that reads subscribes; a change calls `Reactivity::trigger` with the keys it changed (a key's readers,
//! its ancestors' and its descendants' are concerned). Finer reads exist where the TS UI reads fields of a record:
//! `session.message.ids(id)` (the transcript's shape), `session.message.part(id, message, index)`, and
//! `data.track(key)` for any key. `data.untrack(|| ..)` reads without subscribing.
//!
//! **Calling operations.** Data-layer operations return a [`data::Promise`] (`.wait().await`; the request has
//! started when the call returns): `data.session.prompt(PromptInput { request: obj! { "sessionID" => id, "text" =>
//! text }, .. })`, `data.session.sync(id, false)`, `data.location.sync(None)`. Server operations the TUI calls
//! directly go through the client with the TS input object: `client.api().api().session().update(&obj! {
//! "sessionID" => id, "title" => "" }, RequestOptions::default()).await` resolves to a [`client::Value`]; errors
//! carry the TS names and messages (`error.is("SessionNotFoundError")`, `error.message()`).
//!
//! **Events.** `client.event().on("session.deleted", |event| ..)` and `EventContext::subscribe` deliver
//! [`event::Event`]s (`event.data().str("sessionID")`); the connection delivers each 10 ms flush inside
//! `Reactivity::batch`.
//!
//! # Harness
//!
//! `harness/run.sh` drives the TS originals (in bun, under the TUI's Solid build) and this crate with the same
//! scripted responses, events and virtual time, and compares the results; `harness/record/run.sh` records the data
//! layer's inputs in real TUI scenario runs for it to replay.

pub mod abort;
pub mod bus;
pub mod client;
pub mod connection;
pub mod context;
pub mod data;
pub mod event;
mod generated;
pub mod js;
pub mod json;
pub mod pty;
pub mod reactive;
pub mod retention;
pub mod runtime;
pub mod shared_events;
pub mod transport;
pub mod usage;
