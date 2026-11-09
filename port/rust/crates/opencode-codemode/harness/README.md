# Differential harness

Everything here compares the Rust crate against the TypeScript reference (`port/.reference/packages/codemode`, run
under bun). Expected results always come from running TS. None are written by hand.

## The main check

```sh
harness/check.sh [corpus...] [--show N]     # from anywhere; TZ is pinned to UTC
```

The script builds `examples/codemode_run.rs` in release. For every corpus under `corpus/` (or only the ones named),
it runs `run-ts.ts` (TS) and `codemode_run` (Rust) over each case and has `compare.ts` diff the JSON lines byte for
byte. It then repeats the comparison with `--scan`, which sends every case's code through `scanTools` instead.
`compare.ts` masks diagnostics that JavaScriptCore wrote, such as `JSON.parse`'s messages, as `<engine message>`
(`port/conformance/lib/normalize.ts`), and reports how many cases only matched after that masking. The exit status
is 1 if any case differs.

Each output line is `{"name", "result", "output"}`:

- `result` is `CodeMode.Result` exactly as the tool layer receives it.
- `output` is the text core's `execute` tool gives the model (`formatResult` in core/src/codemode/tool.ts).

Cases with `"hooks": true` also print `events`. With `--scan`, each line is `{"name", "scan"}` instead.

## Corpus format

A corpus is a JSON-lines file with one case per line:

| field | meaning |
| --- | --- |
| `name`, `code` | the case |
| `bindings` | outer globals (`CodeMode.execute`'s `bindings`) |
| `limits` | `{timeoutMs, maxToolCalls, maxOutputBytes}` |
| `tools` | `[{path, description?, input?, output?, behavior?, timing?}]`. `input` is a JSON Schema (default `{type: "object", properties: {}}`). `output` is a JSON Schema, `"nullOrString"` (core's default `Schema.NullOr(Schema.String)`), or absent for `void`. `behavior` is `echo` (the default), `{kind: "value", value}`, `{kind: "fail" \| "die", message}`, or `{kind: "sequence", values: [{ok} \| {fail}]}`. `timing` is `"sync"` (finishes inside the call) or `"yield"` (finishes after `Effect.yieldNow`). |
| `extensions` | `[{name, globals: {name: spec}}]`, host globals described as data. A spec is `{kind: "echo" \| "args", async?}`, `{kind: "value", value, async?}`, `{kind: "throw", name?, message, fields?, async?}`, `{kind: "response", body}` (a fetch-like Response), or `{kind: "object", members: {name: spec}}`. |
| `hooks` | `true` records every `tool.before`/`tool.after`/`extension.before`/`extension.after` as `events` |
| `scanOnly` | `true` skips execution, for cases written only for `scanTools` (some never finish when run) |

The two runners build the same host behavior from these descriptions. Host timing is modelled to match: a promise
that a TS global returns settles one host turn after the call, and a `"yield"` tool settles two turns after it,
because a microtask runs before `Effect.yieldNow`'s macrotask.

## Corpora

| file | cases | source | regenerate |
| --- | --- | --- | --- |
| `conformance.jsonl` | 168 | every `execute` program in `port/conformance/scenarios/codemode-*.test.ts`, with fake `opencode.*` tools | `bun harness/extract-conformance.ts > corpus/conformance.jsonl` |
| `upstream.jsonl` | 1,483 | every distinct execution in `packages/codemode/test` that has no tools, extensions, or hooks (1,900 recorded; the 412 others are skipped) | `bun harness/record-upstream.ts > corpus/upstream.jsonl` |
| `async.jsonl` | 51 | job ordering, `await`, generators, unhandled rejections | `bun harness/gen-cases.ts async` |
| `tools.jsonl` | 33 | tool paths, schemas, search, limits, output bounding | `bun harness/gen-cases.ts tools` |
| `extensions.jsonl` | 50 | host globals: values crossing both ways, errors, promises, timing | `bun harness/gen-cases.ts extensions` |
| `hooks.jsonl` | 13 | hook order and call identity, interrupted calls | `bun harness/gen-cases.ts hooks` |
| `matrix.jsonl` | 156 | operators, coercions, and built-ins crossed with a spread of values | `bun harness/gen-cases.ts matrix` |
| `chains.jsonl` | 28 | expression chains of 3,000 and 45,000 links, in every context that walks or evaluates them | `bun harness/gen-cases.ts chains` |
| `scan.jsonl` | 82 | `scanTools` only: scopes, chains, error positions, depth | `bun harness/gen-cases.ts scan` |

`gen-cases.ts` writes to stdout, so send each kind to `corpus/<kind>.jsonl`. All its output is deterministic.

## Component checks

Each one compares a building block with JavaScriptCore or acorn over generated cases. They run in parallel where
they can, and they clean up the temporary directory under `$TMPDIR` that they use.

| check | compares | default size |
| --- | --- | --- |
| `harness/parser/check.sh` | the acorn port (`src/parser`) with acorn 8.15, as ESTree JSON | about 282,000 programs |
| `harness/numconv/check.sh [count]` | number formatting and parsing (`src/numconv.rs`) | 1,000,000 random cases per function, plus edge cases |
| `harness/jsregex/check.sh [scale]` | the RegExp engine (`src/jsregex`) | about 270,000 cases |
| `harness/jsdate/check.sh [scale] [zone...]` | Date (`src/jsdate`) | about 420,000 values per zone. UTC must match exactly; other zones are reported. |
