# port/

Everything this fork adds to upstream opencode lives here: a Rust implementation of the opencode TUI and
headless server/CLI, and the conformance suites that check it behaves exactly like the TypeScript original.

## The one rule

**Never modify a file outside `port/`.** Upstream pulls then never conflict. The only exceptions are
brand-new files whose names upstream will never use, such as `.github/workflows/port-*.yml`.
`port/scripts/check-upstream-clean.sh` enforces this, and `sync-upstream.sh` runs it before every merge.

When upstream behavior needs a test hook (a pinned clock, for example), keep it as a patch under
`port/patches/` and apply it to a throwaway worktree of the TS reference at test time. Never commit it to
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
| `conformance/` | Scenario suite run against both implementations. Expected outputs are recorded from the TS reference, never hand-written. |
| `coverage/` | Tooling that measures how much of the in-scope TS source each test set reaches. `report.py` holds the scope definition. |
| `rust/` | The Rust implementation (its own Cargo workspace). Not created yet. |
| `scripts/` | Upstream sync, ownership check, dependency linking. |

## Working with upstream's TS packages from port/

`port/` is deliberately not a member of upstream's bun workspace, because joining it would mean editing the
root `package.json`. After `bun install` at the repo root, run `port/scripts/link-deps.sh`. It symlinks
`effect` and `@opencode/*` from `packages/server`'s node_modules into `port/conformance/node_modules`
(gitignored), so the suite loads the same module instances as the server it drives.
`conformance/smoke.test.ts` checks this works.

## Scope

In scope: the TUI and the headless CLI/server, with an exactly matching public interface.

Out of scope:
- the web app, desktop app, `packages/ui` and GUI plugins;
- plugin support;
- every provider except OpenRouter (one hardcoded access point; models are added by hand);
- `opencode mini`;
- the v1 storage migration;
- the AI SDK bridge;
- the model catalog and pricing;
- MCP;
- the legacy v1 config and schema formats.

## Coverage workflow

```sh
python3 -I port/coverage/classify.py                     # sort upstream tests by boundary
port/coverage/run.sh bb  A,B,D,E                         # tests that can run against Rust
port/coverage/run.sh all A,B,C,D,E,E-other,F,G           # full in-scope upstream suite
python3 -I port/coverage/report.py                       # compare, write out/scenario-backlog.csv
```

The scenario backlog lists the lines upstream tests reach only through TS internals. That is the work
queue for `conformance/`.
