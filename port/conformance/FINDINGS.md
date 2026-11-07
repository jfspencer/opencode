# Findings from building the oracle (2026-11-07, upstream 6a89e9f)

The standard records the TS reference **as it behaves, bugs included**. The Rust implementation has to reproduce
everything below unless a deviation is decided deliberately, in which case the affected goldens are re-recorded
from a patched reference or excluded, and the decision is written down here.

## Behavior that looks like upstream bugs (recorded as-is)

**Security-relevant**
- Approving a wrapped command "always" saves the wrapper alone: `sudo rm -rf /var/cache` saves `sudo *`, which
  then allows every sudo command; same for `sh *`, `env *`, `xargs *`, `$(which git) *`.
- Reading files outside the project and code-mode `session_move` to `../x` or `~` ask no approval in this
  configuration. A schedule shell check `cd .. && ls` is allowed without an external-directory decision.
- Code-mode `fetch` calls `globalThis.fetch` directly, bypassing the server's injected fetch
  (`core/src/codemode/fetch.ts`).

**Requests to the model**
- Agent request overlays (`agents.<id>.request` headers/body) are accepted and listed but never applied.
- Plan mode makes an extra model call: its reminder is injected into the request and also queued as input.
- Reasoning streamed only as `reasoning_content` is not echoed back; reasoning from another model is dropped after
  a model switch.
- The title is the reply's first non-empty line, `<think>` tags included.
- An unsupported PDF's error text is glued to the prompt ("Read theseERROR: …"); unsupported audio and video
  attachments are dropped silently.

**API**
- `500 null` instead of 4xx: a relative `location.directory` on create; `GET /api/session?directory=relative`;
  `stats?from=yesterday`; importing a transcript whose message IDs exist (should be 409); `POST /api/shell`
  without `cwd`; `GET /api/shell/:id/output?limit=0`.
- `POST /api/session/:id/agent` with an unknown agent returns 204.
- `/api/command`, `/api/reference` and `/api/vcs*` answer before the location's plugins load (empty results right
  after boot); `/api/agent` and `/api/skill` wait.
- Rejecting a permission without a message interrupts with reason `"shutdown"` and leaves the session without an
  idle marker or outcome (execution claim kept).
- `PATCH title:""` does nothing when plugins haven't loaded yet.
- Malformed percent-encoding in `/api/fs/read/*` returns 404 with an empty body.
- Hidden references still appear in `/api/reference` and to the model.
- A schedule whose `until` already matches is recorded as fired but never prompts.
- Deleting a session with a running background subagent logs a foreign-key failure.
- Session import drops `info.revert`; compaction usage never reaches stats (events are not persisted by default).
- Removing a running shell tells the model `Shell.NotFoundError` (the bare tag).

**TUI**
- `@` completion of a directory gives `@src/util//`.
- `--continue` with no sessions opens a placeholder session "dummy" with a validation-error toast.
- `--agent`/`--model` are applied before agents/models load (`--agent plan` reports "Agent not found", `--model`
  is ignored).
- A theme that fails to load stays saved in config.
- `session.tab.history.back/forward` keybinds exist with no command behind them.
- Typing `wow!` in one burst enters shell mode and drops the `!`.
- A form field with `hidden: true` is still shown.
- A failed attempt's retry notice stays in the transcript after a successful retry.
- A session whose directory is gone shows the raw model ID.

**CLI**
- `run --archived` sessions still appear in `session list`.
- `session import` of an existing session prints an error but exits 0.
- An unknown command word is taken as the TUI's directory and fails with a raw `chdir` error and stack trace.
- Several errors print internal error chains or stack traces (`api` unknown operation, `--server` failures,
  `serve` on a taken port).
- `run --worktree name` (space) silently creates a randomly named worktree; only `--worktree=name` works.
- A `write` permission rule has no effect (the write tool is governed by `edit`).

**Code mode** (Rust must reproduce the legacy interpreter's semantics)
- Tool references compare unequal to themselves; `"models" in tools.opencode` throws; `typeof tools.opencode`
  is `"function"`; `Object.keys(tools)` lists an empty `browser` namespace.
- Deviations from JS: `Number.parseInt !== parseInt`; `Promise.try(5)` throws synchronously; no `%s` in
  `console.log`; `"a.b.".matchAll(".")` returns an array.
- Many error messages are Bun/JavaScriptCore host text passed through verbatim (regex, JSON.parse, URI,
  toFixed): a real porting cost.

## Nondeterminism inside TS itself

The standard leaves these unspecified; scenarios avoid them:
- Multi-file glob/grep result order (ripgrep's parallel walk).
- With the file watcher on, file changes during a location's first boot (worktree creation, `git init`, moving a
  checkout) race the watcher's reload; vcs routes can answer from not-yet-refreshed state.
- On a brand-new session, whether an agent-switch notice names the previous agent.
- Whether the TUI fetches a missing session's terminals before giving up.
- Title generation races a CLI `run` process exiting.
- Text/reasoning deltas and tool progress are batched on 100 ms timers (normalized, see README).

## Harness limits (what the oracle cannot yet prove)

- **CLI line coverage**: the CLI runs as child processes, which Bun's coverage does not follow. Its behavior is
  recorded (70 scenarios); coverage would need e.g. a Node build with `NODE_V8_COVERAGE`.
- **TUI colors**: frames record characters only; selection, syntax highlighting and theme colors are invisible.
- **Watcher-driven and schedule-firing behavior** has few dedicated scenarios; the watcher, fff and the schedule
  runner only became part of the reference late (they match the real server's defaults).
- **Restarts** within a scenario (session recovery), persisted events, transport-level failures as first-class
  fake-model turns, and server disconnects are not yet expressible.
- **Host-specific goldens**: `/etc/shells`, `/usr/bin/sh`, dash/bash error text. The standard reproduces on this
  machine; CI needs the same shells.
