// Generates patch texts (well-formed ones, their mutations, and the edge cases of each parser branch) and files to
// apply their updates to, and records what the reference's `Patch.parse` and `Patch.derive` make of them.
//
// usage: bun patchcases.ts <reference packages dir> <seed> <count>
const [packages, seedArg, countArg] = process.argv.slice(2)
const { Patch } = await import(`${packages}/util/src/patch.ts`)

let seed = Number(seedArg ?? 1)
const random = () => {
  seed = (seed * 1103515245 + 12345) % 2147483648
  return seed / 2147483648
}
const pick = <T>(items: readonly T[]) => items[Math.floor(random() * items.length)]
const chance = (p: number) => random() < p

const words = ["a", "b", "c", "x", "y", "", "  indented", "\tz", "é", "line", "}", "{", "a  ", "“q”", "x—y", "a b", " b"]
const paths = ["a.txt", "src/b.ts", " spaced.md ", "dir/c", "a.txt"]
const fileText = () => {
  const count = Math.floor(random() * 10)
  const lines = Array.from({ length: count }, () => pick(words))
  let out = lines.join("\n")
  if (chance(0.7)) out += "\n"
  if (chance(0.1)) out = "﻿" + out
  if (chance(0.1)) out = out.replaceAll("\n", "\r\n")
  return out
}

/** One update chunk drawn from a file's lines: context, removals, additions. */
const chunkFor = (lines: string[]) => {
  const out: string[] = []
  if (chance(0.3) && lines.length) out.push(`@@ ${chance(0.8) ? pick(lines) : "missing context"}`)
  else if (chance(0.5)) out.push("@@")
  const start = Math.floor(random() * (lines.length + 1))
  const span = Math.floor(random() * 3)
  for (const line of lines.slice(start, start + span)) {
    const kind = random()
    const shown = chance(0.15) ? line.trim() + (chance(0.5) ? " " : "") : line
    if (kind < 0.4) out.push(` ${shown}`)
    else if (kind < 0.8) out.push(`-${shown}`)
    else out.push(`+${pick(words)}`)
  }
  for (let i = Math.floor(random() * 3); i > 0; i--) out.push(`+${pick(words)}`)
  if (chance(0.1)) out.push("")
  if (chance(0.15)) out.push("*** End of File")
  return out
}

const hunk = (file: string) => {
  const kind = random()
  const path = pick(paths)
  if (kind < 0.25) {
    const lines = [`*** Add File: ${path}`]
    for (let i = Math.floor(random() * 4); i > 0; i--) lines.push(`+${pick(words)}`)
    return lines
  }
  if (kind < 0.4) return [`*** Delete File: ${path}`]
  const lines = [`*** Update File: ${path}`]
  if (chance(0.15)) lines.push(chance(0.8) ? `*** Move to: ${pick(paths)}` : "*** Move to:")
  const source = file.replace(/^﻿+/, "").split("\n")
  for (let i = 1 + Math.floor(random() * 2); i > 0; i--) lines.push(...chunkFor(source))
  return lines
}

const mutate = (lines: string[]) => {
  const out = [...lines]
  for (let i = Math.floor(random() * 3); i > 0; i--) {
    const at = Math.floor(random() * (out.length + 1))
    const op = random()
    if (op < 0.25) out.splice(at, 1)
    else if (op < 0.5)
      out.splice(at, 0, pick(["", "garbage", "*** Bogus", "@@", "*** End of File", " ctx", "+", "-", "*** End Patch", "*** Environment ID: x"]))
    else if (op < 0.75 && out[at] !== undefined) out[at] = out[at] + pick([" ", "\r", "\t"])
    else if (out[at] !== undefined) out[at] = "  " + out[at]
  }
  return out
}

const wrap = (body: string[]) => {
  let text = ["*** Begin Patch", ...(chance(0.05) ? ["*** Environment ID: env"] : []), ...body, "*** End Patch"].join("\n")
  if (chance(0.1)) text = text.replaceAll("\n", "\r\n")
  if (chance(0.05)) text = `cat <<'EOF'\n${text}\nEOF`
  else if (chance(0.05)) text = `<<EOF  \n\n${text}\nEOF \n`
  if (chance(0.05)) text = `  \n${text}\n\n`
  return text
}

const fixed = [
  "",
  "*** Begin Patch",
  "*** Begin Patch\n*** End Patch",
  "*** End Patch",
  "*** Begin Patch\n*** Delete File: a.txt\n*** End Patch",
  "*** Begin Patch\n*** Delete File: a.txt\nbody\n*** End Patch",
  "*** Begin Patch\n*** Delete File: a.txt\n*** Other\n*** End Patch",
  "*** Begin Patch\n*** Update File: a.txt\n*** End Patch",
  "*** Begin Patch\n*** Update File: a.txt\n@@\n*** End Patch",
  "*** Begin Patch\n*** Update File: a.txt\n@@\n@@\n x\n*** End Patch",
  "*** Begin Patch\n*** Update File: a.txt\n*** End of File\n*** Move to: b.txt\n x\n*** End Patch",
  "*** Begin Patch\n*** Update File: a.txt\n x\n*** End of File\n\n x\n*** End Patch",
  "*** Begin Patch\n*** Update File: a.txt\n x\n*** End of File\n@@ y\n+z\n*** End Patch",
  "*** Begin Patch\n*** Add File: a.txt\nno plus\n*** End Patch",
  "*** Begin Patch\n*** Environment ID:\n*** Add File: a\n*** End Patch",
  "cat <<EOF\n*** Begin Patch\n*** Add File: a\n+x\n*** End Patch\nEOF",
  '<<"END"\n*** Begin Patch\n*** Add File: a\n+x\n*** End Patch\nEND',
]

const cases = []
const total = Number(countArg ?? 5000)
for (let i = 0; i < total; i++) {
  const files = Array.from({ length: 3 }, fileText)
  let patch: string
  if (i < fixed.length) patch = fixed[i]
  else {
    const body = Array.from({ length: 1 + Math.floor(random() * 3) }, (_, n) => hunk(files[n % files.length])).flat()
    patch = wrap(chance(0.3) ? mutate(body) : body)
  }
  const parsed = Patch.parse(patch)
  const record: Record<string, unknown> = { patch }
  // Effect's `Result`: `{_tag: "Failure", failure}` or `{_tag: "Success", success}`.
  if (parsed._tag === "Failure") {
    record.parsed = { error: parsed.failure.message }
    cases.push(record)
    continue
  }
  record.parsed = { hunks: parsed.success }
  record.files = parsed.success.flatMap((hunk: { type: string; chunks: unknown }, index: number) => {
    if (hunk.type !== "update") return []
    return files.map((original) => {
      let derived
      try {
        derived = Patch.derive("f.txt", hunk.chunks, original)
      } catch (error) {
        derived = { error: (error as Error).message }
      }
      return { hunk: index, original, derived }
    })
  })
  cases.push(record)
}
console.log(JSON.stringify(cases))
