// Compares the TS and Rust outputs of a corpus line by line and reports every difference.
//
// usage: bun harness/compare.ts ts.jsonl rust.jsonl [--show N]
const [tsPath, rustPath] = process.argv.slice(2)
const showIndex = process.argv.indexOf("--show")
const show = showIndex >= 0 ? Number(process.argv[showIndex + 1]) : 5
const read = async (file: string) => (await Bun.file(file).text()).split("\n").filter((line) => line !== "")
const ts = await read(tsPath)
const rust = await read(rustPath)
let differences = 0
let shown = 0
for (let index = 0; index < Math.max(ts.length, rust.length); index++) {
  const expected = ts[index]
  const actual = rust[index]
  if (expected === actual) continue
  differences++
  if (shown++ >= show) continue
  const name = (() => {
    try {
      return JSON.parse(expected ?? actual ?? "{}").name
    } catch {
      return `line ${index + 1}`
    }
  })()
  console.log(`--- ${name}`)
  console.log(`  ts:   ${(expected ?? "<missing>").slice(0, 1500)}`)
  console.log(`  rust: ${(actual ?? "<missing>").slice(0, 1500)}`)
}
console.log(`${Math.max(ts.length, rust.length)} cases, ${differences} differences`)
process.exit(differences === 0 ? 0 : 1)
