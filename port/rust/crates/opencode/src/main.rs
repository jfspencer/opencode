//! The opencode executable (packages/cli): see `cli`.

mod api;
mod app;
mod auth;
mod bus;
mod cli;
mod collate;
mod cors;
mod db;
mod fs;
mod fuzzysort;
mod generate;
mod handlers;
mod inventory;
mod http;
mod id;
mod job;
mod json;
mod llm;
mod location;
mod permission;
mod process;
mod project;
mod project_icon;
mod session;
mod snapshot;
mod text;
mod tool;
mod vcs;
mod worktree;

fn main() {
    cli::main()
}
