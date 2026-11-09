// Times a jsonl case file on both sides: the TS classes in this process, the crate through the driver.
//
// usage: bun harness/time.ts --driver <text-driver> <cases.jsonl> <rust-out.jsonl>
import { readFileSync } from "node:fs"
import { runCase, runEditCase, type Case } from "./ts-exec.ts"

const args = process.argv.slice(2)
const driver = args[args.indexOf("--driver") + 1]
const [input, output] = args.filter((a, i) => !a.startsWith("--") && args[i - 1] !== "--driver")
const cases: Case[] = readFileSync(input, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l))
let t = performance.now()
for (const c of cases) c.k === "edit" ? await runEditCase(c) : runCase(c)
const ts = performance.now() - t
t = performance.now()
Bun.spawnSync([driver, input, output], { stdout: "inherit", stderr: "inherit" })
console.log(`${cases.length} cases: ts ${ts.toFixed(0)}ms, rust ${(performance.now() - t).toFixed(0)}ms`)
