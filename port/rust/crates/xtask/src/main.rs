//! `cargo xtask`: portable development tooling for the opencode Rust port.
//!
//! Run it from `port/rust` (the cargo alias) or through the thin wrappers under `port/scripts` and
//! `port/coverage`. The commands keep the arguments, environment and exit codes of the shell scripts they
//! replace; the logic lives in Rust so macOS and Linux do not need GNU coreutils.

mod archive;
mod junit;
mod process;
mod reference;
mod scripts;
mod suites;
mod vendor;

use std::path::PathBuf;
use std::process::ExitCode;

use process::{Ctx, Error, Result};

const USAGE: &str = "\
usage: cargo xtask <command> [options]

Setup and vendor:
  check-upstream-clean                 fail if the branch changes an upstream-owned path
  vendor-grammars [destination]        fetch and verify the tree-sitter grammars
  vendor-opentui                       fetch the OpenTUI release the TUI harnesses use
  worktree-setup                       link a linked worktree to the main checkout's reference
  link-deps [reference-root]           point port/conformance at a TS reference
  gen-types [generate|check [opts]]    generate or verify the Rust wire types
  opencode-pty [--source] [--install]  provide the persistent-terminal daemon
  check-macos                          cargo check both Apple targets
  sync-upstream                        merge upstream/dev (never pushes)
  reference <build|save|amend|export|status>  maintain the TS reference worktree

Suites and coverage:
  upstream-tests <label> [tree]        run upstream's in-scope tests
  upstream-tests compare <a> <b>       tests failing in b but not a
  determinism [--record] [runs] [shards]
  oracle [files...]                    run conformance scenarios with coverage
  oracle-sharded [label] [shards]
  coverage-run <label> <category,...>  run upstream files with coverage
  rust-conformance [options] [files]   full sharded runner, JUnit parsing and ledger update

The commands still invoke bun, cargo, git and curl. The reference and rust-conformance implementations are
complete; their port/scripts wrappers are not switched yet. The differential harness scripts under
port/rust/crates/*/harness remain shell for now.
";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if !error.to_string().is_empty() {
                eprintln!("xtask: {error}");
            }
            ExitCode::from(error.exit_code() as u8)
        }
    }
}

fn run(arguments: &[String]) -> Result<()> {
    let Some(command) = arguments.first().map(String::as_str) else {
        print!("{USAGE}");
        return Ok(());
    };
    if matches!(command, "help" | "--help" | "-h") {
        print!("{USAGE}");
        return Ok(());
    }
    let args = &arguments[1..];
    let ctx = Ctx::new()?;
    match command {
        "check-upstream-clean" => scripts::check_upstream_clean(&ctx),
        "vendor-grammars" => vendor::vendor_grammars(&ctx, args.first().map(PathBuf::from)),
        "vendor-opentui" => vendor::vendor_opentui(&ctx),
        "worktree-setup" => scripts::worktree_setup(&ctx),
        "link-deps" => scripts::link_deps(&ctx, args.first().map(PathBuf::from)),
        "gen-types" => scripts::gen_types(&ctx, args),
        "opencode-pty" => {
            let binary = scripts::opencode_pty(&ctx, args)?;
            println!("{}", binary.display());
            Ok(())
        }
        "check-macos" => scripts::check_macos(&ctx),
        "sync-upstream" => scripts::sync_upstream(&ctx),
        "upstream-tests" => suites::upstream_tests(&ctx, args),
        "determinism" => suites::determinism(&ctx, args),
        "oracle" => suites::oracle(&ctx, args),
        "oracle-sharded" => suites::oracle_sharded(&ctx, args),
        "coverage-run" => suites::coverage_run(&ctx, args),
        "rust-conformance" => suites::rust_conformance(&ctx, args),
        "reference" => reference::run(&ctx, args),
        other => Err(Error::usage(format!("unknown command {other}\n\n{USAGE}"))),
    }
}
