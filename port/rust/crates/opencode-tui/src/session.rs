//! Submitting from the home prompt (`component/prompt/index.tsx` `submit` without a session).
//!
//! TODO(port-tui): the session feature slice: creating the session (with `--session`'s new ID), selecting its
//! model and agent, sending the prompt, and the session route (`routes/session`) with its transcript.
use crate::ctx::Tui;

pub fn submit_from_home(t: &Tui, value: String) {
    let _ = (t, value);
}
