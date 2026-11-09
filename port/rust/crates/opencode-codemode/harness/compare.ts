// Compares the TS and Rust outputs of a corpus line by line and reports every difference.
//
// Engine diagnostics (JSON.parse, URI and regular-expression syntax errors in JavaScriptCore's words) are masked
// as the conformance standard masks them (port/conformance/lib/normalize.ts `engine`).
//
// usage: bun harness/compare.ts ts.jsonl rust.jsonl [--show N]
import { engine } from "../../../../conformance/lib/normalize.ts"
const [tsPath, rustPath] = process.argv.slice(2)
const showIndex = process.argv.indexOf("--show")
const show = showIndex >= 0 ? Number(process.argv[showIndex + 1]) : 5
const read = async (file: string) => (await Bun.file(file).text()).split("\n").filter((line) => line !== "")
const ts = await read(tsPath)
const rust = await read(rustPath)
let differences = 0
let masked = 0
let shown = 0
for (let index = 0; index < Math.max(ts.length, rust.length); index++) {
  const expected = ts[index]
  const actual = rust[index]
  if (expected === actual) continue
  if (expected !== undefined && actual !== undefined && engine(expected) === engine(actual)) {
    masked++
    continue
  }
  differences++
  if (shown++ >= show) continue
  const name = (() => {
    try {
      return JSON.parse(expected ?? actual ?? "{}").name
    } catch {
      return `line ${index + 1}`
    }
  })()
  const a = engine(expected ?? "<missing>")
  const b = engine(actual ?? "<missing>")
  let at = 0
  while (at < a.length && a[at] === b[at]) at++
  const from = Math.max(0, at - 200)
  console.log(`--- ${name} (first difference at ${at})`)
  console.log(`  ts:   ${a.slice(from, at + 300)}`)
  console.log(`  rust: ${b.slice(from, at + 300)}`)
}
console.log(`${Math.max(ts.length, rust.length)} cases, ${differences} differences (${masked} more equal once engine diagnostics are masked)`)
process.exit(differences === 0 ? 0 : 1)
