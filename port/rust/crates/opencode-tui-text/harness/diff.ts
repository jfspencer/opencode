// The differential check of opencode-tui-text: runs the same operation programs on @opentui/core 0.8.1 (TS
// classes over libopentui.so) and on the Rust crate (examples/text-driver.rs), and compares every op's result.
//
// usage: bun harness/diff.ts --driver <text-driver> --work <dir> [--count N] [--seed S] [--show N] [--case I]
//   --count   random cases (default 3000); hand-picked edge cases always run
//   --seed    PRNG seed (default 1)
//   --show    print the first N differences in full (default 5)
//   --case    run only case I (an index into the generated list) and print its ops
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { Rng, editCase, editEdgeCases, edgeCases, extmarkCase, extmarkEdgeCases, tableCases, textCase } from "./cases.ts"
import { runCase, runEditCase, type Case } from "./ts-exec.ts"

const args = process.argv.slice(2)
const opt = (name: string, fallback?: string) => {
  const i = args.indexOf(`--${name}`)
  return i >= 0 ? args[i + 1] : fallback
}
const driver = opt("driver")!
const work = opt("work")!
const count = Number(opt("count", "3000"))
const editCount = Number(opt("edit-count", opt("count", "3000")))
const seed = Number(opt("seed", "1"))
const show = Number(opt("show", "5"))
const only = opt("case")
const tables = !args.includes("--no-tables")
mkdirSync(work, { recursive: true })

const rng = new Rng(seed)
let cases: Case[] = [...edgeCases(), ...editEdgeCases(), ...extmarkEdgeCases(), ...(tables ? tableCases() : [])]
for (let i = 0; i < count; i++) cases.push(textCase(rng))
const editRng = new Rng(seed * 7919 + 1)
for (let i = 0; i < editCount; i++) cases.push(editCase(editRng))
const extmarkCount = Number(opt("extmark-count", String(Math.ceil(editCount / 2))))
for (let i = 0; i < extmarkCount; i++) cases.push(extmarkCase(editRng))
if (only !== undefined) {
  cases = [cases[Number(only)]]
  writeFileSync(join(work, "case.json"), JSON.stringify(cases[0], null, 1))
}

const started = performance.now()
const expected: unknown[][] = []
for (const c of cases) expected.push(c.k === "edit" ? await runEditCase(c) : runCase(c))
const tsMs = performance.now() - started

const input = join(work, "cases.jsonl")
const output = join(work, "rust.jsonl")
writeFileSync(input, cases.map((c) => JSON.stringify(c)).join("\n") + "\n")
const rustStarted = performance.now()
const proc = Bun.spawnSync([driver, input, output], { stdout: "inherit", stderr: "inherit" })
const rustMs = performance.now() - rustStarted
if (proc.exitCode !== 0) {
  console.error(`the driver failed (exit ${proc.exitCode})`)
  process.exit(1)
}
const actual = readFileSync(output, "utf8")
  .split("\n")
  .filter((l) => l.length > 0)
  .map((l) => JSON.parse(l))

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b)
const clip = (v: unknown) => {
  const s = JSON.stringify(v)
  return s.length > 600 ? s.slice(0, 600) + "…" : s
}

let ops = 0
let differences = 0
let shown = 0
const byOp = new Map<string, number>()
for (let i = 0; i < cases.length; i++) {
  const c = cases[i]
  const e = expected[i]
  const a = actual[i] ?? []
  for (let j = 0; j < c.ops.length; j++) {
    ops++
    if (same(e[j], a[j])) continue
    differences++
    byOp.set(c.ops[j][0], (byOp.get(c.ops[j][0]) ?? 0) + 1)
    if (shown < show) {
      shown++
      console.log(`case ${only ?? i} (${c.m}) op ${j}: ${clip(c.ops[j])}`)
      if (only !== undefined) console.log(`  program: ${clip(c.ops.slice(0, j + 1))}`)
      console.log(`  ts:   ${clip(e[j])}`)
      console.log(`  rust: ${clip(a[j])}`)
    }
    break
  }
}
console.log(
  `${cases.length} cases, ${ops} ops compared, ${differences} differences` +
    (differences ? ` (first per case, by op: ${[...byOp].map(([k, v]) => `${k} ${v}`).join(", ")})` : "") +
    ` [ts ${(tsMs / 1000).toFixed(1)}s, rust ${(rustMs / 1000).toFixed(1)}s]`,
)
process.exit(differences === 0 ? 0 : 1)
