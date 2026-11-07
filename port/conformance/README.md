# Conformance oracle

Scenarios drive opencode **only through its public interface** and record everything observable. The TS
reference produces the recordings ("goldens"); the Rust implementation must reproduce them exactly. The
reference is upstream plus the deviation patches in `port/patches/`, which fix upstream bugs the port does
not reproduce.

Setup (once per checkout, and after every upstream merge or patch change): `port/scripts/reference.sh build`.

```sh
cd port/conformance
bun test                                  # server scenarios (scenarios/) against the TS reference
(cd tui && bun test)                      # TUI scenarios
bun test scenarios/tools.test.ts          # one file
UPDATE_GOLDENS=1 bun test <file>          # re-record (only when the behavior change is intended)
CONFORMANCE_STRICT=1 bun test             # fail on missing goldens instead of recording them (CI)
CONFORMANCE_EXPERIMENTAL=1 bun test       # also run scenarios for experimental flags (code-mode VM)
./determinism.sh [--record] [runs] [shards]   # every scenario, N runs, parallel shards: the gate
LABEL=mine port/coverage/oracle.sh scenarios/x.test.ts   # coverage for a run (from repo root)
python3 -I port/coverage/uncovered.py mine 'core/src/session/compaction' --source   # what it missed
```

A golden is part of the standard only once `determinism.sh` reproduces it on every run.

Large values that repeat across goldens (the system prompt, the tool list, big tool outputs) are stored once
under `goldens/_blobs/` (content-addressed) and referenced as `{"<blob>": "<sha256>"}`. Comparison expands
them, so equality is exact. `bun lib/blobs.ts gc` removes blobs nothing references; `bun lib/blobs.ts compact`
rewrites goldens recorded by older harness versions.

## Drivers

| Driver | Files | Interface |
|---|---|---|
| `lib/scenario.ts` | `scenarios/*.test.ts` | HTTP API, live event stream, project files, the fake model |
| `lib/tui.ts` | `tui/*.test.ts` | keystrokes and mouse into the real TUI, settled screen frames |
| `lib/cli.ts` | `scenarios/cli-*.test.ts` | the CLI as a real process: argv, stdin, stdout/stderr, exit code |

The server-side reference is booted the way `opencode serve` boots it (`packages/server/src/process.ts`):
application routes, resumption of interrupted sessions, the schedule runner, and the real server's Linux
defaults (file watcher on; per scenario: `server: { filewatcher, fff }`), except that fff search is off: the
port omits fff, so file search is ripgrep + fuzzysort ranking, as with `OPENCODE_DISABLE_FFF=1`. Only the listening
socket is replaced by an in-process handler, so `bun test --coverage` can measure the TS reference.

## Writing a scenario

```ts
import { call, say } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

scenario(
  "edit a file with a unique match",
  {
    files: { "src/math.ts": "export const add = (a: number, b: number) => a + b\n" },
    turns: [call("read", { path: "src/math.ts" }), call("edit", { path: "src/math.ts", oldString: "a + b", newString: "b + a" }), say("Done.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Swap the operands")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)
```

- `options`: `files` (project files, fixed mtimes), `git` (init + one commit), `turns` (scripted model replies,
  consumed in order by chat requests), `responders` (functions answering any outbound request, e.g. webfetch
  URLs), `config` (overrides deep-merged into `lib/model.ts`), `projectConfig`, `timeout`, `events`, `server`.
- Before the body runs, the harness waits for the live event stream and for the project location's plugins
  to finish loading; after it, it waits for background work to go quiet. Neither step is recorded.
- `ctx.http(method, url, body?)` calls the API and records the exchange. Requests carry the project directory
  (`x-opencode-directory`) like real clients do.
- `ctx.session()`, `ctx.prompt(id, text)` (sends, then settles), `ctx.settle(id)` (waits for idle, or for a
  permission request / form; returns which), `ctx.until(predicate)` (waits for an event), `ctx.quiesce()`,
  `ctx.note(label, v)`, `ctx.write/read` (project files), `ctx.provider.requests`, `ctx.events`.
- Model turns (`lib/openrouter.ts`): `say(text, usage?)`, `call(tool, input, text?)`, or a full turn
  `{ events: [{reasoning}, {text}, {tool, input}, {tool, rawInput}, {comment}, {until: promise}], finish, usage, truncate }`,
  an HTTP error `{ status, body, headers }`, or a raw body `{ raw, status, headers }`.
- Title-generation requests are answered automatically. Outbound requests that nothing answers get a 599.
  Code paths that call `globalThis.fetch` directly (code-mode `fetch`) reach the same fake (`lib/preload.ts`).

The recording holds the HTTP transcript, the requests sent to OpenRouter (grouped by `prompt_cache_key`,
which child sessions share with their parent), the event stream (grouped by session), and the final project
files, all normalized.

## Rules

1. **Public interface only.** Never import opencode internals in a scenario. Inputs are HTTP calls, project
   files, config, and what the fake model says. That is what lets the same scenario run against Rust.
2. **Never hand-write expected values.** Goldens are recorded from the TS reference.
3. **Don't record a known bug as the standard.** When a golden shows upstream behaving wrongly, fix the
   reference with a deviation patch (`port/patches/README.md`) and re-record.
4. **Check the recording is meaningful.** Read the golden: a scenario that only exercises an argument-validation
   error because of a wrong field name proves nothing about the feature it is named after. Tool argument
   schemas are in any golden's provider request (`tools`).
5. **Deterministic or it doesn't count.** Fix nondeterminism in the scenario (fixed inputs, explicit
   ordering, waiting for the state you record), or as a documented equality rule. Never paper over a real
   behavior difference.
6. **No network.** Everything outbound goes through the fake; the sandbox (`lib/preload.ts`) isolates HOME,
   XDG dirs, the working directory, and the terminal/shell identity.
   Each test process gets one sandbox in `/tmp` and removes it when it ends (`CONFORMANCE_KEEP_SANDBOX=1`
   keeps it). Leave `TMPDIR` unset: TUI frames truncate paths by length, and a sandbox below a directory with
   opencode or Claude config (anywhere under `/home`) would pick that config up.

## Equality rules

Each rule states that the difference it hides is not observable behavior.

- **Generated identity:** IDs (incl. forked `msg_…_N`), UUIDs, 40/64-hex hashes and random session slugs get
  placeholders numbered by first appearance. A slug is named after its session.
- **Clock:** epoch-ms times, ISO times, and today's date in the system prompt's format (`Sat Nov 07 2026`)
  are masked; durations too.
- **Paging cursors** (base64 JSON, in bodies and in URLs) are compared decoded.
- **Host identity:** sandbox paths and their basenames, the real home (`os.homedir()` ignores HOME), hostname,
  OS release, process IDs.
- **Streaming batches:** consecutive text/reasoning/compaction deltas for the same part are joined (they flush
  on a 100 ms timer). Only the last tool-progress event per call is kept (throttled to one per 100 ms).
- **Engine diagnostics:** text the JavaScript engine writes, which opencode passes through inside its own
  messages, is masked as `<engine message>`. This covers `JSON.parse received invalid JSON: …`, `… received
  malformed URI data: …`, the reason in `…not a valid regular expression pattern (…)` and
  `…invalid flags "x" (…)`, and a schedule's `Invalid regular expression "(": …`. Each rule is anchored on
  opencode's own text, so any implementation's single-line diagnostic masks the same way. Built-in errors
  whose message is entirely the engine's (`toFixed()` range, `String.fromCodePoint`, `Number(symbol)`) are
  recorded by error type only, in the scenario itself.
- **Out of scope:** the startup probes of local model servers (LM Studio, Ollama, vLLM) are not recorded.
- **TUI frames** (`lib/tui.ts`): step timing and throughput, and the prompt's randomly chosen example text.
- **CLI** (`lib/cli.ts`): ports, sandbox paths, secrets, locale times, stack frames (documented in the file).

**Unspecified by the standard:** the order of multi-file glob/grep results (it follows ripgrep's parallel walk
and varies between runs upstream). Scenarios pin single-file matches.

## Reference configuration

`lib/model.ts` is the product the Rust side reproduces: OpenRouter only, one model defined in config (v2
config format), every agent on the `max` reasoning variant, no model catalog, no pricing, and out-of-scope
tools hidden (image generation, websearch, MCP resources). Experimental flags keep their release defaults
(off); scenarios for flagged features run only with `CONFORMANCE_EXPERIMENTAL=1`.

Dropped features still have endpoints the TUI calls (`/api/mcp`, `/api/mcp/resource`, `/api/plugin`,
`/api/experimental/migration/v1`, `/api/integration`): the Rust server must answer them exactly as the TS
reference does with nothing configured.

## TUI scenarios

TUI scenarios live in `tui/` and run from that directory (its `bunfig.toml` adds the SolidJS JSX transform):

```ts
import { say } from "../lib/openrouter"
import { tuiScenario } from "../lib/tui"

tuiScenario("prompt and reply", { turns: [say("Hello.")], width: 100, height: 30 }, async (t) => {
  await t.frame("home")
  await t.type("Say hello")
  await t.enter()
  await t.waitFor("Hello.")
  await t.frame("replied")
})
```

The real TUI (`packages/tui`) renders in a headless terminal and talks to the scenario's server over HTTP.
`t` has everything `ctx` has, plus `type`, `press(key)` (characters, KeyCodes names, raw sequences, or names
such as `pageup`, `f2`, `space`), `enter`, `escape`, `tab`, `arrow`, `paste`, `click(x, y)`, `scroll`, `mouse`,
`waitFor(text | regex, timeout)`, `resize(w, h)` and `frame(label)`, which records the settled screen.
`options` adds `width`, `height`, `args` (model, agent, prompt, continue, sessionID), `tui` (TUI config) and
`setup(ctx)` (runs against the server before the TUI starts; its result is merged into `args`). Recordings
also list the endpoints the TUI called (`tui-requests`).

Known limits: frames record characters, not colors or attributes; module-level TUI state (prompt drafts,
"--prompt applied once") persists across scenarios in one bun process, so scenarios start by clearing the
prompt.
