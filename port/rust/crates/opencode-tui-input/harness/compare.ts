// Compares the oracle's and the driver's answers program by program (see run.sh). Writes each differing program,
// with both answers, to <work>/failures/<n>.txt.
//
// usage: bun harness/compare.ts --work <dir> --name <label> [--show N]
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"

const argv = process.argv.slice(2)
const arg = (name: string, fallback?: string) => {
  const i = argv.indexOf(`--${name}`)
  return i >= 0 ? argv[i + 1] : fallback
}
const work = arg("work")!
const label = arg("name", "")!
const show = Number(arg("show", "3"))
const blocks = (file: string) => {
  const out: string[][] = [[]]
  for (const line of readFileSync(join(work, file), "utf8").split("\n").slice(0, -1)) {
    if (line === "end") out.push([])
    else out[out.length - 1].push(line)
  }
  out.pop()
  return out
}
const programs = blocks(`${label}programs.txt`)
const ts = blocks(`${label}ts.txt`)
const rust = blocks(`${label}rust.txt`)
if (ts.length !== programs.length || rust.length !== programs.length)
  throw new Error(`${programs.length} programs, ${ts.length} TS answers, ${rust.length} Rust answers`)

const failuresDir = join(work, `${label}failures`)
mkdirSync(failuresDir, { recursive: true })
let failures = 0
let lines = 0
for (let index = 0; index < programs.length; index++) {
  lines += ts[index].length
  if (ts[index].join("\n") === rust[index].join("\n")) continue
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
  writeFileSync(join(failuresDir, `${index}.txt`), report + "\n")
  if (failures <= show) console.log(report.split("\n").slice(0, 3).join("\n") + "\n")
}
const ops = programs.reduce((sum, program) => sum + program.length, 0)
console.log(`${label || ""}${programs.length} programs, ${ops} ops, ${lines} answer lines: ${failures} differ`)
if (failures) {
  console.log(`failures in ${failuresDir}`)
  process.exit(1)
}
