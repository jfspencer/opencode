// Generates random reactive scripts: bun gen.ts <out dir> <count> <seed>
//
// The language (one op per line; a line ending in "{" opens a block closed by "end"):
//   signal <name> <int>            createSignal
//   list <name> <ints...>          createSignal of an array (always a fresh array: every set notifies)
//   memo <name> <deps...> [{]      createMemo of the deps' sum, logging; its block runs on each run
//   effect|render|computed <name> <deps...> [{]   createEffect / createRenderEffect / createComputed, logging
//                                  the deps' values; its block runs on each run, where
//     write <signal> <offset>      sets the signal to the deps' sum + offset (untracked)
//   oneffect <name> defer|now <deps...>   createEffect(on(deps, ...))
//   mount <name> / cleanup <name>  onMount / onCleanup at the current owner
//   for <name> <list> [{]          createMemo(mapArray(list, row)), each row logging, with an index effect
//   actions                        the rest runs after the root is created, outside it:
//   set <signal> <int> / setlist <list> <ints...> / batch { ... } / read <signal or memo> / dispose
// Writes only go to signals after every signal the writer reads, so no script loops forever.
import { mkdirSync, writeFileSync } from "node:fs"
import path from "node:path"

const [out, countText, seedText] = process.argv.slice(2)
let seed = Number(seedText ?? 1) >>> 0
const rand = () => {
  seed = (seed * 1664525 + 1013904223) >>> 0
  return seed / 2 ** 32
}
const pick = <T>(items: T[]) => items[Math.floor(rand() * items.length)]
const int = (n: number) => Math.floor(rand() * n)

function program(): string {
  const lines: string[] = []
  const signalCount = 3 + int(5)
  const signals = Array.from({ length: signalCount }, (_, i) => `s${i}`)
  for (const s of signals) lines.push(`signal ${s} ${int(4)}`)
  const lists = rand() < 0.5 ? ["l0"] : []
  for (const l of lists) lines.push(`list ${l} ${shuffle([1, 2, 3, 4, 5]).slice(0, int(5)).join(" ")}`)
  const readable = [...signals]
  let n = 0
  readables = readable
  const computation = (depth: number, indent: string) => {
    const kind = pick(["memo", "effect", "render", "computed", "effect", "oneffect", "mount", "cleanup"])
    const name = `c${n++}`
    if (kind === "mount" || kind === "cleanup") return lines.push(`${indent}${kind} ${name}`)
    if (kind === "oneffect") {
      const deps = pickDeps(readable)
      return lines.push(`${indent}oneffect ${name} ${pick(["defer", "now"])} ${deps.join(" ")}`)
    }
    // A writer reads signals only, and writes to a later one.
    const writer = depth === 0 && kind !== "memo" && rand() < 0.4
    const deps = writer ? pickDeps(signals.slice(0, signalCount - 1)) : pickDeps(readable)
    const block = depth < 2 && rand() < 0.5
    lines.push(`${indent}${kind} ${name} ${deps.join(" ")}${block || writer ? " {" : ""}`)
    if (writer) {
      const top = Math.max(...deps.map((d) => Number(d.slice(1))))
      lines.push(`${indent}  write s${top + 1 + int(signalCount - top - 1)} ${int(3)}`)
    }
    if (block) for (let i = 0; i < 1 + int(3); i++) computation(depth + 1, indent + "  ")
    if (block || writer) lines.push(`${indent}end`)
    if (kind === "memo" && depth === 0) readable.push(name)
  }
  for (let i = 0; i < 2 + int(6); i++) computation(0, "")
  for (const l of lists) {
    lines.push(`for f0 ${l} {`)
    if (rand() < 0.5) lines.push(`  effect r0 ${pick(signals)}`)
    lines.push(`  cleanup r1`)
    lines.push(`end`)
  }
  lines.push("actions")
  const action = (indent: string, depth: number) => {
    const r = rand()
    if (r < 0.15 && depth === 0) {
      lines.push(`${indent}batch {`)
      for (let i = 0; i < 1 + int(3); i++) action(indent + "  ", depth + 1)
      lines.push(`${indent}end`)
    } else if (r < 0.4 && depth > 0) {
      lines.push(`${indent}read ${pick(readables)}`)
    } else if (r < 0.5 && lists.length) {
      lines.push(`${indent}setlist l0 ${shuffle([1, 2, 3, 4, 5, 6]).slice(0, int(6)).join(" ")}`)
    } else lines.push(`${indent}set ${pick(signals)} ${int(5)}`)
  }
  for (let i = 0; i < 2 + int(6); i++) action("", 0)
  if (rand() < 0.3) lines.push("dispose")
  return lines.join("\n") + "\n"
}

let readables: string[] = []

function pickDeps(from: string[]) {
  const deps = new Set<string>()
  for (let i = 0; i < 1 + int(3); i++) deps.add(pick(from))
  return [...deps]
}

function shuffle<T>(items: T[]) {
  const copy = [...items]
  for (let i = copy.length - 1; i > 0; i--) {
    const j = int(i + 1)
    ;[copy[i], copy[j]] = [copy[j], copy[i]]
  }
  return copy
}

mkdirSync(out, { recursive: true })
for (let i = 0; i < Number(countText); i++) writeFileSync(path.join(out, `${i}.txt`), program())
