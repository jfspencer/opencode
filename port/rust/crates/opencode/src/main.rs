//! The opencode executable (packages/cli): see `cli`.

mod api;
mod app;
mod auth;
mod bus;
mod cli;
mod codemode;
mod collate;
mod cors;
mod credential;
mod db;
mod file_access;
mod form;
mod flag;
mod formatter;
mod fs;
mod fuzzysort;
mod generate;
mod npm;
mod handlers;
mod instruction_discovery;
mod integration;
mod inventory;
mod http;
mod id;
mod jsdiff;
mod job;
mod json;
mod llm;
mod location;
mod location_activity;
mod location_watch;
mod permission;
mod persistent_pty;
mod process;
mod project;
mod project_icon;
mod repository_cache;
mod pty;
mod schedule;
mod session;
mod shell;
mod snapshot;
mod text;
mod tool;
mod url;
mod vcs;
mod watch;
mod worktree;
mod zone;

fn main() {
    cli::main()
}
