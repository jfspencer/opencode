// The differential check: generates the cases (hand-picked and seeded random), runs the TS driver (the reference's
// @opencode/client under the TUI's Solid build) and the Rust driver on them, and compares their results case by
// case. Exits non-zero on any difference.
//
// Data cases are generated against the TS data layer itself (each step answers what it actually requested), then
// both drivers replay the concrete steps.
//
// usage: bun harness/diff.ts --driver <sync-driver> --work <dir> [--seed N] [--count N] [--only kind,...] [--show N]
//        [--cases <extra.jsonl>...]
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import path from "node:path"
import { connectionCases, randomConnectionCases, requestCases, sseCases } from "./cases/client"
import { contextCases } from "./cases/context"
import { dataCases } from "./cases/data"
import { Random } from "./cases/random"

const args = process.argv.slice(2)
const flag = (name: string) => {
  const index = args.indexOf(name)
  return index >= 0 ? args[index + 1] : undefined
}
const flags = (name: string) => args.flatMap((arg, index) => (arg === name ? [args[index + 1]] : []))
const crate = path.resolve(import.meta.dir, "..")
const reference = path.resolve(crate, "../../../.reference")
const driver = flag("--driver")!
const work = flag("--work")!
const seed = Number(flag("--seed") ?? 1)
const count = Number(flag("--count") ?? 1)
const show = Number(flag("--show") ?? 5)
const only = flag("--only")?.split(",")
mkdirSync(work, { recursive: true })

const preload = Bun.resolveSync("@opentui/solid/preload", path.join(reference, "packages/tui"))
async function run(command: string[], output: string) {
  const child = Bun.spawn(command, { stdout: "pipe", stderr: "pipe" })
  const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited])
  writeFileSync(output, stdout)
  if (code !== 0) {
    console.error(`${command.join(" ")} exited ${code}:\n${stderr.slice(-4000)}`)
    process.exit(1)
  }
  return stdout.split("\n").filter(Boolean).map((line) => JSON.parse(line) as { name: string; result: unknown })
}
const tsDriver = (file: string, output: string) =>
  run(["bun", "--preload", preload, path.join(crate, "harness/ts/driver.ts"), reference, file], output)
const write = (file: string, cases: unknown[]) => writeFileSync(file, cases.map((c) => JSON.stringify(c)).join("\n") + "\n")

const random = new Random(seed)
const cases: unknown[] = []
const wants = (kind: string) => !only || only.includes(kind)
if (wants("request")) cases.push(...requestCases(random, 600 * count))
if (wants("sse")) cases.push(...sseCases(random, 400 * count))
if (wants("connection")) cases.push(...connectionCases(), ...randomConnectionCases(random, 150 * count))
/** Runs generated cases against TS once, keeping the concrete steps it took, for both drivers to replay. */
async function concrete(generated: unknown[], label: string) {
  const file = path.join(work, `generate-${label}.jsonl`)
  write(file, generated)
  const results = await tsDriver(file, path.join(work, `generated-${label}.jsonl`))
  return results.map((line, index) => {
    const result = line.result as { steps?: unknown[]; crashed?: string }
    if (!result.steps) throw new Error(`generating ${line.name} failed: ${result.crashed ?? JSON.stringify(result).slice(0, 500)}`)
    const spec = generated[index] as Record<string, unknown>
    const { generate: _, ...rest } = spec
    return { ...rest, steps: result.steps }
  })
}
if (wants("data")) {
  const { fixed, generated } = dataCases(seed, 300 * count)
  cases.push(...fixed, ...(await concrete(generated, "data")))
}
if (wants("context")) cases.push(...(await concrete(contextCases(seed, 100 * count), "context")))
for (const file of flags("--cases"))
  cases.push(...readFileSync(file, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line)))
const casesFile = path.join(work, "cases.jsonl")
write(casesFile, cases)

const [ts, rust] = await Promise.all([tsDriver(casesFile, path.join(work, "ts.jsonl")), run([driver, casesFile], path.join(work, "rust.jsonl"))])

// JavaScriptCore's own error wording (a JSON syntax error's message, a TypeError from a missing value) is not
// ported: both sides' texts become one.
function normalize(value: unknown): unknown {
  if (typeof value === "string")
    return value
      .replace(/^(MalformedResponse: |Transport: )?(JSON Parse error: .*|invalid JSON at byte .*)$/s, "$1<engine message>")
      .replace(/^.* is not (an object|a function).*$/s, "<engine message>")
  if (Array.isArray(value)) return value.map(normalize)
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, normalize(v)]))
  return value
}

/** The first difference; with `ordered`, objects must also list their keys in the same order. */
function difference(a: unknown, b: unknown, ordered: boolean, at = ""): string | undefined {
  if (a === b) return
  if (typeof a !== typeof b || a === null || b === null || typeof a !== "object")
    return `${at || "."}: ${JSON.stringify(a)?.slice(0, 300)} != ${JSON.stringify(b)?.slice(0, 300)}`
  if (Array.isArray(a) !== Array.isArray(b)) return `${at}: array/object mismatch`
  if (Array.isArray(a)) {
    const other = b as unknown[]
    if (a.length !== other.length) return `${at}: length ${a.length} != ${other.length}`
    for (let i = 0; i < a.length; i++) {
      const found = difference(a[i], other[i], ordered, `${at}[${i}]`)
      if (found) return found
    }
    return
  }
  const left = a as Record<string, unknown>
  const right = b as Record<string, unknown>
  for (const key of new Set([...Object.keys(left), ...Object.keys(right)])) {
    const found = difference(left[key], right[key], ordered, `${at}.${key}`)
    if (found) return found
  }
  if (ordered && RECORDS.test(at) && Object.keys(left).join("\u0000") !== Object.keys(right).join("\u0000"))
    return `${at || "."}: key order ${JSON.stringify(Object.keys(left))} != ${JSON.stringify(Object.keys(right))}`
}

// The store's records (sessions, statuses, transcripts, ... by ID; locations by key) keep their order, which
// `Object.values`/`entries` readers show. Inside a record, JS keeps the place of a key set to `undefined` (absent
// from JSON), so key order there is not compared.
const RECORDS = /^\.results\[\d+\]\.store\.(session\.[A-Za-z]+|project\.(info|permission)|location)$/

const kinds = new Map<string, { total: number; failed: number }>()
let failures = 0
for (let i = 0; i < Math.max(ts.length, rust.length); i++) {
  const spec = cases[i] as { kind: string; name: string }
  const stats = kinds.get(spec?.kind ?? "?") ?? { total: 0, failed: 0 }
  kinds.set(spec?.kind ?? "?", stats)
  stats.total++
  const found = difference(normalize(ts[i]?.result), normalize(rust[i]?.result), spec?.kind === "data" || spec?.kind === "context")
  const crashed = JSON.stringify(ts[i]?.result ?? "").includes('"crashed"')
  if (!found && !crashed) continue
  stats.failed++
  failures++
  if (failures <= show) {
    console.log(`DIFF ${spec?.name}: ${found ?? "crashed"}`)
    console.log(`  case: ${JSON.stringify(spec).slice(0, 1500)}`)
    console.log(`  ts:   ${JSON.stringify(ts[i]?.result).slice(0, 1500)}`)
    console.log(`  rust: ${JSON.stringify(rust[i]?.result).slice(0, 1500)}`)
  }
}
for (const [kind, stats] of kinds) console.log(`${kind}: ${stats.total - stats.failed}/${stats.total} identical`)
console.log(failures === 0 ? `0 differences in ${cases.length} cases` : `${failures} differences in ${cases.length} cases`)
process.exit(failures === 0 ? 0 : 1)
