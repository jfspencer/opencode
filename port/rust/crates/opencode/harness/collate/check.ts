// The differential check of src/collate.rs (see run.sh): the Rust driver against ICU's own collation elements (the
// dump gen/src/collation.ts makes) and against Bun's `localeCompare`, which every TS sort the port mirrors uses.
//
// usage: bun check.ts --driver <driver> --icu <collation-dump> --dump <dump> --work <dir> [--count N] [--seed S] [--show N]
import { spawnSync } from "node:child_process"
import { closeSync, openSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"

const arg = (name: string, fallback?: string) => {
  const index = process.argv.indexOf(`--${name}`)
  if (index === -1) {
    if (fallback === undefined) throw new Error(`--${name} is required`)
    return fallback
  }
  return process.argv[index + 1]!
}
const driver = arg("driver")
const icuDump = arg("icu")
const work = arg("work")
const dumpFile = arg("dump")
const count = Number(arg("count", "400000"))
const seed = Number(arg("seed", "1"))
const show = Number(arg("show", "20"))

// mulberry32
let state = seed >>> 0
const random = () => {
  state = (state + 0x6d2b79f5) >>> 0
  let t = state
  t = Math.imul(t ^ (t >>> 15), t | 1)
  t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296
}
const pick = <T>(items: readonly T[]) => items[Math.floor(random() * items.length)]!
const range = (first: number, last: number) => Array.from({ length: last - first + 1 }, (_, i) => first + i)
const hex = (text: string) => [...text].map((c) => c.codePointAt(0)!.toString(16).toUpperCase()).join(" ")
const show16 = (text: string) => JSON.stringify(text) + ` [${hex(text)}]`

/** Runs the driver (or ICU's dump with `--ces`) over lines of input; returns its output lines. */
function run(mode: string, lines: string[], exe = driver) {
  const input = join(work, `${mode}.in`)
  const output = join(work, `${mode}.out`)
  writeFileSync(input, lines.join("\n") + "\n")
  const [stdin, stdout] = [openSync(input, "r"), openSync(output, "w")]
  const result = spawnSync(exe, [mode], { stdio: [stdin, stdout, "inherit"] })
  closeSync(stdin)
  closeSync(stdout)
  if (result.status !== 0) throw new Error(`${exe} ${mode} failed`)
  const out = readFileSync(output, "utf8").split("\n")
  out.pop()
  if (out.length !== lines.length) throw new Error(`driver ${mode}: ${out.length} lines for ${lines.length}`)
  return out
}

let failures = 0
const report = (title: string, total: number, differences: string[]) => {
  console.log(`${title}: ${total} cases, ${differences.length} differences`)
  for (const line of differences.slice(0, show)) console.log(`  ${line}`)
  failures += differences.length
}

// --- 1. Collation elements against ICU's ------------------------------------------------------------------------
const dump = readFileSync(dumpFile, "utf8").split("\n")
type Entry = { cps: number[]; ces: string[] }
const entries: Entry[] = []
const ccc = new Map<number, number>()
const decomposable = new Set<number>()
const explicit: number[] = []
const unassignedCps: number[] = []
const contractionStrings: number[][] = []
const lead = (p: number) => p >>> 24
const allPrimaries = new Set<number>()
const parsed = dump
  .filter((line) => line && !line.startsWith("#"))
  .map((line) => {
    const [left, right] = line.split(";") as [string, string]
    const [tag, ...fields] = left.trim().split(/\s+/)
    return { tag: tag!, fields, right: right.trim() }
  })
for (const { tag, fields, right } of parsed) {
  if (tag === "N") {
    const cp = parseInt(fields[0]!, 16)
    if (+fields[1]!) ccc.set(cp, +fields[1]!)
    if (right) decomposable.add(cp)
    continue
  }
  for (const item of right.split(/\s+/).filter(Boolean)) {
    const p = parseInt(item.split(":")[0]!, 16)
    if (p !== 0 && lead(p) < 0xfe) allPrimaries.add(p)
  }
}
const rank = new Map([...allPrimaries].sort((a, b) => a - b).map((p, i) => [p, i + 1]))
/** ICU's elements as the table weighs them: ranked primaries, masked tertiaries. */
const weighed = (text: string) =>
  text
    .split(/\s+/)
    .filter(Boolean)
    .map((item) => {
      const [p, s, t] = item.split(":").map((h) => parseInt(h, 16)) as [number, number, number]
      const weight = p === 0 || lead(p) >= 0xfe ? p : (rank.get(p) ?? `unknown primary ${p.toString(16)}`)
      return `${weight.toString(16)}:${s.toString(16)}:${(t & 0x3f3f).toString(16)}`
    })
for (const { tag, fields, right } of parsed) {
  if (tag === "N") continue
  const cps = fields.map((h) => parseInt(h, 16))
  const ces = weighed(right)
  entries.push({ cps, ces })
  if (tag === "C") (lead(parseInt(right.split(":")[0]!, 16)) === 0xfe ? unassignedCps : explicit).push(cps[0]!)
  else contractionStrings.push(cps)
}
{
  const got = run(
    "elements",
    entries.map((e) => e.cps.map((cp) => cp.toString(16).toUpperCase()).join(" ")),
  )
  const differences: string[] = []
  entries.forEach((entry, i) => {
    const want = entry.ces.join(" ")
    if (got[i] !== want) differences.push(`${entry.cps.map((cp) => cp.toString(16)).join(" ")}: icu ${want} rust ${got[i]}`)
  })
  report("elements of every code point, contraction and prefix mapping (ICU)", entries.length, differences)
}

// --- 2. Order against Bun's localeCompare ----------------------------------------------------------------------
// Every string compared below also has its elements checked against ICU's CollationElementIterator, which runs the
// same iteration as ucol_strcoll (from the start of the string).
const generated = new Set<string>()
const elementsCheck = () => {
  const strings = [...generated]
  const lines = strings.map(hex)
  const [icu, rust] = [run("--ces", lines, icuDump), run("elements", lines)]
  const differences: string[] = []
  strings.forEach((text, i) => {
    const want = weighed(icu[i]!).join(" ")
    if (rust[i] !== want) differences.push(`${show16(text)}: icu ${want} rust ${rust[i]}`)
  })
  report("elements of every generated string (ICU)", strings.length, differences)
}
const sign = (n: number) => (n < 0 ? -1 : n > 0 ? 1 : 0)
const compareAll = (title: string, pairs: Array<[string, string]>) => {
  // (Single code points were checked with the dump.)
  for (const text of pairs.flat()) if (text.length < 64 && !/^.?$/su.test(text) && generated.size < 3_000_000) generated.add(text)
  const got = run(
    "compare",
    pairs.map(([a, b]) => `${hex(a)}\t${hex(b)}`),
  )
  const differences: string[] = []
  pairs.forEach(([a, b], i) => {
    const bun = sign(a.localeCompare(b))
    if (String(bun) !== got[i]) differences.push(`${show16(a)} vs ${show16(b)}: bun ${bun} rust ${got[i]}`)
  })
  report(title, pairs.length, differences)
}
const sortAll = (title: string, items: string[]) => {
  const bun = items.toSorted((a, b) => a.localeCompare(b))
  const rust = run("sort", items.map(hex))
  const differences: string[] = []
  for (let i = 0; i < bun.length && differences.length < 1000; i++)
    if (hex(bun[i]!) !== rust[i]) differences.push(`at ${i}: bun ${show16(bun[i]!)} rust [${rust[i]}]`)
  report(title, items.length, differences)
  // Neighbours: a stable sort can hide a tie one side sees and the other does not.
  const pairs: Array<[string, string]> = []
  for (let i = 0; i + 1 < bun.length; i++) pairs.push([bun[i]!, bun[i + 1]!], [bun[i + 1]!, bun[i]!])
  compareAll(`${title}: neighbours both ways`, pairs)
}

const scalars = [...range(0, 0xd7ff), ...range(0xe000, 0x10ffff)]
const chr = (cp: number) => String.fromCodePoint(cp)
sortAll("every code point on its own, sorted", scalars.map(chr))

// Pools of interesting code points.
const marks = [...ccc.keys()]
const starters = [...new Set(contractionStrings.map((s) => s[0]!))]
const continuations = [...new Set(contractionStrings.flatMap((s) => s.slice(1)))]
// Han ideographs of every block (the unified ones; CJK compatibility ideographs and radicals have pools of their own).
const han = explicit.filter((cp) => (cp >= 0x3400 && cp <= 0x4dbf) || (cp >= 0x4e00 && cp <= 0x9fff) || (cp >= 0x20000 && cp <= 0x3ffff))
const pools: Record<string, number[]> = {
  ascii: range(0x20, 0x7e),
  asciiLetters: [...range(0x41, 0x5a), ...range(0x61, 0x7a), ...range(0x30, 0x39)],
  controls: [...range(0, 0x1f), ...range(0x7f, 0x9f), 0xad, ...range(0x200b, 0x200f), ...range(0x2028, 0x202e), ...range(0x2060, 0x206f), 0xfeff, ...range(0xe0000, 0xe007f)],
  latin: [...range(0xc0, 0x24f), ...range(0x1e00, 0x1eff), ...range(0x2c60, 0x2c7f), ...range(0xa720, 0xa7ff)],
  marks,
  starters,
  continuations,
  decomposable: [...decomposable].filter((cp) => cp < 0xac00 || cp > 0xd7a3),
  han,
  hanCompat: [...range(0xf900, 0xfad9), ...range(0x2f800, 0x2fa1d)],
  radicals: [...range(0x2e80, 0x2ef3), ...range(0x2f00, 0x2fd5), ...range(0x31c0, 0x31e5)],
  hangul: [...range(0xac00, 0xd7a3)],
  jamo: [...range(0x1100, 0x11ff), ...range(0xa960, 0xa97c), ...range(0xd7b0, 0xd7fb), ...range(0x3131, 0x318e), ...range(0xffa0, 0xffdc)],
  thaiLao: [...range(0x0e01, 0x0e5b), ...range(0x0e81, 0x0edf)],
  tibetan: range(0x0f00, 0x0fda),
  kana: [...range(0x3041, 0x30ff), ...range(0x31f0, 0x31ff), ...range(0xff65, 0xff9f), ...range(0x1b000, 0x1b16f), 0x3099, 0x309a, 0x30fc, 0x309d, 0x309e, 0x30fd, 0x30fe, 0x3005, 0x3031, 0x3032],
  emoji: [...range(0x1f300, 0x1faff), ...range(0x2600, 0x27bf), ...range(0x1f3fb, 0x1f3ff), 0x200d, 0xfe0f, 0xfe0e, ...range(0x1f1e6, 0x1f1ff), 0x20e3],
  selectors: [...range(0xfe00, 0xfe0f), ...range(0xe0100, 0xe01ef), ...range(0x180b, 0x180f)],
  special: [0xfffe, 0xffff, 0xfffd, ...range(0xfdd0, 0xfdef), 0x1fffe, 0x1ffff, 0x10fffe, 0x10ffff, 0xe000, 0xf8ff, 0xf0000, 0x100000],
  middleDot: [0x4c, 0x6c, 0xb7, 0x387, 0x13f, 0x140, 0x301, 0x327, 0x4b, 0x6b],
  unassigned: unassignedCps,
  explicit,
  scripts: [...range(0x370, 0x52f), ...range(0x591, 0x6ff), ...range(0x900, 0xdff), ...range(0x1000, 0x109f), ...range(0x1780, 0x17ff), ...range(0x1a00, 0x1aff), ...range(0xaa00, 0xaadf), ...range(0x11000, 0x1137f), ...range(0x16100, 0x1613f), ...range(0x16d40, 0x16d7f)],
  digits: [...range(0x30, 0x39), ...range(0x660, 0x669), ...range(0x966, 0x96f), ...range(0xff10, 0xff19), ...range(0x1d7ce, 0x1d7ff), 0xb2, 0xb3, 0xb9, 0xbc, 0xbd, 0x2460, 0x2070],
  symbols: [...range(0x2000, 0x2bff), ...range(0x3000, 0x303f), ...range(0xfe30, 0xfe6f), ...range(0xff01, 0xff5e), ...range(0x1d400, 0x1d7ff), ...range(0x20a0, 0x20c1)],
}
const poolNames = Object.keys(pools)
const fromPool = () => {
  const name = pick(poolNames)
  return pick(pools[name]!.length ? pools[name]! : pools.ascii!)
}
/** A random string: code points from one or a few pools, contraction strings and their parts mixed in. */
const randomString = (maxLength = 5) => {
  const length = 1 + Math.floor(random() * maxLength)
  const local = random() < 0.5 ? [pick(poolNames), pick(poolNames)] : poolNames
  const cps: number[] = []
  while (cps.length < length) {
    const r = random()
    if (r < 0.15) cps.push(...pick(contractionStrings))
    else if (r < 0.35) cps.push(pick(marks))
    else cps.push(pick(pools[pick(local)]!.length ? pools[pick(local)]! : pools.ascii!))
  }
  return String.fromCodePoint(...cps.map((cp) => (cp >= 0xd800 && cp <= 0xdfff ? 0x61 : cp)))
}
const mutate = (text: string) => {
  const cps = [...text].map((c) => c.codePointAt(0)!)
  const at = Math.floor(random() * (cps.length + 1))
  switch (Math.floor(random() * 10)) {
    case 0:
      cps.splice(at, 0, pick(marks))
      break
    case 1:
      if (cps.length) cps.splice(Math.min(at, cps.length - 1), 1)
      break
    case 2:
      if (cps.length > 1) {
        const i = Math.min(at, cps.length - 2)
        ;[cps[i], cps[i + 1]] = [cps[i + 1]!, cps[i]!]
      }
      break
    case 3:
      return text.toUpperCase()
    case 4:
      return text.toLowerCase()
    case 5:
      return text.normalize(pick(["NFC", "NFD", "NFKC", "NFKD"]))
    case 6:
      cps.splice(at, 0, fromPool())
      break
    case 7:
      if (cps.length) cps[Math.min(at, cps.length - 1)] = fromPool()
      break
    case 8:
      cps.push(...pick(contractionStrings))
      break
    default:
      return text + randomString(2)
  }
  return String.fromCodePoint(...cps)
}

// Pairs.
{
  const pairs: Array<[string, string]> = []
  for (let i = 0; i < count; i++) {
    const a = randomString()
    const r = random()
    pairs.push([a, r < 0.4 ? randomString() : r < 0.9 ? mutate(a) : mutate(mutate(a))])
  }
  compareAll(`generated pairs (seed ${seed})`, pairs)
}
// Pairs of every pool against itself, short strings: the levels and the tie-breaking of each script.
{
  const pairs: Array<[string, string]> = []
  for (const name of poolNames) {
    const pool = pools[name]!
    if (!pool.length) continue
    for (let i = 0; i < Math.min(20000, pool.length * 4); i++) {
      const length = 1 + Math.floor(random() * 3)
      const a = String.fromCodePoint(...Array.from({ length }, () => pick(pool)))
      const b = String.fromCodePoint(...Array.from({ length: 1 + Math.floor(random() * 3) }, () => pick(pool)))
      pairs.push([a, b])
    }
  }
  compareAll("pairs within each pool", pairs)
}
// Every contraction and prefix mapping against its pieces, its pieces in other orders and with marks inside.
{
  const pairs: Array<[string, string]> = []
  for (const cps of contractionStrings) {
    const whole = String.fromCodePoint(...cps)
    for (let k = 0; k < 6; k++) {
      const other = [...cps]
      const at = 1 + Math.floor(random() * (other.length - 1 || 1))
      other.splice(at, 0, pick(marks))
      pairs.push([whole, String.fromCodePoint(...other)])
      pairs.push([String.fromCodePoint(...other), mutate(String.fromCodePoint(...other))])
    }
    pairs.push([whole, String.fromCodePoint(...cps.toReversed())])
    for (const cp of cps) pairs.push([whole, chr(cp)])
  }
  compareAll("contractions with marks inside and around", pairs)
}
// The FCD machinery: starters, contraction starters and prefix characters followed by runs of marks in any order
// (BMP and supplementary, Tibetan composite vowels, marks that start contractions themselves), and pairs that share
// a prefix and differ inside such a run, so the comparison starts after an identical prefix.
{
  const supplementaryMarks = marks.filter((cp) => cp > 0xffff)
  const tibetanVowels = [0x0f71, 0x0f72, 0x0f73, 0x0f74, 0x0f75, 0x0f7a, 0x0f80, 0x0f81, 0x0fb2, 0x0fb3, 0x0f90, 0x0f40]
  const lead = () => {
    const r = random()
    return r < 0.3 ? pick(starters) : r < 0.4 ? pick([0x4c, 0x6c, 0xb7, 0x387]) : r < 0.5 ? pick(tibetanVowels) : fromPool()
  }
  const run = () =>
    Array.from({ length: Math.floor(random() * 5) }, () => {
      const r = random()
      return r < 0.3 ? pick(supplementaryMarks) : r < 0.45 ? pick(tibetanVowels) : r < 0.55 ? pick(continuations) : r < 0.6 ? 0xb7 : pick(marks)
    })
  const piece = () => [lead(), ...run()]
  const pairs: Array<[string, string]> = []
  for (let i = 0; i < count / 2; i++) {
    const shared = Array.from({ length: Math.floor(random() * 3) }, piece).flat()
    const [a, b] = [[...shared, ...piece()], [...shared, ...piece()]]
    if (random() < 0.5) b.splice(shared.length, 0, ...a.slice(shared.length, shared.length + Math.floor(random() * 3)))
    pairs.push([String.fromCodePoint(...a), String.fromCodePoint(...b)])
  }
  compareAll("marks around starters, contractions and prefixes", pairs)
}
// ASCII strings: JavaScriptCore compares these on its own fast path.
{
  const pairs: Array<[string, string]> = []
  const ascii = pools.ascii!
  for (let i = 0; i < count / 4; i++) {
    const a = String.fromCodePoint(...Array.from({ length: Math.floor(random() * 8) }, () => pick(random() < 0.7 ? pools.asciiLetters! : ascii)))
    pairs.push([a, random() < 0.5 ? mutate(a).replace(/[^\x00-\x7f]/gu, "") : a + pick(["", "a", "A", "-", "_", " ", "0"])])
  }
  for (const a of range(0, 0x7f)) for (const b of range(0, 0x7f)) pairs.push([chr(a), chr(b)], [`x${chr(a)}y`, `x${chr(b)}y`])
  compareAll("ASCII pairs", pairs)
}
// Random samples sorted whole.
for (let k = 0; k < 20; k++) sortAll(`random sample ${k + 1} sorted`, Array.from({ length: 3000 }, () => randomString(6)))

// Hand-picked cases: every pair among them.
{
  const picked = [
    "", " ", "a", "A", "á", "á", "á", "Á", "ä", "ä", "ae", "æ", "Æ", "aE", "ß", "ss", "SS", "ẞ",
    "l·l", "L·L", "l·l", "ŀl", "Ŀl", "ĺ·", "l·", "l", "·", "ll", "lĺ",
    "ch", "cz", "c", "d", "й", "й", "и", "ё", "ё", "е",
    "ཱི", "ཱི", "ཱི", "ྲཱི", "ྲཱི", "ྲཱྀ", "ཷ", "ྲཱྀ",
    "เก", "เ", "ก", "แก", "ເກ", "ເ", "ກ",
    "가", "가", "각", "각", "ᄀ", "ᅡ", "ᆨ", "ㄱ", "각ᆨ",
    "一", "丁", "㐀", "𠀀", "⼀", "⺀", "豈", "豈", "豈", "\u{2f800}", "丽", "\u{30000}", "\u{31350}", "\u{33479}", "鿿",
    "ア", "あ", "ｱ", "アー", "あー", "ア゙", "ヴ", "ゝ", "ゞ", "ヽ", "カゝ", "かゞ",
    "￾", "￿", "�", "﷑", "﷑A", "﷐", "\u{10ffff}", "\u{e0001}", "\u0000", "a\u0000b", "ab",
    "😀", "😀\u{1f3fb}", "👍", "👍\u{1f3fd}", "👨‍👩", "❤", "❤️", "\u{1fae9}", "\u{1f6dc}",
    "1", "１", "¹", "①", "10", "2", "٢", "a1", "a01", "a٠",
    "x̣́", "x̣́", "ạ́", "ấ", "ậ", "ậ", "ậ",
    "آ", "آ", "ا", "क़", "क़", "क",
    "\u{16d63}\u{16d67}\u{16d67}", "\u{1611e}\u{1611e}\u{1611f}", "ೋ", "ෝ",
  ]
  const pairs: Array<[string, string]> = []
  for (const a of picked) for (const b of picked) pairs.push([a, b])
  compareAll("hand-picked pairs", pairs)
}

elementsCheck()

console.log(failures === 0 ? "collate: no differences" : `collate: ${failures} differences`)
process.exit(failures === 0 ? 0 : 1)
