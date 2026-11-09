//! The opencode executable (packages/cli): see `cli`.

mod api;
mod app;
mod auth;
mod bus;
mod cli;
mod collate;
mod cors;
mod db;
mod file_access;
mod form;
mod flag;
mod formatter;
mod fs;
mod fuzzysort;
mod generate;
mod handlers;
mod instruction_discovery;
mod inventory;
mod http;
mod id;
mod jsdiff;
mod job;
mod json;
mod llm;
mod location;
mod permission;
mod process;
mod project;
mod project_icon;
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
