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
| `rust/` | The Rust implementation: a Cargo workspace (`crates/*`) and the wire-type generator (`gen/`). See [Rust wire types](#rust-wire-types). |
| `scripts/` | Upstream sync, ownership check, building the reference, dependency linking, generating the wire types. |

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
broken strings and numbers) must be rejected by Rust wherever TS rejects them, and written identically wherever
TS accepts them; hand-picked edge cases (the conformance reference config, hooks, codec normalizations) run the
same way. The generator stops on any schema feature or transformation it does not know, so an upstream change
cannot silently produce a wrong type; a fresh generation must equal the checked-in code byte for byte.

Mapping rules. A Rust value is the canonical wire form: decoding accepts exactly what TS's `Schema.decode` accepts,
and encoding writes what TS would write for the decoded value.

| Schema | Rust |
|---|---|
| wire context | HTTP bodies use HttpApi's JSON codec (`toCodecJson`); SSE `data` and the config file use the plain encoded side through `JSON.stringify`; path, query and headers use the string tree. One schema can get two types (`FooPlain`) when the contexts differ on the wire. |
| optional key | `Option<T>`, absent = `None`. `undefined` does not exist in JSON: under the JSON codec `Schema.optional(X)` is `X \| null`, so `Option<Option<X>>` keeps absent and `null` apart; in plain contexts `null` is rejected. opencode's `optional` helper never takes `null`. `key?: never` must be absent. |
| `X \| null` | `Option<X>` |
| unions, tagged or not | an enum with one variant per member, tried in declaration order, first match wins (Effect `anyOf`; `oneOf` requires exactly one). Members keep their tag field (a zero-size `lit::` type), so field order and output match. |
| literals, literal unions | `lit::X` unit types; enums of unit variants |
| identified (branded, refined) schemas | newtypes (`SessionID(String)`) with their checks; anonymous refined values get the checks inline |
| records | `rt::Record<K, V>` (insertion order kept); keys a branded key type rejects are skipped, as in TS; a struct with an index signature gets a `rest` record |
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
- a `DateTimeUtcFromMillis` out of range is a TS defect (HTTP 500), not a schema error; Rust rejects it like any
  invalid value, so the server must map it;
- error messages (`InvalidRequestError.message`) are not Effect's wording;
- durations beyond 2^127 nanoseconds are rejected (TS uses BigInt); hex/octal/binary `Number()` strings beyond
  2^128 may differ in the last place.
