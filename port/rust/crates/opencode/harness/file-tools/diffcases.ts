// Generates random before/after texts and jsdiff 8.0.4's fileDiff output for them.
// The reference's own `diff` (8.0.4), as packages/core resolves it.
const { createTwoFilesPatch, diffLines } = await import(
  new URL("../../../../../.reference/packages/core/node_modules/diff/libesm/index.js", import.meta.url).href
)

let seed = Number(process.argv[2] ?? 1)
const random = () => {
  seed = (seed * 1103515245 + 12345) % 2147483648
  return seed / 2147483648
}
const pick = <T>(items: T[]) => items[Math.floor(random() * items.length)]
const words = ["a", "b", "c", "x", "y", "", "  indented", "\tz", "é", "line", "}", "{"]
const text = () => {
  const count = Math.floor(random() * 14)
  const lines = Array.from({ length: count }, () => pick(words))
  const endings = () => pick(["\n", "\n", "\n", "\r\n"])
  let out = lines.map((line) => line + endings()).join("")
  if (random() < 0.3) out = out.replace(/\r?\n$/, "")
  return out
}
const mutate = (input: string) => {
  const lines = input.split("\n")
  const ops = Math.floor(random() * 5)
  for (let i = 0; i < ops; i++) {
    const at = Math.floor(random() * (lines.length + 1))
    const op = random()
    if (op < 0.33) lines.splice(at, 0, pick(words))
    else if (op < 0.66) lines.splice(at, 1)
    else lines[at] = pick(words)
  }
  return lines.join("\n")
}
const cases = []
for (let i = 0; i < Number(process.argv[3] ?? 2000); i++) {
  const before = text()
  const after = random() < 0.2 ? text() : mutate(before)
  const counts = diffLines(before, after).reduce(
    (r, item) => ({
      additions: r.additions + (item.added ? (item.count ?? 0) : 0),
      deletions: r.deletions + (item.removed ? (item.count ?? 0) : 0),
    }),
    { additions: 0, deletions: 0 },
  )
  cases.push({ before, after, patch: createTwoFilesPatch("f.txt", "f.txt", before, after), ...counts })
}
console.log(JSON.stringify(cases))
