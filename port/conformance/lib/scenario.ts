// Scenario runner. A scenario drives an implementation only through its public interface (HTTP API, live
// event stream, the fake OpenRouter, project files) and records everything observable into a transcript,
// which is normalized and compared with a golden recorded from the TS reference.
//
// Implementations are pluggable (lib/server.ts, CONFORMANCE_IMPL): the TS reference in-process (the default, so
// `bun test --coverage` measures what the scenarios reach), or a process started like `opencode serve` (the TS
// reference's CLI, or the Rust build), configured only through its environment and reached over HTTP.
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
import { isAllowedCorsOrigin } from "../ref/packages/server/src/cors"
import { RequestSite } from "../ref/packages/server/src/request-site"
import { withoutParentSpan } from "../ref/packages/server/src/request-tracing"
import { createRoutes } from "../ref/packages/server/src/routes"
import type { ServerOptions } from "../ref/packages/server/src/options"
import { matchGolden } from "./golden"
import { config as makeConfig } from "./model"
import { normalizer } from "./normalize"
import { FakeOpenRouter, type Responder, type Turn } from "./openrouter"
import { AUTHORIZATION, daemons, implementation, kill, startProcess } from "./server"

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
   * (packages/cli/src/server-process.ts), except fff: file watcher on, fff search off (the port omits fff;
   * file search is ripgrep + fuzzysort, as with OPENCODE_DISABLE_FFF=1).
   */
  readonly server?: { readonly filewatcher?: boolean; readonly fff?: boolean }
  /**
   * Boot the project location and wait for its plugins before the body runs (default). Turn off for scenarios
   * whose subject is the first boot itself (project identity, location loading).
   */
  readonly boot?: boolean
  /**
   * The scenario drives processes of its own (lib/cli.ts): no server is started, and requests through the
   * scenario's server (`http`, `session`, `prompt`...) fail.
   */
  readonly standalone?: boolean
  /**
   * Why the scenario can't drive an implementation running as a process (lib/server.ts); it is skipped there.
   * Only for inputs a process can't be given through its public interface.
   */
  readonly inProcessOnly?: string
  /**
   * Why the scenario needs an implementation running as a process (CONFORMANCE_IMPL); it is skipped in-process. For
   * what only a real listener has, like WebSocket upgrades (`ctx.socket`).
   */
  readonly processOnly?: string
  /**
   * Outbound requests the implementation sends concurrently, so that their arrival order is timing (an npm
   * install's prefetched packuments and its tarball downloads). Within each recorded group, the requests the
   * predicate matches keep the positions they arrived in but fill them in a fixed order (by their recorded form):
   * they are compared as a multiset. Default: every request is compared in arrival order.
   */
  readonly unordered?: (request: { readonly method: string; readonly url: string }) => boolean
}

/** One frame a WebSocket received: text as sent, binary as bytes. */
export type Frame =
  | { readonly type: "text"; readonly data: string }
  | { readonly type: "binary"; readonly data: Uint8Array }

export interface SocketOptions {
  /** Extra request headers (the harness sends the server password unless `auth` is false). */
  readonly headers?: Record<string, string>
  /** Send the server password (default true). Clients that cannot set headers (browsers) present a ticket instead. */
  readonly auth?: boolean
  /** Scope the connection to the project with `x-opencode-directory`, as `http` does (default true). */
  readonly directory?: boolean
  /**
   * How a received frame is recorded (default: text as `{ text }`, binary as `{ binary: <utf-8> }`). Return
   * `undefined` to leave a frame out.
   */
  readonly record?: (frame: Frame) => unknown
  /**
   * Frames whose boundaries are timing rather than behavior (a terminal's output chunks): consecutive recorded
   * items for which this returns a key are joined into one, by concatenating that key's string.
   */
  readonly merge?: (item: unknown) => string | undefined
}

export interface Socket {
  /** Whether the upgrade succeeded. A rejected one is recorded as `opened: false` (the client sees no status). */
  readonly opened: boolean
  /** The recorded items received so far, merged as `merge` says. */
  readonly items: readonly unknown[]
  send(data: string | Uint8Array): void
  /** Resolve once the items received so far satisfy the predicate (rejects after `timeout` ms). */
  until(predicate: (items: readonly unknown[]) => boolean, timeout?: number): Promise<void>
  /** Resolve with the close code and reason once the connection closes. */
  readonly closed: Promise<{ readonly code: number; readonly reason: string }>
  /** Close from the client (code 1000) and wait for the close to complete. */
  close(): Promise<void>
}

export interface HttpResult {
  readonly status: number
  readonly body: any
}

export interface Ctx {
  readonly project: string
  /** The scenario's file (relative to port/conformance) and name, for logs that are not recorded. */
  readonly label: { readonly file: string; readonly scenario: string }
  /** The implementation's raw request handler (for drivers that need a real HTTP endpoint, like the TUI). */
  readonly handler: (request: Request) => Promise<Response>
  readonly provider: FakeOpenRouter
  /** Raw HTTP call against the implementation; recorded in the transcript. */
  http(method: string, url: string, body?: unknown, headers?: Record<string, string>): Promise<HttpResult>
  /**
   * Open a WebSocket to the implementation (only as a process: mark the scenario `processOnly`). Recorded in the
   * transcript: the URL, whether it opened, what was sent and received in order, and how it closed.
   */
  socket(url: string, options?: SocketOptions): Promise<Socket>
  /**
   * A raw WebSocket to the implementation (only as a process), carrying the server password; not recorded. For
   * drivers that pass a client's socket through (the TUI's terminal pane).
   */
  connect(url: string, headers?: Record<string, string>): WebSocket
  /**
   * Stop the server process and start a new one in its place, with extra environment (only as a process); the
   * event stream and `http` follow it. Recorded as a `restart` entry. The database is in memory, so the new server
   * starts without sessions: what survives is what lives outside it (a handed-off terminal daemon).
   */
  restart(env?: Record<string, string>): Promise<void>
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
  if (options.inProcessOnly && implementation().kind === "process") return test.skip(name, () => {})
  if (options.processOnly && implementation().kind !== "process") return test.skip(name, () => {})
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
  let serverOrigin = IN_PROCESS
  const events: unknown[] = []

  const settings = {
    config: JSON.stringify(makeConfig(options.config)),
    project: options.projectConfig ?? true,
    filewatcher: options.server?.filewatcher ?? true,
    fff: options.server?.fff ?? false,
    standalone: options.standalone ?? false,
  }
  const label = { file: path.relative(import.meta.dir + "/..", file), scenario: name }
  await withServer(
    settings,
    provider,
    label,
    async (handler, origin, replace) => {
      serverOrigin = origin
      const call = async (method: string, url: string, payload?: unknown, headers: Record<string, string> = {}) => {
        const response = await handler(
          new Request(new URL(url, IN_PROCESS), {
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
      let stream = options.standalone ? { cancel() {} } : await subscribe(handler, events)
      const until = async (predicate: (event: any) => boolean, from = 0, timeout = 20_000) => {
        const deadline = Date.now() + timeout
        while (Date.now() < deadline) {
          const found = events.slice(from).find(predicate)
          if (found) return found
          await new Promise((resolve) => setTimeout(resolve, 10))
        }
        throw new Error("timed out waiting for event")
      }
      // Title generation is background work outside the session's busy period: settling also waits until every
      // title the fake answered for the session has been applied (a rename), so it never races the scenario's
      // next request. Bounded: an answered title that changes nothing renames nothing.
      const titled = async (sessionID: string) => {
        const deadline = Date.now() + 10_000
        const expected = () => provider.titles.filter((title) => title.session === sessionID && title.answered).length
        const renamed = () =>
          events.filter((event: any) => event?.type === "session.renamed" && eventSession(event) === sessionID).length
        while (renamed() < expected() && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 10))
      }
      // The event stream reaches the harness on its own connection, so it can lag an HTTP response (a process's
      // does). Once a session has settled, wait until the stream has delivered every durable event the session's
      // log holds: a barrier, so `ctx.events.length` afterwards counts everything that turn published.
      const caughtUp = async (sessionID: string) => {
        const response = await handler(
          new Request(`${IN_PROCESS}/api/experimental/session/${sessionID}/log`, {
            headers: { accept: "text/event-stream", "x-opencode-directory": encodeURIComponent(project) },
          }),
        )
        if (response.status !== 200) return void (await response.text())
        // The log holds no payloads (the server keeps none), only the session's latest sequence.
        const last = Math.max(-1, ...[...(await response.text()).matchAll(/"seq":(\d+)/g)].map((match) => Number(match[1])))
        const deadline = Date.now() + 10_000
        while (last >= 0 && !delivered(events, sessionID, last) && Date.now() < deadline)
          await new Promise((resolve) => setTimeout(resolve, 10))
      }
      // Title generation first (lib/openrouter.ts, titleOrder): the server says whether a session is untitled.
      if (!options.standalone)
        provider.titleOrder = {
          async expects(session) {
            const result = await call("GET", `/api/session/${session}`)
            const info = result.body?.data ?? result.body
            return (
              result.status === 200 &&
              info?.parentID === undefined &&
              (info?.title === undefined || UNTITLED.test(String(info.title)))
            )
          },
          applied: (session) =>
            until((event) => event?.type === "session.renamed" && eventSession(event) === session, 0, 60_000).then(
              () => undefined,
              () => undefined,
            ),
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
      if (options.boot !== false && !options.standalone) await call("GET", "/api/agent")
      await quiesce()
      const sockets: Array<{ ws: WebSocket; mute: () => void }> = []
      const socket = async (url: string, socketOptions: SocketOptions = {}): Promise<Socket> => {
        if (origin === IN_PROCESS)
          throw new Error("ctx.socket needs the implementation as a process: mark the scenario processOnly")
        const record = socketOptions.record ?? recordFrame
        const merge = socketOptions.merge
        // What the transcript records, in order (sent and received), and the received items alone.
        const log: unknown[] = []
        const items: unknown[] = []
        const entry: { kind: string; url: string; opened: boolean; frames: unknown[]; closed?: unknown } = {
          kind: "socket",
          url,
          opened: false,
          frames: log,
        }
        transcript.push(entry)
        const append = (list: unknown[], item: unknown) => {
          const key = merge?.(item)
          const last = list.at(-1)
          if (key !== undefined && last !== undefined && merge?.(last) === key)
            list[list.length - 1] = { [key]: String((last as any)[key]) + String((item as any)[key]) }
          else list.push(item)
        }
        const ws = new WebSocket(origin.replace(/^http/, "ws") + url, {
          headers: {
            ...(socketOptions.directory === false ? {} : { "x-opencode-directory": encodeURIComponent(project) }),
            ...(socketOptions.auth === false ? {} : { authorization: AUTHORIZATION }),
            ...socketOptions.headers,
          },
        } as unknown as string[])
        ws.binaryType = "arraybuffer"
        let opened = false
        let recording = true
        sockets.push({ ws, mute: () => (recording = false) })
        const closed = new Promise<{ code: number; reason: string }>((resolve) => {
          ws.addEventListener("close", (event) => {
            const result = { code: event.code, reason: event.reason }
            if (opened && recording) entry.closed = result
            resolve(result)
          })
        })
        ws.addEventListener("message", (event) => {
          const frame: Frame =
            typeof event.data === "string"
              ? { type: "text", data: event.data }
              : { type: "binary", data: new Uint8Array(event.data as ArrayBuffer) }
          const item = recording ? record(frame) : undefined
          if (item === undefined) return
          append(log, item)
          append(items, item)
        })
        opened = await new Promise<boolean>((resolve) => {
          ws.addEventListener("open", () => resolve(true))
          ws.addEventListener("error", () => resolve(false))
          void closed.then(() => resolve(false))
          setTimeout(() => resolve(false), 10_000)
        })
        entry.opened = opened
        return {
          opened,
          items,
          send(data) {
            ws.send(data)
            log.push(typeof data === "string" ? { sent: data } : { sentBinary: Buffer.from(data).toString("base64") })
          },
          async until(predicate, timeout = 10_000) {
            const deadline = Date.now() + timeout
            while (!predicate(items)) {
              if (Date.now() > deadline) throw new Error(`timed out waiting on socket ${url}: ${JSON.stringify(items)}`)
              await new Promise((resolve) => setTimeout(resolve, 10))
            }
          },
          closed,
          async close() {
            ws.close(1000)
            await closed
          },
        }
      }
      const ctx: Ctx = {
        project,
        label,
        handler,
        provider,
        events,
        until,
        quiesce,
        socket,
        connect(url, headers = {}) {
          if (origin === IN_PROCESS) throw new Error("ctx.connect needs the implementation as a process")
          const ws = new WebSocket(origin.replace(/^http/, "ws") + url, {
            headers: { ...headers, authorization: AUTHORIZATION },
          } as unknown as string[])
          sockets.push({ ws, mute: () => {} })
          return ws
        },
        async restart(env = {}) {
          if (!replace)
            throw new Error("ctx.restart needs the implementation as a process: mark the scenario processOnly")
          stream.cancel()
          origin = serverOrigin = await replace(env)
          stream = await subscribe(handler, events)
          transcript.push({ kind: "restart" })
        },
        async http(method, url, payload, headers) {
          const result = await call(method, url, payload, headers)
          transcript.push({
            kind: "http",
            method,
            url,
            ...(payload === undefined ? {} : { request: payload }),
            ...result,
          })
          return result
        },
        async settle(sessionID, from = events.length) {
          // Idle, or blocked on a question to the user (permission request or form).
          const blocked = until(
            (event) =>
              (event?.type === "permission.asked" || event?.type === "form.created") &&
              eventSession(event) === sessionID,
            from,
            60_000,
          ).then((event) => event.type as string)
          const idle = call("POST", `/api/experimental/session/${sessionID}/wait`).then(() => "idle")
          const state = await Promise.race([idle, blocked])
          await caughtUp(sessionID)
          await titled(sessionID)
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
        async wait(sessionID) {
          const result = await ctx.http("POST", `/api/experimental/session/${sessionID}/wait`)
          await caughtUp(sessionID)
          return result
        },
      }
      await body(ctx)
      // Sockets the scenario left open close unrecorded (their entries keep no `closed`).
      for (const { ws, mute } of sockets) {
        mute()
        if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) ws.close()
      }
      // Let background work (titles, snapshots, location loads) finish before recording and shutdown.
      await quiesce()
      stream.cancel()
    },
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
    // The server's own address: a process's 127.0.0.1:<port>, or the in-process stand-in.
    server: serverOrigin,
  })
  const normalizedTranscript = normalize.value(transcript)
  const normalizedProvider = normalize.value(
    unorder(groupBy(provider.requests.filter(inScopeRequest), requestGroup, stripIndex), options.unordered),
  )
  const recorded = options.events === false ? [] : coalesce(events.filter(keepEvent))
  const grouped = groupBy(recorded, eventGroup, (event) => event)
  for (const key of Object.keys(grouped)) grouped[key] = orderConcurrentResults(grouped[key])
  const normalizedEvents = normalize.value(grouped)
  matchGolden(file, name, {
    transcript: normalizedTranscript,
    provider: normalizedProvider,
    events: normalizedEvents,
    files: normalize.value(snapshot(project)),
  })
}

/**
 * Runs `body` against a fresh server of the implementation under test (lib/server.ts): the TS reference
 * in-process, or a process started like `opencode serve`.
 */
async function withServer(
  settings: { config: string; project: boolean; filewatcher: boolean; fff: boolean; standalone: boolean },
  provider: FakeOpenRouter,
  label: { file: string; scenario: string },
  body: (
    handler: (request: Request) => Promise<Response>,
    origin: string,
    restart?: (env: Record<string, string>) => Promise<string>,
  ) => Promise<void>,
) {
  if (settings.standalone)
    return body(async () => {
      throw new Error("this scenario has no server (standalone)")
    }, IN_PROCESS)
  const impl = implementation()
  if (impl.kind === "process") {
    let server = await startProcess(impl, { ...settings, fetch: provider.fetch, label })
    // A restart stops the process and starts another in its place; the handler always reaches the current one. The
    // old server's terminal daemon is kept (for a handoff) and killed with the last server.
    const kept: number[] = []
    const restart = async (env: Record<string, string>) => {
      kept.push(...daemons(server.pid))
      await server.stop({ keepDaemon: true })
      server = await startProcess(impl, { ...settings, fetch: provider.fetch, label, env })
      return server.origin
    }
    try {
      await body((request) => server.handler(request), server.origin, restart)
    } finally {
      await server.stop()
      kill(kept)
    }
    return
  }
  await Effect.runPromise(
    Effect.scoped(
      Effect.gen(function* () {
        const handler = yield* boot({
          // The oracle's build identity (lib/server.ts): experimental flags off, as in releases.
          app: { name: "cli", version: "conformance", channel: "conformance" },
          database: { path: ":memory:" },
          config: { project: settings.project, content: settings.config },
          // No model catalog (live or bundled snapshot): the only models are the ones defined in config.
          models: { fetch: false, snapshot: false },
          fs: { filewatcher: settings.filewatcher, fff: settings.fff },
          fetch: provider.fetch,
        })
        yield* Effect.promise(() => body(handler, IN_PROCESS))
      }),
    ),
  )
}

/**
 * The TS reference, booted the way `opencode serve` boots it (packages/server/src/process.ts), minus the
 * listening socket: application routes, resumption of interrupted sessions, and the schedule runner.
 */
const boot = Effect.fn("Conformance.boot")(function* (options: ServerOptions) {
  const context = yield* Layer.build(
    // The in-process server's address (what its requests use), listed in server info as a real server's is.
    createRoutes(options, () => [IN_PROCESS], []).pipe(Layer.provide(HttpServer.layerServices)),
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

/** `ScenarioOptions.unordered`: the matching requests of each group refill their own positions in a fixed order. */
function unorder(groups: Record<string, unknown[]>, unordered: ScenarioOptions["unordered"]) {
  if (!unordered) return groups
  const out: Record<string, unknown[]> = {}
  for (const [key, requests] of Object.entries(groups)) {
    const matches = (request: unknown) => unordered(request as { method: string; url: string })
    const slots = requests.flatMap((request, index) => (matches(request) ? [index] : []))
    const sorted = slots
      .map((slot) => [JSON.stringify(requests[slot]), requests[slot]] as const)
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
      .map(([, request]) => request)
    const list = [...requests]
    slots.forEach((slot, k) => (list[slot] = sorted[k]))
    out[key] = list
  }
  return out
}

function requestGroup(request: { kind: string; body: unknown }) {
  const session = (request.body as { prompt_cache_key?: unknown } | undefined)?.prompt_cache_key
  return typeof session === "string" ? `${request.kind}:${session}` : request.kind
}

/** A WebSocket frame as recorded by default: text as `{ text }`, binary as `{ binary: <utf-8> }`. */
function recordFrame(frame: Frame) {
  return frame.type === "text" ? { text: frame.data } : { binary: new TextDecoder().decode(frame.data) }
}

function eventSession(event: unknown): string | undefined {
  const data = (event as { data?: Record<string, any> })?.data
  const id = data?.sessionID ?? data?.form?.sessionID ?? data?.request?.sessionID
  return typeof id === "string" ? id : undefined
}

const eventGroup = (event: unknown) => eventSession(event) ?? "global"

/**
 * Whether the stream has delivered every event of the session's durable log through sequence `last`. The stream
 * delivers a session's events in order, so a sequence at or past `last` proves it. Without one, the sequences
 * not delivered are either still on their way or durable events the stream never carries: the server's filter
 * (isOpenCodeEvent, server/src/event-feed.ts) keeps internal events off it, and the only one published is
 * `session.usage.recorded` (title and compaction calls). The server answers each with a public
 * `session.usage.updated`, after it commits; it also answers every step end, and every failed step that cost
 * something (core/src/session/projector.ts). So the usage updates beyond those the delivered steps explain prove
 * as many usage records delivered, and everything published before them.
 */
function delivered(events: readonly any[], sessionID: string, last: number) {
  const seqs = new Set(
    events.filter((event) => event?.durable?.aggregateID === sessionID).map((event) => Number(event.durable.seq)),
  )
  if ([...seqs].some((seq) => seq >= last)) return true
  const own = events.filter((event) => eventSession(event) === sessionID)
  const missing = last + 1 - [...seqs].filter((seq) => seq <= last).length
  const updates = own.filter((event) => event.type === "session.usage.updated").length
  const steps = own.filter(
    (event) =>
      event.type === "session.step.ended" ||
      (event.type === "session.step.failed" && event.data?.cost !== undefined && event.data?.tokens !== undefined),
  ).length
  return missing <= updates - steps
}

const IN_PROCESS = "http://opencode.local"

// An untitled session's title (packages/util/src/session-title-fallback.ts): none, or the root fallback.
const UNTITLED = /^New session - \d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/

async function subscribe(handler: (request: Request) => Promise<Response>, events: unknown[]) {
  const response = await handler(new Request(`${IN_PROCESS}/api/event`, { headers: { accept: "text/event-stream" } }))
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

/**
 * Equality rule: tools called in one step run concurrently, so the order in which their results arrive is timing.
 * Consecutive results of one step are compared in call order (by call ID); their durable sequence numbers keep
 * the order of arrival.
 */
function orderConcurrentResults(list: readonly unknown[]) {
  const RESULT = /^session\.tool\.(success|failed)$/
  const out = [...list] as Record<string, any>[]
  let start = 0
  while (start < out.length) {
    const step = out[start]?.data?.assistantMessageID
    let end = start
    while (end < out.length && RESULT.test(out[end]?.type) && out[end]?.data?.assistantMessageID === step) end++
    if (end - start > 1) {
      const run = out.slice(start, end)
      const seqs = run.map((event) => event.durable?.seq).sort((a, b) => a - b)
      run
        .sort((a, b) => String(a.data?.id).localeCompare(String(b.data?.id)))
        .forEach((event, index) => {
          out[start + index] =
            event.durable && seqs[index] !== undefined
              ? { ...event, durable: { ...event.durable, seq: seqs[index] } }
              : event
        })
    }
    start = Math.max(end, start + 1)
  }
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
