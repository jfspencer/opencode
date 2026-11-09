// Generates src/jsregex/tables.rs: every piece of Unicode data the RegExp engine needs, read out of
// JavaScriptCore itself (run under bun), so the Rust engine agrees with the oracle character for character:
//
//   - the case-insensitive equivalence classes of non-unicode /i (JSC's UCS2 canonicalization table, which is
//     older than its Unicode data) and of /iu (simple case folding), found by matching every candidate;
//   - the sets of every \p{...} name JSC accepts (General_Category, Script, Script_Extensions, binary
//     properties), extracted by running /\p{X}+/gu over a string of every code point;
//   - the properties of strings of /v (RGI_Emoji, ...), as the candidates JSC accepts;
//   - \s, and the characters JSC accepts in group names.
//
// Candidate names come from a PropertyValueAliases.txt (any recent version: names JSC rejects are dropped) and
// candidate emoji sequences from an emoji-test.txt; JSC decides membership.
//
// usage: bun harness/jsregex/gen-tables.ts <PropertyValueAliases.txt> <emoji-test.txt> > src/jsregex/tables.rs

import { readFileSync } from "node:fs"

const [aliasesPath, emojiTestPath] = process.argv.slice(2)
if (!aliasesPath || !emojiTestPath) throw new Error("usage: gen-tables.ts <PropertyValueAliases.txt> <emoji-test.txt>")

const hex = (n: number) => "0x" + n.toString(16).toUpperCase()

// Every code point but the surrogates, in order; unit index -> code point is arithmetic.
const allParts: string[] = []
for (let c = 0; c < 0xd800; c++) allParts.push(String.fromCharCode(c))
for (let c = 0xe000; c < 0x110000; c++) allParts.push(String.fromCodePoint(c))
const ALL = allParts.join("")
const cpAt = (i: number) => (i < 0xd800 ? i : i < 0xf800 ? i + 0x800 : 0x10000 + ((i - 0xf800) >> 1))
let highs = ""
for (let c = 0xd800; c < 0xdc00; c++) highs += String.fromCharCode(c)
let lows = ""
for (let c = 0xdc00; c < 0xe000; c++) lows += String.fromCharCode(c)

type Ranges = Array<[number, number]>
const merge = (ranges: Ranges): Ranges => {
  ranges.sort((a, b) => a[0] - b[0])
  const out: Ranges = []
  for (const [a, b] of ranges) {
    const last = out[out.length - 1]
    if (last && a <= last[1] + 1) last[1] = Math.max(last[1], b)
    else out.push([a, b])
  }
  return out
}

// The set of code points a single-character regex body matches (`body` is used as `[...]` contents or an escape).
const extract = (body: string, flags: string): Ranges => {
  const ranges: Ranges = []
  const re = new RegExp("(?:" + body + ")+", "g" + flags)
  for (const m of ALL.matchAll(re)) {
    const a = cpAt(m.index!)
    const b = cpAt(m.index! + m[0].length - 1)
    if (a < 0xd800 && b >= 0xe000) {
      ranges.push([a, 0xd7ff], [0xe000, b])
    } else ranges.push([a, b])
  }
  for (const [text, base] of [
    [highs, 0xd800],
    [lows, 0xdc00],
  ] as const) {
    for (const m of text.matchAll(re)) ranges.push([base + m.index!, base + m.index! + m[0].length - 1])
  }
  return merge(ranges)
}

const accepts = (pattern: string, flags: string) => {
  try {
    new RegExp(pattern, flags)
    return true
  } catch {
    return false
  }
}

// ---- property names -------------------------------------------------------------------------------------------
const aliasLines = readFileSync(aliasesPath, "utf8").split("\n")
const valueNames = (prop: string) => {
  const groups: string[][] = []
  for (const line of aliasLines) {
    const body = line.split("#")[0].trim()
    if (!body) continue
    const fields = body.split(";").map((f) => f.trim())
    if (fields[0] !== prop) continue
    groups.push(fields.slice(1).filter((f) => f.length > 0))
  }
  return groups
}

const binaryCandidates = [
  ["ASCII"],
  ["ASCII_Hex_Digit", "AHex"],
  ["Alphabetic", "Alpha"],
  ["Any"],
  ["Assigned"],
  ["Bidi_Control", "Bidi_C"],
  ["Bidi_Mirrored", "Bidi_M"],
  ["Case_Ignorable", "CI"],
  ["Cased"],
  ["Changes_When_Casefolded", "CWCF"],
  ["Changes_When_Casemapped", "CWCM"],
  ["Changes_When_Lowercased", "CWL"],
  ["Changes_When_NFKC_Casefolded", "CWKCF"],
  ["Changes_When_Titlecased", "CWT"],
  ["Changes_When_Uppercased", "CWU"],
  ["Dash"],
  ["Default_Ignorable_Code_Point", "DI"],
  ["Deprecated", "Dep"],
  ["Diacritic", "Dia"],
  ["Emoji"],
  ["Emoji_Component", "EComp"],
  ["Emoji_Modifier", "EMod"],
  ["Emoji_Modifier_Base", "EBase"],
  ["Emoji_Presentation", "EPres"],
  ["Extended_Pictographic", "ExtPict"],
  ["Extender", "Ext"],
  ["Grapheme_Base", "Gr_Base"],
  ["Grapheme_Extend", "Gr_Ext"],
  ["Hex_Digit", "Hex"],
  ["IDS_Binary_Operator", "IDSB"],
  ["IDS_Trinary_Operator", "IDST"],
  ["IDS_Unary_Operator", "IDSU"],
  ["ID_Continue", "IDC"],
  ["ID_Start", "IDS"],
  ["ID_Compat_Math_Start"],
  ["ID_Compat_Math_Continue"],
  ["Ideographic", "Ideo"],
  ["Join_Control", "Join_C"],
  ["Logical_Order_Exception", "LOE"],
  ["Lowercase", "Lower"],
  ["Math"],
  ["Modifier_Combining_Mark", "MCM"],
  ["Noncharacter_Code_Point", "NChar"],
  ["Pattern_Syntax", "Pat_Syn"],
  ["Pattern_White_Space", "Pat_WS"],
  ["Prepended_Concatenation_Mark", "PCM"],
  ["Quotation_Mark", "QMark"],
  ["Radical"],
  ["Regional_Indicator", "RI"],
  ["Sentence_Terminal", "STerm"],
  ["Soft_Dotted", "SD"],
  ["Terminal_Punctuation", "Term"],
  ["Unified_Ideograph", "UIdeo"],
  ["Uppercase", "Upper"],
  ["Variation_Selector", "VS"],
  ["White_Space", "space"],
  ["XID_Continue", "XIDC"],
  ["XID_Start", "XIDS"],
  ["Full_Composition_Exclusion", "Comp_Ex"],
  ["Hyphen"],
  ["Other_Alphabetic", "OAlpha"],
]

const stringPropertyNames = [
  "Basic_Emoji",
  "Emoji_Keycap_Sequence",
  "RGI_Emoji_Modifier_Sequence",
  "RGI_Emoji_Flag_Sequence",
  "RGI_Emoji_Tag_Sequence",
  "RGI_Emoji_ZWJ_Sequence",
  "RGI_Emoji",
]

const sets: Ranges[] = []
const setIndex = new Map<string, number>()
const internSet = (ranges: Ranges) => {
  const key = ranges.map(([a, b]) => a + "-" + b).join(",")
  let index = setIndex.get(key)
  if (index === undefined) {
    index = sets.length
    sets.push(ranges)
    setIndex.set(key, index)
  }
  return index
}

const problems: string[] = []
type Entry = [string, number]
const gcEntries: Entry[] = []
const loneEntries: Entry[] = []
for (const names of valueNames("gc")) {
  const accepted = names.filter((n) => accepts(`\\p{gc=${n}}`, "u"))
  for (const n of names) {
    const forms = [`\\p{gc=${n}}`, `\\p{General_Category=${n}}`, `\\p{${n}}`].map((p) => accepts(p, "u"))
    if (forms.some((f) => f !== forms[0])) problems.push(`gc ${n}: inconsistent acceptance ${forms}`)
  }
  if (!accepted.length) continue
  const index = internSet(extract(`\\p{gc=${accepted[0]}}`, "u"))
  for (const n of accepted) gcEntries.push([n, index])
}
const scEntries: Entry[] = []
const scxEntries: Entry[] = []
for (const names of valueNames("sc")) {
  for (const n of names) {
    const forms = [`\\p{sc=${n}}`, `\\p{Script=${n}}`, `\\p{scx=${n}}`, `\\p{Script_Extensions=${n}}`].map((p) =>
      accepts(p, "u"),
    )
    if (forms.some((f) => f !== forms[0])) problems.push(`sc ${n}: inconsistent acceptance ${forms}`)
  }
  const accepted = names.filter((n) => accepts(`\\p{sc=${n}}`, "u"))
  if (!accepted.length) continue
  const sc = internSet(extract(`\\p{sc=${accepted[0]}}`, "u"))
  const scx = internSet(extract(`\\p{scx=${accepted[0]}}`, "u"))
  for (const n of accepted) {
    scEntries.push([n, sc])
    scxEntries.push([n, scx])
  }
}
for (const names of binaryCandidates) {
  const accepted = names.filter((n) => accepts(`\\p{${n}}`, "u"))
  if (!accepted.length) continue
  const index = internSet(extract(`\\p{${accepted[0]}}`, "u"))
  for (const n of accepted) loneEntries.push([n, index])
}
// Spellings JSC must reject: loose matching, the wrong property for a value.
for (const bad of ["\\p{lu}", "\\p{Uppercase Letter}", "\\p{uppercase_letter}", "\\p{Latin}", "\\p{sc=Lu}", "\\p{gc=Latin}", "\\p{Script}", "\\p{ASCII=Y}"]) {
  if (accepts(bad, "u")) problems.push(`JSC accepts ${bad}`)
}

// ---- \s and group-name characters ----------------------------------------------------------------------------
const space = extract("\\s", "u")
const spaceNonUnicode = extract("\\s", "")
if (JSON.stringify(space) !== JSON.stringify(spaceNonUnicode)) problems.push("\\s differs between modes")

const nameStart: Ranges = []
const namePart: Ranges = []
const addPoint = (ranges: Ranges, c: number) => {
  const last = ranges[ranges.length - 1]
  if (last && last[1] === c - 1) last[1] = c
  else ranges.push([c, c])
}
for (let c = 0; c < 0x110000; c++) {
  if (c >= 0xd800 && c < 0xe000) continue
  const ch = String.fromCodePoint(c)
  if (ch === ">") continue
  // `(?<!>)` and `(?<=>)` are lookbehinds, not groups.
  if (ch !== "!" && ch !== "=" && accepts(`(?<${ch}>)`, "")) addPoint(nameStart, c)
  if (accepts(`(?<a${ch}>)`, "")) addPoint(namePart, c)
}
// Spot-check that unicode mode agrees.
for (const c of [0x24, 0x5f, 0x41, 0x30, 0x200c, 0x200d, 0x2118, 0x1d49c, 0xe0100, 0xb7, 0x387, 0x19da, 0x2e2f]) {
  const ch = String.fromCodePoint(c)
  if (accepts(`(?<${ch}>)`, "") !== accepts(`(?<${ch}>)`, "u")) problems.push(`group name start ${hex(c)} differs by mode`)
  if (accepts(`(?<a${ch}>)`, "") !== accepts(`(?<a${ch}>)`, "u")) problems.push(`group name part ${hex(c)} differs by mode`)
}

// ---- case-insensitive equivalence classes --------------------------------------------------------------------
// Non-unicode: every code unit against every code unit.
let bmp = ""
for (let c = 0; c < 0x10000; c++) bmp += String.fromCharCode(c)
const ucs2Class = new Int32Array(0x10000).fill(-1)
const ucs2Classes: number[][] = []
for (let c = 0; c < 0x10000; c++) {
  if (ucs2Class[c] >= 0) continue
  const re = new RegExp("\\u" + c.toString(16).padStart(4, "0"), "gi")
  const members: number[] = []
  for (const m of bmp.matchAll(re)) members.push(m.index!)
  if (!members.includes(c)) problems.push(`ucs2 ${hex(c)} does not match itself`)
  if (members.length > 1) {
    for (const d of members) {
      if (ucs2Class[d] >= 0) problems.push(`ucs2 ${hex(d)} in two classes`)
      ucs2Class[d] = ucs2Classes.length
    }
    ucs2Classes.push(members)
  } else ucs2Class[c] = -2
}
// Symmetry: every member's own class is the same.
for (const members of ucs2Classes) {
  for (const d of members) {
    const re = new RegExp("\\u" + d.toString(16).padStart(4, "0"), "gi")
    const got = [...bmp.matchAll(re)].map((m) => m.index!)
    if (got.join() !== members.join()) problems.push(`ucs2 class of ${hex(d)} is not symmetric`)
  }
}

// Unicode: candidates are the characters JSC's own data says take part in case mapping.
const caseRanges = extract("[\\p{CWCM}\\p{CWCF}\\p{CWKCF}\\p{Cased}]", "u")
const candidates: number[] = []
for (const [a, b] of caseRanges) for (let c = a; c <= b; c++) candidates.push(c)
const haystack = candidates.map((c) => String.fromCodePoint(c)).join("")
const unitToCandidate = new Map<number, number>()
{
  let unit = 0
  for (const c of candidates) {
    unitToCandidate.set(unit, c)
    unit += c > 0xffff ? 2 : 1
  }
}
const uniClassOf = new Map<number, number>()
const uniClasses: number[][] = []
for (const c of candidates) {
  if (uniClassOf.has(c)) continue
  const re = new RegExp("\\u{" + c.toString(16) + "}", "giu")
  const members = [...haystack.matchAll(re)].map((m) => unitToCandidate.get(m.index!)!)
  if (!members.includes(c)) problems.push(`unicode ${hex(c)} does not match itself`)
  if (members.length > 1) {
    for (const d of members) {
      if (uniClassOf.has(d)) problems.push(`unicode ${hex(d)} in two classes`)
      uniClassOf.set(d, uniClasses.length)
    }
    uniClasses.push(members)
  }
}
for (const members of uniClasses) {
  for (const d of members.slice(1)) {
    const re = new RegExp("\\u{" + d.toString(16) + "}", "giu")
    const got = [...haystack.matchAll(re)].map((m) => unitToCandidate.get(m.index!)!)
    if (got.join() !== members.join()) problems.push(`unicode class of ${hex(d)} is not symmetric`)
  }
}
// Nothing outside the candidates is related to a class member.
{
  const members = merge(uniClasses.flat().map((c) => [c, c] as [number, number]))
  const body = "[" + members.map(([a, b]) => `\\u{${a.toString(16)}}-\\u{${b.toString(16)}}`).join("") + "]"
  const got = extract(body, "iu")
  if (JSON.stringify(got) !== JSON.stringify(members)) problems.push("unicode classes reach outside the candidates")
}

// ---- properties of strings ------------------------------------------------------------------------------------
const sequences = new Set<string>()
for (const line of readFileSync(emojiTestPath, "utf8").split("\n")) {
  const m = /^([0-9A-F ]+?)\s*;\s*(fully-qualified|component|minimally-qualified|unqualified)/.exec(line)
  if (!m) continue
  sequences.add(String.fromCodePoint(...m[1].trim().split(/\s+/).map((h) => parseInt(h, 16))))
}
const emojiish = extract("[\\p{Emoji}\\p{Emoji_Component}\\p{Extended_Pictographic}\\p{RI}]", "u")
const singles: number[] = []
for (const [a, b] of emojiish) for (let c = a; c <= b; c++) singles.push(c)
for (const c of singles) {
  sequences.add(String.fromCodePoint(c, 0xfe0f))
  sequences.add(String.fromCodePoint(c, 0xfe0e))
}
for (const k of "#*0123456789") sequences.add(k + "️⃣")
for (let a = 0x1f1e6; a <= 0x1f1ff; a++) for (let b = 0x1f1e6; b <= 0x1f1ff; b++) sequences.add(String.fromCodePoint(a, b))
const modifierBases = extract("\\p{EBase}", "u")
for (const [a, b] of modifierBases)
  for (let c = a; c <= b; c++) for (let m = 0x1f3fb; m <= 0x1f3ff; m++) sequences.add(String.fromCodePoint(c, m))
type StringProp = { name: string; single: number; strings: number[][] }
const stringProps: StringProp[] = []
for (const name of stringPropertyNames) {
  const re = new RegExp(`^\\p{${name}}$`, "v")
  const single: Ranges = []
  for (const c of singles) if (re.test(String.fromCodePoint(c))) single.push([c, c])
  // Single code points outside the emoji-ish candidates must not be members.
  const outsideRe = new RegExp(`[\\p{${name}}--[\\p{Emoji}\\p{Emoji_Component}\\p{Extended_Pictographic}\\p{RI}]]`, "gv")
  for (const m of ALL.matchAll(outsideRe))
    if ([...m[0]].length === 1) problems.push(`${name} has member ${hex(m[0].codePointAt(0)!)} outside the candidates`)
  const strings: number[][] = []
  for (const s of sequences) {
    const cps = [...s].map((ch) => ch.codePointAt(0)!)
    if (cps.length > 1 && re.test(s)) strings.push(cps)
  }
  strings.sort((x, y) => (x.join() < y.join() ? -1 : 1))
  stringProps.push({ name, single: internSet(merge(single)), strings })
}

if (problems.length) {
  console.error(problems.join("\n"))
  process.exit(1)
}


// ---- output ---------------------------------------------------------------------------------------------------
// Numbers are written as base-64 varints (5 data bits per character, bit 5 = more follows), so the tables stay a
// fraction of the size of array literals. See `decode` in src/jsregex/unicode.rs.
const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
const varint = (value: number) => {
  let out = ""
  let v = value
  do {
    const group = v & 31
    v = Math.floor(v / 32)
    out += ALPHABET[group | (v > 0 ? 32 : 0)]
  } while (v > 0)
  return out
}
// A set: first, length, then for each further range (gap - 1, length), all relative.
const encodeSet = (ranges: Ranges) => {
  let out = ""
  let previous = -1
  for (const [a, b] of ranges) {
    out += varint(previous < 0 ? a : a - previous - 2) + varint(b - a)
    previous = b
  }
  return out
}
const zigzag = (n: number) => (n >= 0 ? n * 2 : -n * 2 - 1)
// Strings: for each, its code point count, then each code point as a zigzag delta from the previous one.
const encodeStrings = (strings: number[][]) => {
  let out = ""
  let previous = 0
  for (const s of strings) {
    out += varint(s.length)
    for (const c of s) {
      out += varint(zigzag(c - previous))
      previous = c
    }
  }
  return out
}
const encodePairs = (pairs: Array<[number, number]>) => {
  let out = ""
  let previous = 0
  for (const [c, rep] of pairs) {
    out += varint(c - previous) + varint(zigzag(rep - c))
    previous = c
  }
  return out
}
const pairs = (classes: number[][]) => {
  const out: Array<[number, number]> = []
  for (const members of classes) {
    const rep = Math.min(...members)
    for (const m of members) if (m !== rep) out.push([m, rep])
  }
  return out.sort((a, b) => a[0] - b[0])
}

const lines: string[] = []
lines.push("// Generated by harness/jsregex/gen-tables.ts from JavaScriptCore (bun " + Bun.version + "); do not edit.")
lines.push("// Every string is base-64 varint data; see `decode` in unicode.rs.")
lines.push("")
lines.push("/// Code point sets (encoded ranges), indexed by the tables below.")
lines.push("pub static SETS: &[&str] = &[")
for (const ranges of sets) lines.push(`    ${JSON.stringify(encodeSet(ranges))},`)
lines.push("];")
const entries = (name: string, list: Entry[]) => {
  list.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0))
  lines.push(`pub static ${name}: &[(&str, u16)] = &[`)
  for (const [n, i] of list) lines.push(`    (${JSON.stringify(n)}, ${i}),`)
  lines.push("];")
}
entries("GENERAL_CATEGORY", gcEntries)
entries("SCRIPT", scEntries)
entries("SCRIPT_EXTENSIONS", scxEntries)
// Lone names: General_Category values and binary properties.
entries("LONE", [...gcEntries, ...loneEntries])
lines.push("/// Properties of strings (v flag): name, set of single code points, multi-code-point strings (encoded).")
lines.push("pub static STRING_PROPERTIES: &[(&str, u16, &str)] = &[")
for (const p of stringProps) lines.push(`    (${JSON.stringify(p.name)}, ${p.single}, ${JSON.stringify(encodeStrings(p.strings))}),`)
lines.push("];")
lines.push(`pub static SPACE: &str = ${JSON.stringify(encodeSet(space))};`)
lines.push(`pub static GROUP_NAME_START: &str = ${JSON.stringify(encodeSet(merge(nameStart)))};`)
lines.push(`pub static GROUP_NAME_PART: &str = ${JSON.stringify(encodeSet(merge(namePart)))};`)
lines.push("/// Non-unicode /i: (code unit, smallest unit of its class) for each unit in a class of two or more (encoded).")
lines.push(`pub static UCS2_FOLD: &str = ${JSON.stringify(encodePairs(pairs(ucs2Classes)))};`)
lines.push("/// /iu: (code point, smallest code point of its class) for each code point in a class of two or more (encoded).")
lines.push(`pub static UNICODE_FOLD: &str = ${JSON.stringify(encodePairs(pairs(uniClasses)))};`)
console.log(lines.join("\n"))
console.error(
  `sets ${sets.length}, ranges ${sets.reduce((n, s) => n + s.length, 0)}, gc ${gcEntries.length}, sc ${scEntries.length}, lone ${loneEntries.length}, ucs2 classes ${ucs2Classes.length}, unicode classes ${uniClasses.length}, strings ${stringProps.map((p) => p.strings.length)}`,
)
