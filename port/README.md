# port/

Everything this fork adds to upstream opencode lives here: a Rust implementation of the opencode TUI and
headless server/CLI, and the conformance suites that check it behaves exactly like the TypeScript original.

## The one rule

**Never modify a file outside `port/`.** Upstream pulls then never conflict. The only exceptions are
brand-new files whose names upstream will never use, such as `.github/workflows/port-*.yml`.
`port/scripts/check-upstream-clean.sh` enforces this, and `sync-upstream.sh` runs it before every merge.

Changes to upstream code (fixes for upstream bugs the port must not reproduce) live as patches in
`port/patches/`, applied to a separate worktree of upstream (`port/.reference`). Never commit them to
upstream paths.

## Remotes

| Remote | Points at | Use |
|---|---|---|
| `origin` | `github.com/jfspencer/opencode` | push here |
| `upstream` | `github.com/anomalyco/opencode` | fetch only (push URL disabled) |

The default branch is `dev`, as upstream's is. Sync with `port/scripts/sync-upstream.sh`: it merges
`upstream/dev` into `dev` (merge, never rebase, so `origin` never needs a force-push) and prints the
upstream range to run through the conformance loop.

## Layout

| Path | What |
|---|---|
| `conformance/` | The conformance oracle: scenarios (HTTP API, TUI, CLI) recorded from the TS reference, never hand-written. See `conformance/README.md` and `conformance/FINDINGS.md`. |
| `patches/` | Deviation patches: fixes for upstream bugs, applied to the TS reference so the standard requires the fixed behavior. See `patches/README.md`. |
| `.reference/` | (gitignored) The TS reference: a sparse worktree of upstream plus the deviation patches, built by `scripts/reference.sh`. |
| `coverage/` | Tooling that measures how much of the in-scope TS source each test set reaches. `report.py` holds the scope definition. |
| `rust/` | The Rust implementation (its own Cargo workspace). Not created yet. |
| `scripts/` | Upstream sync, ownership check, building the reference, dependency linking. |

## Working with upstream's TS packages from port/

`port/` is deliberately not a member of upstream's bun workspace, because joining it would mean editing the
root `package.json`. Instead, `port/scripts/reference.sh build` creates the TS reference (upstream plus
`port/patches/`, with its own `bun install`, which takes seconds because bun hardlinks its cache). It then
runs `port/scripts/link-deps.sh`, which points `port/conformance/ref` at the reference and symlinks
`effect`, `@opencode/*` and the TUI's dependencies from the reference's node_modules. The suite then loads
the same module instances as the server it drives. To compare against plain upstream, run
`port/scripts/link-deps.sh .` after `bun install` at the repo root.

## Scope

In scope: the TUI and the headless CLI/server, with an exactly matching public interface.

Out of scope:
- the web app, desktop app, `packages/ui` and GUI plugins;
- plugin support;
- every provider except OpenRouter (one hardcoded access point; models are added by hand);
- `opencode mini`;
- the v1 storage migration and the legacy v1 config and schema formats;
- the AI SDK bridge;
- the model catalog and pricing;
- MCP, websearch and image generation (their tools are hidden from the model in the reference config);
- fff file search (file search is ripgrep + fuzzysort, as with `OPENCODE_DISABLE_FFF=1`);
- experimental flags, which are off in release builds (e.g. the code-mode bytecode VM).

Known upstream bugs are not ported: each is fixed by a deviation patch (`patches/`), so the standard
requires the fixed behavior. Text written by the JavaScript engine itself (JSON and regular-expression
parser diagnostics, built-in range errors) is not ported either: the standard leaves its wording open.

Dropped features keep the API endpoints the TUI calls, answered as the TS reference does with nothing
configured (see `conformance/README.md`). The coverage scope is defined in `coverage/report.py`.

## Coverage workflow

```sh
python3 -I port/coverage/classify.py                     # sort upstream tests by boundary
port/coverage/run.sh bb  A,B,D,E                         # tests that can run against Rust
port/coverage/run.sh all A,B,C,D,E,E-other,F,G           # full in-scope upstream suite
python3 -I port/coverage/report.py                       # compare, write out/scenario-backlog.csv
```

The scenario backlog lists the lines upstream tests reach only through TS internals. That is the work
queue for `conformance/`.
