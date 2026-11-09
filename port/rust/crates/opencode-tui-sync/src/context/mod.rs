//! The TUI's data contexts (packages/tui/src/context), as plain values the app's providers hold:
//!
//! | module | ports |
//! |---|---|
//! | [`client`] | `client.tsx` (and `util/media.ts`): the client, its event bus, the connection, media bytes |
//! | [`data`] | `data.tsx`: the data layer over the client, `location.sync` and `session.title` |
//! | [`event`] | `event.ts`: `useEvent()`, events with their directory |
//! | [`location`] | `location.tsx` (and `@opencode/util/retry`): the location the TUI is in, synced with retries |
//! | [`args`] | `args.tsx`: the command line's arguments |
//!
//! Not here, by their owners' slices: `storage.tsx` and `session-terminals.tsx` (the tabs/terminal slice), and the
//! UI contexts (route, local, theme, keymap, dialogs, ...), which read these.

pub mod args;
pub mod client;
pub mod data;
pub mod event;
pub mod location;
pub mod retry;
