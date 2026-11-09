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

`rust/crates/opencode` is the `opencode` executable. So far it runs `opencode serve` (`--hostname`, `--port`), configured
like the TS server through the environment (`OPENCODE_PASSWORD`/`OPENCODE_SERVER_PASSWORD`, `OPENCODE_DB`, XDG
directories). It is one process on a single-threaded tokio runtime: hyper for HTTP/1.1, SQLite through rusqlite.

| Module | TS counterpart |
|---|---|
| `http.rs`, `auth.rs` | `server/src/process.ts`, `auth.ts`, `middleware/authorization.ts`: Basic credentials (or `auth_token`), routing over the generated route table (static segments over parameters over wildcards) |
| `api.rs` | the HttpApi pipeline: location middleware, then the request decoded against the endpoint's generated schemas (`roots::roundtrip`), a rejection being a 400 `InvalidRequestError` naming the part |
| `handlers/` | `server/src/handlers/*.ts`, one module per area of the API (`handlers/mod.rs` routes an operation by its name) |
| `inventory.rs` | `core/src/config*.ts`, `agent.ts`, `provider.ts`, `model.ts`, ...: what a location's config and built-in plugins register (agents, providers, models), loaded at boot |
| `db.rs` | `core/src/database`: the same pragmas, and a fresh database is bootstrapped with upstream's exact DDL and migration journal (`src/generated/database.rs`, from `schema.gen.ts` and `migration.gen.ts` by `gen/src/database.ts`), so either server opens a database the other created |
| `bus.rs` | `core/src/bus.ts`: durable events get the next sequence of their aggregate and are projected in their commit's transaction; payloads are not retained (as with `opencode serve`); live subscribers get every event after it commits |
| `project.rs`, `location.rs` | `core/src/project.ts`, `location*.ts`: project identity (directory hash; git remote, cached ID or root commit), `worktree.resolved` adoption, location boot and its announcements |
| `session.rs` | `core/src/session.ts`, `session/store.ts`, `session/projector.ts` |

Status (2026-11-08): 13 of the 18 session CRUD scenarios pass; the rest of the standard needs agents, config, the
model loop and tools. On those 18 scenarios the release build peaks at a median 8.3 MiB per server process, the TS
server at 311.7 MiB. Unported behavior is marked `TODO(port)` in the source; an endpoint without a handler answers 501 (after the 404 of
an unknown session, which most session routes check first).

`scripts/rust-conformance.sh` builds the executable with the oracle's build identity and runs the standard against it
(`CONFORMANCE_IMPL`, see `conformance/README.md`), sharded; the last run's reports are in `rust/target/conformance-logs`. Every scenario that passes is listed in the ledger,
`conformance/rust-passing.txt`: a ledgered scenario that fails is a regression (exit 1); new passes are listed and
`--update` records them.

```sh
port/scripts/rust-conformance.sh                       # every server scenario (debug build)
port/scripts/rust-conformance.sh scenarios/session-api-crud.test.ts
port/scripts/rust-conformance.sh --update              # record new passes in the ledger
CONFORMANCE_MEMORY_LOG=m.jsonl port/scripts/rust-conformance.sh --release; bun conformance/memory.ts m.jsonl
```
