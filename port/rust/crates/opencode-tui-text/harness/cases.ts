// Deterministic case generation for the differential check: Unicode-heavy text (CJK, emoji with ZWJ, skin tones
// and flags, combining marks, Indic conjuncts, tabs, controls, special spaces, long and empty lines, CRLF) and
// random operation programs over text buffers and views, plus hand-picked edge cases.
import type { Case, Op } from "./ts-exec.ts"

export class Rng {
  private state: number
  constructor(seed: number) {
    this.state = seed >>> 0 || 1
  }
  next(): number {
    // mulberry32
    let t = (this.state = (this.state + 0x6d2b79f5) >>> 0)
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
  int(n: number): number {
    return Math.floor(this.next() * n)
  }
  range(a: number, b: number): number {
    return a + this.int(b - a + 1)
  }
  pick<T>(items: readonly T[]): T {
    return items[this.int(items.length)]
  }
  chance(p: number): boolean {
    return this.next() < p
  }
}

const cp = (...points: number[]) => String.fromCodePoint(...points)

// Fixed graphemes and sequences that exercise the width and break rules.
export const SPECIALS: string[] = [
  "é", "é̂̃", "́", "⃝", "a⃝", "ö̲",
  "👍", "👍🏽", "👍🏿", "🏽", "👨‍👩‍👧‍👦", "👩🏽‍🚀", "🏳️‍🌈", "🏴‍☠️", "👨🏻‍❤️‍💋‍👨🏿", "🧑‍🤝‍🧑", "❤️", "❤", "☺️", "☺︎", "✌🏼",
  "🇯🇵", "🇺🇸", "🇨🇭🇩🇪", "🇦", "🇦🇧🇨", "1️⃣", "#️⃣", "*⃣", "©️", "™", "↔️",
  "‍", "‌", "​", "⁠", "﻿", "­", "͏", "᠎", "a‍b", "👨‍", "‍👨",
  "क्ष", "क्‍ष", "क्‌ष", "नमस्ते", "र्क", "कि", "कः", "স্ত্র", "ক্ষ", "த்து",
  "각", "ᅟᅠ", "ᄀ", "ᆨ", "각", "한국어",
  "؀a", "؀١", "ൎക", "กำ", "ภาษาไทย", "مرحبا", "بَّ", "שָׁלוֹם",
  " ", "　", " ", " ", " ", " ",
  "\t", "\t\t", " \t ", "\x01", "\x1b", "\x7f", "\x00", "\u0085", "\u009b", "\u0080", "\u009f",
  "你好", "世界", "日本語", "カタカナ", "ﾊﾝｶｸ", "ｱﾞ", "が", "・", "、", "。", "！", "，", "：", "？",
  "Ｆｕｌｌ", "𠀀", "𪚥", "⸺", "⸻", "͸", "", "￿", "�",
  "a-b", "a/b", "a\\b", "(x)", "[y]", "{z}", "x.y,z;w:v!u?", "foo_bar", "x日y", "日a", "丽abc",
]

const WORDS = ["hello", "world", "a", "I", "the", "lorem", "ipsum", "dolor", "x", "foo_bar", "12345", "opencode", "Rust", "TUI", "wrap", "abcdefghijklmnopqrstuvwxyz"]
const PUNCT = [" ", " ", " ", "  ", "-", "/", "\\", ".", ",", ";", ":", "!", "?", "(", ")", "[", "]", "{", "}", "'", '"', "`", "|", "<", ">", "$", "│"]

function randomCodePoint(r: Rng): string {
  for (;;) {
    const roll = r.int(10)
    let c: number
    if (roll < 3) c = r.range(0x20, 0x7e)
    else if (roll < 5) c = r.range(0x80, 0x2fff)
    else if (roll < 7) c = r.range(0x3000, 0xffff)
    else if (roll < 9) c = r.range(0x1f000, 0x1faff)
    else c = r.range(0x10000, 0x10ffff)
    if (c >= 0xd800 && c <= 0xdfff) continue
    if (c === 0x0a || c === 0x0d) continue
    return cp(c)
  }
}

function cjk(r: Rng, n: number): string {
  let s = ""
  for (let i = 0; i < n; i++) {
    const kind = r.int(6)
    if (kind === 0) s += cp(r.range(0x4e00, 0x9fff))
    else if (kind === 1) s += cp(r.range(0x3041, 0x3096))
    else if (kind === 2) s += cp(r.range(0x30a1, 0x30fa))
    else if (kind === 3) s += cp(r.range(0xac00, 0xd7a3))
    else if (kind === 4) s += cp(r.range(0xff01, 0xff5e))
    else s += cp(r.range(0x20000, 0x2a6df))
  }
  return s
}

/** A piece of text without line breaks. */
export function piece(r: Rng): string {
  const kind = r.int(12)
  if (kind < 3) return r.pick(WORDS) + r.pick(PUNCT)
  if (kind < 5) return r.pick(SPECIALS)
  if (kind < 6) return cjk(r, r.range(1, 6))
  if (kind < 7) return randomCodePoint(r)
  if (kind < 8) return r.pick(PUNCT)
  if (kind < 9) return " ".repeat(r.range(1, 4))
  if (kind < 10) return r.pick(["\t", "\x01", "\x1b", "\x7f", "\u0090", "​", " "])
  if (kind < 11) return r.pick(WORDS)
  return cp(0x1f1e6 + r.int(26)) + (r.chance(0.7) ? cp(0x1f1e6 + r.int(26)) : "")
}

export function line(r: Rng, maxPieces = 12): string {
  const n = r.int(maxPieces + 1)
  let s = ""
  for (let i = 0; i < n; i++) s += piece(r)
  return s
}

export function text(r: Rng): string {
  const roll = r.int(10)
  if (roll === 0) return ""
  if (roll === 1) {
    // One very long line.
    let s = ""
    const n = r.range(50, 400)
    for (let i = 0; i < n; i++) s += piece(r)
    return s
  }
  const lines = r.range(1, roll < 6 ? 4 : 10)
  let s = ""
  for (let i = 0; i < lines; i++) {
    if (i > 0) s += r.pick(["\n", "\n", "\n", "\r\n", "\r", "\n\n"])
    s += r.chance(0.15) ? "" : line(r)
  }
  if (r.chance(0.15)) s += r.pick(["\n", "\r\n", "\r"])
  return s
}

const COLORS = [
  [255, 0, 0, 255],
  [0, 255, 0, 255],
  [10, 20, 30, 255],
  [200, 200, 200, 255],
  [1, 2, 3, 255],
]

export function styledChunks(r: Rng): any[] {
  const n = r.range(1, 8)
  const chunks: any[] = []
  for (let i = 0; i < n; i++) {
    const t = r.chance(0.2) ? text(r) : r.chance(0.1) ? "" : line(r, 3)
    const chunk: any = { text: t }
    if (r.chance(0.5)) chunk.fg = r.pick(COLORS)
    if (r.chance(0.3)) chunk.bg = r.pick(COLORS)
    if (r.chance(0.4)) chunk.attributes = r.int(256)
    if (r.chance(0.15)) chunk.link = r.pick(["https://example.com", "https://opencode.ai/x", "", "file:///tmp/a"])
    chunks.push(chunk)
  }
  return chunks
}

const WIDTHS = [0, 1, 2, 3, 4, 5, 7, 8, 10, 13, 16, 20, 33, 40, 80, 120]
const METHODS = ["unicode", "wcwidth", "no-zwj", "unicode-wide"]

function width(r: Rng): number {
  return r.chance(0.6) ? r.pick(WIDTHS) : r.range(1, 60)
}

function highlight(r: Rng, span: number): number[] {
  const start = r.int(span + 3)
  const end = r.chance(0.1) ? start : start + r.range(1, 12)
  const priority = r.chance(0.05) ? 300 : r.int(4)
  const ref = r.chance(0.05) ? 70000 : r.int(4)
  return [start, end, r.range(1, 4), priority, ref]
}

/** A random program over a text buffer and its view. */
export function textCase(r: Rng): Case {
  const ops: Op[] = []
  const m = r.pick(METHODS)
  const opCount = r.range(4, 30)
  if (r.chance(0.5)) {
    for (const name of ["keyword", "string", "comment", "type"]) {
      const spec: any = {}
      if (r.chance(0.7)) spec.fg = r.pick(COLORS)
      if (r.chance(0.4)) spec.bg = r.pick(COLORS)
      if (r.chance(0.5)) spec.bold = r.chance(0.5)
      if (r.chance(0.3)) spec.underline = true
      ops.push(["registerStyle", name, spec])
    }
    ops.push(["syntaxStyle", true])
  }
  const queries = () => {
    ops.push(["info"], ["lineInfo"], ["logical"], ["vcount"])
  }
  for (let i = 0; i < opCount; i++) {
    const roll = r.int(100)
    if (roll < 14) ops.push(["setText", text(r)])
    else if (roll < 22) ops.push(["append", r.chance(0.3) ? text(r) : piece(r)])
    else if (roll < 28) ops.push(["setStyledText", r.chance(0.05) ? [] : styledChunks(r)])
    else if (roll < 29) ops.push([r.pick(["clear", "reset"])])
    else if (roll < 32) ops.push(["tabWidth", r.pick([0, 1, 2, 3, 4, 7, 8, 255, 256])])
    else if (roll < 40) ops.push(["wrapMode", r.pick(["none", "char", "word", "word"])])
    else if (roll < 47) ops.push(["wrapWidth", r.chance(0.1) ? null : width(r)])
    else if (roll < 51) ops.push(["viewport", r.int(8), r.int(6), width(r), r.int(12)])
    else if (roll < 53) ops.push(["viewportSize", width(r), r.int(12)])
    else if (roll < 55) ops.push(["firstLineOffset", r.int(12)])
    else if (roll < 57) ops.push(["truncate", r.chance(0.6)])
    else if (roll < 58) ops.push(["align", r.pick(["left", "center", "right"])])
    else if (roll < 63) ops.push(["measure", width(r), r.int(20)])
    else if (roll < 67) ops.push(["sel", r.int(60), r.int(60)])
    else if (roll < 68) ops.push(["usel", r.int(60)])
    else if (roll < 69) ops.push(["rsel"])
    else if (roll < 75) ops.push([r.pick(["lsel", "ulsel"]), r.range(-3, 30), r.range(-3, 10), r.range(-3, 30), r.range(-3, 10), r.pick(["cell", "word", "line"])])
    else if (roll < 76) ops.push(["rlsel"])
    else if (roll < 77) ops.push(["occ", r.pick(["cell", "boundary"])])
    else if (roll < 80) ops.push(["selinfo"])
    else if (roll < 83) ops.push(["range", r.int(60), r.int(60)])
    else if (roll < 86) ops.push(["addHighlight", r.int(6), highlight(r, 20)])
    else if (roll < 89) ops.push(["addHighlightByCharRange", highlight(r, 60)])
    else if (roll < 90) ops.push(["removeHighlightsByRef", r.int(5)])
    else if (roll < 91) ops.push([r.pick(["clearLineHighlights", "clearAllHighlights"]), r.int(5)])
    else if (roll < 93) ops.push(["hl", r.int(6)])
    else if (roll < 94) ops.push(["sources", r.int(10), r.int(10)])
    else if (roll < 95) ops.push(["newView"])
    else if (roll < 96) ops.push(["defaults", r.chance(0.5) ? r.pick(COLORS) : null, r.chance(0.3) ? r.pick(COLORS) : null, r.chance(0.5) ? r.int(64) : null])
    else if (roll < 97) ops.push(r.chance(0.7) ? ["tabIndicator", r.pick([0, 0x2192, 0x20, 0x41, 0x4e00, 0x1f600])] : ["tabColor", r.pick(COLORS)])
    else if (roll < 99) ops.push(["draw", r.range(1, 40), r.range(1, 12), r.range(-3, 4), r.range(-3, 3)])
    else queries()
  }
  if (r.chance(0.15)) {
    // Equal priorities: ties break by the native hash map's iteration order.
    const n = r.range(2, 40)
    const priority = r.int(2)
    for (let i = 0; i < n; i++) {
      const start = r.int(30)
      const h = [start, start + r.range(1, 20), r.range(1, 4), r.chance(0.8) ? priority : r.int(3), r.int(3)]
      ops.push(r.chance(0.5) ? ["addHighlight", r.int(2), h] : ["addHighlightByCharRange", h])
      if (r.chance(0.1)) ops.push(["removeHighlightsByRef", r.int(3)])
    }
  }
  queries()
  ops.push(["draw", r.range(1, 40), r.range(1, 12), r.range(-3, 4), r.range(-3, 3)])
  ops.push(["text"], ["selinfo"])
  for (const w of [1, 2, 3, 5, 8, 13, 40]) ops.push(["measure", w, 10])
  for (const mode of ["char", "word"]) {
    ops.push(["wrapMode", mode], ["wrapWidth", r.pick([1, 2, 3, 4, 6, 9, 17])], ["lineInfo"], ["logical"])
  }
  return { m, ops }
}

/** Hand-picked cases: every special sequence alone, in lines, at chunk edges, at every width. */
export function edgeCases(): Case[] {
  const cases: Case[] = []
  const texts = [
    "",
    "\n",
    "\r\n",
    "\r",
    "\n\n\n",
    "a\r\nb\rc\nd",
    "hello world",
    "hello  world  ",
    "  leading spaces",
    "\tindented\ttabs\t",
    "a".repeat(300),
    "word ".repeat(80),
    "你好世界".repeat(30),
    "日本語のテキストと English words が混ざっている",
    ...SPECIALS,
    SPECIALS.join(""),
    SPECIALS.join(" "),
    SPECIALS.join("\n"),
  ]
  for (const m of METHODS) {
    for (const t of texts) {
      const ops: Op[] = [["setText", t], ["info"], ["text"], ["logical"]]
      for (const w of [1, 2, 3, 4, 5, 7, 10, 16]) {
        ops.push(["measure", w, 5])
        for (const mode of ["char", "word"]) ops.push(["wrapMode", mode], ["wrapWidth", w], ["logical"], ["vcount"])
      }
      ops.push(["draw", 24, 6, 0, 0], ["wrapMode", "none"], ["sel", 0, 1000], ["selinfo"], ["range", 1, 5], ["range", 0, 3])
      ops.push(["viewport", 2, 0, 10, 4], ["draw", 12, 5, 1, 0], ["tabIndicator", 0x2192], ["truncate", true], ["draw", 12, 5, 0, 0])
      ops.push(["lsel", 0, 0, 3, 0, "word"], ["selinfo"], ["lsel", 1, 0, 1, 0, "line"], ["selinfo"])
      cases.push({ m, ops })
    }
    // Appends that split graphemes across chunks.
    for (const [a, b] of [
      ["e", "́"],
      ["👨", "‍👩"],
      ["👨‍", "👩"],
      ["🇯", "🇵"],
      ["क्", "ष"],
      ["ᄀ", "ᅡ"],
      ["a", "‍"],
      ["\r", "\n"],
      ["x؀", "a"],
      ["👍", "🏽"],
      ["❤", "️"],
    ]) {
      cases.push({
        m,
        ops: [["setText", a], ["append", b], ["info"], ["text"], ["logical"], ["wrapMode", "char"], ["wrapWidth", 1], ["logical"], ["range", 0, 2]],
      })
    }
  }
  return cases
}

/**
 * Exhaustive Unicode cases: every code point alone on a line (its width under each method), and for the code points
 * below U+30000 and the tag/variation block, probes that show how it joins its neighbors: the visual lines of a
 * one-column char wrap are its graphemes.
 */
export function tableCases(): Case[] {
  const cases: Case[] = []
  const points: number[] = []
  for (let c = 0; c <= 0x10ffff; c++) {
    if ((c >= 0xd800 && c <= 0xdfff) || c === 0x0a || c === 0x0d) continue
    points.push(c)
  }
  const block = 8192
  for (const m of METHODS) {
    for (let i = 0; i < points.length; i += block) {
      const lines = points.slice(i, i + block).map((c) => cp(c))
      cases.push({ m, ops: [["setText", lines.join("\n")], ["logical"]] })
    }
  }
  const probes = (c: number) => {
    const x = cp(c)
    return [`a${x}`, `${x}a`, `${x}́`, `${x}${x}`, `क्${x}`, `👍${x}`, `${x}‍👍`, `${x}🏽`, `🇯${x}`]
  }
  const joined = points.filter((c) => c < 0x30000 || (c >= 0xe0000 && c <= 0xe0fff))
  const probeBlock = 1024
  for (const m of METHODS) {
    for (let i = 0; i < joined.length; i += probeBlock) {
      const lines = joined.slice(i, i + probeBlock).flatMap(probes)
      cases.push({ m, ops: [["setText", lines.join("\n")], ["logical"], ["wrapMode", "char"], ["wrapWidth", 1], ["logical"]] })
    }
  }
  return cases
}

/**
 * Hand-picked editing: each special sequence typed one code point at a time between other text, then walked over
 * with the cursor, deleted grapheme by grapheme, undone and redone, under every width method.
 */
export function editEdgeCases(): Case[] {
  const cases: Case[] = []
  for (const m of METHODS) {
    for (const special of SPECIALS) {
      const ops: Op[] = [["eSetText", "ab\ncd"], ["eSetCursor", 0, 1]]
      for (const c of Array.from(special)) ops.push(["eInsert", c], ["eInfo"])
      ops.push(["eText"], ["vVisual"], ["eLeft"], ["eInfo"], ["eLeft"], ["eInfo"], ["eRight"], ["eInfo"], ["eRight"], ["eInfo"])
      ops.push(["eNextWord"], ["ePrevWord"], ["eDown"], ["eInfo"], ["eUp"], ["eInfo"], ["vVEol"], ["vSol"])
      ops.push(["eSetCursor", 0, 100], ["eBackspace"], ["eInfo"], ["eText"], ["eBackspace"], ["eInfo"], ["eText"])
      ops.push(["eSetCursor", 0, 0], ["eDelChar"], ["eInfo"], ["eText"], ["eDelChar"], ["eText"])
      for (let i = 0; i < 6; i++) ops.push(["eUndo"], ["eText"], ["eInfo"])
      for (let i = 0; i < 6; i++) ops.push(["eRedo"], ["eText"], ["eInfo"])
      ops.push(["eTabWidth", 4], ["eUndo"], ["eInfo"], ["eEvents"])
      ops.push(["vWrapMode", "char"], ["vViewportSize", 3, 4], ["eSetText", `x${special}${special}y`], ["eCursorOffset", 2])
      ops.push(["vVisual"], ["vDown"], ["vVisual"], ["vUp"], ["vVisual"], ["vGotoEnd"], ["vVisual"], ["vLineInfo"], ["vDraw", 6, 5, 0, 0])
      ops.push(["vSel", 1, 3], ["vSelinfo"], ["vReplaceSel", special], ["eText"], ["eInfo"], ["vSelinfo"])
      cases.push({ k: "edit", m, w: 10, h: 4, ops })
    }
    // A placeholder over an emptied buffer.
    cases.push({
      k: "edit",
      m,
      w: 12,
      h: 3,
      ops: [
        ["vPlaceholder", [{ text: "Type here… 你好", fg: COLORS[0], attributes: 2 }]],
        ["vLineInfo"],
        ["vDraw", 14, 3, 0, 0],
        ["eInsert", "x"],
        ["vLineInfo"],
        ["eBackspace"],
        ["vLogical"],
        ["vVisual"],
        ["vDown"],
        ["vVisual"],
        ["vMeasure", 5, 3],
        ["vDraw", 14, 3, 0, 0],
        ["vPlaceholder", []],
        ["vLineInfo"],
        ["eEvents"],
      ],
    })
  }
  return cases
}

/** A random program over an edit buffer and its editor view. */
export function editCase(r: Rng): Case {
  const ops: Op[] = []
  const m = r.pick(METHODS)
  const w = r.chance(0.1) ? r.int(3) : r.range(1, 40)
  const h = r.chance(0.1) ? r.int(3) : r.range(1, 12)
  if (r.chance(0.3)) {
    for (const name of ["keyword", "string"]) ops.push(["registerStyle", name, { fg: r.pick(COLORS), bold: r.chance(0.5) }])
    ops.push(["eSyntaxStyle", true])
  }
  if (r.chance(0.5)) ops.push(["vWrapMode", r.pick(["none", "char", "word", "word"])])
  if (r.chance(0.8)) ops.push([r.pick(["eSetText", "eReplaceText"]), text(r)])
  const coord = () => r.range(0, 8)
  const opCount = r.range(5, 60)
  for (let i = 0; i < opCount; i++) {
    const roll = r.int(100)
    if (roll < 10) ops.push(["eInsert", r.chance(0.3) ? text(r) : piece(r)])
    else if (roll < 12) ops.push(["eNewLine"])
    else if (roll < 17) ops.push([r.pick(["eDelChar", "eBackspace", "eBackspace"])])
    else if (roll < 19) ops.push(["eDelRange", coord(), r.range(0, 20), coord(), r.range(0, 20)])
    else if (roll < 20) ops.push(["eDelLine"])
    else if (roll < 30) ops.push([r.pick(["eLeft", "eRight", "eUp", "eDown", "eLeft", "eRight"])])
    else if (roll < 35) ops.push([r.pick(["vUp", "vDown"])])
    else if (roll < 37) ops.push(["eSetCursor", coord(), r.range(0, 30)])
    else if (roll < 38) ops.push(["eGotoLine", coord()])
    else if (roll < 39) ops.push([r.pick(["eCursorOffset", "vCursorOffset"]), r.range(0, 80)])
    else if (roll < 42) ops.push([r.pick(["eUndo", "eUndo", "eRedo"])])
    else if (roll < 43) ops.push([r.pick(["eClear", "eClearHistory", "eSetText", "eReplaceText"]), text(r)])
    else if (roll < 44) ops.push(["eTabWidth", r.pick([2, 3, 4, 8, 0, 300])])
    else if (roll < 46) ops.push(["vViewportSize", r.range(0, 40), r.range(0, 12)])
    else if (roll < 47) ops.push(["vViewport", r.int(10), r.int(10), r.range(1, 40), r.range(1, 12), r.chance(0.5)])
    else if (roll < 48) ops.push(["vScrollMargin", r.pick([0, 0.1, 0.15, 0.25, 0.5, 0.9])])
    else if (roll < 50) ops.push(["vWrapMode", r.pick(["none", "char", "word"])])
    else if (roll < 53) ops.push(["vSel", r.int(60), r.int(60)])
    else if (roll < 54) ops.push([r.pick(["vUSel", "vSelInclusive"]), r.int(60), r.int(60)])
    else if (roll < 55) ops.push([r.pick(["vRSel", "vRLSel", "vCell"])])
    else if (roll < 59)
      ops.push([
        r.pick(["vLSel", "vULSel"]),
        r.range(-2, 30),
        r.range(-2, 12),
        r.range(-2, 30),
        r.range(-2, 12),
        r.chance(0.6),
        r.chance(0.4),
        r.pick(["cell", "word", "line"]),
      ])
    else if (roll < 60) ops.push(["vOcc", r.pick(["cell", "boundary"])])
    else if (roll < 62) ops.push([r.pick(["vDelSel", "vReplaceSel"]), piece(r)])
    else if (roll < 63) ops.push(["vGotoEnd"])
    else if (roll < 66) ops.push([r.pick(["vNextWord", "vPrevWord", "vEol", "vSol", "vVEol"])])
    else if (roll < 69) ops.push([r.pick(["eNextWord", "ePrevWord", "eEol", "eEof", "ePrevChar"])])
    else if (roll < 71) ops.push([r.pick(["eOffsetToPos", "eLineStart"]), r.range(0, 80)])
    else if (roll < 72) ops.push(["ePosToOffset", coord(), r.range(0, 20)])
    else if (roll < 74) ops.push([r.pick(["eRange", "eRangeCoords"]), r.int(40), r.int(40), r.int(4), r.int(20)])
    else if (roll < 75) ops.push(["eMeasure", line(r, 4)])
    else if (roll < 76) ops.push(["eMeasureStatic", text(r), r.pick([0, 2, 4, 5])])
    else if (roll < 78)
      ops.push(["vPlaceholder", r.chance(0.2) ? [] : styledChunks(r).map((c) => ({ text: c.text, fg: c.fg, bg: c.bg, attributes: c.attributes ?? 0 }))])
    else if (roll < 79) ops.push(["vTabIndicator", r.pick([0, 0x2192, 0x41])])
    else if (roll < 81) ops.push(["eAddHlRange", highlight(r, 40)])
    else if (roll < 82) ops.push(["eAddHl", r.int(4), highlight(r, 20)])
    else if (roll < 83) ops.push(["eDefaults", r.chance(0.5) ? r.pick(COLORS) : null, r.chance(0.5) ? r.pick(COLORS) : null, r.chance(0.5) ? r.int(64) : null])
    else if (roll < 85) ops.push(["vMeasure", r.range(0, 40), r.range(0, 10)])
    else if (roll < 88) ops.push(["eEvents"])
    else if (roll < 89) ops.push(["vNewView", r.range(1, 40), r.range(1, 12)])
    else if (roll < 91) ops.push(["vDraw", r.range(1, 40), r.range(1, 12), r.range(-2, 3), r.range(-2, 3)])
    else ops.push([r.pick(["eInfo", "vVisual", "vGetViewport", "vLineInfo", "vVcount", "vTotal", "vSelinfo", "vCursor", "eText", "vLogical"])])
  }
  ops.push(["eEvents"], ["eText"], ["eInfo"], ["vVisual"], ["vGetViewport"], ["vLineInfo"], ["vLogical"], ["vSelinfo"], ["vVcount"], ["vTotal"])
  ops.push(["vDraw", r.range(1, 40), r.range(1, 12), 0, 0])
  return { k: "edit", m, w, h, ops }
}

/** A random program over an edit buffer with extmarks (the controller wraps the editing methods). */
export function extmarkCase(r: Rng): Case {
  const ops: Op[] = []
  const m = r.pick(METHODS)
  for (const name of ["keyword", "string"]) ops.push(["registerStyle", name, { fg: r.pick(COLORS) }])
  ops.push(["eSyntaxStyle", true])
  if (r.chance(0.5)) ops.push(["vWrapMode", r.pick(["none", "char", "word"])])
  ops.push(["eSetText", text(r)])
  if (r.chance(0.3)) ops.push(["eEvents"])
  ops.push(["xRegisterType", r.pick(["prompt-part", "paste"])])
  const create = (): Op => {
    const start = r.int(40)
    const o: any = { start, end: start + r.range(0, 10), virtual: r.chance(0.6) }
    if (r.chance(0.6)) o.styleId = r.range(1, 2)
    if (r.chance(0.4)) o.priority = r.chance(0.05) ? 300 : r.int(3)
    if (r.chance(0.7)) o.typeId = r.range(0, 2)
    if (r.chance(0.3)) o.meta = r.int(1000)
    if (r.chance(0.2)) o.data = r.int(1000)
    return ["xCreate", o]
  }
  for (let i = r.range(1, 5); i > 0; i--) ops.push(create())
  const opCount = r.range(5, 50)
  for (let i = 0; i < opCount; i++) {
    const roll = r.int(100)
    if (roll < 12) ops.push(["eInsert", r.chance(0.2) ? text(r) : piece(r)])
    else if (roll < 22) ops.push([r.pick(["eBackspace", "eDelChar"])])
    else if (roll < 34) ops.push([r.pick(["eLeft", "eRight", "eLeft", "eRight", "vUp", "vDown"])])
    else if (roll < 37) ops.push(["eCursorOffset", r.int(50)])
    else if (roll < 39) ops.push(["eDelRange", r.int(4), r.int(20), r.int(4), r.int(20)])
    else if (roll < 40) ops.push([r.pick(["eDelLine", "eNewLine"])])
    else if (roll < 43) ops.push(["vSel", r.int(50), r.int(50)])
    else if (roll < 45) ops.push([r.pick(["vReplaceSel", "vDelSel"]), piece(r)])
    else if (roll < 46) ops.push(["vRSel"])
    else if (roll < 52) ops.push([r.pick(["eUndo", "eUndo", "eRedo"])])
    else if (roll < 53) ops.push([r.pick(["eSetText", "eReplaceText", "eClear"]), text(r)])
    else if (roll < 60) ops.push(create())
    else if (roll < 62) ops.push(["xDelete", r.range(1, 8)])
    else if (roll < 63) ops.push([r.pick(["xClear", "xDestroy"])])
    else if (roll < 70) ops.push(["eEvents"])
    else if (roll < 73) ops.push(["vDraw", r.range(1, 30), r.range(1, 8), 0, 0])
    else if (roll < 76) ops.push(["eHl", r.int(3)])
    else if (roll < 80) ops.push([r.pick(["xAll", "xVirtual"])])
    else if (roll < 82) ops.push(["xAtOffset", r.int(40)])
    else if (roll < 84) ops.push(["xForType", r.int(3)])
    else if (roll < 86) ops.push(["xGet", r.range(1, 8)])
    else if (roll < 87) ops.push([r.pick(["xTypeId", "xRegisterType"]), r.pick(["prompt-part", "paste", "x"])])
    else if (roll < 88) ops.push(["xTypeName", r.range(0, 3)])
    else if (roll < 90) ops.push([r.pick(["xMeta", "xData"]), r.range(1, 8)])
    else if (roll < 91) ops.push(["xAdjustDel", r.int(40), r.int(8), r.chance(0.7)])
    else if (r.chance(0.05)) ops.push(["vNewView", r.range(5, 30), r.range(1, 8)])
    else ops.push([r.pick(["eInfo", "eText", "vVisual"])])
  }
  ops.push(["eEvents"], ["xAll"], ["eHl", 0], ["eHl", 1], ["eText"], ["eInfo"], ["vDraw", 30, 6, 0, 0])
  return { k: "edit", m, w: r.range(5, 30), h: r.range(1, 8), ops }
}

/** Hand-picked extmark programs: virtual ranges walked over and deleted around, over wide and joining text. */
export function extmarkEdgeCases(): Case[] {
  const cases: Case[] = []
  const texts = ["abc def ghi", "a你好b\ncd🇺🇸e\n\tf", "x👩‍👩‍👧ýz\nline two", "ab\n\ncd\n"]
  for (const m of METHODS) {
    for (const t of texts) {
      for (const [start, end] of [[1, 3], [0, 2], [2, 6], [3, 4], [4, 4], [5, 9]]) {
        const ops: Op[] = [["registerStyle", "keyword", { fg: COLORS[0] }], ["eSyntaxStyle", true], ["eSetText", t]]
        ops.push(["xCreate", { start, end, virtual: true, styleId: 1, meta: 7 }], ["xCreate", { start: end + 1, end: end + 3, styleId: 1, priority: 2 }])
        ops.push(["eCursorOffset", 0], ["eHl", 0], ["eHl", 1])
        for (let i = 0; i < 14; i++) ops.push(["eRight"], ["eInfo"])
        for (let i = 0; i < 14; i++) ops.push(["eLeft"], ["eInfo"])
        for (let i = 0; i < 4; i++) ops.push(["vDown"], ["eInfo"], ["vUp"], ["eInfo"], ["eRight"])
        for (let offset = 0; offset < 12; offset++) ops.push(["eCursorOffset", offset], ["eInfo"])
        ops.push(["eCursorOffset", end], ["eBackspace"], ["eText"], ["xAll"], ["eHl", 0], ["eUndo"], ["xAll"], ["xMeta", 1], ["eRedo"], ["xAll"])
        ops.push(["eCursorOffset", start], ["eDelChar"], ["eText"], ["xAll"], ["eUndo"], ["eUndo"], ["xAll"], ["eText"], ["xMeta", 1])
        ops.push(["eCursorOffset", start + 1], ["eInsert", "Q\n"], ["xAll"], ["eHl", 0], ["eHl", 1], ["eNewLine"], ["xAll"], ["eDelLine"], ["xAll"])
        ops.push(["vSel", start, end + 2], ["vReplaceSel", "zz"], ["xAll"], ["eText"], ["eUndo"], ["xAll"], ["eUndo"], ["xAll"], ["eRedo"], ["xAll"])
        ops.push(["vSel", 0, 3], ["vDelSel"], ["xAll"], ["eSetText", t], ["xAll"], ["xCreate", { start: 1, end: 2, priority: 300, styleId: 1 }])
        ops.push(["xAll"], ["xAdjustDel", 0, 1, true], ["eHl", 0], ["xDestroy"], ["xCreate", { start: 0, end: 1 }], ["xAdjustDel", 0, 1, true])
        ops.push(["eHl", 0], ["eRight"], ["eInfo"], ["eEvents"], ["vDraw", 12, 4, 0, 0])
        cases.push({ k: "edit", m, w: 7, h: 4, ops })
      }
    }
  }
  return cases
}
