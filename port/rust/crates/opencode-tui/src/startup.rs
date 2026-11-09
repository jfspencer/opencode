//! How the TUI applies its launch arguments (`app.tsx`, with `context/args.tsx`): `--continue` (the initial route
//! is a placeholder session until the latest session is found, else home), `--session` (an existing session opens;
//! a missing one is the first new session's ID, see `prompt`), `--fork` of either, and `--prompt`, which goes
//! into the first composer: home's (`routes/home.tsx`) or the opened session's (the route's `prompt`).
//!
//! The route's `prompt` (`route.navigate({ type: "session", sessionID, prompt })`) is [`take_route_prompt`]: the
//! session route takes it when it binds its composer.
//!
//! TODO(port-tui): `--agent` and `--model` (`local.agent.set`, `local.model.set` with the invalid-format toast);
//! no CLI flag reaches them.
use std::cell::RefCell;

use opencode_protocol::Json;
use opencode_tui_core::reactive::on_mount;
use opencode_tui_sync::client::RequestOptions;
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::obj;

use crate::ctx::{Args, Route, Tui};
use crate::prompt::history::PromptInfo;

thread_local! {
    static ROUTE_PROMPT: RefCell<Option<(String, PromptInfo)>> = const { RefCell::new(None) };
}

/// The session the TUI opens on (`initialRoute`): `--continue` holds a placeholder until the latest session is
/// found.
pub fn initial_route(args: &Args) -> Route {
    if args.continue_ {
        return Route::Session { session_id: "dummy".to_string() };
    }
    Route::Home
}

/// `startupPrompt`: `--prompt` as a composer value.
fn startup_prompt(args: &Args) -> Option<PromptInfo> {
    args.prompt
        .clone()
        .map(|text| PromptInfo { text, files: Some(vec![]), agents: Some(vec![]), skills: None, pasted: vec![], mode: None })
}

/// `route.prompt` of the session route the TUI navigated to: taken once, by that session's composer.
pub fn take_route_prompt(session_id: &str) -> Option<PromptInfo> {
    ROUTE_PROMPT.with(|p| {
        let mut p = p.borrow_mut();
        if p.as_ref().is_some_and(|(id, _)| id == session_id) {
            return p.take().map(|(_, prompt)| prompt);
        }
        None
    })
}

/// `route.navigate({ type: "session", sessionID, prompt })`.
fn navigate(t: &Tui, session_id: String, prompt: Option<PromptInfo>) {
    ROUTE_PROMPT.with(|p| *p.borrow_mut() = prompt.map(|prompt| (session_id.clone(), prompt)));
    t.route.set(Route::Session { session_id });
}

/// App's launch-argument effects, in `<App>`'s body.
pub fn install(t: &Tui) {
    let args = t.args.clone();
    let startup = startup_prompt(&args);
    {
        let t = t.clone();
        let startup = startup.clone();
        let args = args.clone();
        on_mount(move || {
            if let (Some(session), false) = (args.session_id.clone(), args.fork) {
                navigate(&t, session, startup.clone());
            }
        });
    }
    if args.continue_ {
        let t = t.clone();
        let startup = startup.clone();
        let fork = args.fork;
        let directory = t.data.directory.get_untracked().unwrap_or_else(|| {
            t.data.store.location.default().str("directory").unwrap_or_default().to_string()
        });
        let input = obj! {
            "limit" => 1.0,
            "order" => "desc",
            "parentID" => Json::Null,
            "archived" => false,
            "directory" => directory.as_str(),
        };
        let call = t.data.client.call_named("session.list", &input, RequestOptions::default());
        tokio::task::spawn_local(async move {
            let response = match call.await {
                Ok(response) => response.json(),
                Err(error) => {
                    t.toast.error(error.message());
                    t.route.set(Route::Home);
                    return;
                }
            };
            let first = crate::data::array(&response).first().and_then(|s| s.str("id")).map(str::to_string);
            // The placeholder session route waits for this: home when there is nothing to continue.
            let Some(session) = first else {
                t.route.set(Route::Home);
                return;
            };
            if !fork {
                navigate(&t, session, startup);
                return;
            }
            let call = t.data.client.call_named("session.fork", &obj! { "sessionID" => session.as_str() }, RequestOptions::default());
            match call.await {
                Ok(result) => {
                    let id = result.json().str("id").unwrap_or_default().to_string();
                    navigate(&t, id, startup);
                }
                Err(error) => {
                    t.toast.error(error.message());
                    t.route.set(Route::Home);
                }
            }
        });
    }
    if let (Some(session), true) = (args.session_id.clone(), args.fork) {
        let t = t.clone();
        let call = t.data.client.call_named("session.fork", &obj! { "sessionID" => session.as_str() }, RequestOptions::default());
        tokio::task::spawn_local(async move {
            match call.await {
                Ok(result) => {
                    let id = result.json().str("id").unwrap_or_default().to_string();
                    navigate(&t, id, startup);
                }
                Err(error) => t.toast.error(error.message()),
            }
        });
    }
}
