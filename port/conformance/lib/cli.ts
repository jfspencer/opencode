// CLI driver. Runs the real opencode CLI (`packages/cli/src/index.ts`) as child processes, exactly as a user's
// shell would, and records what is observable from outside: argv, stdin, exit code, stdout and stderr, the
// requests the processes send to the fake OpenRouter (recorded by `scenario()`), files the commands produce, and
// HTTP exchanges with servers the CLI started. It is built on `scenario()`; the in-process server that
// `scenario()` boots is not used by CLI scenarios.
//
// Sandbox: every scenario gets its own directory (`<sandbox>`, under the run's CONFORMANCE_ROOT) used as HOME,
// XDG_{CONFIG,DATA,STATE,CACHE}_HOME and TMPDIR of every CLI process it starts, so sessions, credentials, service
// config and the service registration (XDG_STATE_HOME/opencode/service-*.json, the only way clients discover
// the background service) never leave the scenario. The user's real service is registered under their real
// XDG_STATE_HOME and is therefore invisible to these processes.
//
// How the CLI gets the reference configuration: as the user's global config file,
// `<sandbox>/config/opencode/opencode.json` (XDG_CONFIG_HOME), holding `lib/model.ts`'s config with
// `providers.openrouter.settings.baseURL` pointed at the fake OpenRouter (served with Bun.serve on a random
// port). No model catalog: `OPENCODE_DISABLE_MODELS_FETCH=1` turns off the live fetch and
// `OPENCODE_MODELS_PATH=<sandbox>/models.json` (an empty catalog, `{}`) replaces the bundled snapshot, so the
// only model is the configured one, as in-process.
//
// Sandbox setup that is not recorded (a Rust harness must do the same):
// - the config file above, `<sandbox>/models.json` = `{}`;
// - `opencode service set port <free port>`: the managed service (which most commands start on demand, as for
//   the user) listens on a port of its own instead of the channel default, so it can never collide with another
//   service on this machine. The port is recorded as `<service-port>`.
//
// Process hygiene: CLI processes are started through `<sandbox>/opencode.ts` (a one-line module importing the CLI
// entry), so every process the CLI spawns itself (standalone servers, the managed service, ACP's server) carries
// the sandbox path in its command line. Each direct child runs in its own process group, which is killed when the
// command ends; at the end of the scenario every process whose command line mentions the sandbox is stopped
// (its process group too), and the scenario fails if any survives.
//
// Equality rules (CLI records only, applied before `lib/normalize.ts`; each hides an address or a value that is
// chosen at random by the OS or the CLI, not behavior):
// - the fake OpenRouter's origin `http://127.0.0.1:<random port>` is written `<openrouter>`, and its requests
//   are recorded against OpenRouter's real origin (as in-process);
// - ports reserved by the scenario (`ctx.port()`) are written `<port-N>`, the service port `<service-port>`, and
//   any other `127.0.0.1:<port>` (servers the CLI binds to port 0: `serve --port 0`, private servers)
//   `127.0.0.1:<port>`; other hosts' ports are kept;
// - HTTP Basic credentials sent by `ctx.fetch` are recorded decoded (`Basic base64(opencode:<password>)`) so the
//   rules above apply to the password;
// - `<sandbox>` for the sandbox directory, `<repo>` for the repository (paths in stack traces);
// - random 32-byte secrets the CLI generates (43 base64url characters: service and server passwords) are
//   `<random-secret>`;
// - dates printed with the platform's locale formatting (`toLocaleString`) are `<locale-time>`;
// - in plain-text output, integers that are epoch milliseconds between 2020 and 2040 are `<time>` (the same rule
//   lib/normalize.ts applies to JSON numbers);
// - today's date (`YYYY-MM-DD`, UTC) is `<today>` and the current year in `stats`' "YYYY so far" is `<year>`;
//   `stats`' activity calendar is reduced to its count of active days, and its tool latencies (`durationP50`,
//   the p50 column) are `<duration>` (see `stats()` below);
// - JavaScript stack frames (`    at fn (file:line:col)`) and Bun's crash banner (`Bun v1.x (Linux x64)`) are
//   dropped: they describe the reference's runtime, not the CLI's behavior. When the last frame of an error opens
//   the block of its `[cause]` (`... {`), the brace stays, on the error's message line.
// Records: one `cli` note per command (argv, stdin, extra env with unset variables as null, signal sent, exit code,
// stdout, stderr), an `env` note when the scenario changes the environment of all its commands, `http` notes for
// `ctx.fetch`, `file` notes for `ctx.file`; provider requests and project files as in every scenario.
// Interactive runs (`ctx.terminal`) record the final screen of an 80x24 terminal instead of the byte stream.
// Output that is JSON (pretty-printed with 2 spaces, or compact) is recorded parsed, as `{ json }`, when
// re-serializing it reproduces the output byte for byte; newline-delimited JSON as `{ ndjson }` under the same
// condition; anything else as its exact lines (`text.split("\n")`, so a trailing newline shows as a final "").
import { spawnSync } from "node:child_process"
import { existsSync, mkdirSync, readdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs"
import path from "node:path"
import { config as makeConfig, OPENROUTER_BASE_URL } from "./model"
import { scenario, type Ctx, type ScenarioOptions } from "./scenario"

// The TS reference being driven (port/conformance/ref, set by port/scripts/link-deps.sh).
const REPO = realpathSync(path.resolve(import.meta.dir, "../ref"))
const ENTRY = path.join(REPO, "packages/cli/src/index.ts")
// The CLI's TUI modules (imported by its config layer) need the SolidJS transform that packages/cli/bunfig.toml
// preloads; CLI processes run in the project directory, so the preload is passed through BUN_OPTIONS, which
// processes spawned by the CLI inherit. A compiled opencode binary has the transform built in.
const PRELOAD = Bun.resolveSync("@opentui/solid/preload", path.join(REPO, "packages/cli"))

export interface CliOptions extends ScenarioOptions {
  /** Environment for every CLI process of the scenario, on top of the sandbox's (`undefined` unsets). */
  readonly env?: Record<string, string | undefined>
  /** Write the reference config as the global config file (default true). */
  readonly globalConfig?: boolean
}

export interface RunOptions {
  /** Text piped to stdin; without it stdin is /dev/null. */
  readonly stdin?: string
  /** Working directory (default: the project). */
  readonly cwd?: string
  /** Extra environment for this process (`undefined` unsets); recorded. */
  readonly env?: Record<string, string | undefined>
  readonly timeout?: number
}

export interface CliResult {
  readonly code: number | null
  readonly signal: string | null
  readonly stdout: string
  readonly stderr: string
}

export interface CliProcess {
  readonly pid: number
  /** Output so far. */
  readonly stdout: () => string
  readonly stderr: () => string
  /** Resolve with the first complete line (from now or earlier) matching, on stdout unless told otherwise. */
  line(match: RegExp | ((line: string) => boolean), stream?: "stdout" | "stderr", timeout?: number): Promise<string>
  write(text: string): void
  /** Close stdin. */
  end(): void
  /** Send a signal (default SIGTERM), wait for the exit and record the run. */
  stop(signal?: NodeJS.Signals, timeout?: number): Promise<CliResult>
  /** Wait for the process to exit by itself and record the run. */
  exited(timeout?: number): Promise<CliResult>
}

export interface TerminalStep {
  readonly waitFor: string
  readonly type: string
}

export interface CliCtx extends Ctx {
  /** The sandbox directory used as HOME / XDG dirs / TMPDIR by the scenario's CLI processes. */
  readonly sandbox: string
  /** Run `opencode <args>` to completion; records argv, stdin, extra env, exit code, stdout and stderr. */
  cli(args: readonly string[], options?: RunOptions): Promise<CliResult>
  /** Run independent commands concurrently; recorded in the order given. */
  parallel(commands: readonly (readonly string[] | ({ readonly args: readonly string[] } & RunOptions))[]): Promise<CliResult[]>
  /** Start `opencode <args>` and leave it running (serve, acp); recorded when it ends. */
  start(args: readonly string[], options?: Omit<RunOptions, "stdin"> & { stdin?: "pipe" | "ignore" }): CliProcess
  /**
   * Run `opencode <args>` in a pseudo-terminal (80x24) and answer its prompts: each step waits until the screen
   * shows `waitFor`, then types `type` ("\r" is Enter). Records argv, what was typed, the exit code and the final
   * screen (prompt redraws and spinner frames overwrite each other on a screen, so it does not depend on timing).
   */
  terminal(args: readonly string[], steps: readonly TerminalStep[], options?: Omit<RunOptions, "stdin">): Promise<{ code: number | null; screen: string[] }>
  /** HTTP request against a server the CLI started; recorded like `ctx.http`. */
  fetch(method: string, url: string, body?: unknown, headers?: Record<string, string>): Promise<{ status: number; body: any }>
  /** Reserve a free local TCP port; it is recorded as `<port-N>`. */
  port(): Promise<number>
  /** Record a file under the sandbox (e.g. "config/opencode/service-local.json"), or note its absence. */
  file(relative: string): void
}

const live = new Set<string>()
process.once("exit", () => {
  for (const sandbox of live) spawnSync("pkill", ["-KILL", "-f", "--", escapeRegex(path.join(sandbox, "opencode.ts"))])
})

export function cliScenario(name: string, options: CliOptions, body: (ctx: CliCtx) => Promise<void>) {
  // scenario() finds the golden directory from the calling test file in a stack trace, which intermittently
  // misses it under load (goldens then land in goldens/unknown). Name the file explicitly: bun test sets Bun.main
  // to the test file being loaded.
  const file = /\.test\.tsx?$/.test(Bun.main) ? Bun.main : new Error().stack?.match(/(\/[^():\s]+\.test\.tsx?)/)?.[1]
  const previous = Error.prepareStackTrace
  if (file) Error.prepareStackTrace = () => `Error\n    at (${file}:1:1)`
  try {
    register(name, options, body)
  } finally {
    Error.prepareStackTrace = previous
  }
}

function register(name: string, options: CliOptions, body: (ctx: CliCtx) => Promise<void>) {
  scenario(name, { timeout: 120_000, events: false, ...options }, async (ctx) => {
    const root = process.env.CONFORMANCE_ROOT!
    const sandbox = path.join(root, `cli-${path.basename(ctx.project)}`)
    for (const dir of ["home", "config", "data", "state", "cache", "tmp"]) mkdirSync(path.join(sandbox, dir), { recursive: true })
    const entry = path.join(sandbox, "opencode.ts")
    writeFileSync(entry, `import ${JSON.stringify(ENTRY)}\n`)
    writeFileSync(path.join(sandbox, "models.json"), "{}\n")
    live.add(sandbox)

    const upstream = new URL(OPENROUTER_BASE_URL)
    const openrouter = Bun.serve({
      hostname: "127.0.0.1",
      port: 0,
      idleTimeout: 0,
      async fetch(request) {
        const url = new URL(request.url)
        const payload = request.method === "GET" || request.method === "HEAD" ? undefined : await request.arrayBuffer()
        return ctx.provider.fetch(
          new Request(upstream.origin + url.pathname + url.search, { method: request.method, headers: request.headers, body: payload }),
        )
      },
    })
    const fakeOrigin = `http://127.0.0.1:${openrouter.port}`

    if (options.globalConfig !== false) {
      const reference = makeConfig(options.config) as { providers: { openrouter: { settings: Record<string, unknown> } } }
      reference.providers.openrouter.settings.baseURL = fakeOrigin + upstream.pathname
      mkdirSync(path.join(sandbox, "config", "opencode"), { recursive: true })
      writeFileSync(path.join(sandbox, "config", "opencode", "opencode.json"), JSON.stringify(reference, null, 2) + "\n")
    }

    const env: Record<string, string> = {
      PATH: process.env.PATH ?? "/usr/bin:/bin",
      HOME: path.join(sandbox, "home"),
      XDG_CONFIG_HOME: path.join(sandbox, "config"),
      XDG_DATA_HOME: path.join(sandbox, "data"),
      XDG_STATE_HOME: path.join(sandbox, "state"),
      XDG_CACHE_HOME: path.join(sandbox, "cache"),
      TMPDIR: path.join(sandbox, "tmp"),
      TZ: "UTC",
      LANG: "C.UTF-8",
      OPENROUTER_API_KEY: process.env.OPENROUTER_API_KEY!,
      OPENCODE_DISABLE_MODELS_FETCH: "1",
      // The port omits fff file search (ripgrep + fuzzysort instead).
      OPENCODE_DISABLE_FFF: "1",
      OPENCODE_MODELS_PATH: path.join(sandbox, "models.json"),
      BUN_OPTIONS: `--preload ${PRELOAD}`,
      BUN_RUNTIME_TRANSPILER_CACHE_PATH: path.join(root, "bun-transpiler-cache"),
    }
    for (const [key, value] of Object.entries(process.env))
      if (key.startsWith("GIT_") && value !== undefined) env[key] = value
    applyEnv(env, options.env)

    // Values replaced in every record, longest first.
    const replacements = new Map<string, string>([
      [fakeOrigin, "<openrouter>"],
      [sandbox, "<sandbox>"],
      [REPO, "<repo>"],
    ])
    const ports = new Map<number, string>()
    const children = new Set<number>()

    const scrubText = (input: string) => {
      // Spinner animation frames ("◒  Removing credential" then cursor-left + clear-line) only appear when an
      // operation outlasts a spinner tick: timing, not behavior. The settled line that follows is kept.
      let out = input.replace(/[◐◓◑◒][^\n]*?(\x1b)?\[999D(\x1b)?\[J/g, "")
      for (const [raw, placeholder] of [...replacements].sort((a, b) => b[0].length - a[0].length))
        out = out.split(raw).join(placeholder)
      for (const [port, placeholder] of ports) out = out.replace(new RegExp(`(?<![0-9])${port}(?![0-9])`, "g"), placeholder)
      out = out.replace(/127\.0\.0\.1:\d{2,5}\b/g, "127.0.0.1:<port>")
      out = out.replace(/\b\d{1,2}\/\d{1,2}\/\d{4}, \d{1,2}:\d{2}:\d{2}(\s?[AP]M)?/g, "<locale-time>")
      // Whole tokens only: a 13-digit run inside a hex ID is not a timestamp.
      out = out.replace(/(?<![0-9A-Za-z])\d{13}(?![0-9A-Za-z])/g, (raw) => (Number(raw) > 1.58e12 && Number(raw) < 2.21e12 ? "<time>" : raw))
      out = out.replace(/(?<![A-Za-z0-9_-])[A-Za-z0-9_-]{43}(?![A-Za-z0-9_-])/g, (raw) => (isCursor(raw) ? raw : "<random-secret>"))
      const now = new Date()
      out = out.split(now.toISOString().slice(0, 10)).join("<today>")
      out = out.split(`${now.getUTCFullYear()} so far`).join("<year> so far")
      return out
    }
    const scrubValue = (input: unknown, key = ""): unknown => {
      if (typeof input === "string") return scrubText(input)
      if (typeof input === "number") return /^duration/i.test(key) ? "<duration>" : (ports.get(input) ?? input)
      if (Array.isArray(input)) return input.map((item) => scrubValue(item, key))
      if (input && typeof input === "object")
        return Object.fromEntries(Object.entries(input).map(([name, value]) => [scrubText(name), scrubValue(value, name)]))
      return input
    }
    const recordOutput = (text: string): unknown => {
      if (text === "") return ""
      const json = asJson(text)
      if (json !== undefined) return { json: scrubValue(json) }
      const ndjson = asNdjson(text)
      if (ndjson !== undefined) return { ndjson: scrubValue(ndjson) }
      const lines: string[] = []
      for (const line of stats(scrubText(text)).split("\n")) {
        if (/^Bun v\d+\.\d+\.\d+ \(/.test(line)) continue
        if (!/^\s+at .+/.test(line)) {
          lines.push(line)
          continue
        }
        // A frame that opens an error's cause block hands its brace to the line before it.
        if (line.endsWith(" {") && lines.length > 0 && !lines[lines.length - 1].endsWith(" {")) lines[lines.length - 1] += " {"
      }
      return lines
    }

    const spawn = (args: readonly string[], options: RunOptions & { stdinMode?: "pipe" | "ignore" } = {}) => {
      const cwd = options.cwd ?? ctx.project
      const childEnv = { ...env, PWD: cwd }
      applyEnv(childEnv, options.env)
      const child = Bun.spawn([process.execPath, entry, ...args], {
        cwd,
        env: childEnv,
        stdin: options.stdin !== undefined ? new TextEncoder().encode(options.stdin) : (options.stdinMode ?? "ignore"),
        stdout: "pipe",
        stderr: "pipe",
        detached: true,
      })
      children.add(child.pid)
      return child
    }

    const record = (
      args: readonly string[],
      options: { stdin?: string; cwd?: string; env?: Record<string, string | undefined>; signal?: string },
      result: CliResult,
    ) => {
      ctx.note("cli", {
        argv: args.map(scrubText),
        ...(options.stdin !== undefined ? { stdin: scrubText(options.stdin) } : {}),
        ...(options.cwd !== undefined && options.cwd !== ctx.project ? { cwd: scrubText(options.cwd) } : {}),
        ...(options.env ? { env: scrubValue(unsetAsNull(options.env)) } : {}),
        ...(options.signal ? { sent: options.signal } : {}),
        exit: result.code,
        ...(result.signal ? { signal: result.signal } : {}),
        stdout: recordOutput(result.stdout),
        stderr: recordOutput(result.stderr),
      })
    }

    const finish = async (child: ReturnType<typeof spawn>, stdout: Promise<string>, stderr: Promise<string>, timeout: number) => {
      let timedOut = false
      const timer = setTimeout(() => {
        timedOut = true
        killGroup(child.pid)
      }, timeout)
      try {
        const [code, out, err] = await Promise.all([child.exited, stdout, stderr])
        if (timedOut) throw new Error(`opencode timed out after ${timeout}ms; stdout: ${out}; stderr: ${err}`)
        return { code, signal: child.signalCode ?? null, stdout: out, stderr: err } satisfies CliResult
      } finally {
        clearTimeout(timer)
        // Anything the command left in its own process group (it never starts the service there).
        killGroup(child.pid)
        children.delete(child.pid)
      }
    }

    const exec = (args: readonly string[], options: RunOptions) => {
      const child = spawn(args, options)
      return finish(child, new Response(child.stdout).text(), new Response(child.stderr).text(), options.timeout ?? 60_000)
    }

    const cli: CliCtx = {
      ...ctx,
      sandbox,
      async cli(args, options = {}) {
        const result = await exec(args, options)
        record(args, options, result)
        return result
      },
      async parallel(commands) {
        const items = commands.map((item) => ("args" in item ? item : { args: item as readonly string[] }))
        const results = await Promise.all(items.map((item) => exec(item.args, item)))
        items.forEach((item, index) => record(item.args, item, results[index]))
        return results
      },
      start(args, options = {}) {
        const child = spawn(args, { ...options, stdin: undefined, stdinMode: options.stdin ?? "pipe" })
        const buffers = { stdout: "", stderr: "" }
        let written: string | undefined
        const waiters: { stream: "stdout" | "stderr"; test: (line: string) => boolean; resolve: (line: string) => void }[] = []
        const check = () => {
          for (const waiter of [...waiters]) {
            const lines = buffers[waiter.stream].split("\n").slice(0, -1)
            const found = lines.find(waiter.test)
            if (found === undefined) continue
            waiters.splice(waiters.indexOf(waiter), 1)
            waiter.resolve(found)
          }
        }
        const pump = async (stream: ReadableStream<Uint8Array>, key: "stdout" | "stderr") => {
          const decoder = new TextDecoder()
          for await (const chunk of stream) {
            buffers[key] += decoder.decode(chunk, { stream: true })
            check()
          }
          return buffers[key]
        }
        const stdout = pump(child.stdout as ReadableStream<Uint8Array>, "stdout")
        const stderr = pump(child.stderr as ReadableStream<Uint8Array>, "stderr")
        let done: Promise<CliResult> | undefined
        const wait = (timeout: number, signal?: NodeJS.Signals) => {
          done ??= finish(child, stdout, stderr, timeout).then((result) => {
            record(args, { stdin: written, cwd: options.cwd, env: options.env, signal }, result)
            return result
          })
          return done
        }
        const stdin = child.stdin as import("bun").FileSink | undefined
        return {
          pid: child.pid,
          stdout: () => buffers.stdout,
          stderr: () => buffers.stderr,
          line(match, stream = "stdout", timeout = 30_000) {
            const test = typeof match === "function" ? match : (line: string) => match.test(line)
            return new Promise<string>((resolve, reject) => {
              const timer = setTimeout(() => {
                reject(new Error(`timed out waiting for ${stream} line ${match}; stdout: ${buffers.stdout}; stderr: ${buffers.stderr}`))
              }, timeout)
              waiters.push({ stream, test, resolve: (line) => (clearTimeout(timer), resolve(line)) })
              check()
            })
          },
          write(text) {
            written = (written ?? "") + text
            stdin?.write(text)
            stdin?.flush()
          },
          end() {
            stdin?.end()
          },
          stop(signal = "SIGTERM", timeout = 30_000) {
            if (!done) process.kill(child.pid, signal)
            return wait(timeout, signal)
          },
          exited: (timeout = 30_000) => wait(timeout),
        }
      },
      async terminal(args, steps, options = {}) {
        const cwd = options.cwd ?? ctx.project
        const childEnv = { ...env, PWD: cwd, TERM: "xterm-256color" }
        applyEnv(childEnv, options.env)
        let raw = ""
        const decoder = new TextDecoder()
        const child = Bun.spawn([process.execPath, entry, ...args], {
          cwd,
          env: childEnv,
          terminal: { cols: 80, rows: 24, data: (_terminal, data) => void (raw += decoder.decode(data, { stream: true })) },
        })
        children.add(child.pid)
        const timer = setTimeout(() => killGroup(child.pid), options.timeout ?? 60_000)
        try {
          for (const step of steps) {
            const deadline = Date.now() + 30_000
            while (!render(raw).join("\n").includes(step.waitFor)) {
              if (Date.now() > deadline) throw new Error(`terminal never showed ${JSON.stringify(step.waitFor)}: ${JSON.stringify(render(raw))}`)
              await Bun.sleep(20)
            }
            child.terminal!.write(step.type)
          }
          const code = await child.exited
          await Bun.sleep(50)
          const screen = render(raw)
          ctx.note("cli", {
            argv: args.map(scrubText),
            ...(options.cwd !== undefined && options.cwd !== ctx.project ? { cwd: scrubText(options.cwd) } : {}),
            ...(options.env ? { env: scrubValue(unsetAsNull(options.env)) } : {}),
            terminal: { cols: 80, rows: 24, typed: steps.map((step) => step.type) },
            exit: code,
            screen: screen.map(scrubText),
          })
          return { code, screen }
        } finally {
          clearTimeout(timer)
          child.terminal?.close()
          killGroup(child.pid)
          children.delete(child.pid)
        }
      },
      async fetch(method, url, payload, headers = {}) {
        const response = await fetch(url, {
          method,
          headers: { ...(payload === undefined ? {} : { "content-type": "application/json" }), ...headers },
          body: payload === undefined ? undefined : JSON.stringify(payload),
        })
        const text = await response.text()
        const result = { status: response.status, body: parseBody(text) }
        // Response headers that carry behavior (authentication challenges, CORS decisions).
        const kept = [...response.headers].filter(([key]) => key === "www-authenticate" || key.startsWith("access-control-"))
        ctx.note("http", {
          method,
          url: scrubText(url),
          ...(payload === undefined ? {} : { request: scrubValue(payload) }),
          ...(Object.keys(headers).length ? { headers: scrubValue(Object.fromEntries(Object.entries(headers).map(credentials))) } : {}),
          status: result.status,
          ...(kept.length ? { responseHeaders: scrubValue(Object.fromEntries(kept)) } : {}),
          body: scrubValue(result.body),
        })
        return result
      },
      async port() {
        const port = await freePort()
        ports.set(port, `<port-${[...ports.values()].filter((name) => name.startsWith("<port-")).length + 1}>`)
        return port
      },
      file(relative) {
        const full = path.join(sandbox, relative)
        ctx.note("file", { path: `<sandbox>/${relative}`, content: existsSync(full) ? recordOutput(readFileSync(full, "utf8")) : null })
      },
    }

    // Provider requests are recorded by scenario(); give them the same names for the sandbox and the fake.
    const scrubPaths = (input: unknown): unknown => {
      if (typeof input === "string") {
        let out = input
        for (const [raw, placeholder] of replacements) out = out.split(raw).join(placeholder)
        return out
      }
      if (Array.isArray(input)) return input.map(scrubPaths)
      if (input && typeof input === "object")
        return Object.fromEntries(Object.entries(input).map(([key, value]) => [key, scrubPaths(value)]))
      return input
    }

    try {
      // Sandbox setup (unrecorded): pin the managed service to a free port of its own.
      const servicePort = await freePort()
      ports.set(servicePort, "<service-port>")
      const setup = spawn(["service", "set", "port", String(servicePort)])
      const setupResult = await finish(setup, new Response(setup.stdout).text(), new Response(setup.stderr).text(), 60_000)
      if (setupResult.code !== 0) throw new Error(`sandbox setup failed: ${setupResult.stderr}`)
      // The scenario's own environment changes are part of its input: recorded once (unset variables as null).
      if (options.env) ctx.note("env", scrubValue(unsetAsNull(options.env)))
      await body(cli)
    } finally {
      const requests = ctx.provider.requests as unknown[]
      requests.splice(0, requests.length, ...requests.map(scrubPaths))
      await cleanup(sandbox, children)
      live.delete(sandbox)
      await openrouter.stop(true)
    }
  })
}

/** Stop every process the scenario started, directly or through the CLI; fail if any survives. */
async function cleanup(sandbox: string, children: Set<number>) {
  for (const pid of children) killGroup(pid)
  const pattern = escapeRegex(path.join(sandbox, "opencode.ts"))
  for (let attempt = 0; attempt < 100; attempt++) {
    const pids = pgrep(pattern)
    if (pids.length === 0) return
    for (const pid of pids) killGroup(pid)
    await Bun.sleep(50)
  }
  throw new Error(`processes left running: ${pgrep(pattern).join(", ")}`)
}

function killGroup(pid: number) {
  for (const target of [-pid, pid]) {
    try {
      process.kill(target, "SIGKILL")
    } catch {}
  }
}

function pgrep(pattern: string) {
  const result = spawnSync("pgrep", ["-f", "--", pattern], { encoding: "utf8" })
  return result.stdout
    .split("\n")
    .filter(Boolean)
    .map(Number)
    .filter((pid) => pid !== process.pid)
}

function escapeRegex(text: string) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
}

async function freePort() {
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response() })
  const port = server.port!
  await server.stop(true)
  return port
}

function unsetAsNull(env: Record<string, string | undefined>) {
  return Object.fromEntries(Object.entries(env).map(([key, value]) => [key, value ?? null]))
}

function applyEnv(target: Record<string, string>, overrides: Record<string, string | undefined> | undefined) {
  for (const [key, value] of Object.entries(overrides ?? {})) {
    if (value === undefined) delete target[key]
    else target[key] = value
  }
}

function asJson(text: string) {
  const trimmed = text.trimStart()
  if (!trimmed.startsWith("{") && !trimmed.startsWith("[")) return undefined
  try {
    const parsed = JSON.parse(text)
    if (text === JSON.stringify(parsed, null, 2) + "\n" || text === JSON.stringify(parsed) + "\n") return parsed
  } catch {}
  return undefined
}

function asNdjson(text: string) {
  if (!text.endsWith("\n")) return undefined
  const lines = text.slice(0, -1).split("\n")
  if (!lines.every((line) => line.startsWith("{"))) return undefined
  try {
    const parsed = lines.map((line) => JSON.parse(line))
    if (parsed.every((value, index) => JSON.stringify(value) === lines[index])) return parsed
  } catch {}
  return undefined
}

/**
 * The screen a terminal would show after `output`: enough of a VT100 for prompt libraries (cursor movement,
 * line and screen erasure; colors and modes are ignored). Returns the non-empty prefix of lines, right-trimmed.
 */
function render(output: string) {
  const lines: string[][] = [[]]
  let row = 0
  let col = 0
  const line = () => (lines[row] ??= [])
  const pattern = /\x1b\[([?0-9;]*)([A-Za-z])|\x1b[()][0-9A-Za-z]|\x1b[=>78]|\r|\n|\x07|\x08|[^\x1b\r\n\x07\x08]/gu
  for (const match of output.matchAll(pattern)) {
    const [token, params, command] = match
    if (command !== undefined) {
      const n = Number(params?.split(";")[0] || 1) || 1
      if (params?.startsWith("?") || command === "m") continue
      if (command === "A") row = Math.max(0, row - n)
      else if (command === "B") row += n
      else if (command === "C") col += n
      else if (command === "D") col = Math.max(0, col - n)
      else if (command === "G") col = Math.max(0, (Number(params) || 1) - 1)
      else if (command === "H") [row, col] = [(Number(params?.split(";")[0]) || 1) - 1, (Number(params?.split(";")[1]) || 1) - 1]
      else if (command === "K") {
        const mode = Number(params || 0)
        if (mode === 2) lines[row] = []
        else if (mode === 0) line().length = Math.min(line().length, col)
      } else if (command === "J") {
        if (Number(params || 0) === 0) {
          line().length = Math.min(line().length, col)
          lines.length = Math.min(lines.length, row + 1)
        }
      }
      continue
    }
    if (token === "\r") col = 0
    else if (token === "\n") row++
    else if (token === "\x08") col = Math.max(0, col - 1)
    else if (token.length === 1 || token.codePointAt(0)! > 0xffff) {
      if (token.startsWith("\x1b") || token === "\x07") continue
      const current = line()
      while (current.length < col) current.push(" ")
      current[col++] = token
    }
  }
  const text = Array.from({ length: lines.length }, (_, index) => (lines[index] ?? []).join("").trimEnd())
  while (text.length > 0 && text[text.length - 1] === "") text.pop()
  return text
}

/**
 * `stats` output that depends on the day it runs or on execution time: the activity calendar's layout (today's
 * position, the months shown) is reduced to the number of days it shows as active, and the tool table's p50
 * latency column to `<duration>`.
 */
function stats(text: string) {
  return text
    .replace(/^activity\n([\s\S]*?)\n(\s*less ·░▒▓█ more)$/m, (_, grid: string, legend: string) => {
      const active = [...grid].filter((char) => "░▒▓█".includes(char)).length
      return `activity\n<calendar: ${active} active ${active === 1 ? "day" : "days"}>\n${legend}`
    })
    .replace(/^TOOL RELIABILITY\n(?:.+\n?)*/m, (block) => block.replace(/ +\d+(\.\d+)?(ms|s|m)$/gm, " <duration>"))
}

function credentials([key, value]: [string, string]): [string, string] {
  if (key.toLowerCase() !== "authorization" || !value.startsWith("Basic ")) return [key, value]
  return [key, `Basic base64(${Buffer.from(value.slice(6), "base64").toString("utf8")})`]
}

function isCursor(text: string) {
  try {
    return typeof JSON.parse(Buffer.from(text, "base64url").toString("utf8")) === "object"
  } catch {
    return false
  }
}

function parseBody(text: string) {
  if (!text) return null
  try {
    return JSON.parse(text)
  } catch {
    return text
  }
}
