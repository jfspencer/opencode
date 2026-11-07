// Scenario runner. A scenario drives an implementation only through its public interface (HTTP API, live
// event stream, the fake OpenRouter, project files) and records everything observable into a transcript,
// which is normalized and compared with a golden recorded from the TS reference.
//
// Implementations are pluggable. Today: the TS reference in-process (so `bun test --coverage` measures what
// the scenarios reach). The CLI driver (lib/cli.ts) already runs real processes; the Rust implementation will
// run the same way.
import { test } from "bun:test"
import { spawnSync } from "node:child_process"
import {
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  readlinkSync,
  realpathSync,
  utimesSync,
  writeFileSync,
} from "node:fs"
import { homedir, hostname, release } from "node:os"
import path from "node:path"
import { Context, Effect, Layer } from "effect"
import { HttpEffect, HttpMiddleware, HttpRouter, HttpServer } from "effect/http"
import { Schedule } from "@opencode/core/schedule"
import { SessionRestart } from "@opencode/core/session/execution/restart"
import { isAllowedCorsOrigin } from "../../../packages/server/src/cors"
import { RequestSite } from "../../../packages/server/src/request-site"
import { withoutParentSpan } from "../../../packages/server/src/request-tracing"
import { createRoutes } from "../../../packages/server/src/routes"
import type { ServerOptions } from "../../../packages/server/src/options"
import { matchGolden } from "./golden"
import { config as makeConfig } from "./model"
import { normalizer } from "./normalize"
import { FakeOpenRouter, type Responder, type Turn } from "./openrouter"

export interface ScenarioOptions {
  /** Config overrides merged into the base OpenRouter-only config. */
  readonly config?: Record<string, unknown>
  /** Files to create in the project directory before the server starts. */
  readonly files?: Record<string, string>
  /** Initialise the project as a git repository with one commit. */
  readonly git?: boolean
  /** Scripted OpenRouter chat turns, consumed in order. */
  readonly turns?: readonly Turn[]
  readonly responders?: readonly Responder[]
  /** Load project config files (opencode.json, AGENTS.md...) from the project directory. */
  readonly projectConfig?: boolean
  readonly timeout?: number
  /** Don't record the event stream (for scenarios whose event interleaving is legitimately unordered). */
  readonly events?: boolean
  /**
   * Server process settings. Defaults are those of the real `opencode serve` on Linux
   * (packages/cli/src/server-process.ts): file watcher on, fff search on.
   */
  readonly server?: { readonly filewatcher?: boolean; readonly fff?: boolean }
  /**
   * Boot the project location and wait for its plugins before the body runs (default). Turn off for scenarios
   * whose subject is the first boot itself (project identity, location loading).
   */
  readonly boot?: boolean
}

export interface HttpResult {
  readonly status: number
  readonly body: any
}

export interface Ctx {
  readonly project: string
  /** The implementation's raw request handler (for drivers that need a real HTTP endpoint, like the TUI). */
  readonly handler: (request: Request) => Promise<Response>
  readonly provider: FakeOpenRouter
  /** Raw HTTP call against the implementation; recorded in the transcript. */
  http(method: string, url: string, body?: unknown, headers?: Record<string, string>): Promise<HttpResult>
  /** Record an extra observation under a label. */
  note(label: string, value: unknown): void
  write(file: string, content: string): void
  read(file: string): string
  /** Create a session in the project directory; returns its id. */
  session(payload?: Record<string, unknown>): Promise<string>
  /** Send a prompt, then settle (idle, or blocked on a permission request or form). */
  prompt(sessionID: string, text: string, payload?: Record<string, unknown>): Promise<HttpResult>
  /** Wait until the session is idle or blocked on the user; records and returns which. */
  settle(sessionID: string, fromEvent?: number): Promise<string>
  wait(sessionID: string): Promise<HttpResult>
  /** Resolve with the first event (from index `from`) matching the predicate. */
  until(predicate: (event: any) => boolean, from?: number, timeout?: number): Promise<any>
  /** Wait until no new event has arrived for `quiet` ms (background work has settled). */
  quiesce(quiet?: number, max?: number): Promise<void>
  /** Events received so far (raw, not normalized). */
  readonly events: readonly unknown[]
}

type Entry = { readonly kind: string } & Record<string, unknown>

export function scenario(name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) {
  const file = callerFile()
  test(name, () => run(file, name, options, body), options.timeout ?? 60_000)
}

async function run(file: string, name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) {
  const root = process.env.CONFORMANCE_ROOT!
  const project = realpathSync(mkdtempSync(path.join(root, "project-")))
  // Distinct, fixed modification times (some outputs are ordered by mtime).
  Object.entries(options.files ?? {}).forEach(([rel, content], index) => {
    mkdirSync(path.dirname(path.join(project, rel)), { recursive: true })
    writeFileSync(path.join(project, rel), content)
    const time = new Date(Date.UTC(2026, 0, 1, 0, 0, index))
    utimesSync(path.join(project, rel), time, time)
  })
  if (options.git) {
    git(project, "init", "-q", "-b", "main")
    git(project, "add", "-A")
    git(project, "commit", "-q", "--allow-empty", "-m", "initial")
  }
  const provider = new FakeOpenRouter(options.turns ?? [], options.responders ?? [])
  // Direct globalThis.fetch calls (see lib/preload.ts) reach this scenario's fake network too.
  const sandbox = globalThis as { __conformanceFetch?: typeof fetch }
  sandbox.__conformanceFetch = provider.fetch as typeof fetch
  const transcript: Entry[] = []
  const events: unknown[] = []

  await Effect.runPromise(
    Effect.scoped(
      Effect.gen(function* () {
        const handler = yield* boot({
          app: { version: "conformance" },
          database: { path: ":memory:" },
          config: { project: options.projectConfig ?? true, content: JSON.stringify(makeConfig(options.config)) },
          // No model catalog (live or bundled snapshot): the only models are the ones defined in config.
          models: { fetch: false, snapshot: false },
          fs: { filewatcher: options.server?.filewatcher ?? true, fff: options.server?.fff ?? true },
          fetch: provider.fetch,
        })
        const call = async (method: string, url: string, payload?: unknown, headers: Record<string, string> = {}) => {
          const response = await handler(
            new Request(new URL(url, "http://opencode.local"), {
              method,
              headers: {
                // Clients scope non-session requests to a project the same way.
                "x-opencode-directory": encodeURIComponent(project),
                ...(payload === undefined ? {} : { "content-type": "application/json" }),
                ...headers,
              },
              body: payload === undefined ? undefined : JSON.stringify(payload),
            }),
          )
          const text = await response.text()
          return { status: response.status, body: parse(text) }
        }
        // Subscribe before anything happens, and only proceed once the stream is live.
        const stream = yield* Effect.promise(() => subscribe(handler, events))
        const until = async (predicate: (event: any) => boolean, from = 0, timeout = 20_000) => {
          const deadline = Date.now() + timeout
          while (Date.now() < deadline) {
            const found = events.slice(from).find(predicate)
            if (found) return found
            await new Promise((resolve) => setTimeout(resolve, 10))
          }
          throw new Error("timed out waiting for event")
        }
        const quiesce = async (quiet = 150, max = 5_000) => {
          const deadline = Date.now() + max
          let seen = events.length
          let since = Date.now()
          while (Date.now() < deadline && Date.now() - since < quiet) {
            await new Promise((resolve) => setTimeout(resolve, 10))
            if (events.length !== seen) {
              seen = events.length
              since = Date.now()
            }
          }
        }
        // Boot the project location and wait for its plugins (agents, commands, skills...) to finish loading,
        // as a client would before using it. Unrecorded: it is setup, not part of the scenario.
        yield* Effect.promise(async () => {
          if (options.boot !== false) await call("GET", "/api/agent")
          await quiesce()
        })
        const ctx: Ctx = {
          project,
          handler,
          provider,
          events,
          until,
          quiesce,
          async http(method, url, payload, headers) {
            const result = await call(method, url, payload, headers)
            transcript.push({ kind: "http", method, url, ...(payload === undefined ? {} : { request: payload }), ...result })
            return result
          },
          async settle(sessionID, from = events.length) {
            // Idle, or blocked on a question to the user (permission request or form).
            const blocked = until(
              (event) =>
                (event?.type === "permission.asked" || event?.type === "form.created") && eventSession(event) === sessionID,
              from,
              60_000,
            ).then((event) => event.type as string)
            const idle = call("POST", `/api/experimental/session/${sessionID}/wait`).then(() => "idle")
            const state = await Promise.race([idle, blocked])
            transcript.push({ kind: "settled", sessionID, state })
            return state
          },
          note(label, value) {
            transcript.push({ kind: "note", label, value })
          },
          write(rel, content) {
            mkdirSync(path.dirname(path.join(project, rel)), { recursive: true })
            writeFileSync(path.join(project, rel), content)
          },
          read: (rel) => readFileSync(path.join(project, rel), "utf8"),
          async session(payload = {}) {
            const result = await ctx.http("POST", "/api/session", { location: { directory: project }, ...payload })
            if (result.status !== 200) throw new Error(`session.create failed: ${JSON.stringify(result.body)}`)
            return result.body.data?.id ?? result.body.id
          },
          async prompt(sessionID, text, payload = {}) {
            const from = events.length
            const result = await ctx.http("POST", `/api/session/${sessionID}/prompt`, { text, ...payload })
            await ctx.settle(sessionID, from)
            return result
          },
          wait: (sessionID) => ctx.http("POST", `/api/experimental/session/${sessionID}/wait`),
        }
        yield* Effect.promise(() => body(ctx))
        // Let background work (titles, snapshots, location loads) finish before recording and shutdown.
        yield* Effect.promise(() => quiesce())
        stream.cancel()
      }),
    ),
  )

  sandbox.__conformanceFetch = undefined
  // Normalize in a fixed order so placeholder numbering is anchored on the HTTP transcript, whose order the
  // scenario controls. Concurrent work (title generation, subagents) may interleave across sessions, so
  // provider traffic and events are grouped (provider traffic by prompt_cache_key, which child sessions share
  // with their parent; events by session); order within a group is behavior and is kept.
  const normalize = normalizer({
    project,
    root,
    home: process.env.HOME!,
    // Clients abbreviate paths (e.g. the TUI shows "/…/<root-name>/<project-name>").
    "project-name": path.basename(project),
    "root-name": path.basename(root),
    // os.homedir() ignores the sandbox HOME (it reads the password database), e.g. for `~` expansion.
    "real-home": homedir(),
    hostname: hostname(),
    "os-release": release(),
  })
  const normalizedTranscript = normalize.value(transcript)
  const normalizedProvider = normalize.value(groupBy(provider.requests.filter(inScopeRequest), requestGroup, stripIndex))
  const recorded = options.events === false ? [] : coalesce(events.filter(keepEvent))
  const normalizedEvents = normalize.value(groupBy(recorded, eventGroup, (event) => event))
  matchGolden(file, name, {
    transcript: normalizedTranscript,
    provider: normalizedProvider,
    events: normalizedEvents,
    files: normalize.value(snapshot(project)),
  })
}

/**
 * The TS reference, booted the way `opencode serve` boots it (packages/server/src/process.ts), minus the
 * listening socket: application routes, resumption of interrupted sessions, and the schedule runner.
 */
const boot = Effect.fn("Conformance.boot")(function* (options: ServerOptions) {
  const context = yield* Layer.build(
    createRoutes(options, () => [], []).pipe(Layer.provide(HttpServer.layerServices)),
  ).pipe(withoutParentSpan)
  yield* Effect.forkDetach(Context.get(context, SessionRestart.Service).resumeSuspendedSessions)
  yield* Context.get(context, Schedule.Service).run.pipe(Effect.forkScoped)
  return Context.get(context, HttpRouter.HttpRouter)
    .asHttpEffect()
    .pipe(
      RequestSite.middleware(options),
      HttpMiddleware.cors({ allowedOrigins: (origin) => isAllowedCorsOrigin(origin, options), maxAge: 86_400 }),
      HttpEffect.toWebHandlerWith(context),
    )
})

function groupBy<T>(items: readonly T[], key: (item: T) => string, map: (item: T) => unknown) {
  const out: Record<string, unknown[]> = {}
  for (const item of items) (out[key(item)] ??= []).push(map(item))
  return out
}

const stripIndex = ({ index: _index, ...request }: { index: number } & Record<string, unknown>) => request

function requestGroup(request: { kind: string; body: unknown }) {
  const session = (request.body as { prompt_cache_key?: unknown } | undefined)?.prompt_cache_key
  return typeof session === "string" ? `${request.kind}:${session}` : request.kind
}

function eventSession(event: unknown): string | undefined {
  const data = (event as { data?: Record<string, any> })?.data
  const id = data?.sessionID ?? data?.form?.sessionID ?? data?.request?.sessionID
  return typeof id === "string" ? id : undefined
}

const eventGroup = (event: unknown) => eventSession(event) ?? "global"

async function subscribe(handler: (request: Request) => Promise<Response>, events: unknown[]) {
  const response = await handler(new Request("http://opencode.local/api/event", { headers: { accept: "text/event-stream" } }))
  const reader = response.body!.getReader()
  const decoder = new TextDecoder()
  const connected = Promise.withResolvers<void>()
  let buffer = ""
  const pump = async () => {
    while (true) {
      const next = await reader.read().catch(() => ({ done: true, value: undefined }))
      if (next.done) return connected.resolve()
      buffer += decoder.decode(next.value, { stream: true })
      let boundary = buffer.indexOf("\n\n")
      while (boundary !== -1) {
        const frame = buffer.slice(0, boundary)
        buffer = buffer.slice(boundary + 2)
        const data = frame
          .split("\n")
          .filter((line) => line.startsWith("data:"))
          .map((line) => line.slice(5).trimStart())
          .join("\n")
        if (data) {
          const event = parse(data)
          events.push(event)
          if ((event as { type?: unknown })?.type === "server.connected") connected.resolve()
        }
        boundary = buffer.indexOf("\n\n")
      }
    }
  }
  void pump()
  await Promise.race([connected.promise, new Promise((resolve) => setTimeout(resolve, 5_000))])
  return { cancel: () => void reader.cancel().catch(() => undefined) }
}

// Startup probes of local model servers (LM Studio, Ollama, vLLM) belong to out-of-scope providers.
const LOCAL_PROBE = /^http:\/\/(127\.0\.0\.1|localhost):(1234|11434|8000)\//
const inScopeRequest = (request: { url: string }) => !LOCAL_PROBE.test(request.url)

// Volatile by contract or pure timing: not part of the behavior we compare.
const DROPPED_EVENTS = /^(server\.(connected|heartbeat)|.*\.heartbeat)$/
function keepEvent(event: unknown) {
  const type = (event as { type?: unknown })?.type
  return typeof type !== "string" || !DROPPED_EVENTS.test(type)
}

/**
 * Equality rules for streamed events, whose batching is timing:
 * - consecutive `*.delta` events that differ only in `delta` (same part) are one event with the joined text
 *   (deltas are flushed on a 100 ms timer);
 * - only the last `session.tool.progress` per tool call is kept (progress is throttled to one per 100 ms).
 */
function coalesce(events: readonly unknown[]) {
  const lastProgress = new Map<string, number>()
  events.forEach((event, index) => {
    const e = event as { type?: string; data?: Record<string, unknown> }
    if (e.type === "session.tool.progress") lastProgress.set(`${e.data?.sessionID}:${e.data?.id}`, index)
  })
  const out: Record<string, any>[] = []
  events.forEach((event, index) => {
    const e = event as Record<string, any>
    if (e?.type === "session.tool.progress" && lastProgress.get(`${e.data?.sessionID}:${e.data?.id}`) !== index) return
    const previous = out.at(-1)
    if (
      typeof e?.type === "string" &&
      e.type.endsWith(".delta") &&
      previous?.type === e.type &&
      typeof e.data?.delta === "string" &&
      typeof previous.data?.delta === "string" &&
      sameExceptDelta(previous.data, e.data)
    ) {
      out[out.length - 1] = { ...previous, data: { ...previous.data, delta: previous.data.delta + e.data.delta } }
      return
    }
    out.push(e)
  })
  return out
}

function sameExceptDelta(a: Record<string, unknown>, b: Record<string, unknown>) {
  const strip = ({ delta: _delta, ...rest }: Record<string, unknown>) => JSON.stringify(rest)
  return strip(a) === strip(b)
}

/** Project files after the scenario. Symlinks and special files are recorded as what they are. */
function snapshot(dir: string) {
  const out: Record<string, string> = {}
  const walk = (current: string) => {
    for (const entry of readdirSync(current).sort()) {
      if (entry === ".git") continue
      const full = path.join(current, entry)
      const rel = path.relative(dir, full)
      const stat = lstatSync(full)
      if (stat.isSymbolicLink()) out[rel] = `<symlink -> ${readlinkSync(full)}>`
      else if (stat.isDirectory()) walk(full)
      else if (stat.isFIFO()) out[rel] = "<fifo>"
      else if (stat.isSocket()) out[rel] = "<socket>"
      else if (stat.isFile()) {
        const content = readFileSync(full)
        out[rel] = content.length > 20_000 ? `<${content.length} bytes>` : content.toString("utf8")
      }
    }
  }
  walk(dir)
  return out
}

function git(cwd: string, ...args: string[]) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" })
  if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${result.stderr}`)
}

function parse(text: string) {
  if (!text) return null
  try {
    return JSON.parse(text)
  } catch {
    return text
  }
}

// The golden directory comes from the calling test file. Stack traces are capped at 10 frames by default,
// which under load can stop short of the test file; never fall back to a shared directory.
function callerFile() {
  const limit = Error.stackTraceLimit
  Error.stackTraceLimit = 200
  const stack = new Error().stack?.split("\n") ?? []
  Error.stackTraceLimit = limit
  const match = stack.map((line) => line.match(/\(?(\/[^():]+\.test\.tsx?)/)).find(Boolean)
  if (!match) throw new Error(`scenario(): could not determine the calling test file from:\n${stack.join("\n")}`)
  return match[1]
}
