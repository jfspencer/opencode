// The oracle side of the parser check: parses every program of a JSONL corpus (one JSON string per line) with
// acorn the way packages/codemode does and prints one JSON line per program: the tree, or
// `{"error":{"message","pos","loc"}}`. Object keys are sorted; a BigInt literal's value is written
// `{"$bigint": digits}`, a regex literal's value `{"$regexp": true}` (acorn's RegExp, or null where the host cannot
// build one), and a non-finite number `{"$num": "Infinity"}`. src/parser/estree.rs writes the same text.
//
// usage: bun harness/parser/acorn-dump.ts corpus.jsonl > acorn.jsonl
import { parse } from "../../../../../.reference/packages/codemode/node_modules/acorn/dist/acorn.mjs"

const options = {
  ecmaVersion: "latest",
  sourceType: "script",
  allowReturnOutsideFunction: true,
  allowAwaitOutsideFunction: true,
  allowImportExportEverywhere: true,
  locations: true,
} as const

function replacer(this: Record<string, unknown>, key: string, value: unknown) {
  if (key === "value" && this.type === "Literal" && this.regex) return { $regexp: true }
  if (typeof value === "bigint") return { $bigint: value.toString() }
  if (typeof value === "number" && !Number.isFinite(value)) return { $num: String(value) }
  if (value && typeof value === "object" && !Array.isArray(value))
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((name) => [name, (value as Record<string, unknown>)[name]]),
    )
  return value
}

function dump(code: string) {
  try {
    return JSON.stringify(parse(code, options), replacer)
  } catch (error) {
    if (!(error instanceof SyntaxError)) return JSON.stringify({ crash: String(error) })
    const located = error as SyntaxError & { pos: number; loc: { line: number; column: number } }
    return JSON.stringify({ error: { message: located.message, pos: located.pos, loc: located.loc } }, replacer)
  }
}

const lines = (await Bun.file(Bun.argv[2]).text()).split("\n").filter((line) => line.length > 0)
await Bun.write(Bun.stdout, lines.map((line) => dump(JSON.parse(line)) + "\n").join(""))
