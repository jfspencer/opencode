// Generates the parser check's corpus (one JSON string per line) on stdout, deterministically:
//   1. every program the codemode conformance scenarios run (port/conformance/scenarios/codemode-*.test.ts),
//      imported with stub harness modules, plus every string and template literal in those files;
//   2. the hand-written snippets of snippets.ts (syntax forms, every acorn error message, regexps, literals, ...);
//   3. those snippets wrapped in function, generator, async, strict, class and block contexts;
//   4. seeded random mutations (deletions, insertions, swaps, truncations) of the valid programs.
//
// usage: bun harness/parser/gen-corpus.ts > corpus.jsonl
import { readdirSync } from "node:fs"
import path from "node:path"
import { parse } from "../../../../../.reference/packages/codemode/node_modules/acorn/dist/acorn.mjs"
import { contexts, snippets } from "./snippets.ts"
import { collected } from "./scenario-stub.ts"

const scenariosDir = path.resolve(import.meta.dir, "../../../../../conformance/scenarios")
const stub = path.resolve(import.meta.dir, "scenario-stub.ts")

Bun.plugin({
  name: "conformance-stubs",
  setup(build) {
    build.onLoad({ filter: /conformance\/lib\/(openrouter|scenario|model)\.ts$/ }, () => ({
      contents: `export * from ${JSON.stringify(stub)}`,
      loader: "ts",
    }))
  },
})

const options = {
  ecmaVersion: "latest",
  sourceType: "script",
  allowReturnOutsideFunction: true,
  allowAwaitOutsideFunction: true,
  allowImportExportEverywhere: true,
  locations: true,
} as const

function accepts(code: string) {
  try {
    parse(code, options)
    return true
  } catch {
    return false
  }
}

// mulberry32
function random(seed: number) {
  let state = seed >>> 0
  return () => {
    state = (state + 0x6d2b79f5) >>> 0
    let t = state
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

/** The string and template literals of a TypeScript file, cooked, as further candidate programs. */
function literals(source: string) {
  const js = new Bun.Transpiler({ loader: "ts" }).transformSync(source)
  const found: string[] = []
  const walk = (node: unknown) => {
    if (!node || typeof node !== "object") return
    if (Array.isArray(node)) return node.forEach(walk)
    const record = node as Record<string, unknown>
    if (record.type === "Literal" && typeof record.value === "string" && record.value.length > 2) found.push(record.value)
    if (record.type === "TemplateLiteral") {
      const quasis = record.quasis as Array<{ value: { cooked: string | null } }>
      const text = quasis.map((quasi) => quasi.value.cooked ?? "").join("x")
      if (text.length > 2) found.push(text)
    }
    for (const value of Object.values(record)) walk(value)
  }
  walk(parse(js, { ecmaVersion: "latest", sourceType: "module" }))
  return found
}

const files = readdirSync(scenariosDir)
  .filter((name) => name.startsWith("codemode-") && name.endsWith(".test.ts"))
  .sort()
const scenarioPrograms: string[] = []
for (const name of files) {
  const file = path.join(scenariosDir, name)
  await import(file)
  scenarioPrograms.push(...literals(await Bun.file(file).text()))
}
scenarioPrograms.unshift(...collected)

const out = new Set<string>()
const add = (code: string) => out.add(code)

for (const code of scenarioPrograms) add(code)
for (const code of snippets) add(code)
for (const code of snippets) for (const wrap of contexts) add(wrap(code))

// Mutations of the accepted programs: small, seeded edits around tokens and at random offsets.
const pieces = [
  "", " ", "\n", ";", ",", ".", "(", ")", "[", "]", "{", "}", "=", "=>", "+", "-", "*", "/", "%", "!", "?", "?.", ":",
  "'", '"', "`", "${", "\\", "\\u", "#", "@", "<", ">", "&", "|", "^", "~", "0", "1", "0x", "n", "e", "_", "a", "let",
  "var", "const", "await", "async", "yield", "function", "class", "new", "this", "super", "return", "import", "export",
  "in", "of", "get", "set", "static", "using", " ", "\r\n", "/*", "*/", "//", "...", "**", "??", "&&", "||",
]
const accepted = [...out].filter(accepts)
const rand = random(20251108)
const pick = <T>(list: readonly T[]) => list[Math.floor(rand() * list.length)]
for (const code of accepted) {
  const count = Math.max(4, Math.min(24, Math.ceil(code.length / 40)))
  for (let i = 0; i < count; i++) {
    const at = Math.floor(rand() * (code.length + 1))
    const kind = rand()
    let mutated: string
    if (kind < 0.3) mutated = code.slice(0, at) + code.slice(at + 1 + Math.floor(rand() * 3))
    else if (kind < 0.65) mutated = code.slice(0, at) + pick(pieces) + code.slice(at)
    else if (kind < 0.8) mutated = code.slice(0, at) + code.slice(at + 1, at + 2) + code.slice(at, at + 1) + code.slice(at + 2)
    else if (kind < 0.9) mutated = code.slice(0, at)
    else {
      const to = Math.floor(rand() * (code.length + 1))
      mutated = code.slice(0, at) + code.slice(Math.min(at, to), Math.max(at, to)) + code.slice(at)
    }
    add(mutated)
  }
}

await Bun.write(Bun.stdout, [...out].map((code) => JSON.stringify(code) + "\n").join(""))
