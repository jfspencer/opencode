// The differential check of opencode-html against the TS reference: runs every input through the TS functions in
// port/.reference and through the Rust crate (examples/html-driver.rs), and compares the UTF-8 bytes of the
// results. A TS string with a lone surrogate is compared as TextEncoder writes it (U+FFFD).
//
// usage: bun harness/diff.ts --driver <html-driver binary> --work <dir> [--count N] [--seed S] [--upstream <jsonl>]
//        [--show N]
import { mkdirSync, readFileSync, writeFileSync, existsSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import {
  BUDGET,
  CHUNK,
  CONTENT_TYPES,
  EOF_TAILS,
  MAX,
  Rng,
  budget,
  chunkStraddle,
  document,
  eofQuirk,
  mutate,
  numbering,
  pages,
  scenarioBodies,
  soup,
  splitAtBudget,
  upstreamBudgetCases,
  wrappedSlices,
} from "./corpus"

const argv = process.argv.slice(2)
const arg = (name: string, fallback?: string) => {
  const i = argv.indexOf(`--${name}`)
  return i >= 0 ? argv[i + 1] : fallback
}
const here = dirname(fileURLToPath(import.meta.url))
const port = join(here, "../../../..")
const reference = join(port, ".reference")
const driver = arg("driver")!
const work = arg("work")!
const count = Number(arg("count", "20000"))
const seed = Number(arg("seed", "1"))
const show = Number(arg("show", "5"))
const upstream = arg("upstream")
mkdirSync(join(work, "failures"), { recursive: true })

// ---- the TS reference
const tool = join(reference, "packages/core/src/tool")
const { convertHTMLToMarkdown, MAX_MARKDOWN_BYTES } = await import(join(tool, "html-markdown.ts"))
const { extractTextFromHTML } = await import(join(tool, "plugin/webfetch.ts"))
const { Mime } = await import(join(reference, "packages/core/src/mime.ts"))
const { collectBoundedResponseBody } = await import(join(tool, "http-body.ts"))
if (MAX_MARKDOWN_BYTES !== MAX) throw new Error(`MAX_MARKDOWN_BYTES is ${MAX_MARKDOWN_BYTES}, the corpus assumes ${MAX}`)
const effect = await import(Bun.resolveSync("effect", tool))
const effectHttp = await import(Bun.resolveSync("effect/http", tool))

// webfetch.ts keeps these helpers private: evaluate their source as written.
const webfetchSource = readFileSync(join(tool, "plugin/webfetch.ts"), "utf8")
const grab = (pattern: RegExp, what: string) => {
  const match = webfetchSource.match(pattern)
  if (!match) throw new Error(`webfetch.ts no longer defines ${what} as the harness expects`)
  return match[0]
}
const helperSource = [
  grab(/^const mimeFrom = .*$/m, "mimeFrom"),
  grab(/^const isImageAttachment = [\s\S]*?\n(?=const )/m, "isImageAttachment"),
  grab(/^const convert = [\s\S]*?\n}\n/m, "convert"),
].join("\n")
const helpers = new Function(
  "convertHTMLToMarkdown",
  "extractTextFromHTML",
  new Bun.Transpiler({ loader: "ts" }).transformSync(helperSource) + "\nreturn { mimeFrom, isImageAttachment, convert }",
)(convertHTMLToMarkdown, extractTextFromHTML)

const encoder = new TextEncoder()
// Inputs are strings sent as UTF-8: decode them keeping a leading BOM. Only the body decoding check (kind d) uses
// the decoder webfetch.ts uses, which drops it.
const decoder = new TextDecoder("utf-8", { ignoreBOM: true })
const bodyDecoder = new TextDecoder()

type Kind = "m" | "t" | "c" | "v" | "d" | "b"
type Case = { source: string; kind: Kind; input: Uint8Array }

const tsAnswer = async (kind: Kind, input: Uint8Array): Promise<string> => {
  const text = () => decoder.decode(input)
  try {
    switch (kind) {
      case "m":
        return convertHTMLToMarkdown(text())
      case "t":
        return extractTextFromHTML(text())
      case "c": {
        const mime = helpers.mimeFrom(text())
        const image = helpers.isImageAttachment(mime)
        const textual = Mime.isTextual(mime)
        // webfetch.ts execute: the image check, then the textual one.
        const check = image
          ? `Unsupported fetched image content type: ${mime}`
          : !textual
            ? `Unsupported fetched file content type: ${mime}`
            : "ok"
        return `${mime}\n${image}\n${textual}\n${check}`
      }
      case "v": {
        const [contentType, format, ...content] = text().split("\0")
        return helpers.convert(content.join("\0"), contentType, format)
      }
      case "d":
        return bodyDecoder.decode(input)
      case "b": {
        const [max, contentLength, chunks] = text().split("\0")
        const sizes = chunks ? chunks.split(",").map(Number) : []
        const stream = new ReadableStream({
          start(controller) {
            for (const size of sizes) controller.enqueue(new Uint8Array(size))
            controller.close()
          },
        })
        const headers = contentLength === "-" ? {} : { "content-length": contentLength }
        const response = effectHttp.HttpClientResponse.fromWeb(
          effectHttp.HttpClientRequest.get("http://body.invalid/"),
          new Response(stream, { headers }),
        )
        const exit = await effect.Effect.runPromiseExit(
          collectBoundedResponseBody(response, Number(max), () => new Error("too large")),
        )
        return exit._tag === "Success" ? `ok:${exit.value.byteLength}` : "too-large"
      }
    }
  } catch (error) {
    return `THROW: ${error instanceof Error ? `${error.name}: ${error.message}` : String(error)}`
  }
}

// ---- running a batch through Rust
let batchNo = 0
const rust = (cases: Case[]) => {
  const records = join(work, "records.bin")
  const answers = join(work, "answers.bin")
  const size = cases.reduce((n, c) => n + 5 + c.input.byteLength, 0)
  const buf = new Uint8Array(size)
  const view = new DataView(buf.buffer)
  let at = 0
  for (const c of cases) {
    buf[at] = c.kind.charCodeAt(0)
    view.setUint32(at + 1, c.input.byteLength, true)
    buf.set(c.input, at + 5)
    at += 5 + c.input.byteLength
  }
  writeFileSync(records, buf)
  const run = Bun.spawnSync([driver, records, answers], { stderr: "pipe" })
  if (run.exitCode !== 0) throw new Error(`driver failed (${run.exitCode}): ${run.stderr.toString()}`)
  const out = new Uint8Array(readFileSync(answers))
  const outView = new DataView(out.buffer, out.byteOffset)
  const results: Uint8Array[] = []
  at = 0
  while (at < out.byteLength) {
    const len = outView.getUint32(at, true)
    results.push(out.subarray(at + 4, at + 4 + len))
    at += 4 + len
  }
  if (results.length !== cases.length) throw new Error(`driver answered ${results.length} of ${cases.length}`)
  return results
}

const totals = new Map<string, { cases: number; diffs: number }>()
let failures = 0
const equal = (a: Uint8Array, b: Uint8Array) => a.byteLength === b.byteLength && a.every((x, i) => x === b[i])
const preview = (bytes: Uint8Array | string, n = 300) => {
  const s = typeof bytes === "string" ? bytes : decoder.decode(bytes)
  return JSON.stringify(s.length > n ? s.slice(0, n) + `…(${s.length} units)` : s)
}

let pending: Case[] = []
let pendingBytes = 0
const flush = async () => {
  if (!pending.length) return
  const cases = pending
  pending = []
  pendingBytes = 0
  batchNo++
  const results = rust(cases)
  for (const [i, c] of cases.entries()) {
    const expected = encoder.encode(await tsAnswer(c.kind, c.input))
    const total = totals.get(c.source) ?? { cases: 0, diffs: 0 }
    totals.set(c.source, total)
    total.cases++
    if (equal(expected, results[i])) continue
    total.diffs++
    failures++
    const file = join(work, "failures", `${failures}-${c.kind}.in`)
    writeFileSync(file, c.input)
    if (failures <= show) {
      console.log(`\nDIFF ${c.source} [${c.kind}] input ${file}`)
      console.log(`  input: ${preview(c.input)}`)
      console.log(`  ts:    ${preview(expected)}`)
      console.log(`  rust:  ${preview(results[i])}`)
      // the first differing byte, with context
      let d = 0
      while (d < expected.byteLength && d < results[i].byteLength && expected[d] === results[i][d]) d++
      console.log(`  first difference at byte ${d}: ts ${preview(expected.subarray(Math.max(0, d - 40), d + 60), 200)}`)
      console.log(`                          rust ${preview(results[i].subarray(Math.max(0, d - 40), d + 60), 200)}`)
    }
  }
}

const add = async (source: string, kind: Kind, input: string | Uint8Array) => {
  const bytes = typeof input === "string" ? encoder.encode(input) : input
  pending.push({ source, kind, input: bytes })
  pendingBytes += bytes.byteLength
  if (pending.length >= 4000 || pendingBytes > 48 * 1024 * 1024) await flush()
}
const html = async (source: string, input: string) => {
  // Both sides see the same string: the UTF-8 round trip replaces any lone surrogate.
  const bytes = encoder.encode(input)
  await add(source, "m", bytes)
  await add(source, "t", bytes)
}

const started = performance.now()
const rng = new Rng(seed)

// 1. The HTML the conformance scenarios serve.
for (const { body, contentType } of scenarioBodies(join(port, "conformance/scenarios/tools-webfetch.test.ts"))) {
  await html("scenarios", body)
  for (const format of ["markdown", "text", "html"]) await add("scenarios", "v", `${contentType}\0${format}\0${body}`)
}

// 2. The upstream tests' inputs.
if (upstream && existsSync(upstream)) {
  const seen = new Set<string>()
  for (const line of readFileSync(upstream, "utf8").split("\n")) {
    if (!line) continue
    const { html: input } = JSON.parse(line) as { html: string }
    if (seen.has(input)) continue
    seen.add(input)
    await html("upstream tests", input)
  }
} else console.log("(no recording of the upstream tests: run harness/run.sh)")
for (const input of upstreamBudgetCases()) await html("upstream tests", input)

// 3. Hand-written pages, as written, mutated, and run together into documents that span several write chunks.
const written = pages(join(here, "pages"))
for (const page of written) {
  await html("pages", page.html)
  for (let i = 0; i < 40; i++) await html("pages (mutated)", mutate(rng, page.html))
}
for (let i = 0; i < 60; i++) {
  let long = ""
  const target = CHUNK * (1 + rng.int(rng.chance(0.8) ? 4 : 16)) + rng.int(CHUNK)
  while (long.length < target) long += rng.chance(0.2) ? mutate(rng, rng.pick(written).html) : rng.pick(written).html
  await html("pages (long)", long)
}

// 4. Generated documents.
for (let i = 0; i < count; i++) {
  const r = rng.next()
  if (r < 0.45) await html("soup", soup(rng, 1 + rng.int(rng.chance(0.9) ? 40 : 400)))
  else if (r < 0.9) await html("documents", document(rng))
  else await html("soup x documents", soup(rng, 10) + document(rng) + soup(rng, 10))
}
// Deep nesting around the 10 000-tag fallback.
for (let i = 0; i < 40; i++) {
  const depth = 9_990 + rng.int(30)
  const tag = rng.pick(["div", "span", "b", "li", "blockquote", "pre", "a", "code", "table", "object", "td"])
  const inner = rng.pick(["text", "<script>x</script>", "<p>a</p>", soup(rng, 5), "<em>x</em>&amp;"])
  const wrap = rng.pick(["", "<div hidden>", "<object>", "<pre>", "<a href=x>", "<code>", "<table><tr><td>"])
  await html("deep", `<p>start</p>${wrap}${`<${tag}>`.repeat(depth)}${inner}${rng.chance(0.7) ? `</${tag}>`.repeat(depth) : ""}<p>end ${soup(rng, 3)}</p>`)
}
// Around the 64 Ki-unit write chunks.
for (let i = 0; i < Math.max(200, count / 20); i++) await html("chunk boundaries", chunkStraddle(rng))
for (const input of wrappedSlices()) await html("chunk boundaries", input)
// End of input inside a construct.
for (let i = 0; i < Math.max(300, count / 20); i++) await html("end of input", eofQuirk(rng))
for (const tail of EOF_TAILS) await html("end of input", tail)
// List numbers.
for (let i = 0; i < Math.max(300, count / 20); i++) await html("list numbers", numbering(rng))
// At the 5 MiB budget.
for (let i = 0; i < Math.max(30, count / 400); i++) await html("budget", budget(rng))
await html("budget", "x".repeat(MAX))
await html("budget", "x".repeat(MAX + 1))
await html("budget", "é".repeat(MAX / 2 + 10))
await html("budget", `<pre>${"` x ".repeat(4096)}</pre>`)
await html("budget", `<p>${"*".repeat(BUDGET / 2 - 2)}</p><p><code>\`a</code></p>`)
for (const input of splitAtBudget()) await html("budget", input)

// 5. webfetch helpers.
for (const contentType of CONTENT_TYPES) await add("content types", "c", contentType)
for (let i = 0; i < 2000; i++) {
  let ct = rng.pick(CONTENT_TYPES)
  if (rng.chance(0.5)) ct = ct.toUpperCase()
  if (rng.chance(0.3)) ct = rng.pick([" ", "\t", " ", "﻿", ""]) + ct + rng.pick([" ", "; charset=x", ";", "　"])
  if (rng.chance(0.2)) ct = ct.replace(/t/g, () => rng.pick(["t", "T", "ť"]))
  await add("content types", "c", ct)
  const format = rng.pick(["markdown", "text", "html"])
  await add("convert", "v", `${rng.chance(0.5) ? rng.pick(CONTENT_TYPES) : ct}\0${format}\0${rng.chance(0.5) ? soup(rng, 10) : document(rng, 3)}`)
}
for (let i = 0; i < 3000; i++) {
  const bytes = new Uint8Array(rng.int(40))
  for (let k = 0; k < bytes.length; k++)
    bytes[k] = rng.chance(0.5) ? rng.pick([0xef, 0xbb, 0xbf, 0xed, 0xa0, 0x80, 0xf0, 0x9f, 0xf4, 0x90, 0xc0, 0xc2, 0xe0, 0xff, 0xfe]) : rng.int(256)
  if (rng.chance(0.2)) bytes.set([0xef, 0xbb, 0xbf].slice(0, bytes.length))
  await add("decoding", "d", bytes)
}
const lengths = ["", "0", "-0", "1", "99", "100", "101", " 100", "100 ", "+100", "-1", "1e3", "0x10", "12abc", "abc",
  "9007199254740991", "9007199254740992", "99999999999999999999", "5242880", "5242881", "00101", " 101", "１０１"]
for (let i = 0; i < 1500; i++) {
  const max = rng.pick([0, 1, 99, 100, 101, 1000, 5242880])
  const contentLength = rng.chance(0.15) ? "-" : rng.chance(0.7) ? rng.pick(lengths) : String(rng.int(300))
  const chunks = Array.from({ length: rng.int(4) }, () => rng.int(rng.chance(0.8) ? 60 : 3)).join(",")
  // A header value fetch's Headers would refuse or rewrite is not one the tool can see.
  let valid = true
  try {
    valid = contentLength === "-" || new Headers({ "content-length": contentLength }).get("content-length") === contentLength
  } catch {
    valid = false
  }
  if (valid) await add("body limit", "b", `${max}\0${contentLength}\0${chunks}`)
}
await flush()

// ---- summary
let total = 0
console.log(`\nseed ${seed}, ${batchNo} batches, ${((performance.now() - started) / 1000).toFixed(1)} s`)
for (const [source, { cases, diffs }] of totals) {
  total += cases
  console.log(`  ${source.padEnd(20)} ${String(cases).padStart(7)} cases  ${diffs ? `${diffs} DIFFERENT` : "identical"}`)
}
console.log(`  ${"total".padEnd(20)} ${String(total).padStart(7)} cases  ${failures} different`)
if (failures) console.log(`failing inputs: ${join(work, "failures")}`)
process.exit(failures ? 1 : 0)
