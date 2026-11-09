// Finds the shortest prefix of a case (work/case.json, written by `diff.ts --case I`) whose final probe ops differ
// between TS and Rust: each prefix runs with the probes appended.
//
// usage: bun harness/bisect.ts --driver <text-driver> --work <dir> [probe ops as JSON, default [["vVisual"]]]
import { readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { runCase, runEditCase, type Case } from "./ts-exec.ts"

const args = process.argv.slice(2)
const opt = (name: string) => args[args.indexOf(`--${name}`) + 1]
const work = opt("work")
const driver = opt("driver")
const probe = JSON.parse(args.find((a) => a.startsWith("[")) ?? '[["vVisual"]]')
const base: Case = JSON.parse(readFileSync(join(work, "case.json"), "utf8"))
const cases: Case[] = []
for (let n = 0; n <= base.ops.length; n++) cases.push({ ...base, ops: [...base.ops.slice(0, n), ...probe] })
const expected: unknown[][] = []
for (const c of cases) expected.push(c.k === "edit" ? await runEditCase(c) : runCase(c))
writeFileSync(join(work, "bisect.jsonl"), cases.map((c) => JSON.stringify(c)).join("\n") + "\n")
Bun.spawnSync([driver, join(work, "bisect.jsonl"), join(work, "bisect-rust.jsonl")], { stdout: "inherit", stderr: "inherit" })
const actual = readFileSync(join(work, "bisect-rust.jsonl"), "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l))
for (let n = 0; n < cases.length; n++) {
  const e = expected[n].slice(n)
  const a = actual[n].slice(n)
  if (JSON.stringify(e) !== JSON.stringify(a)) {
    console.log(`first differing prefix: ${n} ops; last op ${JSON.stringify(base.ops[n - 1])}`)
    console.log(`  ts:   ${JSON.stringify(e)}`)
    console.log(`  rust: ${JSON.stringify(a)}`)
    process.exit(1)
  }
}
console.log("no prefix differs")
