// Random edits through the pure matching functions of the reference's edit.ts (extracted by run.sh into edit-core.ts).
import { findOccurrences, matchView, missHint } from "./edit-core-mod.ts"
const fns = () => ({ findOccurrences, matchView, missHint })
let seed = Number(process.argv[2] ?? 1)
const random = () => {
  seed = (seed * 1103515245 + 12345) % 2147483648
  return seed / 2147483648
}
const pick = <T,>(items: T[]) => items[Math.floor(random() * items.length)]
const pieces = ["a", "b", "x = 1", "  return a + b", "‘q’", "“d”", "a—b", "a b", " ", "\t", "}", "é", "foo  ", "bar\t"]
const line = () => Array.from({ length: 1 + Math.floor(random() * 2) }, () => pick(pieces)).join("")
const text = (n: number) => {
  let out = ""
  for (let i = 0; i < n; i++) out += line() + pick(["\n", "\n", "\r\n"])
  return random() < 0.3 ? out.replace(/\r?\n$/, "") : out
}
const normalizeVariant = (s: string) =>
  s.replace(/'/g, () => pick(["'", "’"])).replace(/-/g, () => pick(["-", "—"])).replace(/ /g, () => pick([" ", " ", " "]))
const cases = []
for (let i = 0; i < Number(process.argv[3] ?? 2000); i++) {
  const source = text(1 + Math.floor(random() * 8))
  const lines = source.split(/\r?\n/)
  const start = Math.floor(random() * lines.length)
  const count = 1 + Math.floor(random() * 3)
  let old = lines.slice(start, start + count).join("\n")
  const mode = random()
  if (mode < 0.2) old = normalizeVariant(old)
  else if (mode < 0.35) old = old.split("\n").map((l) => l.trimEnd()).join("\n")
  else if (mode < 0.45) old = old + "\n"
  else if (mode < 0.55) old = line()
  else if (mode < 0.6) old = old.replace(/\n/g, "\r\n")
  if (old === "") old = "a"
  const replacement = random() < 0.5 ? text(1 + Math.floor(random() * 3)).replace(/\r?\n$/, "") : line()
  const replaceAll = random() < 0.3
  const { findOccurrences, matchView, missHint } = fns()
  const endingsOnly = old.replaceAll("\r", "") === replacement.replaceAll("\r", "")
  const { matches, replace } = endingsOnly
    ? { matches: findOccurrences(source, old), replace: () => replacement }
    : matchView(source, old, replacement)
  const replaced = (replaceAll ? matches : matches.slice(0, 1))
    .toReversed()
    .reduce((content: string, m: any) => `${content.slice(0, m.start)}${replace(m)}${content.slice(m.end)}`, source)
  cases.push({ source, old, new: replacement, replaceAll, count: matches.length, replaced, hint: matches.length === 0 ? (missHint(source, old, replacement) ?? null) : null })
}
console.log(JSON.stringify(cases))
