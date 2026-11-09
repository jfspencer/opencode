// Compares the acorn and Rust dumps of a corpus line by line and reports the differences: their count and, for the
// first few, the program and the first JSON path where the two disagree.
//
// One known, deliberate gap is accounted for separately: the tree keeps a literal's `raw`, a directive, a regex
// pattern and an error message as Rust text, so a lone surrogate in those (only possible when the program text
// itself holds one) becomes U+FFFD. Lines that match once acorn's side gets the same replacement in exactly those
// places are counted as "lone-surrogate" lines, not as differences.
//
// usage: bun harness/parser/compare.ts corpus.jsonl acorn.jsonl rust.jsonl [shown=10]
const [corpusPath, acornPath, rustPath, shownArg] = Bun.argv.slice(2)
const read = async (path: string) => (await Bun.file(path).text()).split("\n").filter((line) => line.length > 0)
const corpus = await read(corpusPath)
const acorn = await read(acornPath)
const rust = await read(rustPath)
const shown = Number(shownArg ?? 10)

function firstDifference(a: unknown, b: unknown, path: string): string | undefined {
  if (a === b) return undefined
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null || Array.isArray(a) !== Array.isArray(b))
    return `${path}: acorn ${JSON.stringify(a)?.slice(0, 300)} / rust ${JSON.stringify(b)?.slice(0, 300)}`
  const keys = [...new Set([...Object.keys(a), ...Object.keys(b)])]
  for (const key of keys) {
    const found = firstDifference((a as Record<string, unknown>)[key], (b as Record<string, unknown>)[key], `${path}.${key}`)
    if (found) return found
  }
  return undefined
}

const lossy = (text: string) => text.replace(/[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/g, "�")

/** acorn's dump with lone surrogates replaced where the Rust tree holds Rust text. */
function lossyDump(line: string) {
  return JSON.stringify(
    JSON.parse(line, function (key, value) {
      if (typeof value !== "string") return value
      const holder = this as Record<string, unknown>
      const rustText =
        (key === "raw" && holder.type === "Literal") ||
        (key === "directive" && holder.type === "ExpressionStatement") ||
        (key === "pattern" && typeof holder.flags === "string") ||
        (key === "message" && typeof holder.pos === "number")
      return rustText ? lossy(value) : value
    }),
  )
}

if (acorn.length !== corpus.length || rust.length !== corpus.length)
  console.log(`line counts differ: corpus ${corpus.length}, acorn ${acorn.length}, rust ${rust.length}`)
let differences = 0
let surrogates = 0
let errors = 0
for (let index = 0; index < corpus.length; index++) {
  if (acorn[index]?.startsWith('{"error"')) errors++
  if (acorn[index] === rust[index]) continue
  if (rust[index] !== undefined && lossyDump(acorn[index]) === JSON.stringify(JSON.parse(rust[index]))) {
    surrogates++
    continue
  }
  differences++
  if (differences > shown) continue
  const program = JSON.parse(corpus[index]) as string
  const where = rust[index] === undefined ? "rust output missing" : firstDifference(JSON.parse(acorn[index]), JSON.parse(rust[index]), "$")
  console.log(`#${index} ${JSON.stringify(program.length > 400 ? program.slice(0, 400) + "..." : program)}\n  ${where}`)
}
console.log(
  `${corpus.length} programs (${errors} rejected by acorn), ${differences} differences` +
    (surrogates ? `, ${surrogates} lone-surrogate lines (see compare.ts)` : ""),
)
process.exit(differences === 0 ? 0 : 1)
