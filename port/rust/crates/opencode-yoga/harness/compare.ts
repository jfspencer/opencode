// Compares the oracle's and the driver's answers program by program (see run.sh), and what Yoga's default logger
// printed in each process. Writes each differing program, with both answers, to <work>/failures/<n>.txt.
//
// usage: bun harness/compare.ts --work <dir> [--show N]
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"

const argv = process.argv.slice(2)
const arg = (name: string, fallback?: string) => {
  const i = argv.indexOf(`--${name}`)
  return i >= 0 ? argv[i + 1] : fallback
}
const work = arg("work")!
const show = Number(arg("show", "3"))
const blocks = (file: string) => {
  const text = readFileSync(join(work, file), "utf8")
  const out: string[][] = [[]]
  for (const line of text.split("\n").slice(0, -1)) {
    if (line === "end") out.push([])
    else out[out.length - 1].push(line)
  }
  out.pop()
  return out
}
const programs = blocks("programs.txt")
const ts = blocks("ts.txt")
const rust = blocks("rust.txt")
if (ts.length !== programs.length || rust.length !== programs.length)
  throw new Error(`${programs.length} programs, ${ts.length} TS answers, ${rust.length} Rust answers`)

mkdirSync(join(work, "failures"), { recursive: true })
let failures = 0
let lines = 0
for (let index = 0; index < programs.length; index++) {
  lines += ts[index].length
  const a = ts[index].join("\n")
  const b = rust[index].join("\n")
  if (a === b) continue
  failures++
  let first = 0
  while (ts[index][first] === rust[index][first]) first++
  const report = [
    `program ${index}: answers differ from line ${first + 1}`,
    `ts:   ${ts[index][first]}`,
    `rust: ${rust[index][first]}`,
    "",
    "--- program",
    ...programs[index],
    "--- ts",
    ...ts[index],
    "--- rust",
    ...rust[index],
  ].join("\n")
  writeFileSync(join(work, "failures", `${index}.txt`), report + "\n")
  if (failures <= show) console.log(report.split("\n").slice(0, 3).join("\n") + "\n")
}
const tsLog = readFileSync(join(work, "ts-stdout.txt"), "utf8")
const rustLog = readFileSync(join(work, "rust-stdout.txt"), "utf8")
const logLines = tsLog.split("\n").filter(Boolean).length
const sameLog = tsLog === rustLog
const ops = programs.reduce((sum, program) => sum + program.length, 0)
console.log(
  `${programs.length} programs, ${ops} ops, ${lines} answer lines: ${failures} differ; ` +
    `Yoga log ${sameLog ? "identical" : "DIFFERS"} (${logLines} lines)`,
)
if (failures || !sameLog) {
  console.log(`failures in ${join(work, "failures")}`)
  process.exit(1)
}
