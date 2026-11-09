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
| `rust/` | The Rust implementation: a Cargo workspace (`crates/*`) and the generator of its wire types and database bootstrap (`gen/`). See [Rust wire types](#rust-wire-types) and [Rust server](#rust-server). |
| `scripts/` | Upstream sync, ownership check, building the reference, dependency linking, generating the wire types, running the standard against the Rust build. |

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
- experimental flags, which are off in release builds (e.g. the code-mode bytecode VM);
- Windows (the port targets Linux and macOS; TS's `win32` branches are not ported).

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

## Rust wire types

`rust/crates/opencode-protocol` holds Rust types for the whole public wire format: every HttpApi endpoint (path
parameters, query, headers, payloads, successes and errors by status, plus a route table in `routes.rs`), the SSE
event union and every event definition (`events.rs`), and the v2 config file. `src/generated/` is generated from
the TS reference's Effect schemas by `rust/gen` (TS, run with bun): it imports `@opencode/protocol` and
`@opencode/schema` from `.reference` and walks the Effect Schema AST on the encoded side. Never edit it; the
hand-written runtime it builds on is `src/wire/`. `rust/crates/protocol-check` is the round-trip harness.

```sh
port/scripts/reference.sh build      # after every upstream merge or patch change
port/scripts/gen-types.sh            # regenerate; review the diff of src/generated/
port/scripts/gen-types.sh check      # up to date? cargo build + tests; property round-trip against TS
port/scripts/gen-types.sh check --samples 60 --mutants 4 --seed 3   # a deeper run
```

`check` is the proof: TS generates random valid values for each of the ~450 root types (Effect's `Arbitrary`,
fixed seed), Rust decodes and re-encodes them, and the text must be byte-identical to TS's
`JSON.stringify(encode(decode(JSON.parse(text))))`. Mutants of each value (wrong types, missing and extra keys,
broken strings and numbers) must be rejected by Rust wherever TS rejects them, with the same message, and written
identically wherever TS accepts them; hand-picked edge cases (the conformance reference config, hooks, codec
normalizations, message order) run the same way, and so does every schema rejection the conformance transcripts
recorded (`InvalidRequestError` of kind Payload, Query or Params, rebuilt from the transcript), which must also give
the recorded message. Each root is decoded as HttpApi decodes it: a payload through `Schema.Union(payloadSchemas)`,
a query through `Schema.toCodecArrayFromSingle(query)`. The generator stops on any schema feature or transformation
it does not know, so an upstream change cannot silently produce a wrong type; a fresh generation must equal the
checked-in code byte for byte.

Rejections: `Issue`'s `Display` (also `Issue::message()`) is exactly the text TS's `SchemaError.message` gives for the
same input, which the TS server puts in `InvalidRequestError.message` (before its own truncation at 1024 characters,
`packages/server/src/middleware/schema-error.ts`, which the Rust server must apply too). That text is Effect's
default formatter (`SchemaIssue.makeFormatterDefault`) under HttpApi's parse options, which opencode leaves at
Effect's defaults: `errors: "first"` (each struct, array or check reports its first problem only), no `reportInput`
(no `got <value>`), `onExcessProperty: "ignore"`. A leaf is `<message>` or `<message>\n  at ["key"][0]`; a union
whose candidate members all fail lists each member's leaf, joined by newlines. The generator computes every message
from the TS AST as Effect does: a type error is `Expected <expected>`, where expected is the identifier (of the last
check, else of the node) or the node's own text (`string | null`, `"asc" | "desc"`, `{ readonly "type": "x", ... }`),
unless a `message` annotation replaces it; a failed check is its filter's `message`, else `Expected <its expected
annotation>`; a missing key is `Missing key` (or `messageMissingKey`); a few codecs' fixed texts (template literals,
`Schema.Number` under the JSON codec, base64, durations) are read off the TS parser. Decoding follows Effect's
order: struct keys in declaration order (`key?: never` included), then index signatures in JS key order; checks in
declaration order; and a union tries only the members Effect's candidate index selects (by runtime type and
discriminating literal fields, `wire/union.rs`). Failures outside the schema are the server's to word: a body that
is not JSON is `Expected a valid JSON body` (HttpApiBuilder); an empty JSON body is decoded as `undefined`, which
reports as `null` does for every payload type.

Mapping rules. A Rust value is the canonical wire form: decoding accepts exactly what TS's `Schema.decode` accepts,
and encoding writes what TS would write for the decoded value.

| Schema | Rust |
|---|---|
| wire context | HTTP bodies use HttpApi's JSON codec (`toCodecJson`); SSE `data` and the config file use the plain encoded side through `JSON.stringify`; path, query and headers use the string tree. One schema can get two types (`FooPlain`) when the contexts differ on the wire. |
| optional key | `Option<T>`, absent = `None`. `undefined` does not exist in JSON: under the JSON codec `Schema.optional(X)` is `X \| null`, so `Option<Option<X>>` keeps absent and `null` apart; in plain contexts `null` is rejected. opencode's `optional` helper never takes `null`. `key?: never` must be absent. |
| `X \| null` | `Option<X>` |
| unions, tagged or not | an enum with one variant per member; the members Effect's candidate index selects for the input are tried in declaration order, first match wins (Effect `anyOf`; `oneOf` requires exactly one). Members keep their tag field (a zero-size `lit::` type), so field order and output match. |
| literals, literal unions | `lit::X` unit types; enums of unit variants |
| identified (branded, refined) schemas | newtypes (`SessionID(String)`) with their checks; anonymous refined values get the checks inline |
| records | `rt::Record<K, V>` (JS key order, as TS decodes them); keys a branded key type rejects are skipped, as in TS; a struct with an index signature gets a `rest` record |
| classes, tagged errors | structs of their encoded fields, `_tag` included |
| recursion | `Box` where a definition reaches itself other than through `Vec` / `Record` |
| `Schema.Struct({})` | `rt::NotNull`: any non-null value, returned unchanged (as TS does) |
| transformations | wire side only, but the decode of known ones is reproduced so the stored value is canonical: `NumberFromString` (JS `Number(s)`, so `"0x10"` is 16 and `""` is 0), `Trim`, `DateTimeUtcFromMillis` (truncates, \|n\| ≤ 8.64e15), `ArrayEnsure` (one item is written bare), decoding defaults, `catchDecoding` (invalid values dropped), `fromJsonString`, `DurationFromString` (written as `"<n> millis"`), base64 bytes. Custom TS functions are reproduced by hand in `src/wire/hook.rs`, fingerprinted by their exact source so the generator stops when upstream changes them. |
| numbers | JSON numbers are `f64`, as in JS (`1e400` is `Infinity`); `Schema.Int` (a safe integer) is `i64`, and `1.0` or `-0` read as integers. Output is JS `Number#toString`: shortest round-trip digits, ties to even, `1e+21`, `0.000001`, `-0` as `0`; non-finite numbers are `null` (`JSON.stringify`), or `"NaN"` / `"Infinity"` under the JSON codec (`rt::JsNumber`). |
| strings, key order | `JSON.stringify` escaping; fields in declaration order; object keys in JS property order (array-index keys first); unknown keys ignored and dropped (Effect's default `onExcessProperty: "ignore"`) |

Enforced on decode: patterns (JS regex syntax translated to Rust's `regex`), min/max length in UTF-16 units,
starts/ends with, includes, trimmed (JS whitespace), safe integer, finite, bounds, array length, uniqueness, literal
sets. Known gaps, also listed at the top of `generated/types.rs`:
- strings with lone surrogates (`"\ud800"`): JS accepts them, a Rust `String` cannot hold them, so Rust rejects;
- `session.import` messages: TS first upgrades messages exported before v2.1, Rust only takes the current shape;
- a union whose overlapping members transform differently: TS encodes with the first member whose decoded side
  fits, Rust with the member that decoded (not observed in the round-trip);
- a `DateTimeUtcFromMillis` out of range is a TS defect (HTTP 500), not a schema error with a message; Rust rejects
  it with `Invalid date`, so the server must map that to the 500 TS answers (for example `POST
  /api/session/:sessionID/view` with `{"idle": 9e15}`);
- the messages are those of the inputs Rust and TS both reject; where acceptance differs (the gaps above and
  below), so does the outcome;
- durations beyond 2^127 nanoseconds are rejected (TS uses BigInt); hex/octal/binary `Number()` strings beyond
  2^128 may differ in the last place.

## Rust server

`rust/crates/opencode` is the `opencode` executable: the CLI (see `cli/`) and the server it runs, configured
like the TS server through the environment (`OPENCODE_PASSWORD`/`OPENCODE_SERVER_PASSWORD`, `OPENCODE_DB`, XDG
directories). It is one process on a single-threaded tokio runtime: hyper for HTTP/1.1, SQLite through rusqlite.

| Module | TS counterpart |
|---|---|
| `cli.rs`, `cli/`, `cors.rs` | `packages/cli`: effect/cli 4.1's parsing, help and error texts over the command tree (`cli/spec.rs`, `parse.rs`, `help.rs`; the shell completion scripts are recorded from the reference); the server a command talks to (`--server`, a private `serve --stdio` child, or the background service, which `service.rs` configures, discovers, starts with contenders and stops, under the channel's XDG files); `serve` in its three modes; `run` (session target, stdin and attachments, the prompt over the event stream with its text or JSON rendering, permission and question handling, signals); `session`, `api`, `models`, `debug`, `reload`, `stats`, `auth` (in @clack/prompts' frames) and `acp` (the Agent Client Protocol over stdio); CORS preflight and the request-site check. Not ported: the TUI (default command), `mini`, `mcp`, `plugin`, `upgrade`, `uninstall`, `pair`, `--wizard` |
| `http.rs`, `auth.rs` | `server/src/process.ts`, `auth.ts`, `middleware/authorization.ts`: Basic credentials (or `auth_token`), routing over the generated route table (static segments over parameters over wildcards) |
| `api.rs` | the HttpApi pipeline: location middleware, then the request decoded against the endpoint's generated schemas (`roots::roundtrip`), a rejection being a 400 `InvalidRequestError` naming the part |
| `handlers/` | `server/src/handlers/*.ts`, one module per area of the API (`handlers/mod.rs` routes an operation by its name) |
| `inventory.rs`, `inventory/` | `core/src/config.ts`, `config/*`, `agent.ts`, `provider.ts`, `model.ts`, `command.ts`, `skill.ts`, `reference.ts` and the plugins that fill them: config discovery, substitution, JSONC, v2 normalization; agents, providers and models (no catalog), commands, skills, references; the dropped features' endpoints answer as TS does with nothing configured. `inventory/yaml.rs` is js-yaml 3.15.2 `safeLoad`, `inventory/markdown.rs` gray-matter 4.0.3 (both checked against the real libraries); built-in texts and the plugin and integration tables are recorded from the reference (`inventory/builtin/`). `Location::inventory()` is a snapshot; a config change rebuilds it and announces what changed |
| `vcs.rs`, `worktree.rs` | `core/src/vcs.ts`, `vcs/patch.ts`, `plugin/vcs/git.ts`, `hg.ts`, `worktree.ts`, `worktree/*`: through the git CLI |
| `fs.rs`, `fuzzysort.rs`, `collate.rs` | `core/src/filesystem.ts`, `search.ts`, `file-watch.ts`, `ripgrep.ts` (ripgrep from `PATH` or `$XDG_CACHE_HOME/opencode/bin`, never downloaded); fuzzysort 3.1's `go` with its exact scores and tie order; `localeCompare` (Bun's ICU 78 `ucol_strcoll` at root: the identical-prefix skip, FCD iteration, contractions and prefix mappings ported from ICU, over ICU's own root data, CLDR's radical-stroke Han included, dumped from the system's ICU 78 into `generated/collation.bin` by `gen/src/collation.ts`; `crates/opencode/harness/collate/run.sh` checks every code point, contraction and generated string against ICU's elements and Bun's order: 0 differences on about 8.5M cases) |
| `process.rs`, `text.rs`, `project_icon.rs` | `AppProcess.run` (its "Command failed" text), JS string helpers, `project/icon.ts` |
| `db.rs` | `core/src/database`: the same pragmas, and a fresh database is bootstrapped with upstream's exact DDL and migration journal (`src/generated/database.rs`, from `schema.gen.ts` and `migration.gen.ts` by `gen/src/database.ts`), so either server opens a database the other created |
| `bus.rs` | `core/src/bus.ts`: durable events get the next sequence of their aggregate and are projected in their commit's transaction; payloads are not retained (as with `opencode serve`); live subscribers get every event after it commits |
| `project.rs`, `location.rs` | `core/src/project.ts`, `location*.ts`: project identity (directory hash; git remote, cached ID or root commit), `worktree.resolved` adoption, location boot, reload and eviction, and its announcements |
| `session.rs` | `core/src/session.ts`, `session/store.ts`, `session/projector.ts` |
| `session/`, `llm.rs`, `llm/` | `core/src/session/*` (runner, execution, step, inbox, context, prompt, command, instructions, retry, title, move) and the OpenRouter chat-completions transport (`llm/http.rs`: proxy-aware rustls) |
| `tool.rs`, `tool/` | `core/src/tool.ts`, `tool/plugin/*`: the registry, snapshot filtering, Code Mode catalog, output bounding; each tool's definition is recorded from the reference |
| `job.rs`, `session/subagent.rs`, `tool/subagent.rs` | `core/src/job.ts`, `job/*`, `session/subagent-job.ts`, `subagent-completion.ts`, `background.ts`, `tool/plugin/subagent.ts`, subagent commands: a process-local job registry (one generation per job; block, background, cancel; backgrounded work kept in the durable `job` table until its completion notice is admitted); a subagent is a job running a child session, whose last answer is the tool's result or a synthetic notice that wakes the parent |
| `snapshot.rs`, `session/revert.rs`, `session/diff.rs` | `core/src/snapshot.ts` over `git.ts`'s store operations, through the git CLI: each project worktree gets a git store under `$XDG_DATA_HOME/opencode/snapshot` that borrows the source's objects and starts from its index; captures go through a temporary index and the source's ignore rules filter what callers see. Steps record their start and end snapshots and changed files; `session/revert.ts` stages, clears and commits reverts (the next prompt or compaction commits a staged one); `session/diff.ts` diffs a turn |
| `session/compaction.rs` | `core/src/session/compaction.ts` and the compaction parts of `runner/llm.ts`, `runner/step.ts`, `history.ts`, `recent-text.ts`, `config/plugin/compaction.ts`: automatic compaction before a step past the threshold, overflow recovery when the provider rejects a step as too long before any output, manual compaction at a step boundary, background summaries; the summary request is the step's own request plus the summary prompt (reusing the prompt cache), resent without its kept tail or as a shortened transcript when too long; the history later requests see starts at the latest completed compaction |
| `tool/webfetch.rs`, `tool/skill.rs`, `tool/arguments.rs`, `url.rs` | `tool/plugin/webfetch.ts`, `tool/http-body.ts` (on `opencode-html`), `tool/plugin/skill.ts`, `tool/runtime.ts`'s input issues, the WHATWG `URL` subset the tools need: URL and credential checks, a permission check per redirect, the Cloudflare retry and the timeout, through the proxy-aware client |
| `instruction_discovery.rs`, `jsdiff.rs`, `session/instructions.rs` | `instruction-discovery.ts`, `config/plugin/instruction.ts`, `session/instructions.ts`, `reference/instructions.ts`, jsdiff 8.0.4: the AGENTS.md walk (global, project up to the root or home), configured paths, globs and URLs (with diffs for edits), reference guidance, unavailable sources blocking the first admission, nested AGENTS.md after reads |
| `inventory/skills.rs`, `watch.rs` | `config/plugin/skill.ts`, `skill/discovery.ts`, `filesystem/watcher.ts`: URL skill indexes pulled into the cache; one inotify instance per process (Linux; macOS watches are a TODO) watching skill sources and instruction files, rescanned 100 ms after a burst of changes |
| `schedule.rs`, `schedule/`, `zone.rs`, `flag.rs`, `tool/schedule.rs` | `core/src/schedule.ts`, `schedule/check.ts`, `flag.ts`, `tool/plugin/schedule.ts`, Effect's `Cron` and `DateTime` zones: schedules (`in`, `at`, `every`, `cron`; `if`/`until` shell checks under `Permission.Unattended`) and the leader-leased firing loop, zones from the system tzdata, feature flags; code checks and the schedule tools' execution are TODO |
| `session/transfer.rs`, `session/stats.rs`, `session/blob.rs` | `session/transfer.ts`, `Session.fork`, `session-legacy.ts`, `session/stats.ts`, `session/blob.ts`: export (sanitized), import, fork, statistics, media blobs on read, import and fork |
| `shell.rs`, `shell/`, `file_access.rs`, `tool/shell.rs`, `session/shell.rs` | `core/src/shell.ts`, `shell/select.ts`, `file-access.ts`, `tool/plugin/shell.ts`, `session/shell.ts`, `session/environment.ts`, `server/src/handlers/shell.ts`: a location's shell commands (each leads its own process group; stdout and stderr stream from two pipes into `<data>/shell/<project>/<id>.out` with byte and newline counts; timeouts send SIGTERM, then SIGKILL; 25 exited commands kept; an hourly 7-day sweep), shell selection, the shell tool (workdir resolution, `opencode-shell` analysis, `external_directory` and `shell` permissions, a job that blocks, is backgrounded or notifies the session), users' `!commands`, and the session environment |
| `tool/{read,write,edit,glob,grep}.rs`, `tool/file_*.rs`, `formatter.rs` | `core/src/tool/plugin/{read,write,edit,glob,grep}.ts`, `file-diff.ts`, `tool/read-filesystem.ts`, `tool/search.ts`, `file-write.ts`, `file-mutation.ts`, `ripgrep.ts`, `formatter.ts`, `formatter/builtins.ts`, `plugin/tool-input-repair.ts` and `tool/runtime.ts`'s input decoding, `tool.ts`'s `normalizeImages`: read pages text in 256 KiB chunks (2,000 lines or 50 KiB), lists directories, returns images and PDFs inline, suggests near-miss names, and loads nearby `AGENTS.md` once per session; write and edit approve the external directory, then the edit, with a jsdiff preview, keep BOMs and run the configured formatters; edit matches the LF view, then typographic variants, then trailing whitespace; glob and grep run ripgrep, bounded by the limit and 30 s. `harness/file-tools/run.sh` checks diffs, edit matching and read pages against the reference on random cases. TODO(port): `patch` (shown only to `gpt-*` models), `todowrite` (off by flag), resizing large images, installing npm formatters |
| `codemode/`, `tool/execute.rs`, `tool/opencode.rs` | `core/src/codemode/*`, `tool/plugin/opencode.ts`: the `execute` tool on the `opencode-codemode` interpreter. A run is a thread-pinned `!Send` future whose tool calls run on the server thread; dropping it cancels exactly. The Code Mode catalog and its instructions (partial catalog, diffs) come from the registered tools and namespaces; Code Mode `fetch` follows the webfetch rules and handles redirects itself; progress rows are throttled to 100 ms; `codemode::check` runs schedule code checks; the opencode.* tools (sessions, worktrees, model search) use definitions recorded from the reference. A call or `fetch` the user declines ends the program uncaught and fails the execute call with the decline, so the turn stops as for a direct call (D517); a rejection with feedback stays catchable |
| `session/blob.rs`, `session/parts.rs`, `credential.rs`, `inventory/config_watch.rs`, `tool/patch.rs`, `tool/patch_text.rs`, `tool/todo.rs`, `tool/file_media.rs`, `npm.rs`, `session/restart.rs` | `session/blob.ts` and the projector's part-row writes (media stored once as `media:` refs under `experimental.media_blob`, assistant parts as `session_part` rows under `experimental.part_rows`); `credential.ts` and `integration.ts`'s connections (stored keys, used ahead of a configured `apiKey`); `config/watch.ts` (a config change reloads the location); `tool/plugin/patch.ts` and `@opencode/util/patch` (shown only to GPT models; `harness/patch/run.sh`), `tool/plugin/todo.ts` (the `todo` flag) and `plugin/optimize.ts`'s prompts; `image/photon-normalize.ts` on photon's own image 0.24.9 and codec versions (byte-identical; `harness/images/run.sh`) and `boundImages`; `@opencode/util/npm` `which` (npm formatters install into the cache); `session/execution/restart.ts` (backgrounded jobs recovered by the managed server; `harness/restart/run.sh`). `/openapi.json` is recorded from the reference by `gen/src/openapi.ts` |
| `pty.rs`, `pty/`, `persistent_pty.rs`, `persistent_pty/`, `handlers/pty.rs`, `http/websocket.rs` | `core/src/pty.ts`, `pty/*`, `persistent-pty/index.ts`, `persistent-pty/daemon.ts`, `server/src/handlers/pty.ts`, `pty-socket.ts`, `persistent-pty.ts`, the ticketed connects of `middleware/authorization.ts`: a location's terminals on openpty (the login flag first, D223; the last 2 Mi UTF-16 units kept for replay; 25 exited kept) and their WebSocket (replay from a cursor, the `0x00{"cursor":N}` frame, live text, 1000 on exit, 4404 when there is nothing to attach); persistent terminals in the reference's `opencode-pty` daemon (`OPENCODE_PTY_BIN`, else `opencode-pty` on `PATH`; protocol 7; started detached on first use; a handoff claimed at startup from `OPENCODE_PTY_HANDOFF`) and their socket (JSON control messages, binary output, controller, observer and framed input, unframed input at the latest size, D224); single-use connect tickets scoped to their location; a minimal RFC 6455 server over hyper's upgrade (one write per frame) |
| `permission.rs`, `handlers/permission.rs` | `core/src/permission.ts`, `permission/saved.ts`, `server/src/handlers/permission.ts`: pending requests (asked by tools through `tool::Context::assert` or through the API, listed, answered; a rejection settles the session's other requests, "always" saves approvals and settles what they now allow), rule evaluation (the session's agent or the default agent, session rules, saved approvals, managed policies), saved approvals listed, edited and removed; closed with the location, before its forms |
| `form.rs`, `form/`, `handlers/form.rs`, `tool/question.rs` | `core/src/form.ts`, `server/src/handlers/form.ts` and the session form endpoints, `tool/plugin/question.ts`: a location's forms in recency order (settled ones kept ten minutes), definitions and answers validated as the JS engine does (`form/formats.rs`: patterns on `opencode_codemode::jsregex`, dates on `opencode_codemode::jsdate`, `URL.canParse` on the `url` crate), cancelled when interrupted or at shutdown; the question tool asks through a form |

Status (2026-11-09): every server scenario of the standard passes (697 run, 3 skipped by the standard), including the
70 CLI scenarios: session CRUD, projects, locations, the file system, version control, worktrees, config, agents,
providers and models, the execution loop, compaction, permissions, questions and forms, subagents and jobs, snapshots
and reverts, schedules, session transfer, webfetch, skills and instructions, shell, the file tools and the patch tool,
Code Mode, stored credentials, media blobs, restart recovery, the PTY APIs, and the CLI (`run`, `serve`, `service`,
`session`, `api`, `auth`, `stats`, `acp` and the rest). The TUI is not ported. Release builds peak (VmHWM) at a median of 19.6 MiB per server process
over 600 passing scenarios (p90 21.3 MiB); an idle server holds about 2 MiB of anonymous memory, the rest being clean
pages of the 20 MB executable. The TS server peaks at a median of about 312 MiB, and at 377 to 728 MiB on the five
scenarios where Rust peaks highest (62 to 141 MiB: large attachments, Code Mode limits, the webfetch budget). Unported behavior is marked `TODO(port)` in the source; an
endpoint without a handler answers 501 (after the 404 of an unknown session, which most session routes check first).

`scripts/rust-conformance.sh` builds the executable with the oracle's build identity and runs the standard against it
(`CONFORMANCE_IMPL`, see `conformance/README.md`), sharded; the last run's reports are in `rust/target/conformance-logs`. Every scenario that passes is listed in the
ledger, `conformance/rust-passing.txt`: a ledgered scenario that fails is a regression (exit 1); new passes are listed and
`--update` adds them. Nothing removes a line but a hand edit: a ledgered scenario that was renamed or deleted is listed
as not run.

```sh
port/scripts/rust-conformance.sh                       # every server scenario (debug build)
port/scripts/rust-conformance.sh scenarios/session-api-crud.test.ts
port/scripts/rust-conformance.sh --update              # record new passes in the ledger
CONFORMANCE_MEMORY_LOG=m.jsonl port/scripts/rust-conformance.sh --release; bun conformance/memory.ts m.jsonl
```

### Support crates

Self-contained ports of the libraries the tools build on, each proven byte for byte against the TS code it replaces
by a differential harness of its own.

`rust/crates/opencode-html` is the webfetch tool's body processing: `html_to_markdown` (`html-markdown.ts`),
`html_to_text` (`extractTextFromHTML` in `webfetch.ts`), and in `webfetch` the pure response steps (Accept header,
content-type checks, body limit, decoding, choice of conversion). Both converters run on a port of htmlparser2 8.0.2's
Tokenizer and Parser as the reference patches it (D113: a tag the input ends inside is dropped and not closed, a
digitless `&#x` at the end stays text; D114: an astral entity is one code point), with entities 4.5.0's decode trie,
so they see the same events as TS, quirks included. Strings are UTF-8 with JS's TextEncoder byte counts and
html-markdown.ts's 64 Ki-unit write chunks, which end between characters (D114). The patched TS never returns a lone
surrogate, so the output is the TS string exactly: the U+FFFD gap is gone. `harness/run.sh` runs the upstream converter tests with a recording preload, then compares TS and
Rust on the scenario bodies, the recorded upstream inputs, hand-written pages and their mutations, and generated cases
(tag soup, nesting, chunk boundaries, end of input, list numbers, the budget, content types, decoding, the body
limit): 0 differences on about 150k cases.

```sh
port/rust/crates/opencode-html/harness/run.sh                         # entity table, unit tests, upstream tests, differential
port/rust/crates/opencode-html/harness/run.sh --count 60000 --seed 2  # a deeper run
port/rust/crates/opencode-html/harness/run.sh --coverage              # needs cargo-llvm-cov
```

`rust/crates/opencode-shell` is the shell tool's command analysis, dependency-free: `scan` (`shell/scan.ts` for Bash,
Zsh, POSIX sh and PowerShell, with the same work budget, check order and opaque reasons), `parse` (`shell/parse.ts`:
resources, saved patterns, arity and wrapper tables, directory changes; `FileAccess.resolvePath`), `wildcard` and
`permission` (`Wildcard.match`, `Permission.evaluate`), `select` (the pure part of `shell/select.ts`), and `result`
(`shell/result.ts`: output capture and paging, the tool's result shapes and texts). Strings are UTF-16 inside, so
offsets and `?` wildcards count what TS counts. `harness/check.sh` compares the TS modules and the crate as JSON,
byte for byte, on the inputs upstream's own shell, permission and tool tests produce, the conformance scenarios'
commands, generated and mutated commands, and the real TS `Shell` service's capture and paging: 0 differences on about
416k cases (`--cases 30000 --seed 7` for a deeper run). After a bun upgrade, `check.sh` fails until the Unicode table
is regenerated (`bun harness/gen-unicode.ts`). See the crate's README.

`rust/crates/opencode-codemode` is the default code-mode interpreter (the bytecode VM is not ported): what
`CodeMode.execute` hands the tool layer and the text core's `execute` tool gives the model. It has a parser checked
against acorn, a tree-walking interpreter with the stdlib, JS RegExp, number conversion and Date, the tool runtime,
host extensions (the server's `fetch`), hooks, and `scanTools`. Deep programs grow the stack on demand; TS's stack
limits are emulated by deterministic budgets, so a program gets the same catchable RangeError. `harness/check.sh`
runs every corpus case (the conformance scenarios' `execute` programs, upstream's, and targeted ones: async, tools,
extensions, hooks, chains, deep structures, interrupts) through TS and Rust, then every program through `scanTools`: 0
differences; component checks cover the parser (about 282k programs against acorn), number conversion, RegExp and
Date. A host call can end interrupted on its own (`ToolOutcome::Interrupted`, `HostOutcome::Interrupted`): no catch,
finally or rejection handler sees it, and it interrupts the execution once it reaches the body. See the crate's README
and `harness/README.md`.
