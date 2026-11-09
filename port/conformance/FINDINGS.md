# Findings: upstream behavior the oracle recorded

The Rust port does not ship upstream bugs it knows about. Each bug below is fixed in the TS reference by a
deviation patch (`port/patches/`; the patch's commit message is the full record: what upstream does, what the
fix does, which scenarios show it, which upstream tests changed). The goldens are recorded from the patched
reference, so the standard requires the fixed behavior. Upstream's own in-scope suite passes on the patched
reference exactly as well as on plain upstream (`port/scripts/upstream-tests.sh`).

Status as of 2026-11-07, upstream 6a89e9f.

## Fixed by deviation patches

**Permissions and requests to the model**
- D101: approving a wrapped command "always" saved the wrapper alone (`sudo rm -rf x` saved `sudo *`). It now
  saves the wrapped command (`sudo rm *`); commands only the shell can identify are saved exactly.
- D102: plan mode's reminder was also queued as input, costing an extra model call after compaction or a revert.
- D103: generated titles kept `<think>` blocks (a regression of upstream fix #1325).
- D104: an unreadable attachment's error text was glued to the prompt ("Read theseERROR: …").
- D105: audio and video attachments were dropped silently; they now reach the model, which reports it cannot read
  them.
- D106: an option before the subcommand (`git -C src status`) saved a broad prefix (`git -C *`); the command is now
  saved exactly. Changes one upstream parity snapshot (AWS numeric option prefix).
- D107: a command named like a JavaScript object property (`toString x`) saved the pattern " *".
- D108: a command with a leading assignment (`FOO=1 git status`) saved a pattern that never matched it; it is now
  saved exactly, so it never approves another assignment (`LD_PRELOAD=x git status`).
- D109: `*` and `?` taken from command text acted as wildcards in saved patterns (`r? x` saved `r? *`, which
  matches `rm …`); they are now escaped (`[*]`, `[?]`).
- D110: attachments of unknown type and images other than png/jpeg/gif/webp were dropped silently; they are now
  reported to the model.
- D111: a symlink inside the project pointing outside escaped `external_directory` rules; containment now uses
  real paths. Reverses a deliberate upstream choice (symlinks reached through an internal path kept internal
  semantics); six upstream tests updated.
- D112: one-shot generations never retried a refused connection under Bun: the retry recognized Node's
  `ECONNREFUSED` only, and Bun's fetch reports `ConnectionRefused`. Found by giving the transport scenarios the
  errors Bun actually throws instead of Node-shaped ones.

**Tools** (D113–D122; found by the Rust crates' differential harnesses and the file-tools and shell slices)
- D113: webfetch HTML that ends inside markup: htmlparser2 8.0.2 replayed the last UTF-16 unit after `</div foo`
  or `<div/` (`ao`, or a lone surrogate), kept `<s`/`<t` as text, lost the text before a digitless `&#x`, and closed
  a tag it never opened, so html-markdown.ts closed the wrong frame (`<ul><li>a<b ` gave `- a**`). Fixed in a
  patched dependency (`patches/htmlparser2@8.0.2.patch` in the reference, as upstream patches others).
- D114: the 5 MiB markdown budget split surrogate pairs: an astral entity reached html-markdown.ts as two halves,
  and its 64 Ki-unit chunks could cut a pair; each half counted three bytes (a lone `\ud835` at the edge).
- D115: a page served as `Text/HTML` came back as raw HTML (case-sensitive media type check).
- D116: "always" on a quoted, escaped or path-qualified name saved a broad prefix: `'sudo' ls` saved `'sudo' *`
  (approving `'sudo' rm -rf /`), `'git' status` `'git' *`, `/usr/bin/git status` `/usr/bin/git *`.
- D117: a quoted or escaped cd (`'cd' /outside`) reported no directory, bypassing external_directory rules.
- D118: PowerShell `$env:toString` resolved an Object.prototype member as a directory.
- D119: a pattern's `?` matched one UTF-16 unit instead of one character (`a?` missed `a😀`).
- D120: grep's model listing had a blank line after every match (ripgrep's line terminator).
- D121: a CRLF file kept its `\r` in an unpaged read and lost it in a paged one; both drop it now.
- D122: grep on a FIFO or socket searched the directory around it; it fails like glob's wrong-kind path.

**HTTP API**
- D201: a relative `location.directory` (create, import, `GET /api/session?directory=`) answered `500 null`; now 400.
- D202: non-numeric stats ranges (`from=yesterday`) answered 500; now 400.
- D203: importing a transcript whose message IDs exist answered 500; now 409.
- D204: `POST /api/shell` with a missing cwd answered 500; now 400 `invalid_cwd`.
- D205: `GET /api/shell/:id/output?limit=0` answered 500; now an empty page.
- D206: switching a session to an unknown agent answered 204; now 404.
- D207: command, reference and vcs routes answered before the location's plugins loaded (empty right after boot).
- D208: `PATCH title: ""` did nothing before plugins loaded.
- D209: deleting a session with a running background subagent woke the deleted parent (foreign-key failure).
- D210: session import dropped `info.revert`.
- D211: compaction usage never reached session statistics.
- D212: removing a running shell told the model the bare tag `Shell.NotFoundError`.
- D213: declining a tool call or dismissing a question ended the turn as a "shutdown", keeping the execution
  claim (the declined turn resumed on the next server start); it is now a user interrupt.
- D214: stats ranges outside what a date can represent answered 500 (`from=-1e300`); now 400. Wide valid ranges
  query only the recorded history inside them.
- D215: creating a session with an unknown agent was accepted; now 404, like D206.
- D216: a spawn failure Bun throws directly (E2BIG: a command over 128 KiB) crashed the spawner, so
  `POST /api/shell` never answered and the shell tool hung the session for good.
- D217: `POST /api/shell` start failures answered 500 with an empty body; a NUL in the command, an oversized
  command and an inaccessible cwd are now 400s.
- D218: `POST /api/session/:id/shell` answered 500 with an empty body for the same failures; now the same 400s.
- D219: a path through a file (`GET /api/fs/read/README.md/x`, `GET /api/fs/list?path=README.md/x`, `POST
  /api/experimental/fs/watch`) answered 500 with an empty body: the lookup fails with ENOTDIR, which only NotFound
  handling missed. It now answers 404 `FileNotFoundError`, as a missing path does.
- D220: `PATCH /api/project/:id` with a relative `canonical` answered 500 with an empty body, even for an unknown
  project (the absolute-path column throws before the lookup); now 400 `InvalidRequestError`, like D201.
- D221: `GET /api/agent/:id` and `GET /api/provider/:id` answered before the location's plugins loaded: as a
  location's first request, 404 for `general` or `openrouter`, and `build` without its configured model and
  description. They now wait, like their list routes (D207).
- D222: `PUT /api/session/:id/environment` accepted a NUL, after which every shell command of the session failed
  as "The command contains a NUL character" on field `command`; the PUT is now a 400 `invalid_environment`.

**TUI**
- D301: `@` completion of a directory gave `@src/util//`.
- D302: `--continue` with no sessions opened a placeholder session "dummy" with a validation-error toast.
- D303: `--agent`/`--model` were applied before agents and models loaded (`--agent plan` reported "Agent not
  found"; `--model` was ignored). The CLI's root command no longer has these flags (nor `--fork`): the TUI still
  takes them as arguments, but no command passes them, so their scenarios run in-process only.
- D304: a theme that failed to load stayed saved in config.
- D305: the documented `session.tab.history.back/forward` keybinds had no command behind them.
- D306: keys typed in one burst read a stale cursor (`wow!` entered shell mode and dropped the `!`).
- D307: a form field with `hidden: true` was shown.
- D308: a failed attempt's retry notice stayed after a successful retry (also in `GET /message`: core and client
  projections kept `retry`).
- D309: a session whose directory is gone showed the raw model ID.
- D310: a command run by a key sequence (ctrl+x n, ctrl+x w) that navigated made the keymap revalidate the
  finished sequence while Solid disposed the old view; the layer conditions it read re-ran the route mid-disposal
  and the home screen that mounted was never cleaned up. Each new session or closed tab leaked five
  terminal-resize listeners, and at 16 the runtime printed `MaxListenersExceededWarning` over the TUI.
- D311: a jump in a list dialog (End right after opening settings) scrolled from stale row positions and left the
  selection off screen: a renderer that draws on demand had not laid out the rows below the viewport.

**CLI**
- D401: `run --archived` sessions still appeared in `session list`.
- D402: `session import` of an existing session exited 0. Upstream asserted exit 0 deliberately; changed because
  nothing is imported and every other failed import exits 1.
- D403: an unknown command word (taken as the TUI's directory) failed with a raw `chdir` error and stack trace.
- D404: `api` with an unknown operation printed an internal Effect error chain.
- D405: `--server` connection failures printed an error chain.
- D406: `serve` on a taken port printed an internal error.

**Code mode** (both the legacy interpreter and the experimental VM)
- D501–D503: tool references compared unequal to themselves, `in` threw on them, and `typeof` of a namespace was
  `"function"`.
- D504: `Number.parseInt !== parseInt` (also `parseFloat`).
- D505: `Promise.try(5)` threw synchronously instead of rejecting.
- D506: `console.log` ignored format specifiers (`%s %d %i %f %o %O %c %%`).
- D507: `String(Symbol.iterator)` printed an internal name.
- D508: symbols serialized as `{}` in `JSON.stringify` and program results.
- D509, D510: `Object.prototype.toString` tags for Math, JSON, console and async generators.
- D511: `JSON.parse(obj)` ignored the object's own `toString`.
- D512: `String.raw` rejected a string `raw`.
- D513: `btoa`/`atob` threw TypeError instead of `InvalidCharacterError`.
- D514–D516 (scanTools, found by the Rust codemode crate): a `var` in a class static block, and a function body's
  declarations, hid `tools` from the scan (so `function (a = tools.x()) { var tools }` failed as an unknown tool);
  paths came out in syntax-tree key order instead of first-seen order. Both engines reject classes, so D514 shows
  only where the scan is used alone (schedule-check validation); D516 is not observable through code mode.

## Recorded behavior that is not a bug (evidence)

- `external_directory` is allowed by default, so reads outside the project and code-mode `session_move` to `..`
  ask no approval: documented (`permissions.mdx`), upstream #53585. With an `ask` rule all three paths ask
  (scenarios in codemode-tools and project-schedule).
- Code-mode `fetch` uses `globalThis.fetch`: documented in `server/src/options.ts`; the real server injects none.
- Agent request overlays (`agents.<id>.request`) are accepted but not sent: documented as not yet implemented
  (`agents.mdx`: "preserves these values but does not yet send them with model requests"; `migrate-v1.mdx`). No
  V2 code has ever read them: they replaced `agents.<id>.options` in #30581, and requests merged only provider and
  model overlays before and after. If upstream implements them, the goldens change (project-agents "a custom agent
  shapes the provider request", agent-loop-agents "request overlays from provider, model and agent config").
- Reasoning without `reasoning_details` is not echoed back, and another model's reasoning is dropped after a
  switch: deliberate (upstream #37708, `to-llm-message.ts`).
- A `write` permission rule has no effect: `edit` governs write and patch (`permissions.mdx`, `migrate-v1.mdx`).
- A tool failure whose cause no rule classifies has type `unknown` (the read tool's binary, offset, path-kind and
  media-limit errors; webfetch's and shell's wrapped errors), while a failure without a cause is `tool.execution`.
  `toSessionError` classifies by the wrapped cause on purpose (#38367), upstream tests assert `unknown` for these
  (tool-read, tool-webfetch, tool-shell), and nothing reads the type, so it is left as designed.
- `run --worktree name` keeps `name` in the message: values attach with `=` only, by design (`flag.ts`, #60712).
- Malformed percent-encoding answers 404 with an empty body on every route: Effect's router, before any handler.
- Hidden references stay in `/api/reference` and agent instructions: documented (`references.mdx`).
- A schedule whose `until` already matches completes without prompting: asserted by upstream tests and stated in
  the tool description.
- Code mode: an empty `browser` namespace is listed (asserted by core tests); `matchAll` returns an array, and
  functions print `[object Function]`, Date `toString` is ISO, etc. (documented in `interpreter-support.md`).

## Known and still open

- PowerShell equivalents of D101/D106 (`Invoke-Expression`/`iex`, options before the subcommand).
- OpenRouter's attribution headers (`HTTP-Referer: https://opencode.ai/`, `X-Title: opencode`) reach a catalog
  provider but not a provider defined only in config, such as the reference's: the provider plugins
  (`plugin/provider/openrouter.ts`, also Cerebras, Kilo, ZenMux...) transform providers before the config plugin
  registers them (`plugin/internal.ts`, `pre` before `post`), so they never see one. V1 sent them for a
  config-defined `openrouter` too (#613: its custom loaders ran after config providers joined the provider
  table). The headers only attribute requests to opencode on OpenRouter; nothing
  user-visible changes. Not patched: a fix reorders plugin defaults and config overrides, and every golden with a
  provider request (655) would gain both headers.
- `POST /api/experimental/fs/write` answers 500 with an empty body for any write that fails (a path through a
  file, a directory, no permission): the endpoint declares no errors, so the right answer is a design choice.
- A possible plan-mode race (a prompt before the creation reminder is queued); never observed.
- CLI errors print JS stack frames (`Cause.pretty`). The CLI driver masks frames, so they are not part of the
  standard; the Rust CLI prints messages only.
- `session import` reaches the server through `Effect.promise`, so a server failure there is a defect with a trace.
- `session export`'s interactive picker lists archived sessions.
- `structuredClone`'s DataCloneError is a TypeError with a prefix (upstream's documented convention).
- D302 leaves the `"dummy"` placeholder sentinel in place (harmless).
- D105/D110 risk: a model configured with audio input on OpenRouter would receive audio the OpenAI-chat request
  builder rejects. The reference config has no such model.
- TUI: a subagent's permission request is attributed to the parent session, and its prompt shows the shell
  command (`$ rm -rf build`) only when the client happens to hold the tool call the request names; otherwise
  it lists the permission pattern (`- rm -rf build`). In-process it always shows the command; against a
  slower server, sometimes not. Not yet diagnosed (loading the requesting session's messages did not change
  it), so not patched; its two scenarios run in-process only.

## Text that is not part of the standard

JavaScript-engine diagnostics (JSON, URI and regular-expression parser errors) are masked, and built-ins whose
message is entirely the engine's are recorded by error type only (`README.md`, equality rules). So is the HTTP
client's wording of a failed connection (Bun's fetch: `The socket connection was closed unexpectedly…`, `Unable to
connect…`): opencode's words around it and the error code it names (`ECONNRESET`, `ConnectionRefused`) stay
specified, since opencode composes the message from the code and its retry decisions read it. Runtime messages
that are cheap to reproduce stay specified: Node-style errno text (`ENOENT: no such file or directory, open 'x'`,
and `getaddrinfo ENOTFOUND openrouter.ai`, whose text carries the code; its scenario runs in-process only).

## Nondeterminism inside TS itself

The standard leaves these unspecified; scenarios avoid them:
- Multi-file glob/grep result order (ripgrep's parallel walk).
- With the file watcher on, file changes during a location's first boot (worktree creation, `git init`, moving a
  checkout) race the watcher's reload; vcs routes can answer from not-yet-refreshed state.
- On a brand-new session, whether an agent-switch notice names the previous agent.
- Whether the TUI fetches a missing session's terminals before giving up.
- Title generation races a CLI `run` process exiting.
- A step starts with the first response chunk, after the request is sent; interrupting in between races.
- Title generation runs beside the session's first step and outside its busy period: where its events land
  among the step's, the running usage totals, and whether it finishes before the scenario's next request (a
  location reload can interrupt it, so the next prompt asks again). The fake model orders it first (README,
  equality rules); found by running the reference as a process.
- Tools called in one step run concurrently; their results arrive in either order (equality rule).
- A background subagent's failure report against the parent's next step.
- A prompt sent while a session's queued move has not run yet; a config write racing the file watcher's
  reload; repository changes made after the location booted (the branch read is the old one until the
  watcher catches up); a CLI signal sent before the CLI printed its header; a request rendered before a
  subagent's tool call loaded. Scenarios wait, boot later, or turn the watcher off.
- Which reads the TUI's data layer makes (it refetches on event bursts): writes only are recorded.
- Text/reasoning deltas and tool progress are batched on 100 ms timers (normalized, see README).
- A shell command's output joins stdout and stderr, two pipes read concurrently: a command that writes to both
  interleaves them in read order. Scenarios write to one stream per command.

## Harness limits (what the oracle cannot yet prove)

- **Process mode** (`CONFORMANCE_IMPL`, see README): every scenario can drive any `opencode` executable over
  its public interface; TUI scenarios run its TUI (`opencode --server`) in a pseudo-terminal. Both modes put
  the TUI in the same terminal (Ghostty's core) and send it the same bytes. What still differs: in-process the
  TUI shares an event loop with the server (timing), its file system is a no-op (`FileSystem.layerNoop`), and
  the console overlay is off; a process's stderr is on its screen.
- **The terminal** is libghostty-vt: OSC 10/11 color queries, XTWINOPS size reports and XTGETTCAP go
  unanswered (the full Ghostty application answers some of them); behavior that depends on those answers is
  what a terminal without them gets.

- **CLI line coverage**: the CLI runs as child processes, which Bun's coverage does not follow. Its behavior is
  recorded (70+ scenarios); coverage would need e.g. a Node build with `NODE_V8_COVERAGE`.
- **TUI colors**: frames record characters only; selection, syntax highlighting and theme colors are invisible.
- **Watcher-driven and schedule-firing behavior** has few dedicated scenarios.
- **PTYs**: nothing covers `pty.*` (REST and the WebSocket I/O of `pty-socket.ts`) or `experimental.persistentPty.*`
  (the TUI's terminal pane, a client of the `opencode-pty` daemon). The harness has no WebSocket client (the
  in-process handler cannot upgrade a request), and persistent PTYs run in a separate daemon.
- **Restarts** within a scenario (session recovery), persisted events and server disconnects are not yet
  expressible. Transport failures are: a connection closed before the response or dropped while it is read
  reaches a process as the real thing (the fake network closes the socket). A refused connection and a name
  that does not resolve cannot reach a process behind the fake network's proxy, so those three scenarios run
  in-process only (`inProcessOnly`).
- **Host-specific goldens**: `/etc/shells`, `/usr/bin/sh`, dash/bash error text. The standard reproduces on this
  machine; CI needs the same shells.
- **Sandbox location**: sandboxes must live in `/tmp` (`TMPDIR` unset). TUI frames truncate paths by length, and
  a sandbox below a directory with opencode or Claude config (e.g. under `/home`) picks that config up.
