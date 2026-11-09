// The implementation a scenario drives, chosen with CONFORMANCE_IMPL:
//   unset or "in-process"  the TS reference booted inside the test process (default; `bun test --coverage`
//                          measures what the scenarios reach)
//   "ts"                   the TS reference's CLI as a separate process, `opencode serve`, run exactly the way
//                          a Rust build is run (this proves the process path against the goldens)
//   an absolute path       an opencode executable (the Rust build)
//
// A process is configured only through its public interface: the environment variables and flags `opencode
// serve` reads (packages/cli/src/server-process.ts), the sandbox HOME/XDG directories set up by lib/preload.ts,
// and the fake network (lib/network.ts). Each in-process boot option has its equivalent:
//   config.content            OPENCODE_CONFIG_CONTENT
//   config.project = false    OPENCODE_DISABLE_PROJECT_CONFIG=1
//   models fetch/snapshot off OPENCODE_DISABLE_MODELS_FETCH=1, OPENCODE_MODELS_PATH=<root>/models.json ({})
//   fs.filewatcher = false    OPENCODE_DISABLE_FILEWATCHER=1
//   fs.fff                    OPENCODE_DISABLE_FFF (the port omits fff: "1" unless a scenario turns fff on)
//   database ":memory:"       OPENCODE_DB=:memory: (from lib/preload.ts)
//   fetch                     HTTP(S)_PROXY to the fake network
//   persistent terminals      OPENCODE_PTY_BIN (the reference's opencode-pty daemon, which the server starts and
//                             owns) and OPENCODE_PTY_SOCKET_DIR (its socket, in the sandbox), from lib/preload.ts
//   app                       the oracle's build identity: name "cli", version "conformance", channel
//                             "conformance". Any channel but local and dev keeps experimental flags off, as in
//                             releases (packages/core/src/flag.ts). Not "latest": a release channel's default
//                             service port (49374) is the one a real installed service listens on, and a process
//                             that ignored the sandbox's pinned port would reach it; this channel's is 39820.
//                             The TS reference gets the identity as `--define`s (packages/cli/src/version.ts); a
//                             Rust build must be compiled with OPENCODE_VERSION=conformance and
//                             OPENCODE_CHANNEL=conformance
// Requests carry the server password (OPENCODE_SERVER_PASSWORD) as HTTP Basic credentials; in-process there is
// no authentication layer, and no scenario observes it.
//
// Records nothing itself. When CONFORMANCE_MEMORY_LOG names a file, the peak resident memory of every server
// process (and every TUI process, lib/tui.ts) is appended to it as one JSON line per scenario.
import { appendFileSync, existsSync, readdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs"
import path from "node:path"
import { fakeNetwork } from "./network"

export type Implementation =
  | { readonly kind: "in-process" }
  | {
      readonly kind: "process"
      readonly name: string
      readonly command: readonly string[]
      readonly env: Record<string, string>
    }

export const REPO = realpathSync(path.resolve(import.meta.dir, "../ref"))
export const ENTRY = path.join(REPO, "packages/cli/src/index.ts")
const PRELOAD = Bun.resolveSync("@opentui/solid/preload", path.join(REPO, "packages/cli"))

export function implementation(): Implementation {
  const value = process.env.CONFORMANCE_IMPL
  if (!value || value === "in-process") return { kind: "in-process" }
  if (value === "ts") return { kind: "process", name: "ts", command: [process.execPath, ENTRY], env: tsEnv() }
  if (path.isAbsolute(value) && existsSync(value))
    return { kind: "process", name: path.basename(value), command: [value], env: {} }
  throw new Error(
    `CONFORMANCE_IMPL must be "in-process", "ts" or an absolute path to an executable, not ${JSON.stringify(value)}`,
  )
}

/**
 * Environment for a TS reference process run from source. The CLI's TUI modules need the SolidJS transform that
 * packages/cli/bunfig.toml preloads (processes run in other directories, so it goes through BUN_OPTIONS, which
 * processes spawned by the CLI inherit), and the build-time identity of a release build is defined as the
 * release build defines it (packages/cli/src/version.ts), with the oracle's identity (see above). A compiled opencode binary has both built in.
 */
export function tsEnv(): Record<string, string> {
  const defines = { OPENCODE_VERSION: "conformance", OPENCODE_CHANNEL: "conformance", OPENCODE_ARTIFACT: "cli" }
  return {
    BUN_OPTIONS: [
      `--preload ${PRELOAD}`,
      ...Object.entries(defines).map(([key, value]) => `--define ${key}='${JSON.stringify(value)}'`),
    ].join(" "),
    BUN_RUNTIME_TRANSPILER_CACHE_PATH: path.join(process.env.CONFORMANCE_ROOT!, "bun-transpiler-cache"),
  }
}

/** Whether `opencode` is the TS reference's CLI run from source (in-process and "ts"); it needs tsEnv(). */
export function runsTs() {
  const impl = implementation()
  return impl.kind === "in-process" || impl.name === "ts"
}

/** The command that runs `opencode`: the TS reference's CLI through `entry`, or the executable under test. */
export function opencodeCommand(entry = ENTRY): readonly string[] {
  const impl = implementation()
  return runsTs() || impl.kind !== "process" ? [process.execPath, entry] : impl.command
}

export interface ServerSettings {
  readonly config: string
  readonly project: boolean
  readonly filewatcher: boolean
  readonly fff: boolean
  /** The fake network: every outbound request of the process. */
  readonly fetch: (request: Request) => Promise<Response>
  /** For the memory log. */
  readonly label: { readonly file: string; readonly scenario: string }
  /** Extra environment for this process (a restart's, e.g. OPENCODE_PTY_HANDOFF). */
  readonly env?: Record<string, string>
}

export interface ServerProcess {
  readonly handler: (request: Request) => Promise<Response>
  /** Where the process listens (it reports this in server info). */
  readonly origin: string
  readonly pid: number
  /** Stop the process and whatever it started; `keepDaemon` spares its terminal daemon (a restart). */
  stop(options?: { readonly keepDaemon?: boolean }): Promise<void>
}

// Captured at load: scenarios may swap the global fetch (to route an in-process server's direct calls), and
// requests to the implementation must not follow.
const send = globalThis.fetch
const PASSWORD = "conformance-password"
/** The `authorization` header every request to a server process carries. */
export const AUTHORIZATION = `Basic ${btoa(`opencode:${PASSWORD}`)}`

export async function startProcess(
  impl: Extract<Implementation, { kind: "process" }>,
  settings: ServerSettings,
): Promise<ServerProcess> {
  const root = process.env.CONFORMANCE_ROOT!
  const models = path.join(root, "models.json")
  if (!existsSync(models)) writeFileSync(models, "{}\n")
  const network = await fakeNetwork(settings.fetch)
  const env: Record<string, string> = {}
  for (const [key, value] of Object.entries(process.env)) if (value !== undefined) env[key] = value
  Object.assign(env, impl.env, network.env, {
    OPENCODE_CONFIG_CONTENT: settings.config,
    OPENCODE_DISABLE_MODELS_FETCH: "1",
    OPENCODE_MODELS_PATH: models,
    OPENCODE_DISABLE_FFF: settings.fff ? "0" : "1",
    OPENCODE_SERVER_PASSWORD: PASSWORD,
  })
  if (settings.env) Object.assign(env, settings.env)
  if (!settings.project) env.OPENCODE_DISABLE_PROJECT_CONFIG = "1"
  if (!settings.filewatcher) env.OPENCODE_DISABLE_FILEWATCHER = "1"

  const child = Bun.spawn([...impl.command, "serve", "--hostname", "127.0.0.1", "--port", "0"], {
    // The in-process server's working directory (lib/preload.ts).
    cwd: path.join(root, "cwd"),
    env,
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
    detached: true,
  })
  let stdout = ""
  let stderr = ""
  const drain = async (stream: ReadableStream<Uint8Array>, append: (text: string) => void) => {
    const decoder = new TextDecoder()
    for await (const chunk of stream) append(decoder.decode(chunk, { stream: true }))
  }
  // Keep only the tail: the pipes must be drained, but nothing here is recorded.
  void drain(child.stdout, (text) => (stdout = (stdout + text).slice(-64_000)))
  void drain(child.stderr, (text) => (stderr = (stderr + text).slice(-64_000)))

  const deadline = Date.now() + 30_000
  let base: string | undefined
  while (!base) {
    base = stdout.match(/server listening on (http:\/\/\S+)/)?.[1]
    if (base) break
    if (child.exitCode !== null || child.signalCode !== null || Date.now() > deadline) {
      network.stop()
      signal([-child.pid, child.pid, ...descendants(child.pid)], "SIGKILL")
      throw new Error(
        `${impl.name} serve did not start (exit ${child.exitCode ?? child.signalCode}):\n${stdout}\n${stderr}`,
      )
    }
    await Bun.sleep(10)
  }
  const origin = base.replace(/\/$/, "")

  const handler = async (request: Request) => {
    const url = new URL(request.url)
    const headers = new Headers(request.headers)
    headers.set("authorization", AUTHORIZATION)
    // Transparent: the real server compresses for clients that accept it (the TUI's fetch does), and the
    // response is passed on as sent; requests made in-process ask for none, as before.
    if (!headers.has("accept-encoding")) headers.set("accept-encoding", "identity")
    const body = request.method === "GET" || request.method === "HEAD" ? undefined : await request.arrayBuffer()
    return send(origin + url.pathname + url.search, {
      method: request.method,
      headers,
      body,
      redirect: "manual",
      decompress: false,
    })
  }

  return {
    handler,
    origin,
    pid: child.pid,
    async stop(options: { readonly keepDaemon?: boolean } = {}) {
      const memory = status(child.pid)
      // A restart keeps the server's terminal daemon running, as a real restart does: it outlives its server (a
      // detached process) and waits for a handoff's claim; nothing else of the process may survive it.
      const kept = options.keepDaemon ? daemons(child.pid).flatMap((pid) => [pid, ...descendants(pid)]) : []
      const tree = descendants(child.pid).filter((pid) => !kept.includes(pid))
      signal([-child.pid], "SIGTERM")
      await Promise.race([child.exited, Bun.sleep(5_000)])
      // Whatever is left (a server that ignored SIGTERM, tools that started their own process groups).
      signal([-child.pid, child.pid, ...tree, ...descendants(child.pid)], "SIGKILL")
      network.stop()
      const log = process.env.CONFORMANCE_MEMORY_LOG
      if (log && memory)
        appendFileSync(
          log,
          JSON.stringify({ impl: impl.name, role: "server", ...settings.label, peakKiB: memory.peak, rssKiB: memory.rss }) +
            "\n",
        )
    },
  }
}

/** Peak and current resident memory (KiB) of a live process. */
export function status(pid: number) {
  try {
    const text = readFileSync(`/proc/${pid}/status`, "utf8")
    const field = (name: string) => Number(text.match(new RegExp(`^${name}:\\s+(\\d+) kB`, "m"))?.[1])
    return { peak: field("VmHWM"), rss: field("VmRSS") }
  } catch {
    return undefined
  }
}

/** Every live descendant of a process (tools may start their own process groups). */
function descendants(pid: number) {
  const children = new Map<number, number[]>()
  for (const entry of readdirSync("/proc")) {
    if (!/^\d+$/.test(entry)) continue
    try {
      const stat = readFileSync(`/proc/${entry}/stat`, "utf8")
      const parent = Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1])
      const list = children.get(parent) ?? []
      list.push(Number(entry))
      children.set(parent, list)
    } catch {}
  }
  const out: number[] = []
  const walk = (current: number) => {
    for (const child of children.get(current) ?? []) {
      out.push(child)
      walk(child)
    }
  }
  walk(pid)
  return out
}

/** The opencode-pty daemons among a process's descendants (lib/preload.ts names the binary). */
export function daemons(pid: number) {
  return descendants(pid).filter((child) => {
    try {
      return readFileSync(`/proc/${child}/cmdline`, "utf8").split("\0")[0] === process.env.OPENCODE_PTY_BIN
    } catch {
      return false
    }
  })
}

/** Kill processes the scenario let outlive their server (a kept terminal daemon). */
export function kill(pids: readonly number[]) {
  signal(pids, "SIGKILL")
}

function signal(targets: readonly number[], name: NodeJS.Signals) {
  for (const target of targets) {
    try {
      process.kill(target, name)
    } catch {}
  }
}
