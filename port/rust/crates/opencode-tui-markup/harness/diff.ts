// The differential check of opencode-tui-markup: builds the cases of every corpus, answers them with the TS
// reference (harness/ts-driver.ts) and with the Rust crate (examples/markup-driver.rs), and compares the answers
// as JSON values. Reports the case count and differences per corpus; exits 1 on any difference.
//
// usage: bun harness/diff.ts --driver <markup-driver binary> --work <dir> [--count N] [--seed S] [--show N]
//        [--only <corpus prefix>]
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { spawnSync } from "node:child_process"
import { codeSamples, goldenTexts, markedDocuments, randomMarkdown, Rng, specExamples, streamSteps } from "./corpus"
import { dispose, run } from "./ts-driver"

const argv = process.argv.slice(2)
const arg = (name: string, fallback?: string) => {
  const i = argv.indexOf(`--${name}`)
  return i >= 0 ? argv[i + 1] : fallback
}
const driver = arg("driver")!
const work = arg("work")!
const count = Number(arg("count", "3000"))
const seed = Number(arg("seed", "1"))
const show = Number(arg("show", "3"))
const only = arg("only")
mkdirSync(join(work, "failures"), { recursive: true })

type Case = { corpus: string; name: string; input: Record<string, unknown> }
const cases: Case[] = []
const add = (corpus: string, name: string, input: Record<string, unknown>) => {
  if (!only || corpus.startsWith(only)) cases.push({ corpus, name, input })
}

const documents = [
  ...specExamples().map((d) => ({ ...d, corpus: "spec" })),
  ...markedDocuments().map((d) => ({ ...d, corpus: "marked-tests" })),
  ...goldenTexts().map((d) => ({ ...d, corpus: "goldens" })),
]
const rng = new Rng(seed)
for (let i = 0; i < count; i++) documents.push({ name: `random#${i}`, markdown: randomMarkdown(rng), corpus: "random" })

// The markdown props the TUI passes (message-parts.tsx TextPart): trimmed content, top-level blocks, grid tables.
const FG = "#c0caf5"
const BG = "#1a1b26"
const tui = { internalBlockMode: "top-level", tableOptions: { style: "grid", cellPaddingX: 1 }, fg: FG, bg: BG }
const caseRng = new Rng(seed + 1000)
for (const doc of documents) {
  const text = doc.markdown.trim()
  if (!text) continue
  add(`markdown/${doc.corpus}`, doc.name, { op: "markdown", ...tui, conceal: true, steps: [{ content: text, streaming: false }] })
  if (caseRng.chance(0.25)) add(`markdown-source/${doc.corpus}`, doc.name, { op: "markdown", ...tui, conceal: false, steps: [{ content: text, streaming: false }] })
  if (caseRng.chance(0.15) && text.includes("]"))
    add(`markdown-hyperlinks/${doc.corpus}`, doc.name, { op: "markdown", ...tui, conceal: true, hyperlinks: true, steps: [{ content: text, streaming: false }, { hyperlinks: false }] })
  if (caseRng.chance(0.15))
    add(`markdown-coalesced/${doc.corpus}`, doc.name, { op: "markdown", fg: FG, bg: BG, steps: [{ content: doc.markdown, streaming: false }, { conceal: false }] })
  if (caseRng.chance(doc.corpus === "random" ? 0.5 : 0.1) && text.length < 4000) {
    const steps: any[] = streamSteps(caseRng, text).map((content, i) => (i === 0 ? { content, streaming: true } : { content }))
    steps.push({ streaming: false })
    add(`markdown-stream/${doc.corpus}`, doc.name, { op: "markdown", ...tui, conceal: caseRng.chance(0.8), steps })
  }
}

// Code in every bundled language (and aliases, and one with no parser), with and without conceal.
// tree-sitter-agda's layout scanner reads an empty indent stack (`VEC_BACK` of length 0 after `deserialize` with an
// empty buffer), so in TS its result depends on the heap of the parser's earlier parses: compare those cases with a
// fresh TS client (what the first parse in the TUI gives) rather than the shared one.
const FRESH_FILETYPES = new Set(["agda"])
for (const sample of codeSamples()) {
  const { filetype, name, code } = sample
  const fresh = FRESH_FILETYPES.has(filetype)
  add(`highlight/${filetype}`, name, { op: "highlight", content: code, filetype, fresh })
  add(`code/${filetype}`, name, { op: "code", content: code, filetype, conceal: true, fresh })
  add(`code-noconceal/${filetype}`, name, { op: "code", content: code, filetype, conceal: false, fresh })
  if (caseRng.chance(0.15))
    add(`code-base/${filetype}`, name, { op: "code", content: code, filetype, conceal: true, baseHighlight: "markup.quote", fresh })
  if (code.length > 4 && caseRng.chance(0.3)) {
    const cut = 1 + caseRng.int(code.length - 1)
    const prefix = /[\ud800-\udbff]/.test(code[cut - 1]!) ? code.slice(0, cut + 1) : code.slice(0, cut)
    add(`highlight-prefix/${filetype}`, name, { op: "highlight", content: prefix, filetype, fresh })
  }
}
// Rendered markdown as the TUI shows it: every code child of the block model highlighted with the markdown link
// hooks, conceal on (rendered) and off (source), with and without hyperlink support.
for (const doc of documents) {
  const text = doc.markdown.trim()
  if (!text || text.length > 6000) continue
  add(`render/${doc.corpus}`, doc.name, { op: "markdownRender", ...tui, conceal: true, content: text })
  if (caseRng.chance(0.3)) add(`render-source/${doc.corpus}`, doc.name, { op: "markdownRender", ...tui, conceal: false, content: text })
  if (caseRng.chance(0.2) && text.includes("]("))
    add(`render-hyperlinks/${doc.corpus}`, doc.name, { op: "markdownRender", ...tui, conceal: true, hyperlinks: true, content: text })
}
// Markdown highlighting of whole documents (the conceal path frames depend on).
for (const doc of documents) {
  if (doc.markdown.length > 6000) continue
  add(`highlight-md/${doc.corpus}`, doc.name, { op: "highlight", content: doc.markdown, filetype: "markdown" })
  add(`code-md/${doc.corpus}`, doc.name, { op: "code", content: doc.markdown, filetype: "markdown", conceal: caseRng.chance(0.7) })
}

for (const doc of documents) {
  add(`lex/${doc.corpus}`, doc.name, { op: "lex", src: doc.markdown })
  // OpenTUI lexes inline only a block's text; whole documents can take marked (TS too) seconds per call.
  if (doc.markdown.length <= 1000) add(`lexInline/${doc.corpus}`, doc.name, { op: "lexInline", src: doc.markdown })
}

// ---- answer and compare
const inputPath = join(work, "cases.jsonl")
const outputPath = join(work, "answers.jsonl")
writeFileSync(inputPath, cases.map((c) => JSON.stringify(c.input)).join("\n") + "\n")
const started = Date.now()
const result = spawnSync(driver, [inputPath, outputPath], { stdio: ["ignore", "inherit", "inherit"] })
if (result.status !== 0) {
  console.error(`the Rust driver failed (status ${result.status}, signal ${result.signal})`)
  process.exit(1)
}
const rustMs = Date.now() - started
const answers = readFileSync(outputPath, "utf8").split("\n").filter(Boolean)
if (answers.length !== cases.length) {
  console.error(`the Rust driver answered ${answers.length} of ${cases.length} cases`)
  process.exit(1)
}

function equal(a: unknown, b: unknown): boolean {
  if (a === b) return true
  if (typeof a === "number" && typeof b === "number") return Number.isNaN(a) && Number.isNaN(b)
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false
  if (Array.isArray(a) !== Array.isArray(b)) return false
  if (Array.isArray(a)) {
    const bb = b as unknown[]
    return a.length === bb.length && a.every((x, i) => equal(x, bb[i]))
  }
  const ka = Object.keys(a as object).sort()
  const kb = Object.keys(b as object).sort()
  return ka.length === kb.length && ka.every((k, i) => k === kb[i] && equal((a as any)[k], (b as any)[k]))
}

/** The first path where two values differ, for the report. */
function firstDifference(a: unknown, b: unknown, path = "$"): string {
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null || Array.isArray(a) !== Array.isArray(b))
    return `${path}: TS ${JSON.stringify(a)?.slice(0, 300)} vs Rust ${JSON.stringify(b)?.slice(0, 300)}`
  const keys = new Set([...Object.keys(a as object), ...Object.keys(b as object)])
  for (const k of keys) {
    if (!equal((a as any)[k], (b as any)[k])) return firstDifference((a as any)[k], (b as any)[k], `${path}.${k}`)
  }
  return path
}

const stats = new Map<string, { cases: number; diffs: number }>()
let diffs = 0
const tsStarted = Date.now()
for (let i = 0; i < cases.length; i++) {
  const c = cases[i]!
  const ts = JSON.parse(JSON.stringify(await run(c.input)))
  const rust = JSON.parse(answers[i]!)
  const s = stats.get(c.corpus) ?? { cases: 0, diffs: 0 }
  s.cases++
  if (!equal(ts, rust)) {
    s.diffs++
    diffs++
    if (s.diffs <= show) {
      console.log(`DIFF ${c.corpus} ${c.name}\n  input: ${JSON.stringify(c.input).slice(0, 400)}\n  ${firstDifference(ts, rust)}`)
    }
    writeFileSync(join(work, "failures", `${c.corpus.replace(/\W/g, "_")}-${s.diffs}.json`), JSON.stringify({ case: c, ts, rust }, null, 2))
  }
  stats.set(c.corpus, s)
}
const tsMs = Date.now() - tsStarted
for (const [corpus, s] of [...stats].sort()) console.log(`${corpus.padEnd(28)} ${String(s.cases).padStart(7)} cases  ${s.diffs} differences`)
console.log(`${cases.length} cases, ${diffs} differences (Rust ${rustMs} ms, TS ${tsMs} ms)`)
await dispose()
process.exit(diffs === 0 ? 0 : 1)
