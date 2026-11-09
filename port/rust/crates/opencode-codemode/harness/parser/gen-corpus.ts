// Generates the parser check's corpus (one JSON string per line) on stdout, deterministically:
//   1. every program the codemode conformance scenarios run (port/conformance/scenarios/codemode-*.test.ts),
//      imported with stub harness modules, plus every string and template literal in those files;
//   2. the hand-written snippets of snippets.ts (syntax forms, every acorn error message, regexps, literals, ...);
//   3. those snippets wrapped in function, generator, async, strict, class and block contexts;
//   4. seeded random mutations (deletions, insertions, swaps, truncations) of the valid programs.
//
// usage: bun harness/parser/gen-corpus.ts > corpus.jsonl
import { readdirSync, realpathSync } from "node:fs"
import path from "node:path"
import { parse } from "../../../../../.reference/packages/codemode/node_modules/acorn/dist/acorn.mjs"
import { randomPrograms } from "./gen-programs.ts"
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
const rand = random(20251108)
const pick = <T>(list: readonly T[]) => list[Math.floor(rand() * list.length)]
function mutate(code: string) {
  const at = Math.floor(rand() * (code.length + 1))
  const kind = rand()
  if (kind < 0.3) return code.slice(0, at) + code.slice(at + 1 + Math.floor(rand() * 3))
  if (kind < 0.65) return code.slice(0, at) + pick(pieces) + code.slice(at)
  if (kind < 0.8) return code.slice(0, at) + code.slice(at + 1, at + 2) + code.slice(at, at + 1) + code.slice(at + 2)
  if (kind < 0.9) return code.slice(0, at)
  const to = Math.floor(rand() * (code.length + 1))
  return code.slice(0, at) + code.slice(Math.min(at, to), Math.max(at, to)) + code.slice(at)
}
for (const code of [...out].filter(accepts)) {
  const count = Math.max(4, Math.min(24, Math.ceil(code.length / 40)))
  for (let i = 0; i < count; i++) add(mutate(code))
}

// Random programs from a small grammar (gen-programs.ts), mostly valid, with a mutation each.
for (const code of randomPrograms(20251109, 20000)) {
  add(code)
  add(mutate(code))
}

// Token fuzz: seeded random numeric literals, string and template escapes, regular expressions (pattern and flags)
// and identifiers, each in an expression statement.
const fuzz = random(424242)
const fuzzPick = <T>(list: readonly T[]) => list[Math.floor(fuzz() * list.length)]
const fuzzString = (alphabet: readonly string[], max: number) =>
  Array.from({ length: 1 + Math.floor(fuzz() * max) }, () => fuzzPick(alphabet)).join("")
const digits = "0123456789".split("")
const hexDigits = "0123456789abcdefABCDEF".split("")
for (let i = 0; i < 4000; i++) {
  const int = fuzzString(digits, fuzz() < 0.2 ? 40 : 18)
  const frac = fuzz() < 0.5 ? "." + fuzzString(digits, 20) : ""
  const exp = fuzz() < 0.4 ? fuzzPick(["e", "E"]) + fuzzPick(["", "+", "-"]) + fuzzString(digits, 3) : ""
  add(`x = ${int}${frac}${exp}`)
  add(`x = .${fuzzString(digits, 20)}${exp}`)
  add(`x = 0x${fuzzString(hexDigits, 24)}`)
  add(`x = 0${fuzzString("01234567".split(""), 30)}`)
  add(`x = 0b${fuzzString(["0", "1"], 70)}`)
  add(`x = 0o${fuzzString("01234567".split(""), 30)}${fuzz() < 0.3 ? "n" : ""}`)
  add(`x = ${int}${fuzzPick(["n", "_1", "_", "__1", "e", ".e1", "x", ""])}`)
}
const escapes = [
  "a", "\\n", "\\r", "\\t", "\\b", "\\f", "\\v", "\\0", "\\1", "\\7", "\\8", "\\9", "\\00", "\\08", "\\377", "\\400", "\\x4",
  "\\x41", "\\xg", "\\u004", "\\u0041", "\\u{41}", "\\u{}", "\\u{110000}", "\\u{10ffff}", "\\ud800", "\\udc00", "\\\n", "\\\r\n",
  "\\ ", "\\'", '\\"', "\\`", "\\$", "\\\\", "\\a", "\\u", "\\x", "${", "$", "{", " ", "\n", "\r", " ", "é", "😀",
]
for (let i = 0; i < 3000; i++) {
  const body = fuzzString(escapes, 6)
  add(`x = '${body.replace(/'/g, "\\'")}'`)
  add(`x = \`${body}\``)
  add(`x = tag\`${body}\``)
  add(`"use strict"; x = '${body.replace(/'/g, "\\'")}'`)
}
const regexAtoms = [
  "a", "b", ".", "^", "$", "|", "(", ")", "(?:", "(?=", "(?!", "(?<=", "(?<!", "(?<n>", "(?<m>", "\\k<n>", "\\k", "[", "]",
  "[^", "-", "*", "+", "?", "{1}", "{1,}", "{2,1}", "{", "}", "\\d", "\\D", "\\w", "\\s", "\\b", "\\B", "\\1", "\\2", "\\0",
  "\\01", "\\c", "\\cA", "\\x41", "\\x4", "\\u0041", "\\u{41}", "\\ud83d\\ude00", "\\p{L}", "\\P{Lu}", "\\p{RGI_Emoji}",
  "\\p{Script=Greek}", "\\p{Foo}", "\\q{ab}", "&&", "--", "\\-", "\\/", "\\.", "(?i:", "(?-m:", "(?i-s:", "😀", "\\",
]
for (let i = 0; i < 6000; i++) {
  const flags = fuzzPick(["", "", "u", "v", "g", "i", "gu", "uv", "s", "d", "gimsuyd", "y", "gg", "x"])
  const pattern = fuzzString(regexAtoms, 8).replace(/\//g, "\\/")
  add(`x = /${pattern}/${flags}`)
}
const idChars = [
  "a", "$", "_", "0", "é", "ℵ", "𝒜", "‌", "‍", "·", "̀", "゛", "ⸯ", "ᢅ", "\u{1e94b}",
  "\\u0061", "\\u{1d49c}", "\\u0030", "\\u200c", "\\u{2e2f}", "\\u00b7", "\\", "-",
]
for (let i = 0; i < 3000; i++) {
  const name = fuzzString(idChars, 4)
  add(`var ${name} = 1`)
  add(`x.${name}`)
  add(`class A { #${name} }`)
  add(`/(?<${name}>.)/`)
}
for (let code = 0x80; code < 0x30000; code += 97) {
  const char = String.fromCodePoint(code)
  add(`var ${char} = 1; var a${char} = 2`)
  add(`var \\u{${code.toString(16)}} = 1; var a\\u{${code.toString(16)}} = 2`)
}

// Real-world code from the reference checkout: a sorted, evenly spaced sample of the JavaScript under its
// node_modules and the TypeScript of packages/codemode (transpiled), each with a few mutations. `--real` takes a
// larger sample.
const reference = realpathSync(path.resolve(import.meta.dir, "../../../../../.reference"))
const large = Bun.argv.includes("--real")
// bun keeps every installed package once under node_modules/.bun; the other node_modules entries link there
const javascript = [...new Bun.Glob("node_modules/.bun/*/node_modules/**/*.{js,mjs,cjs}").scanSync({ cwd: reference })]
  .sort()
  .filter((file) => {
    const size = Bun.file(path.join(reference, file)).size
    return size > 500 && size < (large ? 60_000 : 12_000)
  })
const wanted = large ? 600 : 80
const sample = javascript.filter((_, index) => index % Math.max(1, Math.floor(javascript.length / wanted)) === 0).slice(0, wanted)
const typescript = [...new Bun.Glob("packages/codemode/src/**/*.ts").scanSync({ cwd: reference })].sort()
const transpiler = new Bun.Transpiler({ loader: "ts", trimUnusedImports: false })
const realWorld: string[] = []
for (const file of sample) realWorld.push(await Bun.file(path.join(reference, file)).text())
for (const file of large ? typescript : typescript.slice(0, 12)) realWorld.push(transpiler.transformSync(await Bun.file(path.join(reference, file)).text()))
for (const code of realWorld) {
  add(code)
  for (let i = 0; i < (large ? 12 : 4); i++) add(mutate(code))
}

await Bun.write(Bun.stdout, [...out].map((code) => JSON.stringify(code) + "\n").join(""))
