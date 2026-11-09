// Records what reaches the TS TUI's data layer in a real run, as a data case the drivers can replay: the event
// batches it receives, the calls the TUI makes into it, the responses to the requests it makes itself, and the
// connection's status. Preloaded into the TS TUI process (harness/record/opencode.sh); writes one JSONL file per
// process to $SYNC_RECORD_DIR.
//
// The TUI's data context (packages/tui/src/context/data.tsx) is instrumented at load: its createData result, the
// API it reads through and the event bus it listens to are wrapped. Only the data layer's own requests are
// recorded (the TUI's direct client calls are not inputs of the data layer); their responses are buffered whole
// before the data layer sees them, so a response's place among the other steps is exact.
import { appendFileSync, mkdirSync } from "node:fs"
import path from "node:path"

const directory = process.env.SYNC_RECORD_DIR
const file = directory ? path.join(directory, `${path.basename(process.cwd())}-${process.pid}.jsonl`) : undefined
if (directory) mkdirSync(directory, { recursive: true })
const started = performance.now()
let last = 0

type Step = Record<string, unknown> & { op: string }
function log(step: Step) {
  if (!file) return
  const now = Math.round(performance.now() - started)
  if (now > last) appendFileSync(file, JSON.stringify({ op: "advance", ms: now - last }) + "\n")
  last = Math.max(last, now)
  appendFileSync(file, JSON.stringify(step) + "\n")
}

// ---- event batches: events delivered in one synchronous run belong to one flush --------------------------
let batch: unknown[] | undefined
function event(details: unknown) {
  if (!batch) {
    const current: unknown[] = []
    batch = current
    queueMicrotask(() => {
      batch = undefined
      log({ op: "events", events: current })
    })
  }
  batch.push(JSON.parse(JSON.stringify(details)))
}

// ---- the data layer's requests: tagged while its API runs synchronously ----------------------------------
let tagged = 0
const realFetch = globalThis.fetch
globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
  if (!tagged) return realFetch(input, init)
  const url = new URL(String(input instanceof Request ? input.url : input))
  const match = `${init?.method ?? "GET"} ${url.pathname}${url.search}`
  try {
    const response = await realFetch(input, init)
    const body = new Uint8Array(await response.arrayBuffer())
    const text = new TextDecoder().decode(body)
    const headers = [...response.headers].filter(([name]) => name === "content-type")
    log({ op: "respond", request: { match }, status: response.status, headers, chunks: text ? [text] : [] })
    return new Response(body, { status: response.status, statusText: response.statusText, headers: response.headers })
  } catch (error) {
    log({ op: "fail", request: { match }, error: { name: (error as Error).name, message: (error as Error).message } })
    throw error
  }
}) as typeof fetch

function tag<T extends object>(target: T): T {
  return new Proxy(target, {
    get(object, key, receiver) {
      const value = Reflect.get(object, key, receiver)
      if (typeof value === "function")
        return (...args: unknown[]) => {
          tagged++
          try {
            return value.apply(object, args)
          } finally {
            tagged--
          }
        }
      if (value && typeof value === "object") return tag(value)
      return value
    },
  })
}

// ---- calls into the data layer ---------------------------------------------------------------------------
const CALLS = new Set(["sync", "invalidate", "create", "compact", "prompt", "evict", "remember", "forget", "setStatus", "loadMore", "reply", "cancel", "refresh", "syncInfo"])
let calling = 0

function plain(value: unknown): unknown {
  if (typeof value === "function") return undefined
  if (Array.isArray(value)) return value.map(plain)
  if (value && typeof value === "object")
    return Object.fromEntries(Object.entries(value).flatMap(([k, v]) => (typeof v === "function" || v instanceof Promise ? [] : [[k, plain(v)]])))
  return value
}

// A copy of the data layer's API whose calls are logged: the data layer's own calls go through its original
// object, so only the TUI's calls are recorded.
function wrap(source: Record<string, unknown>, prefix: string, mint: () => string, session: () => string) {
  const target: Record<string, unknown> = {}
  for (const key of Object.keys(source)) {
    const value = source[key]
    const name = prefix ? `${prefix}.${key}` : key
    if (typeof value !== "function" && !(value && typeof value === "object" && !Array.isArray(value))) {
      Object.defineProperty(target, key, { get: () => source[key], enumerable: true })
      continue
    }
    if (typeof value === "function" && !CALLS.has(key)) {
      target[key] = (...args: unknown[]) => (value as (...a: unknown[]) => unknown).apply(source, args)
      continue
    }
    if (typeof value === "function") {
      target[key] = (...args: unknown[]) => {
        // A call made by another call of the data layer is part of it.
        if (calling) return value(...args)
        // The IDs the data layer would mint are given explicitly, so the replay mints the same ones.
        if (name === "session.prompt" && args[0] && typeof args[0] === "object" && (args[0] as { id?: string }).id === undefined)
          args[0] = { ...(args[0] as object), id: mint() }
        if (name === "session.create" && args[0] && typeof args[0] === "object" && (args[0] as { id?: string }).id === undefined)
          args[0] = { ...(args[0] as object), id: session() }
        log({ op: "call", path: name, args: plain(args) as unknown[] })
        calling++
        try {
          return (value as (...a: unknown[]) => unknown).apply(source, args)
        } finally {
          calling--
        }
      }
      continue
    }
    target[key] = wrap(value as Record<string, unknown>, name, mint, session)
  }
  return target
}

;(globalThis as Record<string, unknown>).__syncRecorder = {
  api: <T extends object>(api: T) => tag(api),
  event: (bus: { on: unknown; listen: (handler: (event: { details: unknown }) => void) => () => void }) => ({
    on: bus.on,
    listen: (handler: (event: { details: unknown }) => void) =>
      bus.listen((received) => {
        event(received.details)
        handler(received)
      }),
  }),
  data: (data: Record<string, unknown>, mint: () => string, session: () => string, directory: string) => {
    log({ op: "init", directory })
    return wrap(data, "", mint, session)
  },
  status: (status: string) => log({ op: "status", value: status }),
}

Bun.plugin({
  name: "opencode-tui-sync-recorder",
  setup(build) {
    build.onLoad({ filter: /packages[/\\]tui[/\\]src[/\\]context[/\\]data\.tsx$/ }, async (args) => {
      let source = await Bun.file(args.path).text()
      const edits: [string, string][] = [
        ["      api: () => withMediaRefs(client.api),", "      api: () => (globalThis as any).__syncRecorder.api(withMediaRefs(client.api)),"],
        ["      event: client.event,", "      event: (globalThis as any).__syncRecorder.event(client.event),"],
        ["    const data = createData({", "    const data = (globalThis as any).__syncRecorder.data(createData({"],
        [
          "    })\n    data satisfies Plugin.Context[\"data\"]",
          "    }), () => __SessionMessage.ID.create(), () => __SessionID.create(), props.directory)\n" +
            "    __createEffect(() => (globalThis as any).__syncRecorder.status(client.connection.status()))\n" +
            "    data satisfies Plugin.Context[\"data\"]",
        ],
      ]
      for (const [from, to] of edits) {
        if (!source.includes(from)) throw new Error(`recorder: cannot instrument ${args.path} (${from})`)
        source = source.replace(from, to)
      }
      source =
        'import { createEffect as __createEffect } from "solid-js"\n' +
        'import { SessionMessage as __SessionMessage } from "@opencode/schema/session-message"\n' +
        'import { SessionID as __SessionID } from "@opencode/schema/session-id"\n' +
        source
      return { contents: source, loader: "tsx" }
    })
  },
})
