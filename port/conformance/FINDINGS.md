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

**TUI**
- D301: `@` completion of a directory gave `@src/util//`.
- D302: `--continue` with no sessions opened a placeholder session "dummy" with a validation-error toast.
- D303: `--agent`/`--model` were applied before agents and models loaded (`--agent plan` reported "Agent not
  found"; `--model` was ignored).
- D304: a theme that failed to load stayed saved in config.
- D305: the documented `session.tab.history.back/forward` keybinds had no command behind them.
- D306: keys typed in one burst read a stale cursor (`wow!` entered shell mode and dropped the `!`).
- D307: a form field with `hidden: true` was shown.
- D308: a failed attempt's retry notice stayed after a successful retry (also in `GET /message`: core and client
  projections kept `retry`).
- D309: a session whose directory is gone showed the raw model ID.

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

## Recorded behavior that is not a bug (evidence)

- `external_directory` is allowed by default, so reads outside the project and code-mode `session_move` to `..`
  ask no approval: documented (`permissions.mdx`), upstream #53585. With an `ask` rule all three paths ask
  (scenarios in codemode-tools and project-schedule).
- Code-mode `fetch` uses `globalThis.fetch`: documented in `server/src/options.ts`; the real server injects none.
- Agent request overlays (`agents.<id>.request`) are accepted but not sent: documented as not yet implemented
  (`agents.mdx`, `migrate-v1.mdx`). If upstream implements them, the goldens change.
- Reasoning without `reasoning_details` is not echoed back, and another model's reasoning is dropped after a
  switch: deliberate (upstream #37708, `to-llm-message.ts`).
- A `write` permission rule has no effect: `edit` governs write and patch (`permissions.mdx`, `migrate-v1.mdx`).
- `run --worktree name` keeps `name` in the message: values attach with `=` only, by design (`flag.ts`, #60712).
- Malformed percent-encoding answers 404 with an empty body on every route: Effect's router, before any handler.
- Hidden references stay in `/api/reference` and agent instructions: documented (`references.mdx`).
- A schedule whose `until` already matches completes without prompting: asserted by upstream tests and stated in
  the tool description.
- Code mode: an empty `browser` namespace is listed (asserted by core tests); `matchAll` returns an array, and
  functions print `[object Function]`, Date `toString` is ISO, etc. (documented in `interpreter-support.md`).

## Known and still open

- PowerShell equivalents of D101/D106 (`Invoke-Expression`/`iex`, options before the subcommand).
- A possible plan-mode race (a prompt before the creation reminder is queued); never observed.
- CLI errors print JS stack frames (`Cause.pretty`). The CLI driver masks frames, so they are not part of the
  standard; the Rust CLI prints messages only.
- `session import` reaches the server through `Effect.promise`, so a server failure there is a defect with a trace.
- `session export`'s interactive picker lists archived sessions.
- `structuredClone`'s DataCloneError is a TypeError with a prefix (upstream's documented convention).
- D302 leaves the `"dummy"` placeholder sentinel in place (harmless).
- D105/D110 risk: a model configured with audio input on OpenRouter would receive audio the OpenAI-chat request
  builder rejects. The reference config has no such model.

## Text that is not part of the standard

JavaScript-engine diagnostics (JSON, URI and regular-expression parser errors) are masked, and built-ins whose
message is entirely the engine's are recorded by error type only (`README.md`, equality rules). Runtime messages
that are cheap to reproduce stay specified: Node-style errno text (`ENOENT: no such file or directory, open 'x'`)
and Bun's connection error (`Unable to connect. Is the computer able to access the url?`).

## Nondeterminism inside TS itself

The standard leaves these unspecified; scenarios avoid them:
- Multi-file glob/grep result order (ripgrep's parallel walk).
- With the file watcher on, file changes during a location's first boot (worktree creation, `git init`, moving a
  checkout) race the watcher's reload; vcs routes can answer from not-yet-refreshed state.
- On a brand-new session, whether an agent-switch notice names the previous agent.
- Whether the TUI fetches a missing session's terminals before giving up.
- Title generation races a CLI `run` process exiting.
- A step starts with the first response chunk, after the request is sent; interrupting in between races.
- Text/reasoning deltas and tool progress are batched on 100 ms timers (normalized, see README).

## Harness limits (what the oracle cannot yet prove)

- **CLI line coverage**: the CLI runs as child processes, which Bun's coverage does not follow. Its behavior is
  recorded (70+ scenarios); coverage would need e.g. a Node build with `NODE_V8_COVERAGE`.
- **TUI colors**: frames record characters only; selection, syntax highlighting and theme colors are invisible.
- **Watcher-driven and schedule-firing behavior** has few dedicated scenarios.
- **Restarts** within a scenario (session recovery), persisted events, transport-level failures as first-class
  fake-model turns, and server disconnects are not yet expressible.
- **Host-specific goldens**: `/etc/shells`, `/usr/bin/sh`, dash/bash error text. The standard reproduces on this
  machine; CI needs the same shells.
- **Sandbox location**: sandboxes must live in `/tmp` (`TMPDIR` unset). TUI frames truncate paths by length, and
  a sandbox below a directory with opencode or Claude config (e.g. under `/home`) picks that config up.
