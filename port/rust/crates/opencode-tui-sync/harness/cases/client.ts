// Cases of the client milestone: operations (request building, responses, errors, aborts), the event stream
// (SSE parsing over arbitrary chunking) and the connection (reconnects, timeouts, backoff, batching).
import { operations } from "../gen-client"
import { Random } from "./random"

const BASE_URLS = ["http://127.0.0.1:4096", "http://127.0.0.1:4096/", "http://localhost:4096/prefix", "http://[::1]:80/a/b/"]
const STRINGS = ["", "a", "ses_abc", "a b", "a/b", "../x", "..", ".", "ü€😀", "%2F", "q?x=1#h", "x+y&z=w", "  pad  ", "tab\tnl\n"]

function value(random: Random, depth = 0): unknown {
  const kind = random.int(depth > 2 ? 4 : 7)
  if (kind === 0) return random.pick(STRINGS)
  if (kind === 1) return random.pick([0, 1, -1, 1.5, 1e21, 123456789, -0.000001])
  if (kind === 2) return random.bool()
  if (kind === 3) return null
  if (kind === 4) return Array.from({ length: random.int(3) }, () => value(random, depth + 1))
  const object: Record<string, unknown> = {}
  for (let i = random.int(3); i > 0; i--) object[random.pick(["directory", "a", "b", "workspaceID", "0", "x y"])] = value(random, depth + 1)
  return object
}

function scalar(random: Random) {
  return random.bool(0.7) ? random.pick(STRINGS) : value(random)
}

const JSON_BODIES = [
  "{}",
  '{"data":{"id":"x"}}',
  '{"data":[1,2,3],"cursor":{}}',
  "[1,2]",
  '"text"',
  "null",
  '{"x":"ü€😀"}',
  '{"data":null}',
  '{"nested":{"a":[{"b":1}]}}',
]

function chunked(random: Random, text: string) {
  const bytes = new TextEncoder().encode(text)
  if (!random.bool(0.4)) return [text]
  const out: string[] = []
  let at = 0
  while (at < bytes.length) {
    const size = 1 + random.int(Math.max(1, Math.min(8, bytes.length - at)))
    out.push("hex:" + Buffer.from(bytes.slice(at, at + size)).toString("hex"))
    at += size
  }
  return out.length ? out : [""]
}

function response(random: Random, op: ReturnType<typeof operations>[number]) {
  const kind = random.int(10)
  const json = [["content-type", random.pick(["application/json", "application/json; charset=utf-8", "Application/JSON", "application/problem+json"])]]
  if (kind <= 3) {
    const body = op.empty || op.binary ? random.pick(["", "{}", "bytes"]) : random.pick(JSON_BODIES)
    return { type: "respond", status: op.success, headers: op.binary ? [["content-type", "application/octet-stream"]] : json, chunks: chunked(random, body) }
  }
  if (kind === 4) {
    const status = random.pick(op.declared)
    const tag = random.pick(["SessionNotFoundError", "InvalidRequestError", "UnauthorizedError", ""])
    const body = random.pick([
      JSON.stringify({ _tag: tag, message: "Session not found: ses_x", sessionID: "ses_x" }),
      JSON.stringify({ _tag: tag, data: { message: "nested" } }),
      JSON.stringify({ name: "Custom", message: 5 }),
      "not json",
      "",
    ])
    return { type: "respond", status, headers: random.bool(0.8) ? json : [["content-type", "text/plain"]], chunks: chunked(random, body) }
  }
  if (kind === 5) return { type: "respond", status: random.pick([500, 418, 302, 201, 204]), headers: json, chunks: ["{}"] }
  if (kind === 6)
    return {
      type: "respond",
      status: op.success,
      headers: random.pick([[["content-type", "text/html"]], [], [["Content-Type", "text/plain; charset=utf-8"]]]),
      chunks: [random.pick(["<html>", "{}", ""])],
    }
  if (kind === 7)
    return { type: "respond", status: op.success, headers: json, chunks: chunked(random, random.pick(["{bad", "", "﻿{}", "  {}  ", '{"a":1}x'])) }
  if (kind === 8) return { type: "fail", error: { name: "TypeError", message: random.pick(["Unable to connect. Is the computer able to access the url?", "boom"]) } }
  return { type: "respond", status: op.success, headers: json, chunks: chunked(random, random.pick(JSON_BODIES)), end: "error", error: { name: "TypeError", message: "body failed" } }
}

export function requestCases(random: Random, count: number) {
  const ops = operations().filter((op) => !op.sse)
  const cases: unknown[] = []
  // Every operation once with a plain input and a success answer.
  for (const op of ops) {
    const input: Record<string, unknown> = {}
    for (const segment of op.path) if ("param" in segment) input[segment.param] = "ses_1"
    for (const [, key] of op.query) input[key] = key === "location" ? { directory: "/tmp/p" } : "1"
    for (const [, key] of op.headers) input[key] = "1"
    if (op.body.kind === "fields") for (const [, key] of op.body.fields) input[key] = key
    if (op.body.kind !== "none" && op.body.kind !== "fields") input[op.body.key] = "payload"
    cases.push({
      kind: "request",
      name: `request ${op.name.join(".")}`,
      baseUrl: BASE_URLS[0],
      op: op.name.join("."),
      input,
      response: { type: "respond", status: op.success, headers: [["content-type", "application/json"]], chunks: ['{"data":{"ok":true},"ok":true}'] },
    })
  }
  for (let i = 0; i < count; i++) {
    const op = random.pick(ops)
    const input: Record<string, unknown> = {}
    const keys = [
      ...op.path.flatMap((segment) => ("param" in segment ? [segment.param] : [])),
      ...op.query.map(([, key]) => key),
      ...op.headers.map(([, key]) => key),
      ...(op.body.kind === "fields" ? op.body.fields.map(([, key]) => key) : op.body.kind === "none" ? [] : [op.body.key]),
    ]
    for (const key of keys) if (random.bool(0.75)) input[key] = op.path.some((s) => "param" in s && s.param === key) ? scalar(random) : value(random)
    // A binary body is bytes in TS; JSON inputs carry it as a string.
    if (op.body.kind === "input" && op.body.binary && op.body.key in input) input[op.body.key] = random.pick(STRINGS)
    const abort = random.bool(0.15)
      ? { when: random.pick(["before", "pending", "body"]), ...(random.bool() ? { reason: { message: "Timed out connecting to server" } } : {}) }
      : undefined
    const answer = response(random, op) as Record<string, unknown>
    if (abort?.when === "body" && answer.type === "respond") answer.end = "open"
    cases.push({
      kind: "request",
      name: `request #${i} ${op.name.join(".")}`,
      baseUrl: random.pick(BASE_URLS),
      ...(random.bool(0.3) ? { headers: { authorization: "Basic b3BlbmNvZGU6eA==", "X-Extra": " v " } } : {}),
      op: op.name.join("."),
      ...(op.optional && random.bool(0.1) ? {} : { input }),
      ...(random.bool(0.2) ? { options: { headers: { "content-type": "text/plain", "x-opencode-directory": "/a b" } } } : {}),
      ...(abort ? { abort } : {}),
      response: answer,
    })
  }
  return cases
}

const EVENTS = [
  { id: "evt_1", type: "server.connected", data: {} },
  { type: "custom.event", data: { a: 1 } },
  { type: "session.text.delta", data: { sessionID: "ses_1", assistantMessageID: "msg_1", delta: "héllo 😀" } },
  { a: [1, 2, { b: "c" }] },
]

function sseText(random: Random) {
  const parts: string[] = []
  const nl = () => random.pick(["\n", "\r\n", "\r"])
  for (let i = random.int(6); i >= 0; i--) {
    const kind = random.int(8)
    const event = JSON.stringify(random.pick(EVENTS))
    if (kind <= 2) parts.push(`data: ${event}${nl()}${nl()}`)
    else if (kind === 3) parts.push(`: keepalive${nl()}${nl()}`)
    else if (kind === 4) parts.push(`event: message${nl()}id: 7${nl()}data:${event}${nl()}${nl()}`)
    else if (kind === 5) parts.push(`data: {"a":${nl()}data: [1,${nl()}data:  2]}${nl()}${nl()}`)
    else if (kind === 6) parts.push(`data: ${random.pick(["{bad", " {}", "﻿{}", '"str"'])}${nl()}${nl()}`)
    else parts.push(`data: ${event}`)
  }
  return parts.join("")
}

export function sseCases(random: Random, count: number) {
  const cases: unknown[] = []
  for (let i = 0; i < count; i++) {
    const text = sseText(random)
    const end = random.pick(["done", "done", "error", "open"])
    const status = random.bool(0.9) ? 200 : random.pick([401, 500])
    cases.push({
      kind: "sse",
      name: `sse #${i}`,
      baseUrl: random.pick(BASE_URLS),
      ...(random.bool(0.2) ? { types: ["custom.event"] } : {}),
      response: {
        status,
        headers: random.bool(0.9) ? [["content-type", random.pick(["text/event-stream", "text/event-stream; charset=utf-8"])]] : [["content-type", "application/json"]],
        chunks: chunked(random, text),
        end,
        error: { name: "TypeError", message: "The socket connection was closed unexpectedly." },
      },
      reads: 12,
      ...(end === "open" || random.bool(0.1) ? { abortAfter: random.int(6) } : {}),
    })
  }
  return cases
}

const SSE = [["content-type", "text/event-stream"]]
const connected = 'data: {"id":"evt_c","type":"server.connected","data":{}}\n\n'
const event = (n: number) => `data: {"id":"evt_${n}","type":"custom.${n}","data":{}}\n\n`

/** Hand-picked connection scripts: the timings TS's reconnect loop is defined by. */
export function connectionCases() {
  const open = (extra: string[] = []) => ({ op: "respond", request: "last", status: 200, headers: SSE, chunks: [connected, ...extra], end: "open" })
  const cases: { name: string; steps: unknown[]; [key: string]: unknown }[] = [
    { name: "connects and batches events", steps: [open([event(1)]), { op: "chunk", request: "last", data: event(2) }, { op: "advance", ms: 10 }, { op: "chunk", request: "last", data: event(3) + event(4) }, { op: "advance", ms: 9 }, { op: "advance", ms: 1 }] },
    { name: "times out connecting", steps: [{ op: "advance", ms: 1999 }, { op: "advance", ms: 1 }, { op: "advance", ms: 1000 }, open(), { op: "advance", ms: 10 }] },
    { name: "first event is not server.connected", steps: [{ op: "respond", request: "last", status: 200, headers: SSE, chunks: [event(1)], end: "open" }, { op: "advance", ms: 1000 }, open(), { op: "advance", ms: 20 }] },
    { name: "server error status", steps: [{ op: "respond", request: "last", status: 500, headers: [["content-type", "application/json"]], chunks: ["{}"] }, { op: "advance", ms: 999 }, { op: "advance", ms: 1 }, open()] },
    { name: "transport failure", steps: [{ op: "fail", request: "last", error: { name: "TypeError", message: "Unable to connect. Is the computer able to access the url?" } }, { op: "advance", ms: 1000 }, { op: "fail", request: "last", error: { name: "TypeError", message: "x" } }, { op: "advance", ms: 1000 }, open(), { op: "advance", ms: 10 }] },
    { name: "stream ends quickly then later", steps: [open(), { op: "advance", ms: 500 }, { op: "end", request: "last" }, { op: "advance", ms: 1000 }, open(), { op: "advance", ms: 1500 }, { op: "end", request: "last" }, { op: "advance", ms: 1000 }, open()] },
    { name: "stream errors", steps: [open([event(1)]), { op: "advance", ms: 10 }, { op: "end", request: "last", error: { name: "TypeError", message: "The socket connection was closed unexpectedly." } }, { op: "advance", ms: 1000 }, open()] },
    { name: "stalls and keepalives", idleTimeout: 1000, steps: [open(), { op: "advance", ms: 900 }, { op: "chunk", request: "last", data: ": keepalive\n\n" }, { op: "advance", ms: 999 }, { op: "advance", ms: 1 }, { op: "advance", ms: 1000 }, open(), { op: "advance", ms: 45000 }] },
    { name: "default stall", steps: [open(), { op: "advance", ms: 44999 }, { op: "advance", ms: 1 }, { op: "advance", ms: 1000 }, open()] },
    { name: "resync reconnects at once", steps: [open(), { op: "advance", ms: 100 }, { op: "resync", reason: "Network connection restored" }, open(), { op: "advance", ms: 10 }, { op: "resync", reason: "again" }, { op: "advance", ms: 5 }] },
    { name: "resync while connecting does nothing", steps: [{ op: "resync", reason: "early" }, { op: "advance", ms: 2000 }, { op: "resync", reason: "late" }] },
    { name: "managed reconnect replaces the client", reconnect: true, steps: [open(), { op: "end", request: "last" }, { op: "reconnect", ok: true }, open(), { op: "end", request: "last" }, { op: "reconnect", ok: false }, { op: "advance", ms: 1000 }, open()] },
    { name: "managed reconnect after repeated failures", reconnect: true, steps: [{ op: "fail", request: "last", error: { name: "TypeError", message: "refused" } }, { op: "reconnect", ok: false }, { op: "advance", ms: 1000 }, { op: "fail", request: "last", error: { name: "TypeError", message: "refused" } }, { op: "reconnect", ok: true }, { op: "advance", ms: 1000 }, open()] },
    { name: "dispose while connected", steps: [open([event(1)]), { op: "dispose" }, { op: "advance", ms: 100 }] },
    { name: "dispose while reconnecting", reconnect: true, steps: [{ op: "fail", request: "last", error: { name: "TypeError", message: "refused" } }, { op: "dispose" }, { op: "reconnect", ok: true }, { op: "advance", ms: 2000 }] },
    { name: "custom flush interval", flushInterval: 50, steps: [open([event(1)]), { op: "advance", ms: 20 }, { op: "chunk", request: "last", data: event(2) }, { op: "advance", ms: 30 }] },
  ]
  return cases.map((spec) => ({ kind: "connection", ...spec, name: `connection ${spec.name}` }))
}

/** Random connection scripts: attempts answered in every way, at every timing. */
export function randomConnectionCases(random: Random, count: number) {
  const cases: unknown[] = []
  for (let i = 0; i < count; i++) {
    const steps: unknown[] = []
    for (let j = random.int(14) + 2; j > 0; j--) {
      const kind = random.int(11)
      if (kind <= 1) steps.push({ op: "respond", request: "last", status: 200, headers: SSE, chunks: [connected, ...(random.bool() ? [event(j)] : [])], end: random.pick(["open", "open", "done"]) })
      else if (kind === 2) steps.push({ op: "respond", request: "last", status: random.pick([200, 401, 500]), headers: random.bool() ? SSE : [["content-type", "application/json"]], chunks: [random.pick([event(j), "{}", ": c\n\n"])], end: "open" })
      else if (kind === 3) steps.push({ op: "fail", request: "last", error: { name: "TypeError", message: "refused" } })
      else if (kind === 4) steps.push({ op: "chunk", request: "last", data: random.pick([event(j), ": keepalive\n\n", "data: {bad\n\n"]) })
      else if (kind === 5) steps.push({ op: "end", request: "last", ...(random.bool() ? { error: { name: "TypeError", message: "closed" } } : {}) })
      else if (kind <= 8) steps.push({ op: "advance", ms: random.pick([1, 9, 10, 500, 999, 1000, 1001, 1999, 2000, 3000, 45000]) })
      else if (kind === 9) steps.push({ op: "resync", reason: "resync" })
      else steps.push({ op: "reconnect", ok: random.bool() })
    }
    cases.push({
      kind: "connection",
      name: `connection #${i}`,
      ...(random.bool(0.3) ? { reconnect: true } : {}),
      ...(random.bool(0.3) ? { idleTimeout: random.pick([500, 1500, 3000]) } : {}),
      steps,
    })
  }
  return cases
}
