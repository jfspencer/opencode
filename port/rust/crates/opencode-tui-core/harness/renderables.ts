// Renderables: LineNumber and Diff scenes built from generated cases (hand-picked and random) on a test renderer,
// rendered for a few frames; every cell's character and colors are recorded. The cases go to a file for the Rust
// side (examples/renderables.rs), which builds the same scenes.
// usage: bun renderables.ts <cases out> <records out> [random cases=300]
import { writeFileSync } from "node:fs"
import { core, testing } from "./core"

const { BoxRenderable, TextRenderable, LineNumberRenderable, DiffRenderable, RGBA } = core

let seed = 0x2468ace
function random() {
  seed ^= seed << 13
  seed >>>= 0
  seed ^= seed >>> 17
  seed ^= seed << 5
  seed >>>= 0
  return seed / 0x100000000
}
const int = (lo: number, hi: number) => lo + Math.floor(random() * (hi - lo + 1))
const pick = <T>(items: readonly T[]) => items[int(0, items.length - 1)]!
const maybe = (p = 0.5) => random() < p

const COLORS = ["#1a4d1a", "#4d1a1a", "#22c55e", "#ef4444", "#888888", "#ffffff", "#20202080", "#3a6ff0", "transparent"]
const WORDS = ["alpha", "beta", "gamma", "delta", "export", "const", "x", "=", "return", "a + b", "{", "}", "()", "word"]
const SIGNS = [" +", " -", "+", "-", ">", "!!", "→", "★"]

type LineColor = string | { gutter?: string; content?: string }
type Sign = { before?: string; beforeColor?: string; after?: string; afterColor?: string }
export type Case =
  | {
      name: string
      kind: "line_number"
      width: number
      height: number
      boxWidth: number
      content: string
      wrap: "word" | "char" | "none"
      fg?: string
      bg?: string
      minWidth?: number
      paddingRight?: number
      lineColors?: [number, LineColor][]
      signs?: [number, Sign][]
      hide?: number[]
      numbers?: [number, number][]
      offset?: number
      show?: boolean
      layout?: { minWidth: number; paddingLeft: number; paddingRight: number; minDigits: number; minAfterWidth: number }
    }
  | {
      name: string
      kind: "diff"
      width: number
      height: number
      boxWidth: number
      diff: string
      view?: "unified" | "split"
      wrap?: "word" | "char" | "none"
      fg?: string
      showLineNumbers?: boolean
      colors?: Record<string, string>
      gutter?: { paddingLeft?: number; paddingRight?: number; minDigits?: number }
      minHeight?: number
      // Set after the first frames (a prop change on a mounted diff).
      later?: { view?: "unified" | "split"; wrap?: "word" | "char" | "none"; diff?: string; boxWidth?: number }
    }

function line() {
  return Array.from({ length: int(0, 12) }, () => pick(WORDS)).join(" ")
}

function lineNumberCase(index: number): Case {
  const lines = Array.from({ length: int(1, 14) }, line)
  const count = lines.length
  const lineColors: [number, LineColor][] = maybe()
    ? Array.from({ length: int(1, 4) }, () => [
        int(0, count),
        maybe() ? pick(COLORS) : { gutter: maybe() ? pick(COLORS) : undefined, content: maybe() ? pick(COLORS) : undefined },
      ])
    : []
  const signs: [number, Sign][] = maybe()
    ? Array.from({ length: int(1, 4) }, () => [
        int(-1, count),
        {
          before: maybe(0.3) ? pick(SIGNS) : undefined,
          beforeColor: maybe(0.3) ? pick(COLORS) : undefined,
          after: maybe(0.7) ? pick(SIGNS) : undefined,
          afterColor: maybe() ? pick(COLORS) : undefined,
        },
      ])
    : []
  return {
    name: `line-number-${index}`,
    kind: "line_number",
    width: int(20, 60),
    height: int(6, 24),
    boxWidth: int(8, 60),
    content: lines.join("\n"),
    wrap: pick(["word", "char", "none"] as const),
    fg: maybe() ? pick(COLORS) : undefined,
    bg: maybe() ? pick(COLORS) : undefined,
    minWidth: maybe(0.3) ? int(0, 6) : undefined,
    paddingRight: maybe(0.3) ? int(0, 3) : undefined,
    lineColors,
    signs,
    hide: maybe(0.3) ? Array.from({ length: int(1, 3) }, () => int(0, count)) : undefined,
    numbers: maybe(0.3) ? Array.from({ length: int(1, 4) }, () => [int(0, count), int(1, 2000)] as [number, number]) : undefined,
    offset: maybe(0.2) ? int(0, 120) : undefined,
    show: maybe(0.1) ? false : undefined,
    layout: maybe(0.2)
      ? { minWidth: int(0, 5), paddingLeft: int(0, 2), paddingRight: int(0, 2), minDigits: int(0, 4), minAfterWidth: int(0, 2) }
      : undefined,
  }
}

function hunk(oldStart: number, newStart: number) {
  const lines: string[] = []
  let old = 0
  let add = 0
  for (let i = int(1, 10); i > 0; i--) {
    const kind = pick([" ", " ", "+", "-"])
    const text = line()
    lines.push(kind + text)
    if (kind !== "+") old++
    if (kind !== "-") add++
  }
  if (maybe(0.1)) lines.push("\\ No newline at end of file")
  return `@@ -${oldStart},${old} +${newStart},${add} @@\n${lines.join("\n")}`
}

function randomDiff() {
  const head = maybe(0.7) ? "--- a/file.ts\n+++ b/file.ts\n" : ""
  const hunks: string[] = []
  let start = int(1, 400)
  for (let i = int(1, 3); i > 0; i--) {
    hunks.push(hunk(start, start + int(-3, 3) < 1 ? 1 : start + int(0, 3)))
    start += int(12, 80)
  }
  return head + hunks.join("\n") + (maybe() ? "\n" : "")
}

function diffCase(index: number): Case {
  return {
    name: `diff-${index}`,
    kind: "diff",
    width: int(30, 160),
    height: int(8, 40),
    boxWidth: int(20, 160),
    diff: randomDiff(),
    view: maybe(0.8) ? pick(["unified", "split"] as const) : undefined,
    wrap: maybe(0.8) ? pick(["word", "char", "none"] as const) : undefined,
    fg: maybe() ? pick(COLORS) : undefined,
    showLineNumbers: maybe(0.1) ? false : undefined,
    colors: maybe()
      ? Object.fromEntries(
          ["addedBg", "removedBg", "contextBg", "addedSignColor", "removedSignColor", "lineNumberFg", "lineNumberBg", "addedLineNumberBg", "removedLineNumberBg", "addedContentBg"]
            .filter(() => maybe(0.4))
            .map((key) => [key, pick(COLORS)]),
        )
      : undefined,
    gutter: maybe(0.4) ? { paddingLeft: maybe() ? int(0, 3) : undefined, minDigits: maybe() ? int(1, 4) : undefined } : undefined,
    minHeight: maybe(0.2) ? int(1, 12) : undefined,
    later: maybe(0.2)
      ? maybe()
        ? { view: pick(["unified", "split"] as const) }
        : maybe()
          ? { boxWidth: int(20, 160) }
          : { diff: randomDiff() }
      : undefined,
  }
}

const SWAP = "@@ -1,5 +1,5 @@\n export function add(a: number, b: number) {\n-  return a + b\n+  return b + a // swapped\n }\n \n export const twice = (n: number) => add(n, n)\n"
const handPicked: Case[] = [
  { name: "ln-basic", kind: "line_number", width: 40, height: 8, boxWidth: 40, content: "one\ntwo\nthree", wrap: "word" },
  { name: "ln-write", kind: "line_number", width: 50, height: 10, boxWidth: 50, content: "export const x = 1\n\nexport function y() {\n  return x + 1\n}", wrap: "word", fg: "#888888", minWidth: 3, paddingRight: 1 },
  { name: "ln-wrap", kind: "line_number", width: 20, height: 12, boxWidth: 20, content: "a long line that wraps around the gutter\nshort", wrap: "word", lineColors: [[0, "#4d1a1a"], [1, { gutter: "#1a4d1a" }]] },
  { name: "ln-signs", kind: "line_number", width: 30, height: 6, boxWidth: 30, content: "a\nb\nc", wrap: "none", signs: [[0, { after: " +", afterColor: "#22c55e" }], [1, { before: "★", after: " -" }]] },
  { name: "diff-unified", kind: "diff", width: 80, height: 12, boxWidth: 80, diff: SWAP, view: "unified", wrap: "word" },
  { name: "diff-split", kind: "diff", width: 160, height: 12, boxWidth: 160, diff: SWAP, view: "split", wrap: "word" },
  { name: "diff-split-narrow", kind: "diff", width: 76, height: 16, boxWidth: 76, diff: SWAP, view: "split", wrap: "word" },
  { name: "diff-gutter", kind: "diff", width: 80, height: 12, boxWidth: 78, diff: SWAP, view: "unified", wrap: "word", gutter: { paddingLeft: 1, minDigits: 1 }, minHeight: 5, colors: { lineNumberBg: "#202020", contextBg: "#202020" } },
  { name: "diff-none", kind: "diff", width: 40, height: 10, boxWidth: 40, diff: SWAP, view: "unified", wrap: "none" },
  { name: "diff-error", kind: "diff", width: 60, height: 10, boxWidth: 60, diff: "--- a/x\nnot a patch", view: "unified" },
]

const count = Number(process.argv[4] ?? 300)
const cases: Case[] = [...handPicked]
for (let i = 0; i < count; i++) cases.push(i % 2 === 0 ? lineNumberCase(i) : diffCase(i))

const hex2 = (value: number) =>
  Math.round(Math.max(0, Math.min(1, value)) * 255)
    .toString(16)
    .padStart(2, "0")
const encode = (c: { r: number; g: number; b: number; a: number }) => hex2(c.r) + hex2(c.g) + hex2(c.b) + hex2(c.a)

function lineColor(value: LineColor) {
  if (typeof value === "string") return value
  return { gutter: value.gutter, content: value.content }
}

async function run(item: Case) {
  const setup = await testing.createTestRenderer({ width: item.width, height: item.height })
  const { renderer } = setup
  const box = new BoxRenderable(renderer, { id: "box", width: item.boxWidth, flexDirection: "column" })
  renderer.root.add(box)
  let mounted: any
  if (item.kind === "line_number") {
    const ln = new LineNumberRenderable(renderer, { id: "ln" })
    if (item.fg) ln.fg = item.fg
    if (item.bg) ln.bg = item.bg
    if (item.minWidth !== undefined) ln.minWidth = item.minWidth
    if (item.paddingRight !== undefined) ln.paddingRight = item.paddingRight
    const text = new TextRenderable(renderer, { id: "t", content: item.content, wrapMode: item.wrap })
    ln.add(text)
    if (item.lineColors) ln.setLineColors(new Map(item.lineColors.map(([l, c]) => [l, lineColor(c)])))
    if (item.signs) ln.setLineSigns(new Map(item.signs))
    if (item.hide) ln.setHideLineNumbers(new Set(item.hide))
    if (item.numbers) ln.setLineNumbers(new Map(item.numbers))
    if (item.offset !== undefined) ln.lineNumberOffset = item.offset
    if (item.layout) ln.setGutterLayout(item.layout)
    if (item.show === false) ln.showLineNumbers = false
    box.add(ln)
    mounted = ln
  } else {
    // As @opentui/solid creates it: the id, then each prop.
    const diff = new DiffRenderable(renderer, { id: "diff" })
    if (item.view) diff.view = item.view
    diff.showLineNumbers = item.showLineNumbers ?? true
    diff.width = "100%"
    if (item.wrap) diff.wrapMode = item.wrap
    if (item.fg) diff.fg = item.fg
    for (const [key, value] of Object.entries(item.colors ?? {})) (diff as any)[key] = value
    diff.diff = item.diff
    if (item.minHeight !== undefined) diff.minHeight = item.minHeight
    if (item.gutter) diff.gutter = item.gutter
    box.add(diff)
    mounted = diff
  }
  for (let i = 0; i < 3; i++) {
    await setup.renderOnce()
    await Promise.resolve()
  }
  if (item.kind === "diff" && item.later) {
    if (item.later.view) mounted.view = item.later.view
    if (item.later.wrap) mounted.wrapMode = item.later.wrap
    if (item.later.diff) mounted.diff = item.later.diff
    if (item.later.boxWidth) box.width = item.later.boxWidth
    for (let i = 0; i < 3; i++) {
      await setup.renderOnce()
      await Promise.resolve()
    }
  }
  const frame = setup.captureSpans()
  const out = [`dump ${item.name}`]
  const chars = setup.captureCharFrame().split("\n")
  frame.lines.forEach((row: any, y: number) => {
    const cells: string[] = []
    for (const span of row.spans) for (let i = 0; i < span.width; i++) cells.push(`${encode(span.fg)}/${encode(span.bg)}`)
    out.push(`${y} ${chars[y] ?? ""}`)
    out.push(`${y} ${cells.join(" ")}`)
  })
  await setup.dispose()
  return out
}

const records: string[] = []
for (const item of cases) {
  try {
    records.push(...(await run(item)))
  } catch (error) {
    records.push(`error ${item.name} ${String(error).split("\n")[0]}`)
  }
}
writeFileSync(process.argv[2]!, cases.map((c) => JSON.stringify(c)).join("\n") + "\n")
writeFileSync(process.argv[3]!, records.join("\n") + "\n")
