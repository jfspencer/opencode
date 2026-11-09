// Property round-trip of the generated Rust wire types against the TS schemas.
//
// For every root type: TS generates random valid values (Effect's Arbitrary), encodes them and writes JSON text J.
// The canonical answer is TS's own JSON.stringify(encode(decode(JSON.parse(J)))). Rust (crates/protocol-check)
// decodes J and writes it back; the text must be byte-identical. Negative cases mutate J's value (wrong types,
// missing and extra keys, broken strings and numbers, ...): wherever TS rejects the mutant Rust must reject it with
// the same message (Effect's formatted issue, `SchemaError.message`, which the TS server returns in
// `InvalidRequestError.message`), and wherever TS accepts it Rust must write the same text TS does. Each root is
// decoded the way HttpApi decodes it (a payload through `Schema.Union(payloadSchemas)`, see roots.ts). The request
// rejections recorded in the conformance transcripts (port/conformance/goldens) run the same way, and must also
// reproduce the recorded message.
//
// usage: bun src/roundtrip.ts [--samples N] [--mutants N] [--seed N] [--only RootName] [--verbose]
// env:   PROTOCOL_CHECK_BIN (default: ../target/debug/protocol-check)
import { readdirSync, readFileSync, statSync } from "node:fs"
import { join } from "node:path"
import { Arbitrary, Effect, Exit, Schema, type SchemaAST } from "effect"
import { baseConfig } from "../../../conformance/lib/model.ts"
import { build } from "./model.ts"
import type { Endpoint, Root } from "./roots.ts"

const arg = (name: string, fallback: number) => {
  const i = process.argv.indexOf(`--${name}`)
  return i === -1 ? fallback : Number(process.argv[i + 1])
}
const SAMPLES = arg("samples", 12)
const MUTANTS = arg("mutants", 2)
const SEED = arg("seed", 20261107)
const only = process.argv.includes("--only") ? process.argv[process.argv.indexOf("--only") + 1] : undefined
const verbose = process.argv.includes("--verbose")
const bin = process.env.PROTOCOL_CHECK_BIN ?? join(import.meta.dir, "../../target/debug/protocol-check")
const REPLACEMENTS = [null, true, false, 0, -1, 1.5, 2 ** 53 + 2, 1e21, -1e-7, "", " ", "x", "NaN", "Infinity", "-0", "1", "true", [], [null], {}, { a: 1 }]

const t0 = performance.now()
const model = build()
const roots = new Map<string, Root>()
for (const r of model.roots) if (!roots.has(r.name) && (!only || r.name === only)) roots.set(r.name, r.root)

/** `message`: TS's formatted message, absent when TS fails with a defect rather than a schema issue. */
type Expect = { ok: true; text: string } | { ok: false; why: string; message?: string }
interface Case {
  readonly root: string
  readonly input: string
  readonly expect: Expect
  readonly kind: "valid" | "mutant" | "edge" | "golden"
}

const stats = { roots: roots.size, noArbitrary: [] as string[], samples: 0, skipped: new Map<string, number>() }
const skip = (why: string) => stats.skipped.set(why, (stats.skipped.get(why) ?? 0) + 1)
// JSON.stringify writes a lone surrogate as an escape; a valid pair is written as the character itself.
const loneSurrogate = (text: string) => /\\ud[89a-f][0-9a-f]{2}/i.test(text)

/** TS's answer for a value: the canonical text, or the rejection and its message. */
function judge(root: Root) {
  // The value is decoded as HttpApi decodes it (`Root.decode`), and written back with the schema itself.
  const decode = Schema.decodeUnknownExit(Schema.make(root.decode ?? root.ast) as Schema.Codec<unknown, unknown>)
  const encode = Schema.encodeUnknownExit(Schema.make(root.ast) as Schema.Codec<unknown, unknown>)
  return (value: unknown): Expect => {
    const decoded = decode(value)
    if (Exit.isFailure(decoded)) {
      const error = (decoded.cause as unknown as { reasons: Array<{ error?: { _tag?: string; message?: string } }> }).reasons[0]?.error
      return error?._tag === "SchemaError" ? { ok: false, why: "TS rejects", message: error.message } : { ok: false, why: "TS fails with a defect" }
    }
    const encoded = encode(decoded.value)
    if (Exit.isFailure(encoded)) return { ok: false, why: "TS cannot encode its decoded value" }
    const text = JSON.stringify(encoded.value)
    return text === undefined ? { ok: false, why: "TS encodes to undefined" } : { ok: true, text }
  }
}

const cases: Case[] = []
for (const [name, root] of roots) {
  const schema = Schema.make(root.ast) as Schema.Codec<unknown, unknown>
  const encode = Schema.encodeUnknownExit(schema)
  const canonical = judge(root)
  let values: ReadonlyArray<unknown>
  try {
    const arbitrary = Arbitrary.schema(schema)
    values = Effect.runSync(Arbitrary.sampleEffect(arbitrary, { count: SAMPLES, seed: SEED ^ hash(name), size: 8 }))
  } catch (e) {
    stats.noArbitrary.push(`${name}: ${String(e).split("\n")[0].slice(0, 120)}`)
    continue
  }
  const random = mulberry32(SEED ^ hash(`${name}/mutants`))
  for (const value of values) {
    stats.samples++
    const encoded = encode(value)
    if (Exit.isFailure(encoded)) {
      skip("TS cannot encode its own generated value")
      continue
    }
    const text = JSON.stringify(encoded.value)
    if (text === undefined) {
      skip("encodes to undefined")
      continue
    }
    if (loneSurrogate(text)) {
      skip("lone surrogate in a string (no Rust String can hold it)")
      continue
    }
    const parsed = JSON.parse(text)
    const expect = canonical(parsed)
    if (!expect.ok) {
      skip(`TS's own JSON does not decode (${expect.why})`)
      continue
    }
    cases.push({ root: name, input: text, expect, kind: "valid" })
    for (let i = 0; i < MUTANTS; i++) {
      const mutant = mutate(parsed, random)
      const input = JSON.stringify(mutant)
      if (input === undefined || input === text) continue
      if (loneSurrogate(input)) {
        skip("mutant with a lone surrogate")
        continue
      }
      cases.push({ root: name, input, expect: canonical(JSON.parse(input)), kind: "mutant" })
    }
  }
}
// Hand-picked inputs the generators rarely produce: the hand-written hooks, codecs' normalizations, and the
// conformance suite's reference config. Judged by TS like the mutants.
const EDGES: ReadonlyArray<{ origin: string; values: ReadonlyArray<unknown> }> = [
  {
    origin: "opencode.json",
    values: [
      baseConfig,
      { model: "openrouter/a/b#max" },
      { model: "a/b#" },
      { model: "/b" },
      { model: "a" },
      { model: "a#x/b" },
      { model: "a/b#c#d" },
      { model: { providerID: "a", model: "b", variant: "c" } },
      { model: { providerID: "a/b", model: "c" } },
      { agents: { build: { model: "x/y#z", color: "red" }, plan: { color: "#00ff00", steps: 3 } } },
      {
        policies: [
          { action: "integration.use", resource: "mcp:github", effect: "allow" },
          { action: "integration.use", resource: "skill:a:b", effect: "deny" },
          { action: "integration.use", resource: "*", effect: "deny" },
          { action: "integration.use", resource: "other:x", effect: "deny" },
          { action: "integration.use", resource: "plugin:", effect: "allow" },
        ],
      },
      { policies: [{ action: "integration.use", resource: 1, effect: "allow" }] },
      { warming: { interval: "5 minutes", duration: "1.5 seconds" } },
      { warming: { interval: "auto", duration: "0.0000001 seconds" } },
      { warming: { duration: "100 nanos" } },
      { warming: { duration: "2 fortnights" } },
      { warming: { duration: "-0 millis" } },
      { experimental: { location_idle_timeout: false } },
      { experimental: { location_idle_timeout: "10 seconds" } },
      { worktree: { directory: "  ~/w  " } },
      { worktree: { directory: "   " } },
      { $schema: "https://opencode.ai/config.json", unknownKey: 1 },
      [],
      null,
    ],
  },
  {
    origin: "GET /api/session/:sessionID/message query",
    values: ["0x10", "1e2", " 7 ", "", "201", "1.0", "Infinity", "-0", "0b11"].map((limit) => ({ limit })),
  },
  {
    origin: "GET /api/session query",
    values: [{ archived: "true" }, { archived: "yes" }, { parentID: "null" }, { parentID: "ses_1" }, { order: "desc", search: " x " }],
  },
  {
    origin: "GET /api/event query",
    values: [{ types: "session.created" }, { types: ["session.created"] }, { types: ["session.created", "rpc.x"] }, { types: [] }, { types: "rpc." }, { types: "nope" }],
  },
  { origin: "PUT /api/session/:sessionID/environment payload", values: [{ variables: [] }, { variables: { a: "x", b: null } }] },
  // Messages: every candidate member of a union reports, in member order.
  {
    origin: "POST /api/experimental/schedule payload",
    values: [
      { prompt: "p", sessionID: "ses_1", when: { in: "1 hour", every: "1 hour" } },
      { prompt: "p", sessionID: "ses_1", when: {} },
      { prompt: "p", sessionID: "ses_1", when: [] },
      { prompt: "p", sessionID: "ses_1", ends: { times: 0.5 }, when: { in: "1 hour" } },
    ],
  },
]
// The same, as JSON text whose key order JSON.stringify would not write: a record reports its first bad value in
// JS key order (array indices first), whatever the order in the text.
const TEXT_EDGES: ReadonlyArray<{ origin: string; texts: ReadonlyArray<string> }> = [
  { origin: "PUT /api/session/:sessionID/environment payload", texts: ['{"variables":{"b":1,"1":2,"0":3}}', '{"variables":{"b":1,"a":"x","0":"y"}}'] },
]
for (const edge of [...EDGES.map((e) => ({ origin: e.origin, texts: e.values.map((v) => JSON.stringify(v)) })), ...TEXT_EDGES]) {
  const root = model.roots.find((r) => r.root.origin === edge.origin)
  if (!root) throw new Error(`no root type for ${edge.origin}`)
  if (only && root.name !== only) continue
  const canonical = judge(root.root)
  for (const input of edge.texts) cases.push({ root: root.name, input, expect: canonical(JSON.parse(input)), kind: "edge" })
}
const goldens = goldenRejections()
for (const g of goldens.cases) if (!only || g.root === only) cases.push(g)
const generated = performance.now()

const proc = Bun.spawnSync([bin], { stdin: Buffer.from(cases.map((c) => `${c.root}\t${c.input}\n`).join("")) })
if (proc.exitCode !== 0) {
  console.error(proc.stderr.toString())
  throw new Error(`${bin} exited with ${proc.exitCode}`)
}
const answers = proc.stdout.toString().split("\n")
const failures: string[] = []
const tally = { valid: 0, mutantAccepted: 0, mutantRejected: 0, mutantDefect: 0, edge: 0, golden: 0 }
cases.forEach((c, i) => {
  const [status, ...rest] = (answers[i] ?? "").split("\t")
  const got = rest.join("\t")
  const message = status === "err" ? (JSON.parse(got) as string) : undefined
  const expect = c.expect
  const pass = expect.ok
    ? status === "ok" && got === expect.text
    : status === "err" && (expect.message === undefined || message === expect.message)
  if (pass) {
    if (c.kind === "valid") tally.valid++
    else if (c.kind === "edge") tally.edge++
    else if (c.kind === "golden") tally.golden++
    else if (expect.ok) tally.mutantAccepted++
    else if (expect.message === undefined) tally.mutantDefect++
    else tally.mutantRejected++
    return
  }
  const show = (s: string) => s.slice(0, 600)
  const wanted = expect.ok ? `ok ${show(expect.text)}` : `rejected${expect.message === undefined ? ` (${expect.why})` : `: ${show(JSON.stringify(expect.message))}`}`
  const answer = status === "err" ? `rejected: ${show(got)}` : `${status} ${show(got)}`
  failures.push(`${c.kind} ${c.root}\n    input:    ${show(c.input)}\n    expected: ${wanted}\n    rust:     ${answer}`)
})

const done = performance.now()
const valid = cases.filter((c) => c.kind === "valid").length
const mutants = cases.filter((c) => c.kind === "mutant")
const edges = cases.filter((c) => c.kind === "edge").length
console.log(
  `round-trip: ${stats.roots} root types, ${stats.samples} generated values -> ${valid} valid cases, ${mutants.length} mutants ` +
    `(${mutants.filter((c) => !c.expect.ok).length} rejected by TS), ` +
    `${edges} edge cases, ${goldens.cases.length} transcript rejections; ${failures.length} failures (generate ${((generated - t0) / 1000).toFixed(1)}s, rust ${((done - generated) / 1000).toFixed(1)}s)`,
)
console.log(
  `  passed: ${tally.valid} valid round-trips byte-identical, ${tally.mutantRejected} mutants rejected by both with the same message, ` +
    `${tally.mutantDefect} rejected by both (TS: a defect, no message), ${tally.mutantAccepted} mutants accepted by both with identical output, ` +
    `${tally.edge} edge cases, ${tally.golden} transcript rejections with the recorded message`,
)
for (const [why, n] of goldens.skipped) console.log(`  transcript rejection not replayed (${why}): ${n}`)
for (const [why, n] of [...stats.skipped].sort()) console.log(`  skipped ${n}: ${why}`)
if (process.argv.includes("--edges"))
  for (const c of cases.filter((c) => c.kind === "edge"))
    console.log(
      `  edge ${c.root}: ${c.input.slice(0, 120)}\n    -> ${c.expect.ok ? c.expect.text.slice(0, 200) : `rejected ${c.expect.message === undefined ? `(${c.expect.why})` : JSON.stringify(c.expect.message).slice(0, 300)}`}`,
    )
if (stats.noArbitrary.length) {
  console.log(`  no arbitrary for ${stats.noArbitrary.length} root types:`)
  for (const n of stats.noArbitrary) console.log(`    ${n}`)
}
if (failures.length) {
  const shown = verbose ? failures : failures.slice(0, 30)
  console.log(`failures${shown.length < failures.length ? ` (first ${shown.length}; --verbose for all)` : ""}:`)
  for (const f of shown) console.log(`  ${f}`)
  process.exit(1)
}

// ---- the conformance transcripts' request rejections ----------------------------------------------------
// Every `InvalidRequestError` a transcript recorded for a schema rejection (kind Payload, Query, Params, Headers):
// the request part is rebuilt from the transcript (placeholders such as `<ses_1>` become plain IDs), TS must give
// the recorded message for it (else the transcript cannot be replayed, and it is counted as such), and Rust must
// give the same message.

function goldenRejections() {
  const dir = join(import.meta.dir, "../../../conformance/goldens")
  const out: Case[] = []
  const skipped = new Map<string, number>()
  const skipGolden = (why: string) => skipped.set(why, (skipped.get(why) ?? 0) + 1)
  const seen = new Set<string>()
  const nameOf = new Map(model.roots.map((r) => [r.root, r.name]))
  const files: string[] = []
  const list = (d: string) => {
    for (const f of readdirSync(d).sort()) {
      const p = join(d, f)
      if (statSync(p).isDirectory()) list(p)
      else if (f.endsWith(".json")) files.push(p)
    }
  }
  list(dir)
  type Step = { method?: string; url?: string; request?: unknown; body?: { _tag?: string; kind?: string; message?: string } }
  const steps: Step[] = []
  const visit = (v: unknown) => {
    if (Array.isArray(v)) return v.forEach(visit)
    if (!v || typeof v !== "object") return
    const step = v as Step
    if (step.body?._tag === "InvalidRequestError" && ["Payload", "Query", "Params", "Headers"].includes(step.body.kind ?? ""))
      steps.push(step)
    Object.values(v).forEach(visit)
  }
  for (const file of files) {
    const text = readFileSync(file, "utf8")
    if (text.includes("InvalidRequestError")) visit(JSON.parse(text))
  }
  for (const step of steps) {
    const kind = step.body!.kind!
    const recorded = step.body!.message!
    const url = new URL(unplaceholder(step.url ?? "") as string, "http://x")
    const request = unplaceholder(step.request)
    if (JSON.stringify(request)?.includes("<")) {
      skipGolden("its input holds a placeholder")
      continue
    }
    const endpoint = route(step.method ?? "", url.pathname)
    if (!endpoint) {
      skipGolden("no endpoint matches")
      continue
    }
    const query: Record<string, string | string[]> = {}
    for (const [k, v] of url.searchParams) {
      const prev = query[k]
      query[k] = prev === undefined ? v : Array.isArray(prev) ? [...prev, v] : [prev, v]
    }
    const body = endpoint.e.payload.find((p) => p.root)
    const part =
      kind === "Params"
        ? { root: endpoint.e.params, input: endpoint.params }
        : kind === "Query"
          ? { root: endpoint.e.query, input: query }
          : kind === "Payload"
            ? { root: body?.root, input: body?.kind === "query" ? query : request }
            : undefined
    if (!part?.root || part.input === undefined) {
      skipGolden(`no ${kind.toLowerCase()} to replay`)
      continue
    }
    const expect = judge(part.root)(part.input)
    if (expect.ok || expect.message === undefined || truncate(expect.message) !== recorded) {
      skipGolden("TS does not give the recorded message for the rebuilt input")
      continue
    }
    const root = nameOf.get(part.root)!
    const input = JSON.stringify(part.input)
    if (seen.has(`${root}\t${input}`)) continue
    seen.add(`${root}\t${input}`)
    out.push({ root, input, expect: { ok: false, why: "recorded", message: expect.message }, kind: "golden" })
  }
  return { cases: out, skipped }
}

// Normalized values in transcripts: `<ses_1>` -> `ses_1`, `<project>` -> `/project`, `<time>` -> epoch millis.
function unplaceholder(v: unknown): unknown {
  if (v === "<time>") return 1767225600000
  if (typeof v === "string")
    return v.replace(/<([a-z]+)_(\d+)>/g, "$1_$2").replace(/<project>/g, "/project").replace(/<iso-time>/g, "2030-01-01T00:00:00.000Z")
  if (Array.isArray(v)) return v.map(unplaceholder)
  if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, unplaceholder(x)]))
  return v
}

// The TS server's own truncation of the reason (packages/server/src/middleware/schema-error.ts).
function truncate(reason: string) {
  return reason.length <= 1024 ? reason : `${reason.slice(0, 1024)}... (${reason.length - 1024} more chars)`
}

// The endpoint for a request: the route whose literal segments match the most, with its path parameters.
function route(method: string, path: string): { e: Endpoint; params: Record<string, string> } | undefined {
  const segments = path.split("/").slice(1)
  let best: { e: Endpoint; params: Record<string, string>; score: number } | undefined
  for (const e of model.endpoints) {
    if (e.method !== method) continue
    const pattern = e.path.split("/").slice(1)
    const params: Record<string, string> = {}
    let score = 0
    let ok = pattern.length === segments.length || pattern[pattern.length - 1] === "*"
    for (let i = 0; ok && i < pattern.length; i++) {
      if (pattern[i] === "*") break
      if (segments[i] === undefined) ok = false
      else if (pattern[i].startsWith(":")) params[pattern[i].slice(1)] = decodeURIComponent(segments[i])
      else if (pattern[i] === segments[i]) score++
      else ok = false
    }
    if (ok && (!best || score > best.score)) best = { e, params, score }
  }
  return best
}

// ---- deterministic randomness and mutation ---------------------------------------------------------------

function hash(s: string) {
  let h = 0x811c9dc5
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 0x01000193)
  return h >>> 0
}

function mulberry32(seed: number) {
  let a = seed >>> 0
  return () => {
    a = (a + 0x6d2b79f5) >>> 0
    let t = a
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

type J = null | boolean | number | string | J[] | { [k: string]: J }

function mutate(value: J, random: () => number): J {
  const pick = <T>(xs: readonly T[]): T => xs[Math.floor(random() * xs.length)]
  // every position in the value, as a path
  const paths: Array<Array<string | number>> = []
  const walk = (v: J, path: Array<string | number>) => {
    paths.push(path)
    if (Array.isArray(v)) v.forEach((x, i) => walk(x, [...path, i]))
    else if (v && typeof v === "object") for (const [k, x] of Object.entries(v)) walk(x, [...path, k])
  }
  walk(value, [])
  // prefer deep positions: they hit leaf checks
  const path = random() < 0.7 && paths.length > 1 ? pick(paths.slice(1)) : pick(paths)
  const copy = structuredClone(value)
  const at = path.reduce<J>((v, k) => (v as Record<string | number, J>)[k], copy)
  const replace = (next: J) => {
    if (path.length === 0) return next
    const parent = path.slice(0, -1).reduce<J>((v, k) => (v as Record<string | number, J>)[k], copy) as Record<string | number, J>
    parent[path[path.length - 1]] = next
    return copy
  }
  const r = random()
  if (at && typeof at === "object" && !Array.isArray(at)) {
    const keys = Object.keys(at)
    if (r < 0.35 && keys.length) {
      delete (at as Record<string, J>)[pick(keys)]
      return copy
    }
    if (r < 0.5) {
      ;(at as Record<string, J>)[pick(["__extra", "0", "type", "toString"])] = pick(REPLACEMENTS)
      return copy
    }
    return replace(pick(REPLACEMENTS))
  }
  if (Array.isArray(at)) {
    if (r < 0.3) return replace([])
    if (r < 0.5 && at.length) return replace([...at, at[0]])
    if (r < 0.6) return replace(at.length ? at[0] : null)
    return replace(pick(REPLACEMENTS))
  }
  if (typeof at === "string") {
    const ops: Array<() => J> = [
      () => "",
      () => ` ${at}`,
      () => `${at} `,
      () => `${at}!`,
      () => at.slice(1),
      () => at.toUpperCase(),
      () => `${at}${at}`.repeat(4),
      () => (at.length ? at.slice(0, -1) + String.fromCharCode(at.charCodeAt(at.length - 1) ^ 1) : "a"),
      () => String(Number(at) + 0.5),
      () => pick(REPLACEMENTS),
    ]
    return replace(pick(ops)())
  }
  if (typeof at === "number") {
    const ops: Array<() => J> = [
      () => at + 0.5,
      () => -at,
      () => at - 1,
      () => at * 1e6,
      () => 2 ** 53 + 2,
      () => -0,
      () => 8.64e15 + 1,
      () => String(at),
      () => pick(REPLACEMENTS),
    ]
    return replace(pick(ops)())
  }
  return replace(pick(REPLACEMENTS))
}
