// Generates crates/opencode/src/generated/collation.bin: ICU's root collation in a compact binary form, for
// `String.prototype.localeCompare` (crates/opencode/src/collate.rs). The TS reference runs on Bun, whose ICU collates
// every locale opencode sees with the CLDR root order (Bun's default locale is en-US, which has no tailoring). CLDR
// root is not the DUCET: Han ideographs are in radical-stroke order (ICU's default "unihan" root), U+FFFE/U+FFFF and
// the U+FDD1 script markers have their own weights, `l·` has a prefix mapping, and scripts are grouped for reordering.
//
// The source is ICU itself: collation-dump.c, compiled against the system's ICU (libicu-dev), dumps the collation
// elements of every code point, contraction and prefix mapping of the root collator, with the NFD data of the same
// ICU. Its major version and Unicode version must be Bun's (Bun 1.4.2: ICU 78.3, Unicode 17.0; ICU 78.2's
// FractionalUCA.txt, the root order's source, is byte-identical to 78.3's).
//   bun src/collation.ts                 regenerate
//   bun src/collation.ts --check         fail if the checked-in file differs from a fresh generation
//   bun src/collation.ts --dump <file>   also keep the raw dump (crates/opencode/harness/collate checks Rust against it)
//
// collate.rs runs ICU's own algorithm over these mappings (FCDUTF16CollationIterator and CollationIterator), so the
// table holds what ICU's CollationData holds that the root's algorithm reads: the mapping of every code point as ICU
// looks it up (precomposed characters included, as they meet ICU's canonical closure), every contraction and prefix
// mapping, the NFD data its normalizer uses, and CollationFCD's quick-check bit sets and the unsafe-backward set
// (both computed by genuca's and CollationDataReader's rules, which were checked against ICU 78.3's
// collationfcd.cpp and collunsafe.h).
//
// Collation elements keep ICU's comparison: the 32-bit primary, the 16-bit secondary and the tertiary masked as
// ICU masks it at tertiary strength without a case level (CollationSettings::ONLY_TERTIARY_MASK, 0x3f3f). Primaries
// below the unassigned-implicit lead byte (0xFE) are renumbered by rank, which keeps their order and makes runs of
// code points with consecutive primaries; the unassigned implicit primaries (Collation::unassignedPrimaryFromCodePoint)
// are computed at run time and stay above every rank, as do the explicit 0xFF primaries (U+FFFD, U+FFFF).
//
// Layout (little-endian u32 words):
//   "UCA3", then the count of each section in order
//   ranges:         (start | kind << 24, end | pair << 21, value), by code point, for every code point with explicit
//                   weights but the Hangul syllables (their elements are their jamo's): kind 0 = one element per code
//                   point whose primary is value + (cp - start) and whose secondary/tertiary are pairs[pair];
//                   kind 1 = the same element for every code point; kind 2 = the elements at value (first << 8 | count)
//   pairs:          secondary << 16 | tertiary
//   elements:       (primary, secondary << 16 | tertiary)
//   contractions:   (cp0, cp1, cp2 or 0, first << 8 | count), in code point order: every contraction (its starter's
//                   own mapping is in ranges, or implicit)
//   prefixes:       (cp, prefix cp, first << 8 | count): the elements of cp after that prefix, by cp
//   ccc:            (start | ccc << 24, end): canonical combining classes
//   decompositions: (cp, first << 8 | count) of the code point pool: the full NFD of every decomposable code point
//                   but the Hangul syllables
//   pool:           code points
//   bits:           three bit sets over UTF-16 code units (2048 words each): CollationFCD's lccc and tccc quick checks
//                   and the root collator's unsafe-backward set
import { spawnSync } from "node:child_process"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const args = process.argv.slice(2)
const check = args.includes("--check")
const keep = args.includes("--dump") ? args[args.indexOf("--dump") + 1] : undefined
const output = join(import.meta.dir, "../../crates/opencode/src/generated/collation.bin")

const dump = (() => {
  const dir = mkdtempSync(join(tmpdir(), "collation-"))
  try {
    const exe = join(dir, "dump")
    const built = spawnSync("cc", ["-O2", "-o", exe, join(import.meta.dir, "collation-dump.c"), "-licui18n", "-licuuc", "-licudata"], {
      encoding: "utf8",
    })
    if (built.status !== 0) throw new Error(`compiling collation-dump.c failed (is libicu-dev installed?):\n${built.stderr}`)
    const ran = spawnSync(exe, [], { encoding: "utf8", maxBuffer: 1 << 30 })
    if (ran.status !== 0) throw new Error(`collation-dump failed:\n${ran.stderr}`)
    return ran.stdout
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})()
if (keep) writeFileSync(keep, dump)

type CE = { p: number; s: number; t: number }
const icu = /^# icu (\S+)/m.exec(dump)?.[1] ?? ""
const unicode = /^# unicode (\S+)/m.exec(dump)?.[1] ?? ""
const bun = process.versions as Record<string, string | undefined>
if (icu.split(".")[0] !== bun.icu?.split(".")[0] || unicode !== bun.unicode)
  throw new Error(`ICU ${icu} (Unicode ${unicode}) is not Bun's ICU ${bun.icu} (Unicode ${bun.unicode})`)

const singles = new Map<number, CE[]>()
const contractions: Array<{ cps: number[]; ces: CE[] }> = []
const prefixed: Array<{ cps: number[]; ces: CE[] }> = []
const ccc = new Map<number, number>()
const nfd = new Map<number, number[]>()
const parseCEs = (text: string): CE[] =>
  text
    .trim()
    .split(/\s+/)
    .filter(Boolean)
    .map((item) => {
      const [p, s, t] = item.split(":").map((h) => parseInt(h, 16)) as [number, number, number]
      return { p, s, t: t & 0x3f3f }
    })
for (const line of dump.split("\n")) {
  if (!line || line.startsWith("#")) continue
  const [left, right] = line.split(";") as [string, string]
  const [tag, ...fields] = left.trim().split(/\s+/)
  if (tag === "N") {
    const cp = parseInt(fields[0]!, 16)
    if (+fields[1]!) ccc.set(cp, +fields[1]!)
    const mapping = right.trim()
    if (mapping) nfd.set(cp, mapping.split(/\s+/).map((h) => parseInt(h, 16)))
    continue
  }
  const cps = fields.map((h) => parseInt(h, 16))
  const ces = parseCEs(right)
  if (tag === "C") singles.set(cps[0]!, ces)
  else if (tag === "X") contractions.push({ cps, ces })
  else if (tag === "P") prefixed.push({ cps, ces })
  else throw new Error(`unknown dump line: ${line}`)
}
if (singles.size !== 0x110000 - 0x800) throw new Error(`expected every scalar value, got ${singles.size}`)

const hangul = (cp: number) => cp >= 0xac00 && cp <= 0xd7a3
const lccc = (cp: number) => (hangul(cp) ? 0 : (ccc.get(nfd.get(cp)?.[0] ?? cp) ?? 0))
const tccc = (cp: number) => (hangul(cp) ? 0 : (ccc.get(nfd.get(cp)?.at(-1) ?? cp) ?? 0))

// The unassigned implicit primary (collation.cpp `Collation::unassignedPrimaryFromCodePoint`).
const unassigned = (cp: number) => {
  let c = cp + 1
  let p = 2 + (c % 18) * 14
  c = Math.floor(c / 18)
  p |= (2 + (c % 254)) << 8
  c = Math.floor(c / 254)
  p |= (4 + (c % 251)) << 16
  return (p | (0xfe << 24)) >>> 0
}
const implicit = (cp: number, ces: CE[]) =>
  ces.length === 1 && ces[0]!.p === unassigned(cp) && ces[0]!.s === 0x500 && ces[0]!.t === 0x500
const same = (a: CE[], b: CE[]) => a.length === b.length && a.every((ce, i) => ce.p === b[i]!.p && ce.s === b[i]!.s && ce.t === b[i]!.t)

const kept = [...singles].filter(([cp, ces]) => !hangul(cp) && !implicit(cp, ces))
if (contractions.some((c) => c.cps.length > 3)) throw new Error("a contraction longer than 3")
const prefixes = prefixed.map((p) => {
  if (p.cps.length !== 2) throw new Error(`prefix longer than one code point: ${p.cps}`)
  const before = singles.get(p.cps[0]!)!
  if (!same(p.ces.slice(0, before.length), before)) throw new Error(`prefix elements: ${p.cps}`)
  return { prefix: p.cps[0]!, cp: p.cps[1]!, ces: p.ces.slice(before.length) }
})
// collate.rs relies on the root's shape: no code point has both prefixes and contractions (a prefix value is never a
// contraction), and the jamo have plain mappings (ICU's HANGUL_NO_SPECIAL_JAMO).
const starters = new Set(contractions.map((c) => c.cps[0]!))
if (prefixes.some((p) => starters.has(p.cp))) throw new Error("a code point with prefixes and contractions")
const jamo = [...Array.from({ length: 19 }, (_, i) => 0x1100 + i), ...Array.from({ length: 21 }, (_, i) => 0x1161 + i), ...Array.from({ length: 27 }, (_, i) => 0x11a8 + i)]
if (jamo.some((cp) => starters.has(cp) || prefixes.some((p) => p.cp === cp))) throw new Error("a jamo with contractions")

// Primary ranks.
const RANKED = 0xfe000000
const primaries = new Set<number>()
for (const ces of [...kept.map(([, ces]) => ces), ...contractions.map((c) => c.ces), ...prefixes.map((p) => p.ces)])
  for (const ce of ces) if (ce.p !== 0 && ce.p < RANKED) primaries.add(ce.p)
const rank = new Map([...primaries].sort((a, b) => a - b).map((p, i) => [p, i + 1]))
const weigh = (ce: CE): CE => ({ p: ce.p === 0 || ce.p >= RANKED ? ce.p : rank.get(ce.p)!, s: ce.s, t: ce.t })

const elements: number[] = []
const elementRefs = new Map<string, number>()
const pushElements = (ces: CE[]) => {
  if (ces.length === 0 || ces.length > 255) throw new Error(`element count ${ces.length}`)
  const key = ces.map((ce) => `${ce.p}:${ce.s}:${ce.t}`).join(" ")
  const known = elementRefs.get(key)
  if (known !== undefined) return known
  const ref = (((elements.length / 2) << 8) | ces.length) >>> 0
  for (const ce of ces) elements.push(ce.p, ((ce.s << 16) | ce.t) >>> 0)
  elementRefs.set(key, ref)
  return ref
}
const pairs: number[] = []
const pairIndex = new Map<number, number>()
const pairOf = (ce: CE) => {
  const word = ((ce.s << 16) | ce.t) >>> 0
  const known = pairIndex.get(word)
  if (known !== undefined) return known
  pairs.push(word)
  pairIndex.set(word, pairs.length - 1)
  return pairs.length - 1
}

// Ranges: runs of single elements with one secondary/tertiary pair and consecutive (kind 0) or equal (kind 1)
// primaries.
const ranges: number[] = []
let count = 0
for (let i = 0; i < kept.length; ) {
  const [start, raw] = kept[i]!
  const ces = raw.map(weigh)
  const run = (step: number) => {
    const ce = ces[0]!
    let k = i + 1
    while (k < kept.length) {
      const [cp, next] = kept[k]!
      if (cp !== start + (k - i) || next.length !== 1) break
      const n = weigh(next[0]!)
      if (n.s !== ce.s || n.t !== ce.t || n.p !== ce.p + step * (k - i)) break
      k++
    }
    return k
  }
  const linear = ces.length === 1 && ces[0]!.p !== 0 && ces[0]!.p < RANKED ? run(1) : i + 1
  const constant = ces.length === 1 ? run(0) : i + 1
  const kind = ces.length !== 1 ? 2 : linear > i + 1 && linear >= constant ? 0 : 1
  const j = kind === 0 ? linear : kind === 1 ? constant : i + 1
  const end = kept[j - 1]![0]
  const pair = kind === 2 ? 0 : pairOf(ces[0]!)
  if (pair >= 1 << 11) throw new Error("too many secondary/tertiary pairs")
  ranges.push((start | (kind << 24)) >>> 0, (end | (pair << 21)) >>> 0, kind === 2 ? pushElements(ces) : ces[0]!.p)
  count += end - start + 1
  i = j
}

const contractionWords = contractions
  .toSorted((a, b) => a.cps[0]! - b.cps[0]! || a.cps[1]! - b.cps[1]! || (a.cps[2] ?? 0) - (b.cps[2] ?? 0))
  .flatMap((c) => [c.cps[0]!, c.cps[1]!, c.cps[2] ?? 0, pushElements(c.ces.map(weigh))])
const prefixWords = prefixes
  .toSorted((a, b) => a.cp - b.cp || a.prefix - b.prefix)
  .flatMap((p) => [p.cp, p.prefix, pushElements(p.ces.map(weigh))])

const cccWords: number[] = []
const classes = [...ccc].sort((a, b) => a[0] - b[0])
for (let i = 0; i < classes.length; ) {
  const [start, value] = classes[i]!
  let j = i + 1
  while (j < classes.length && classes[j]![0] === start + (j - i) && classes[j]![1] === value) j++
  cccWords.push((start | (value << 24)) >>> 0, classes[j - 1]![0])
  i = j
}
const pool: number[] = []
const decompositionWords = [...nfd]
  .filter(([cp]) => !hangul(cp))
  .sort((a, b) => a[0] - b[0])
  .flatMap(([cp, mapping]) => {
    const ref = ((pool.length << 8) | mapping.length) >>> 0
    pool.push(...mapping)
    return [cp, ref]
  })
if (elements.length / 2 >= 1 << 24 || pool.length >= 1 << 24) throw new Error("too many elements")

// Bit sets over code units. genuca's buildAndWriteFCDData: lccc = [[:^lccc=0:][\udc00-\udfff]], tccc = [:^tccc=0:], and
// a lead surrogate is in lccc when one of its supplementary code points has lccc, in tccc when one has tccc or lccc.
// The root's unsafe-backward set (CollationDataReader with collunsafe.h): [:^lccc=0:], the trail surrogates, every
// code point of a contraction but its first, and the lead surrogate of any supplementary code point in the set.
const bitset = () => new Uint32Array(2048)
const set = (bits: Uint32Array, unit: number) => (bits[unit >> 5]! |= 1 << (unit & 31))
const [lcccBits, tcccBits, unsafeBits] = [bitset(), bitset(), bitset()]
const suffixCps = new Set(contractions.flatMap((c) => c.cps.slice(1)))
for (let cp = 0; cp <= 0x10ffff; cp++) {
  if (cp >= 0xd800 && cp <= 0xdfff) {
    if (cp >= 0xdc00) {
      set(lcccBits, cp)
      set(unsafeBits, cp)
    }
    continue
  }
  const unit = cp < 0x10000 ? cp : 0xd800 + ((cp - 0x10000) >> 10)
  const [l, t] = [lccc(cp), tccc(cp)]
  if (l) set(lcccBits, unit)
  if (t || (cp >= 0x10000 && l)) set(tcccBits, unit)
  if (l || suffixCps.has(cp)) set(unsafeBits, unit)
}

const sections = [ranges, pairs, elements, contractionWords, prefixWords, cccWords, decompositionWords, pool, [...lcccBits, ...tcccBits, ...unsafeBits]]
const words = [0x33414355, ...sections.map((s) => s.length), ...sections.flat()]
const bytes = Buffer.alloc(words.length * 4)
words.forEach((word, index) => bytes.writeUInt32LE(word >>> 0, index * 4))

if (check) {
  if (!readFileSync(output).equals(bytes)) throw new Error(`${output} is out of date`)
  console.log(`${output} is up to date`)
} else {
  writeFileSync(output, bytes)
  console.log(
    `wrote ${output} (ICU ${icu}, Unicode ${unicode}): ${count} code points in ${ranges.length / 3} ranges, ` +
      `${pairs.length} pairs, ${elements.length / 2} elements, ${contractions.length} contractions, ` +
      `${prefixes.length} prefixes, ${cccWords.length / 2} ccc ranges, ${decompositionWords.length / 2} decompositions; ` +
      `${bytes.length} bytes`,
  )
}
