// Property round-trip of the generated Rust wire types against the TS schemas.
//
// For every root type: TS generates random valid values (Effect's Arbitrary), encodes them and writes JSON text J.
// The canonical answer is TS's own JSON.stringify(encode(decode(JSON.parse(J)))). Rust (crates/protocol-check)
// decodes J and writes it back; the text must be byte-identical. Negative cases mutate J's value (wrong types,
// missing and extra keys, broken strings and numbers, ...): wherever TS rejects the mutant Rust must reject it, and
// wherever TS accepts it Rust must write the same text TS does.
//
// usage: bun src/roundtrip.ts [--samples N] [--mutants N] [--seed N] [--only RootName] [--verbose]
// env:   PROTOCOL_CHECK_BIN (default: ../target/debug/protocol-check)
import { join } from "node:path"
import { Arbitrary, Effect, Exit, Schema, type SchemaAST } from "effect"
import { baseConfig } from "../../../conformance/lib/model.ts"
import { build } from "./model.ts"

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
const roots = new Map<string, SchemaAST.AST>()
for (const r of model.roots) if (!roots.has(r.name) && (!only || r.name === only)) roots.set(r.name, r.root.ast)

type Expect = { ok: true; text: string } | { ok: false; why: string }
interface Case {
  readonly root: string
  readonly input: string
  readonly expect: Expect
  readonly kind: "valid" | "mutant" | "edge"
}

const stats = { roots: roots.size, noArbitrary: [] as string[], samples: 0, skipped: new Map<string, number>() }
const skip = (why: string) => stats.skipped.set(why, (stats.skipped.get(why) ?? 0) + 1)
// JSON.stringify writes a lone surrogate as an escape; a valid pair is written as the character itself.
const loneSurrogate = (text: string) => /\\ud[89a-f][0-9a-f]{2}/i.test(text)

const cases: Case[] = []
for (const [name, ast] of roots) {
  const schema = Schema.make(ast) as Schema.Codec<unknown, unknown>
  const decode = Schema.decodeUnknownExit(schema)
  const encode = Schema.encodeUnknownExit(schema)
  const canonical = (value: unknown): Expect => {
    const decoded = decode(value)
    if (Exit.isFailure(decoded)) return { ok: false, why: "TS rejects" }
    const encoded = encode(decoded.value)
    if (Exit.isFailure(encoded)) return { ok: false, why: "TS cannot encode its decoded value" }
    const text = JSON.stringify(encoded.value)
    return text === undefined ? { ok: false, why: "TS encodes to undefined" } : { ok: true, text }
  }
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
]
for (const edge of EDGES) {
  const root = model.roots.find((r) => r.root.origin === edge.origin)
  if (!root) throw new Error(`no root type for ${edge.origin}`)
  if (only && root.name !== only) continue
  const schema = Schema.make(root.root.ast) as Schema.Codec<unknown, unknown>
  for (const value of edge.values) {
    const decoded = Schema.decodeUnknownExit(schema)(value)
    const encoded = Exit.isSuccess(decoded) ? Schema.encodeUnknownExit(schema)(decoded.value) : undefined
    const expect: Expect =
      encoded && Exit.isSuccess(encoded) ? { ok: true, text: JSON.stringify(encoded.value) } : { ok: false, why: "TS rejects" }
    cases.push({ root: root.name, input: JSON.stringify(value), expect, kind: "edge" })
  }
}
const generated = performance.now()

const proc = Bun.spawnSync([bin], { stdin: Buffer.from(cases.map((c) => `${c.root}\t${c.input}\n`).join("")) })
if (proc.exitCode !== 0) {
  console.error(proc.stderr.toString())
  throw new Error(`${bin} exited with ${proc.exitCode}`)
}
const answers = proc.stdout.toString().split("\n")
const failures: string[] = []
const tally = { valid: 0, mutantAccepted: 0, mutantRejected: 0, edge: 0 }
cases.forEach((c, i) => {
  const [status, ...rest] = (answers[i] ?? "").split("\t")
  const got = rest.join("\t")
  const okExpected = c.expect.ok
  const pass = okExpected ? status === "ok" && got === (c.expect as { text: string }).text : status === "err"
  if (pass) {
    if (c.kind === "valid") tally.valid++
    else if (c.kind === "edge") tally.edge++
    else if (okExpected) tally.mutantAccepted++
    else tally.mutantRejected++
    return
  }
  failures.push(
    `${c.kind} ${c.root}\n    input:    ${c.input.slice(0, 600)}\n    expected: ${okExpected ? `ok ${(c.expect as { text: string }).text.slice(0, 600)}` : "rejected"}\n    rust:     ${status} ${got.slice(0, 600)}`,
  )
})

const done = performance.now()
const valid = cases.filter((c) => c.kind === "valid").length
const mutants = cases.filter((c) => c.kind === "mutant")
const edges = cases.filter((c) => c.kind === "edge").length
console.log(
  `round-trip: ${stats.roots} root types, ${stats.samples} generated values -> ${valid} valid cases, ${mutants.length} mutants ` +
    `(${mutants.filter((c) => !c.expect.ok).length} rejected by TS), ` +
    `${edges} edge cases; ${failures.length} failures (generate ${((generated - t0) / 1000).toFixed(1)}s, rust ${((done - generated) / 1000).toFixed(1)}s)`,
)
console.log(
  `  passed: ${tally.valid} valid round-trips byte-identical, ${tally.mutantRejected} mutants rejected by both, ${tally.mutantAccepted} mutants accepted by both with identical output, ${tally.edge} edge cases`,
)
for (const [why, n] of [...stats.skipped].sort()) console.log(`  skipped ${n}: ${why}`)
if (process.argv.includes("--edges"))
  for (const c of cases.filter((c) => c.kind === "edge"))
    console.log(`  edge ${c.root}: ${c.input.slice(0, 120)}\n    -> ${c.expect.ok ? (c.expect as { text: string }).text.slice(0, 200) : "rejected"}`)
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
