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
