// Writes src/unicode_tables.rs: the Unicode properties OpenTUI's Zig core reads through uucode, derived from the
// same UCD files (port/.vendor/opentui/packages/native/zig-deps/uucode-opentui/ucd) by the same rules as uucode's
// build components (src/components.zig: UnicodeData, DerivedCoreProperties, EastAsianWidth, GraphemeBreak,
// EmojiData, GraphemeBreakDerived, Wcwidth). OpenTUI's build.zig asks uucode for exactly these fields:
// grapheme_break, east_asian_width, general_category, is_default_ignorable and wcwidth_zero_in_grapheme.
//
// Each code point packs into one u16 (see unicode.rs):
//   bits 0-4  grapheme_break (uucode types.GraphemeBreak, in declaration order)
//   bit  5    east_asian_width is wide or fullwidth
//   bits 6-7  general_category mark class: 0 none, 1 Mn, 2 Mc, 3 Me
//   bit  8    is_default_ignorable
//   bit  9    wcwidth_zero_in_grapheme
// stored as a three-stage table: code point >> SHIFT indexes deduplicated blocks of u8 value indices.
//
// usage: bun scripts/gen-unicode.ts          write the file
//        bun scripts/gen-unicode.ts check    exit 1 if the file differs from what would be written
import { readFileSync, writeFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const here = dirname(fileURLToPath(import.meta.url))
const ucd = join(here, "../../../../.vendor/opentui/packages/native/zig-deps/uucode-opentui/ucd")
const N = 0x110000

const read = (path: string) => readFileSync(join(ucd, path), "utf8").split("\n")
// components.zig `trim`: drop a comment, then surrounding spaces, tabs and carriage returns.
const trim = (line: string) => {
  const hash = line.indexOf("#")
  return (hash >= 0 ? line.slice(0, hash) : line).replace(/^[ \t\r]+|[ \t\r]+$/g, "")
}
const range = (text: string): [number, number] => {
  const dots = text.indexOf("..")
  if (dots >= 0) return [parseInt(text.slice(0, dots), 16), parseInt(text.slice(dots + 2), 16) + 1]
  const cp = parseInt(text, 16)
  return [cp, cp + 1]
}
const fields = (line: string) => line.split(";").map((part) => part.replace(/^[ \t\r]+|[ \t\r]+$/g, ""))

// UnicodeData.txt: general category; gaps are Cn, and a First>/Last> pair fills its range with the first row.
const gc: string[] = []
{
  let rangeRow: string | null = null
  for (const line of read("UnicodeData.txt")) {
    const trimmed = trim(line)
    if (trimmed.length === 0) continue
    const parts = trimmed.split(";")
    const cp = parseInt(parts[0], 16)
    while (gc.length < cp) gc.push(rangeRow ?? "Cn")
    if (rangeRow !== null) {
      if (!parts[1].endsWith("Last>")) throw new Error(`range without Last> at ${parts[0]}`)
      gc.push(rangeRow)
      rangeRow = null
      continue
    }
    if (parts[1].endsWith("First>")) rangeRow = parts[2]
    gc.push(parts[2])
  }
  while (gc.length < N) gc.push("Cn")
}

// DerivedCoreProperties.txt: Default_Ignorable_Code_Point and InCB.
const defaultIgnorable = new Uint8Array(N)
const incb = new Array<string>(N).fill("none")
for (const line of read("DerivedCoreProperties.txt")) {
  const trimmed = trim(line)
  if (trimmed.length === 0) continue
  const [cps, property, value = ""] = fields(trimmed)
  const [start, end] = range(cps)
  if (property === "Default_Ignorable_Code_Point") defaultIgnorable.fill(1, start, end)
  else if (property === "InCB") {
    const kind = { Linker: "linker", Consonant: "consonant", Extend: "extend" }[value]
    if (!kind) throw new Error(`unknown InCB value ${value}`)
    incb.fill(kind, start, end)
  }
}

// DerivedEastAsianWidth.txt: Neutral by default, `@missing` Wide ranges, then the data lines, in file order.
const eaw = new Array<string>(N).fill("N")
for (const line of read("extracted/DerivedEastAsianWidth.txt")) {
  const trimmed = line.replace(/^[ \t\r]+|[ \t\r]+$/g, "")
  if (trimmed.length === 0) continue
  if (trimmed.startsWith("# @missing:")) {
    const [cps, width] = fields(trimmed.slice("# @missing:".length))
    if (width === "Neutral") continue
    if (width !== "Wide") throw new Error(`unknown @missing width ${width}`)
    const [start, end] = range(cps)
    eaw.fill("W", start, end)
    continue
  }
  const data = trim(trimmed)
  if (data.length === 0) continue
  const [cps, width] = fields(data)
  if (!["F", "H", "W", "Na", "A", "N"].includes(width)) throw new Error(`unknown width ${width}`)
  const [start, end] = range(cps)
  eaw.fill(width, start, end)
}

// GraphemeBreakProperty.txt: the original property, Other by default.
const originalGb = new Array<string>(N).fill("other")
{
  const names: Record<string, string> = {
    Prepend: "prepend",
    CR: "cr",
    LF: "lf",
    Control: "control",
    Extend: "extend",
    Regional_Indicator: "regional_indicator",
    SpacingMark: "spacing_mark",
    L: "l",
    V: "v",
    T: "t",
    LV: "lv",
    LVT: "lvt",
    ZWJ: "zwj",
  }
  for (const line of read("auxiliary/GraphemeBreakProperty.txt")) {
    const trimmed = trim(line)
    if (trimmed.length === 0) continue
    const [cps, prop] = fields(trimmed)
    const [start, end] = range(cps)
    originalGb.fill(names[prop] ?? "other", start, end)
  }
}

// emoji-data.txt.
const emoji: Record<string, Uint8Array> = {
  Emoji_Modifier: new Uint8Array(N),
  Emoji_Modifier_Base: new Uint8Array(N),
  Extended_Pictographic: new Uint8Array(N),
  Emoji_Component: new Uint8Array(N),
}
for (const line of read("emoji/emoji-data.txt")) {
  const trimmed = trim(line)
  if (trimmed.length === 0) continue
  const [cps, prop] = fields(trimmed)
  const known = ["Emoji", "Emoji_Presentation", "Emoji_Modifier", "Emoji_Modifier_Base", "Emoji_Component", "Extended_Pictographic"]
  if (!known.includes(prop)) throw new Error(`unknown emoji property ${prop}`)
  const [start, end] = range(cps)
  emoji[prop]?.fill(1, start, end)
}

const GB = [
  "other",
  "control",
  "prepend",
  "cr",
  "lf",
  "regional_indicator",
  "spacing_mark",
  "l",
  "v",
  "t",
  "lv",
  "lvt",
  "zwj",
  "zwnj",
  "extended_pictographic",
  "emoji_modifier_base",
  "emoji_modifier",
  "indic_conjunct_break_extend",
  "indic_conjunct_break_linker",
  "indic_conjunct_break_consonant",
]
const assert = (ok: boolean, what: string, cp: number) => {
  if (!ok) throw new Error(`${what} at ${cp.toString(16)}`)
}

const values = new Uint16Array(N)
for (let cp = 0; cp < N; cp++) {
  // GraphemeBreakDerived.
  const original = originalGb[cp]
  let gb: string
  if (emoji.Emoji_Modifier[cp]) {
    assert(original === "extend" && !emoji.Extended_Pictographic[cp] && !!emoji.Emoji_Component[cp], "emoji modifier", cp)
    gb = "emoji_modifier"
  } else if (emoji.Emoji_Modifier_Base[cp]) {
    assert(original === "other" && !!emoji.Extended_Pictographic[cp], "emoji modifier base", cp)
    gb = "emoji_modifier_base"
  } else if (emoji.Extended_Pictographic[cp]) {
    assert(original === "other", "extended pictographic", cp)
    gb = "extended_pictographic"
  } else if (incb[cp] === "none") {
    if (original === "extend") {
      assert(cp === 0x200c, "extend without InCB", cp)
      gb = "zwnj"
    } else gb = original
  } else if (incb[cp] === "extend") {
    if (cp === 0x200d) {
      assert(original === "zwj", "zwj", cp)
      gb = "zwj"
    } else {
      assert(original === "extend", "InCB extend", cp)
      gb = "indic_conjunct_break_extend"
    }
  } else if (incb[cp] === "linker") {
    assert(original === "extend", "InCB linker", cp)
    gb = "indic_conjunct_break_linker"
  } else {
    assert(original === "other", "InCB consonant", cp)
    gb = "indic_conjunct_break_consonant"
  }

  // Wcwidth.
  const category = gc[cp]
  const wide = eaw[cp] === "W" || eaw[cp] === "F"
  let width: number
  if (category === "Cc" || category === "Cs" || category === "Zl" || category === "Zp") width = 0
  else if (cp === 0x00ad) width = 1
  else if (defaultIgnorable[cp]) width = 0
  else if (cp === 0x2e3a) width = 2
  else if (cp === 0x2e3b) width = 3
  else if (wide) width = 2
  else if (gb === "regional_indicator") width = 2
  else width = 1
  const zeroInGrapheme =
    width === 0 ||
    !!emoji.Emoji_Modifier[cp] ||
    category === "Mn" ||
    category === "Me" ||
    gb === "v" ||
    gb === "t" ||
    gb === "prepend"

  const mark = category === "Mn" ? 1 : category === "Mc" ? 2 : category === "Me" ? 3 : 0
  values[cp] =
    GB.indexOf(gb) | ((wide ? 1 : 0) << 5) | (mark << 6) | (defaultIgnorable[cp] << 8) | ((zeroInGrapheme ? 1 : 0) << 9)
}

// Three stages: block index per 2^SHIFT code points, blocks of value indices (u8), values (u16).
const distinct = [...new Set(values)].sort((a, b) => a - b)
if (distinct.length > 256) throw new Error(`${distinct.length} distinct values do not fit u8 indices`)
const indexOf = new Map(distinct.map((value, index) => [value, index]))
let best: { shift: number; stage1: number[]; stage2: number[] } | null = null
for (let shift = 5; shift <= 9; shift++) {
  const size = 1 << shift
  const blocks = new Map<string, number>()
  const stage1: number[] = []
  const stage2: number[] = []
  for (let start = 0; start < N; start += size) {
    const block = Array.from(values.subarray(start, start + size), (value) => indexOf.get(value)!)
    const key = block.join(",")
    let at = blocks.get(key)
    if (at === undefined) {
      at = stage2.length / size
      blocks.set(key, at)
      stage2.push(...block)
    }
    stage1.push(at)
  }
  const bytes = stage1.length * (stage2.length / size > 255 ? 2 : 1) + stage2.length
  if (!best || bytes < best.stage1.length * 2 + best.stage2.length) best = { shift, stage1, stage2 }
}
const { shift, stage1, stage2 } = best!
const stage1Type = stage2.length >> shift > 255 ? "u16" : "u8"

const rows = (items: ArrayLike<number>, per: number) => {
  const out: string[] = []
  for (let i = 0; i < items.length; i += per) out.push("    " + Array.from(items).slice(i, i + per).join(", ") + ",")
  return out.join("\n")
}
const text = `// Generated by scripts/gen-unicode.ts from uucode's UCD files (zig-deps/uucode-opentui/ucd). Do not edit.

/// Code points per stage-1 entry: \`STAGE1[cp >> SHIFT]\` is a block of \`STAGE2\`.
pub(crate) const SHIFT: u32 = ${shift};

#[rustfmt::skip]
pub(crate) static STAGE1: [${stage1Type}; ${stage1.length}] = [
${rows(stage1, 24)}
];

#[rustfmt::skip]
pub(crate) static STAGE2: [u8; ${stage2.length}] = [
${rows(stage2, 32)}
];

#[rustfmt::skip]
pub(crate) static VALUES: [u16; ${distinct.length}] = [
${rows(distinct, 16)}
];
`
const target = join(here, "../src/unicode_tables.rs")
if (process.argv[2] === "check") {
  const current = readFileSync(target, "utf8")
  if (current !== text) {
    console.error(`${target} is out of date: run bun scripts/gen-unicode.ts`)
    process.exit(1)
  }
  console.log("unicode_tables.rs is up to date")
} else {
  writeFileSync(target, text)
  console.log(`wrote ${target} (shift ${shift}, ${stage1.length} + ${stage2.length} entries, ${distinct.length} values)`)
}
