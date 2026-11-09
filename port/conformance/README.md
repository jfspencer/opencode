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
CONFORMANCE_IMPL=ts bun test              # the TS reference as a separate `opencode serve` process
CONFORMANCE_IMPL=/abs/path/opencode bun test   # an opencode executable (the Rust build)
CONFORMANCE_ACTUAL=<dir> bun test; bun diff.ts <dir>   # write mismatching results, list differing JSON paths
CONFORMANCE_MEMORY_LOG=m.jsonl CONFORMANCE_IMPL=ts bun test; bun memory.ts m.jsonl   # server peak memory
CONFORMANCE_TUI_LOG=1 bun test <tui file>   # every request the TUI makes, with its status, on stderr
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

### Implementations (`CONFORMANCE_IMPL`, `lib/server.ts`)

By default the TS reference runs in-process. With `CONFORMANCE_IMPL=ts` (the TS reference's CLI from source) or
an absolute path to an executable (the Rust build), every scenario starts its own `opencode serve --hostname
127.0.0.1 --port 0` and talks to it over HTTP; CLI scenarios run that executable, and TUI scenarios run its TUI
(`opencode --server <url>`) in a pseudo-terminal against that server (see TUI scenarios). A process is configured
only through its public interface, so whatever passes here passes for a user:

- the sandbox HOME/XDG directories and environment of `lib/preload.ts`, and its working directory;
- the in-process boot options as the environment variables `opencode serve` reads (`OPENCODE_CONFIG_CONTENT`,
  `OPENCODE_DISABLE_PROJECT_CONFIG`, `OPENCODE_DISABLE_MODELS_FETCH` + an empty `OPENCODE_MODELS_PATH`,
  `OPENCODE_DISABLE_FILEWATCHER`, `OPENCODE_DISABLE_FFF`, `OPENCODE_DB=:memory:`); requests carry the
  `OPENCODE_SERVER_PASSWORD` credentials;
- the fake network (`lib/network.ts`) as a forward proxy (`HTTP(S)_PROXY`, empty `NO_PROXY`): plain HTTP is
  answered from the request line, HTTPS through a `CONNECT` tunnel to a local TLS server whose certificate a
  per-run CA signs (trusted through `NODE_EXTRA_CA_CERTS` and `SSL_CERT_FILE`). Requests keep their real
  origins, so they are recorded exactly as in-process, and nothing reaches the real network;
- the oracle's build identity: name `cli`, version `conformance`, channel `conformance`. Every channel but
  `local` and `dev` keeps experimental flags off, as releases do. A release channel (`latest`) is avoided on
  purpose: its default service port, 49374, is where an installed opencode service listens, and a process that
  ignored the sandbox's pinned service port would reach it; `conformance` defaults to 39820. The channel also
  names files (`service-conformance.json`, `opencode-conformance.db`). The in-process boot uses this identity,
  TS processes get it as `--define`s, and a Rust build must be compiled with `OPENCODE_VERSION=conformance
  OPENCODE_CHANNEL=conformance`.

A process run proves the harness path: the TS reference must reproduce every golden as a process before a
Rust build is measured against them. `CONFORMANCE_MEMORY_LOG=<file>` records the peak resident memory of each
server and TUI process; `bun memory.ts <file>...` summarizes it per implementation and role.

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
   behavior difference. A file change after the location booted reaches the server through its file watcher
   (in TS, 120–215 ms later), while the recording ends after 150 ms without events: wait for the events the
   change causes (`ctx.until`), make the change before the location's first request (`boot: false`), or turn
   the watcher off (`server: { filewatcher: false }`).
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
- **HTTP client diagnostics:** the HTTP client's own words for a failed connection, which opencode passes
  through, are masked as `<runtime message>` (Bun's fetch: ``The socket connection was closed unexpectedly. For more
  information, pass `verbose: true` …``, `Unable to connect. Is the computer able to access the url?`). The rule is
  anchored on opencode's text: the rest of the message after `Connection lost while reading the response: `,
  after a network error code and `: ` (`ECONNRESET: `, `ConnectionRefused: `; the runtime codes opencode knows), and
  after the CLI's `Could not reach server at <url>: `. The code itself stays exact: opencode writes it into the
  message and its retry decisions read it (a one-shot generation retries only codes of a connection that never
  opened). An implementation reports the same failures with the same codes, in its own words.
- **Out of scope:** the startup probes of local model servers (LM Studio, Ollama, vLLM) are not recorded.
- **Settling waits for the event stream:** the stream reaches the harness on its own connection and can lag
  an HTTP response. Once a session has settled (`ctx.prompt`, `ctx.settle`, `ctx.wait`), the harness reads the
  session's durable log and waits until the stream has delivered its last event, so `ctx.events` is complete.
  The log's last events can be usage records (`session.usage.recorded`, for a title or compaction call), which
  the server keeps off the stream; it answers each with a public `session.usage.updated`, and the harness counts
  those instead (`delivered` in `lib/scenario.ts`).
- **Title generation first:** a generated title runs beside the session's first step, so its events would
  land among the step's at random. The fake model holds an untitled session's first response until the title
  is applied (or its request failed): title events always come first, for any implementation. Settling a
  session (`ctx.prompt`, `ctx.settle`) also waits for every title the fake answered for it. CLI scenarios,
  whose servers are the CLI's own, hold nothing.
- **Concurrent tools:** tools called in one step run concurrently, so consecutive tool results of one step
  are compared in call order (by call ID), their durable sequence numbers in arrival order.
- **Server address:** a process's `http://127.0.0.1:<port>` and the in-process stand-in
  `http://opencode.local` (listed in server info as a real server lists its address) are both `<server>`.
- **HTTP client defaults:** a provider request's `Accept: */*` is not recorded. HTTP clients send it on the
  wire when the caller sets none, and a request handed over in-process has no header at all; the two are
  equivalent (RFC 9110 §12.5.1). The fake's responses are what a real server sends either way: a reason
  phrase, no body for `HEAD`. On the wire they also carry framing (`transfer-encoding: chunked`,
  `connection: close`); scenarios that print response headers leave framing headers out.
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

The real TUI talks to the scenario's server over HTTP (through a local proxy that notes which endpoints it
calls), in a real terminal: Ghostty's terminal core (libghostty-vt as WASM, from the `ghostty-web` package the
reference already depends on; `lib/terminal.ts`). The TUI's output is interpreted by it; its queries (device
attributes, modes, kitty keyboard flags, dark/light scheme) get Ghostty's answers; keys arrive as Ghostty's key
encoder writes them for the keyboard protocol the TUI turned on, mouse events as the reports it asked for (SGR),
pastes bracketed when it asked for that; frames are read from Ghostty's screen, and where the screen shows a
hyperlink its target is recorded beside the frame (`links:<label>`). Where the TUI runs follows `CONFORMANCE_IMPL`:

- **in-process** (default): `packages/tui`'s `run` on an opentui test renderer created with the TUI's own renderer
  options and attached to the terminal, inside the test process, so coverage measures it. Like the TUI's own
  renderer it draws on demand (no continuous render loop, no forced frames). It is given what
  `opencode --server <url>` gives it (`packages/cli/src/commands/handlers/default.ts`), including an updater that
  never checks on its own.
- **a process** (`ts`, or the Rust build): the implementation's CLI, `opencode --server <url>`, in a pseudo-terminal
  attached to the terminal; resizes reach it as a window resize (SIGWINCH), and the end of a scenario as a closed
  terminal (SIGHUP). Its inputs reach it through its public interface: `--continue`, `--session <id>`, `--prompt`,
  `--auto`; the TUI config as `<XDG_CONFIG_HOME>/opencode/cli.json`; `OPENCODE_DISABLE_AUTOUPDATE=1`;
  per-scenario XDG directories; the fake network. Whatever it writes to the terminal is on the screen, stderr
  included.

`t` has everything `ctx` has, plus `type`, `press(key, modifiers?)` (characters, opentui KeyCodes names, key names
such as `pageup`, `f2`, `space`, or the legacy sequence of one of those keys; modifiers `shift`, `ctrl`, `meta`
(alt), `super`), `enter`, `escape`, `tab`, `arrow`, `paste`, `click(x, y)`, `scroll`, `waitFor(text | regex,
timeout)`, `resize(w, h)` and `frame(label)`, which records the settled screen (and throws once the TUI has
exited). `options` adds `width`, `height`, `args` (`continue`, `sessionID` of an existing session,
`newSessionID`, `prompt`, `auto`; in-process also `model`, `agent`, `fork`, which no CLI flag reaches), `tui`
(TUI config) and `setup(ctx)` (runs against the server before the TUI starts; its result is merged into
`args`). Recordings also list the endpoints the TUI wrote to (`tui-requests`: every method but GET). Reads are
not recorded: the client's data layer refetches on bursts of events and reconciles events that overtake
responses, so which reads it makes is timing, and the frames show what it loaded.

Known limits: frames record characters, not colors or attributes; in-process, module-level TUI state (prompt
drafts, "--prompt applied once") persists across scenarios in one bun process, so scenarios start by clearing
the prompt, and the console overlay is off (it would capture the whole test process's console). The terminal is
libghostty-vt's: it does not answer OSC 10/11 color queries, XTWINOPS size reports or XTGETTCAP, as the full Ghostty
application would.

Debugging: `CONFORMANCE_TUI_RAW=<dir>` keeps everything the TUI wrote to its terminal and every input it was
sent (in-process, the terminal's answers too), per scenario; `CONFORMANCE_TUI_LOG=1` prints each request the TUI
makes.
