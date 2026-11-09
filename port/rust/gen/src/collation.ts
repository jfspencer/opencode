// Generates crates/opencode/src/generated/collation.bin: the Default Unicode Collation Element Table (DUCET) in a
// compact binary form, for `String.prototype.localeCompare` (crates/opencode/src/collate.rs). The TS reference runs on
// Bun, whose ICU collates every locale opencode sees with the CLDR root order (Bun's default locale is en-US, which
// has no tailoring); CLDR root is the DUCET apart from a few scripts.
//
// The input is DUCET's allkeys.txt (UTS #10), for example the copy Perl's Unicode::Collate ships:
//   bun src/collation.ts /usr/share/perl/5.40.1/Unicode/Collate/allkeys.txt
//   bun src/collation.ts <allkeys.txt> --check    fail if the checked-in file differs from a fresh generation
//
// Layout (little-endian u32 words):
//   "UCA1", singles, contractions, elements, implicit ranges
//   singles:      (code point, first element << 8 | element count), by code point
//   contractions: (cp0, cp1, cp2 or 0, first element << 8 | element count), in code point order
//   elements:     primary << 14 | secondary << 5 | tertiary
//   implicit:     (first, last, base): the @implicitweights ranges (Tangut, Nushu, Khitan)
import { readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"

const [input, flag] = process.argv.slice(2)
if (!input) throw new Error("usage: bun src/collation.ts <allkeys.txt> [--check]")
const output = join(import.meta.dir, "../../crates/opencode/src/generated/collation.bin")

const singles = new Map<number, number[]>()
const contractions = new Map<string, { cps: number[]; ces: number[] }>()
const implicit: Array<[number, number, number]> = []
let version = ""

for (const raw of readFileSync(input, "utf8").split("\n")) {
  const line = raw.split("#")[0]!.trim()
  if (!line) continue
  if (line.startsWith("@version")) {
    version = line.slice("@version".length).trim()
    continue
  }
  if (line.startsWith("@implicitweights")) {
    const match = /^@implicitweights\s+([0-9A-F]+)\.\.([0-9A-F]+);\s*([0-9A-F]+)$/.exec(line)
    if (!match) throw new Error(`unknown implicit weights line: ${line}`)
    implicit.push([parseInt(match[1]!, 16), parseInt(match[2]!, 16), parseInt(match[3]!, 16)])
    continue
  }
  if (line.startsWith("@")) throw new Error(`unknown directive: ${line}`)
  const [left, right] = line.split(";")
  const cps = left!.trim().split(/\s+/).map((cp) => parseInt(cp, 16))
  const ces = [...right!.matchAll(/\[([.*])([0-9A-F]{4})\.([0-9A-F]{4})\.([0-9A-F]{4})\]/g)].map((m) => {
    const [p, s, t] = [parseInt(m[2]!, 16), parseInt(m[3]!, 16), parseInt(m[4]!, 16)]
    if (p > 0xffff || s > 0x1ff || t > 0x1f) throw new Error(`weight out of range: ${line}`)
    return ((p << 14) | (s << 5) | t) >>> 0
  })
  if (ces.length === 0 || ces.length > 255) throw new Error(`unexpected element count: ${line}`)
  if (cps.length === 1) singles.set(cps[0]!, ces)
  else if (cps.length <= 3) contractions.set(cps.join(" "), { cps, ces })
  else throw new Error(`contraction longer than 3: ${line}`)
}
if (version !== "13.0.0") console.warn(`note: allkeys.txt version ${version} (generated from 13.0.0 so far)`)

const elements: number[] = []
const words: number[] = []
const push = (ces: number[]) => {
  const start = elements.length
  elements.push(...ces)
  return ((start << 8) | ces.length) >>> 0
}
const singleWords = [...singles.entries()].toSorted((a, b) => a[0] - b[0]).flatMap(([cp, ces]) => [cp, push(ces)])
const contractionWords = [...contractions.values()]
  .toSorted((a, b) => a.cps[0]! - b.cps[0]! || (a.cps[1] ?? 0) - (b.cps[1] ?? 0) || (a.cps[2] ?? 0) - (b.cps[2] ?? 0))
  .flatMap((c) => [c.cps[0]!, c.cps[1]!, c.cps[2] ?? 0, push(c.ces)])
if (elements.length >= 1 << 24) throw new Error("too many elements")

words.push(0x31414355, singles.size, contractions.size, elements.length, implicit.length)
words.push(...singleWords, ...contractionWords, ...elements, ...implicit.flat())
const bytes = Buffer.alloc(words.length * 4)
words.forEach((word, index) => bytes.writeUInt32LE(word >>> 0, index * 4))

if (flag === "--check") {
  if (!readFileSync(output).equals(bytes)) throw new Error(`${output} is out of date`)
} else {
  writeFileSync(output, bytes)
  console.log(`wrote ${output}: ${singles.size} code points, ${contractions.size} contractions, ${elements.length} elements`)
}
