// The TS side of the differential harness: runs each case of a JSONL file against the reference's
// @opencode/client (packages/client/src) and prints one JSON result per case. The Rust side
// (harness/rust/main.rs) prints the same for the crate; harness/diff.ts compares them.
//
// usage: bun --preload <@opentui/solid/preload> harness/ts/driver.ts <reference> <cases.jsonl>
import { readFileSync } from "node:fs"
import path from "node:path"
import * as env from "./env"

env.install()

const reference = process.argv[2]
const casesFile = process.argv[3]
const client = path.join(reference, "packages/client/src")
const { OpenCode } = await import(path.join(client, "promise/index.ts"))
const { createClientConnection } = await import(path.join(client, "solid/connection.ts"))
const solid = await import(Bun.resolveSync("solid-js", path.join(client, "solid")))
const { runData } = await import("./data")
const { runContext } = await import("./context")

type Step = Record<string, unknown> & { op: string }
type Case = Record<string, unknown> & { kind: string; name: string }

function makeClient(spec: Case) {
  return OpenCode.make({
    baseUrl: (spec.baseUrl as string) ?? "http://127.0.0.1:4096",
    headers: (spec.headers as Record<string, string> | undefined) ?? undefined,
    fetch: env.fetch,
  })
}

function recorded(from: number) {
  return env.requests.slice(from).map((request) => request.recorded)
}

async function runRequest(spec: Case) {
  const api = makeClient(spec)
  const op = (spec.op as string).split(".").reduce((node: any, key) => node?.[key], api)
  if (typeof op !== "function") throw new Error(`no operation ${spec.op}`)
  const controller = new AbortController()
  const options = { ...(spec.options as object), signal: spec.abort ? controller.signal : undefined }
  let settled: unknown
  // Operations without an input take the request options first.
  const args = op.length < 2 ? [options] : spec.input === undefined ? [undefined, options] : [spec.input, options]
  const abortWhen = (spec.abort as { when: string; reason?: { message: string } } | undefined) ?? undefined
  const reason = abortWhen?.reason ? env.errorValue({ name: "Error", message: abortWhen.reason.message }) : undefined
  if (abortWhen?.when === "before") controller.abort(reason)
  const promise = Promise.resolve()
    .then(() => op(...args))
    .then(
      (value: unknown) => (settled = env.outcome(value)),
      (error: unknown) => (settled = { error: env.failure(error) }),
    )
  // `op(...)` is called in a microtask here, so let it issue its request.
  await env.drain()
  const request = recorded(0)
  if (abortWhen?.when === "pending") controller.abort(reason)
  const response = spec.response as
    | ({ type: "respond" } & env.ResponseSpec)
    | { type: "fail"; error: { name: string; message: string } }
    | undefined
  if (env.requests[0] && !env.requests[0].settled && !controller.signal.aborted) {
    if (response?.type === "respond") env.respond(0, response)
    if (response?.type === "fail") env.fail(0, response.error)
  }
  await env.drain()
  if (abortWhen?.when === "body") controller.abort(reason)
  await env.drain()
  return { request, result: settled ?? { pending: true } }
}

async function runSse(spec: Case) {
  const api = makeClient(spec)
  const controller = new AbortController()
  let activity = 0
  const iterator = api.event
    .subscribe({
      types: spec.types as string[] | undefined,
      signal: controller.signal,
      onActivity: () => activity++,
    })
    [Symbol.asyncIterator]()
  const reads: unknown[] = []
  const response = spec.response as env.ResponseSpec
  const abortAfter = spec.abortAfter as number | undefined
  for (let i = 0; i < ((spec.reads as number) ?? 20); i++) {
    if (abortAfter !== undefined && i === abortAfter) controller.abort()
    let result: Record<string, unknown> | undefined
    iterator.next().then(
      (item: IteratorResult<unknown>) => (result = item.done ? { done: true } : { value: item.value }),
      (error: unknown) => (result = { error: env.failure(error) }),
    )
    await env.drain()
    if (i === 0 && env.requests[0] && !env.requests[0].settled) env.respond(0, response)
    await env.drain()
    // A read the stream cannot answer yet stays pending: abort to end it.
    const answered = result !== undefined
    if (!answered) {
      reads.push({ pending: true })
      controller.abort()
      await env.drain()
    }
    reads.push(result ?? { pending: true })
    if (!answered || "done" in result! || "error" in result!) break
  }
  await iterator.return?.()
  await env.drain()
  return { request: recorded(0), reads, activity, closed: env.requests[0]?.closed ?? false }
}

async function runConnection(spec: Case) {
  let api = makeClient(spec)
  const delivered: string[][] = []
  const reconnects: { resolve: (value: unknown) => void; reject: (error: unknown) => void }[] = []
  const logs: unknown[] = []
  return solid.createRoot(async (dispose: () => void) => {
    const connection = createClientConnection(api, {
      reconnect: spec.reconnect
        ? (signal: AbortSignal) =>
            new Promise((resolve, reject) => {
              reconnects.push({ resolve, reject })
              signal.addEventListener("abort", () => logs.push("reconnect signal aborted"), { once: true })
            })
        : undefined,
      onEvent: (event: { type: string }) => {
        const batch = delivered.at(-1)
        if (batch && (batch as unknown as { open: boolean }).open) batch.push(event.type)
        else {
          const fresh = Object.assign([event.type], { open: true })
          delivered.push(fresh)
          queueMicrotask(() => (fresh.open = false))
        }
      },
      flushInterval: spec.flushInterval as number | undefined,
      idleTimeout: spec.idleTimeout as number | undefined,
      log: { info: (message: string, data?: unknown) => logs.push([message, data ?? null]) },
    })
    const snapshots: unknown[] = []
    let seen = 0
    // Whether a request was cut is not compared here: after an attempt fails on its first event, whether the stream
    // still reads the rest of a chunk before the abort lands is a microtask race in JS.
    const snapshot = (step: Step) => {
      const requests = recorded(seen).map((request) => request.url)
      seen = env.requests.length
      const batches = delivered.splice(0).map((batch) => [...batch])
      snapshots.push({
        op: step.op,
        now: env.clock() - env.BASE_TIME,
        status: connection.status(),
        attempt: connection.attempt(),
        error: connection.error() ?? null,
        requests,
        batches,
        pendingReconnects: reconnects.length,
        logs: logs.splice(0),
      })
    }
    await env.drain()
    snapshot({ op: "start" })
    for (const step of spec.steps as Step[]) {
      const index = step.request === "last" ? env.requests.length - 1 : (step.request as number)
      switch (step.op) {
        case "respond":
          if (env.requests[index] && !env.requests[index].settled) env.respond(index, step as unknown as env.ResponseSpec)
          break
        case "chunk":
          env.push(index, step.data as string)
          break
        case "end":
          env.end(index, step.error as { name: string; message: string } | undefined)
          break
        case "fail":
          if (env.requests[index] && !env.requests[index].settled) env.fail(index, step.error as never)
          break
        case "advance":
          await env.advance(step.ms as number)
          break
        case "resync":
          connection.internal.resync(step.reason as string)
          break
        case "reconnect": {
          const pending = reconnects.shift()
          if (pending && step.ok) {
            api = makeClient(spec)
            pending.resolve(api)
          } else pending?.reject(env.errorValue({ name: "Error", message: "resolution failed" }))
          break
        }
        case "dispose":
          dispose()
          break
      }
      await env.drain()
      snapshot(step)
    }
    dispose()
    snapshots.push({ history: connection.internal.history().map((h: { created: number; data: unknown }) => ({ ...h, created: h.created - env.BASE_TIME })) })
    return { snapshots }
  })
}

async function runPty(spec: Case) {
  const { decodePersistentPtyMessage, persistentPtyFrame, createPersistentPtyClient } = await import(path.join(client, "solid/pty.ts"))
  const decoded = (spec.texts as string[]).map((text) => {
    const option = decodePersistentPtyMessage(text)
    return option._tag === "Some" ? option.value : null
  })
  const frames = (spec.frames as { cols: number; rows: number; data?: number[] }[]).map((frame) => [
    ...persistentPtyFrame({ cols: frame.cols, rows: frame.rows }, frame.data ? Uint8Array.from(frame.data) : undefined),
  ])
  const connect = spec.connect as { ptyID: string; cursor: number; attachmentID: string; takeover: boolean; url: string; ticket: string }
  const api = OpenCode.make({ baseUrl: connect.url, fetch: env.fetch })
  const opened: string[] = []
  const pty = createPersistentPtyClient(api, { url: connect.url, openSocket: (url: URL) => (opened.push(url.toString()), {}) as WebSocket })
  let error: unknown
  const pending = pty
    .connect({ ptyID: connect.ptyID, cursor: connect.cursor, attachmentID: connect.attachmentID, takeover: connect.takeover })
    .catch((e: unknown) => (error = env.failure(e)))
  await env.drain()
  if (env.requests[0]) env.respond(0, { status: 200, headers: [["content-type", "application/json"]], chunks: [JSON.stringify(connect.ticket === "missing" ? {} : { data: { ticket: connect.ticket } })] })
  await pending
  return { decoded, frames, request: recorded(0), url: opened[0] ?? null, error: error ?? null }
}

const cases = readFileSync(casesFile, "utf8")
  .split("\n")
  .filter(Boolean)
  .map((line) => JSON.parse(line) as Case)
for (const spec of cases) {
  if (process.env.SYNC_TRACE) console.error(spec.name)
  env.reset()
  env.resetRequests()
  let result: unknown
  try {
    if (spec.kind === "request") result = await runRequest(spec)
    else if (spec.kind === "sse") result = await runSse(spec)
    else if (spec.kind === "connection") result = await runConnection(spec)
    else if (spec.kind === "data") result = await runData(spec, { OpenCode, solid, reference })
    else if (spec.kind === "context") result = await runContext(spec, { OpenCode, solid, reference })
    else if (spec.kind === "pty") result = await runPty(spec)
    else throw new Error(`unknown case kind ${spec.kind}`)
  } catch (error) {
    result = { crashed: String(error instanceof Error ? error.stack : error) }
  }
  // Settle anything the case left running before the next one starts.
  await env.advance(120_000)
  process.stdout.write(JSON.stringify({ name: spec.name, result }) + "\n")
}
