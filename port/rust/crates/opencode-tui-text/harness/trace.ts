// Runs work/case.json (written by `diff.ts --case I`) with the probe ops inserted after every op, and prints each
// op's output on both sides, marking the ops whose outputs differ.
//
// usage: bun harness/trace.ts --driver <text-driver> --work <dir> [probe ops as JSON, default [["eInfo"]]]
import { readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { disposeScene, runAnyCase } from "./scene-exec.ts"
import type { Case } from "./ts-exec.ts"

const args = process.argv.slice(2)
const opt = (name: string) => args[args.indexOf(`--${name}`) + 1]
const work = opt("work")
const driver = opt("driver")
const probe: unknown[][] = JSON.parse(args.find((a) => a.startsWith("[")) ?? '[["eInfo"]]')
const base: Case = JSON.parse(readFileSync(join(work, "case.json"), "utf8"))
const ops = base.ops.flatMap((op) => [op, ...probe])
const c: Case = { ...base, ops }
const expected = await runAnyCase(c)
await disposeScene()
writeFileSync(join(work, "trace.jsonl"), JSON.stringify(c) + "\n")
Bun.spawnSync([driver, join(work, "trace.jsonl"), join(work, "trace-rust.jsonl")], { stdout: "inherit", stderr: "inherit" })
const actual = JSON.parse(readFileSync(join(work, "trace-rust.jsonl"), "utf8").split("\n")[0])
for (let i = 0; i < ops.length; i++) {
  const e = JSON.stringify(expected[i])
  const a = JSON.stringify(actual[i])
  console.log(`${e === a ? " " : "!"} ${i} ${JSON.stringify(ops[i])}`)
  console.log(`    ts:   ${e}`)
  if (e !== a) console.log(`    rust: ${a}`)
}
