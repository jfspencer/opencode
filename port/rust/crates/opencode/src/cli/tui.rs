//! The TUI of the default command (`packages/cli/src/commands/handlers/default.ts`): the server it talks to (an
//! explicit `--server`, a private one with `--standalone`, or the background service), whether `--session` names
//! an existing session (else it becomes the first prompt's new session ID), the TUI arguments, and the run.
//!
//! TODO(port): the update preflight and updater (`OPENCODE_DISABLE_AUTOUPDATE` is the only mode the standard
//! exercises), the npm package source for plugins (dropped), the session environment forwarded to a local server.
use super::client::ClientError;
use super::connection;
use super::out::{self, Fail};
use super::parse::{Globals, Input};

async fn session_exists(client: &super::client::Client, id: &str) -> Result<bool, ClientError> {
    match client.call("session.get", &[("sessionID", id)], None, None).await {
        Ok(_) => Ok(true),
        Err(error) if error.tag() == Some("SessionNotFoundError") => Ok(false),
        Err(error) => Err(error),
    }
}

pub async fn run(input: &Input, globals: &Globals) -> out::Result {
    // The root command's own --server/--standalone (handlers/default.ts `input.server`, `input.standalone`).
    let requested_server = input.str("server").map(str::to_string).or_else(|| globals.server.clone());
    let standalone = input.flag("standalone") || globals.standalone;
    let server = connection::resolve(
        globals,
        connection::Args { server: requested_server.clone(), standalone: Some(standalone), check_version: true, ..Default::default() },
    )
    .await?;
    let session = input.str("session").map(str::to_string);
    let exists = match &session {
        Some(id) => session_exists(&server.client(), id).await.map_err(|error| Fail::error(error.message()))?,
        None => false,
    };
    let args = opencode_tui::ctx::Args {
        continue_: input.flag("continue"),
        session_id: session.clone().filter(|_| exists),
        new_session_id: session.filter(|_| !exists),
        prompt: input.str("prompt").map(str::to_string),
        auto: input.flag("auto") || input.flag("yolo") || input.flag("dangerously-skip-permissions"),
        ..Default::default()
    };
    let tui = opencode_tui::app::TuiInput {
        app: opencode_tui::ctx::AppInfo {
            name: std::env::var("OPENCODE_CLIENT").unwrap_or_else(|_| "cli".to_string()),
            version: crate::app::VERSION.to_string(),
            channel: std::env::var("OPENCODE_TUI_CHANNEL").unwrap_or_else(|_| crate::app::CHANNEL.to_string()),
        },
        url: server.endpoint.url.clone(),
        headers: server.endpoint.headers(),
        args,
        local: requested_server.is_none(),
    };
    let code = tokio::task::LocalSet::new().run_until(opencode_tui::app::run(tui)).await;
    server.close().await;
    Ok(code)
}
