# xtask

Portable development tooling for the Rust port. The implementation is compiled Rust, so macOS and Linux take
the same code paths and do not need GNU `realpath`, `timeout`, `stat`, `sed -i`, `tac`, or a particular `tar`.

```sh
cd port/rust
cargo build -p xtask          # once; the `xtask` alias is in port/rust/.cargo/config.toml
cargo xtask check-upstream-clean
cargo xtask vendor-grammars /tmp/grammars
cargo xtask reference status
cargo xtask rust-conformance scenarios/session-api-crud.test.ts
cargo xtask --help
```

Cargo discovers aliases from the current directory upward, so from the repository root use the wrappers
(`port/scripts/...`) or `(cd port/rust && cargo xtask ...)`; a repository-root `.cargo/config.toml` would be
an upstream-owned file.

The old `port/scripts/*.sh` paths remain as thin `exec` wrappers. They run `port/rust/target/debug/xtask`
when it exists, otherwise they fall back to one `cargo run --quiet -p xtask -- <command>`. `link-deps.sh` is
still the shell fallback while the reference is being repaired, and the `reference.sh` / `rust-conformance.sh`
wrappers are not switched yet: both commands are complete in xtask, so each wrapper can become the same
three-line `exec` wrapper used by the other scripts.

## Ported commands

| Command | Arguments | Environment | Exit | Side effects |
|---|---|---|---|---|
| `check-upstream-clean` | none | git remotes/branch | 0 clean, 1 violations | none |
| `vendor-grammars` | `[destination]` | network | 0/1 | downloads into `<destination>/.archives`, writes sources |
| `vendor-opentui` | none | network, git | 0/1 | `port/.vendor/opentui` |
| `worktree-setup` | none (run in a linked worktree) | git | 0/1 | symlinks in the worktree; appends `/port/.reference`, `/port/.vendor` to the main `.git/info/exclude` |
| `link-deps` | `[reference-root]` | none | 0/1 | rewrites `port/conformance/ref` and `port/conformance/node_modules` |
| `gen-types` | `[generate\|check [opts]]`; `--samples N --mutants N --seed N --only Type --verbose` | `REFERENCE` | 0/1 | generated Rust files; links under `port/rust/gen/node_modules`; cargo build/test on `check` |
| `opencode-pty` | `[--source] [--install]` | `PATH` (Zig for `--source`) | 0/1, 2 bad args | `port/rust/target/opencode-pty/<version>/opencode-pty`; release build with `--install` |
| `check-macos` | none | rustup targets, clang | 0/1 | `port/rust/target` (and `target/macos-stub-cc` when cross-checking from Linux) |
| `sync-upstream` | none | git | 0/1 | fetches and merges `upstream/dev`; never pushes |
| `upstream-tests` | `<label> [tree]` or `compare <a> <b>` | none | 0 unless the tooling fails | `port/coverage/out/upstream-<label>/` |
| `determinism` | `[--record] [runs=3] [shards=6]` | `UPDATE_GOLDENS`, `CONFORMANCE_STRICT` | 0/1 | temp logs; re-records goldens with `--record` |
| `oracle` | `[files...]`; `tui/<file>` for TUI | `LABEL` | 0 unless bun cannot start | `port/coverage/out/<label>-*` |
| `oracle-sharded` | `[label=oracle] [shards=8]` | as `oracle` per shard | 0 unless bun cannot start | sharded coverage output |
| `coverage-run` | `<label> <category,...>` | none | 0 unless the tooling fails | `port/coverage/out/<label>-*` |
| `rust-conformance` | `[--update] [--release] [shards] [files...]` | `OPENCODE_VERSION`, `OPENCODE_CHANNEL`, pty embed | 0/1 | vendored sources, `port/rust/target/debug|release/opencode`, per-shard JUnit logs, ledger update |
| `reference` | `build`, `save "D123 ..." [body]`, `amend D123`, `export`, `status` | `REFERENCE`; pinned git identity | 0/1 | creates/updates `port/.reference`, `port/patches/`, `bun install`, link-deps |

`gen-types generate` and `gen-types check` are the only modes; an unknown mode is a usage error.

## Wrappers not switched

`port/scripts/reference.sh`, `port/scripts/rust-conformance.sh` and `port/scripts/link-deps.sh` still contain
their shell implementations. The first two are fully ported; when the active reference/TUI work settles they
should become the same wrapper as the other scripts:

```sh
root=$(git rev-parse --show-toplevel)
bin=$root/port/rust/target/debug/xtask
if [ -x "$bin" ]; then exec "$bin" reference "$@"; fi
exec cargo run --quiet --manifest-path "$root/port/rust/Cargo.toml" -p xtask -- reference "$@"
```

(`reference`, `rust-conformance`, and `link-deps` respectively.) Until then, `cargo xtask reference` and
`cargo xtask rust-conformance` are the complete implementations and produce the same reports as the scripts.

## Scripts still to port

These remain shell because they are thin bun/cargo differential harnesses. Their arguments and side effects
are unchanged; the macOS portability fixes already applied are listed in the main report.

| Script | Arguments | Environment | Exit | Side effects |
|---|---|---|---|---|
| `port/scripts/link-deps.sh` | `[reference-root]` | none | 0/1 | same as `cargo xtask link-deps`; kept while the reference is repaired |
| `port/rust/crates/opencode-codemode/harness/check.sh` | `[corpus...] [--show N]` | `TZ=UTC` | 0/1 | `target/release/examples`, temp JSONL |
| `.../codemode/harness/jsdate/check.sh` | `[scale] [zone...]` | `SEED`, `TZ` | 0/1/2 | `target/release/examples` |
| `.../codemode/harness/jsdate/sweep.sh` | `[scale]` | `TZ` | 0/2 | `target/release/examples` |
| `.../codemode/harness/jsregex/check.sh` | `[scale] [shown]` | none | 0/1 | `target/release/examples` |
| `.../codemode/harness/numconv/check.sh` | `[count] [seed] [fn...]` | `NUMCONV_PROFILE`, `TMPDIR` | 0/1 | temp reports, cargo target |
| `.../codemode/harness/parser/check.sh` | `[corpus.jsonl]` | `WORK`, `SHOWN`, `REAL` | 0/1 | temp corpus/dumps |
| `.../opencode-html/harness/run.sh` | `[--coverage] [--count N] [--seed S] [--show N]` | none | 0/1 | `target/opencode-html` |
| `.../opencode-shell/harness/check.sh` | `[driver options]` | none | 0/1 | cargo target |
| `.../opencode-tui-core/harness/run.sh` | `[buffer cases]` | none | 0/1 | `target/tui-core-harness` |
| `.../opencode-tui-core/harness/reactive/run.sh` | `[count] [seed]` | `OPENCODE_REFERENCE` | 0/1 | `target/opencode-tui-core/reactive` |
| `.../opencode-tui-input/harness/run.sh` | `[--count N] [--seed S] [--show N]` | none | 0/1/2 | `target/opencode-tui-input` |
| `.../opencode-tui-markup/harness/run.sh` | `[--count N] [--seed S] [--show N] [--only corpus]` | none | 0/1 | `target/opencode-tui-markup` |
| `.../opencode-tui-sync/harness/run.sh` | `[--seed N] [--count N] [--only ...] [--show N] [--cases ...]` | none | 0/1 | `target/opencode-tui-sync` |
| `.../opencode-tui-sync/harness/record/run.sh` | `<out dir> <scenario file>...` | — | 0/1 | recording JSONL/logs |
| `.../opencode-tui-sync/harness/record/opencode.sh` | `serve ...` or TUI args | `SYNC_RECORD_*` | 0/1 | recording files |
| `.../opencode-tui-sync/harness/smoke.sh` | none | none | 0/1 | temp sandbox |
| `.../opencode-tui-text/harness/run.sh` | `[--count N] [--seed S] [--show N] [--no-tables] [--case I]` | none | 0/1 | `target/opencode-tui-text` |
| `.../opencode-tui/harness/run.sh` | `[--count N] [--seed S] [--show N] [--only theme\|config]` | none | 0/1 | `target/opencode-tui` |
| `.../opencode-yoga/harness/run.sh` | `[--count N] [--seed S] [--show N]` | none | 0/1/2 | `target/opencode-yoga` |
| `.../opencode/harness/collate/run.sh` | `[--count N] [--seed S] [--show N]` | none | 0/1 | `target/collate` |
| `.../opencode/harness/file-tools/run.sh` | `[cases] [seed] [read cases]` | none | 0/1 | temp JSON; cargo tests |
| `.../opencode/harness/frontmatter/run.sh` | `[cases] [seed]` | `TZ=UTC` | 0/1 | temp JSON; cargo tests |
| `.../opencode/harness/images/run.sh` | `[cases] [seed]` | none | 0/1 | temp JSON; cargo tests |
| `.../opencode/harness/jsonc/run.sh` | `[cases] [seed]` | none | 0/1 | temp JSON; cargo tests |
| `.../opencode/harness/npm/run.sh` | `[semver cases] [seed] [install cases]` | none | 0/1 | temp JSON; cargo tests |
| `.../opencode/harness/patch/run.sh` | `[cases] [seed]` | none | 0/1 | temp JSON; cargo tests |
| `.../opencode/harness/restart/run.sh` | none | `SERVE_FLAGS` | 0/1 | temp sandbox and DB |
| `.../opencode/harness/tool-inputs/run.sh` | `[cases] [seed]` | `TOOL_INPUT_KEEP` | 0/1 | temp JSON; cargo tests |

The Python tools under `port/coverage` (`classify.py`, `report.py`, `uncovered.py`) are not shell scripts and
remain Python; they already use `python3` and portable paths.

## Notes

- Archive extraction uses the `tar` and `flate2` crates in this crate only. It handles the tree-sitter release
  archives' duplicate hardlink entries (`./src/foo` and `src/foo`) without bsdtar.
- `timeout_status` implements the 124 exit code of GNU `timeout` with a poll loop, so `determinism`, `oracle`,
  `coverage-run` and `upstream-tests` keep their limits on macOS.
- Relative symlinks are computed in Rust (`process::relative`), replacing `realpath --relative-to`.
- Checksums use `sha2` in Rust, replacing `sha256sum`.
- The wrappers never build on every invocation: build once, or accept the single `cargo run` fallback.
