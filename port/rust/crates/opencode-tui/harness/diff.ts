// Differential check of opencode-tui's theme and config modules: builds the cases, runs them on the TS reference
// (theme.ts, config.ts) and on the Rust driver (examples/tui-driver.rs), and reports every difference.
//
// usage: bun harness/diff.ts --driver <tui-driver> [--count N] [--seed S] [--show N] [--only theme|config]
import { readFileSync, readdirSync } from "node:fs"
import path from "node:path"

const args = process.argv.slice(2)
const flag = (name: string, fallback?: string) => {
  const i = args.indexOf(name)
  return i >= 0 ? args[i + 1] : fallback
}
const driver = flag("--driver")!
const count = Number(flag("--count", "400"))
const show = Number(flag("--show", "5"))
const only = flag("--only")
let seed = Number(flag("--seed", "1"))

// A small deterministic PRNG (mulberry32), so a run is reproducible from its seed.
function random() {
  seed |= 0
  seed = (seed + 0x6d2b79f5) | 0
  let t = Math.imul(seed ^ (seed >>> 15), 1 | seed)
  t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296
}
const pick = <T,>(items: readonly T[]): T => items[Math.floor(random() * items.length)]!
const int = (n: number) => Math.floor(random() * n)

const reference = process.env.OPENCODE_TUI_REFERENCE!
const assets = path.join(reference, "packages/tui/src/theme/assets")

type Entry = { group: "theme" | "config"; case: unknown }
const cases: Entry[] = []

const { runTheme, builtinNames } = await import("./theme.ts")
const { runConfig, configCases } = only === "theme" ? { runConfig: undefined, configCases: undefined } : await import("./config.ts")

// --- theme cases -----------------------------------------------------------------------------------------------
if (only !== "config") {
  for (const name of builtinNames())
    for (const newDesigns of [false, true])
      for (const mode of ["dark", "light"] as const)
        // opencode2 is only offered while the experiment is on.
        if (name !== "opencode2" || newDesigns) cases.push({ group: "theme", case: { kind: "builtin", name, newDesigns, mode } })

  const v1Files = readdirSync(assets).filter((f) => f.endsWith(".json"))
  const v1 = v1Files.map((f) => JSON.parse(readFileSync(path.join(assets, f), "utf8")))
  const v2 = ["opencode.json", "opencode2.json"].map((f) => JSON.parse(readFileSync(path.join(assets, "v2", f), "utf8")))
  for (const input of v1) for (const newDesigns of [false, true]) cases.push({ group: "theme", case: { kind: "v1", input, newDesigns } })
  for (const input of v2) cases.push({ group: "theme", case: { kind: "document", input, name: "doc" } })

  const leaves = [
    "#fff", "#ffff", "#123456", "#12345678", "#ABCDEF", "#ggg", "red", "transparent", "none", "", "$", "$text.base",
    "$text.muted", "$text", "$hue.accent.200", "$hue.accent.250", "$hue.accent", "$hue.nope.200", "$background.base",
    "$nope", "$text.constructor", "$toString", "got x", "a\"b c", "$hue.neutral.900", 0, -0, 1.5, 1e21, 7, 231, 255, 300,
    null, true, false, [], [1, "x"], {}, { a: 1 }, { dark: "#000000", light: "#ffffff" }, { dark: "darkStep1" },
  ]
  const keys = ["base", "muted", "faint", "text", "background", "hue", "accent", "interactive", "neutral", "gray",
    "100", "200", "900", "1000", "categorical", "@dialog", "@footer", "$hovered", "$disabled", "hovered", "light", "dark",
    "theme", "defs", "primary", "syntax", "markdown", "comment", "", "constructor", "10", "x"]

  const clone = (value: unknown) => structuredClone(value)
  // A random path into an object tree, and a random mutation there.
  function mutate(value: any, depth = 0): any {
    if (value === null || typeof value !== "object" || depth > 6) return pick(leaves)
    if (Array.isArray(value)) {
      const copy = [...value]
      const roll = random()
      if (copy.length && roll < 0.6) copy[int(copy.length)] = mutate(copy[int(copy.length)], depth + 1)
      else if (roll < 0.8) copy.splice(int(copy.length + 1), 0, pick(leaves))
      else copy.splice(int(copy.length), 1)
      return copy
    }
    const entries = Object.keys(value)
    const roll = random()
    const copy = { ...value }
    if (entries.length && roll < 0.65) {
      const key = pick(entries)
      copy[key] = mutate(copy[key], depth + 1)
    } else if (entries.length && roll < 0.8) delete copy[pick(entries)]
    else copy[pick(keys)] = random() < 0.5 ? pick(leaves) : clone(pick(entries.length ? entries.map((k) => value[k]) : [{}]))
    return copy
  }
  for (let i = 0; i < count; i++) {
    const roll = random()
    if (roll < 0.45) {
      let input = clone(pick(v2))
      for (let n = 1 + int(3); n > 0; n--) input = mutate(input)
      cases.push({ group: "theme", case: { kind: "document", input, name: pick(["t", "my theme", "x"]) } })
    } else if (roll < 0.85) {
      let input = clone(pick(v1))
      for (let n = 1 + int(3); n > 0; n--) input = mutate(input)
      cases.push({ group: "theme", case: { kind: "v1", input, newDesigns: random() < 0.5 } })
    } else if (roll < 0.95) {
      const hex = () => "#" + Array.from({ length: 6 }, () => "0123456789abcdef"[int(16)]).join("")
      const palette = Array.from({ length: 16 }, () => (random() < 0.15 ? null : random() < 0.05 ? "" : hex()))
      const colors = {
        palette,
        defaultForeground: random() < 0.2 ? null : hex(),
        defaultBackground: random() < 0.2 ? null : pick([hex(), "#000000", "#ffffff", "#0a0a0a", "#fafafa"]),
      }
      cases.push({ group: "theme", case: { kind: "system", colors, mode: pick(["dark", "light"]), newDesigns: random() < 0.3 } })
    } else {
      const hex = () => "#" + Array.from({ length: pick([6, 8]) }, () => "0123456789abcdef"[int(16)]).join("")
      cases.push({ group: "theme", case: { kind: "tint", base: hex(), overlay: hex(), alpha: pick([0, 0.14, 0.22, 0.25, 0.5, 1, random()]) } })
    }
  }
}

// --- config cases ----------------------------------------------------------------------------------------------
if (only !== "theme" && configCases) for (const c of configCases(count, random)) cases.push({ group: "config", case: c })

// --- run both sides --------------------------------------------------------------------------------------------
const expected: unknown[] = []
for (const entry of cases) {
  const out = entry.group === "theme" ? runTheme(entry.case as any) : await runConfig!(entry.case as any)
  expected.push(JSON.parse(JSON.stringify(out ?? null)))
}

const proc = Bun.spawn([driver], { stdin: "pipe", stdout: "pipe", stderr: "inherit" })
for (const [id, entry] of cases.entries()) proc.stdin.write(JSON.stringify({ id, group: entry.group, case: entry.case }) + "\n")
proc.stdin.end()
const text = await new Response(proc.stdout).text()
const code = await proc.exited
const actual = new Map<number, unknown>()
for (const line of text.split("\n")) {
  if (!line) continue
  const { id, out } = JSON.parse(line)
  actual.set(id, out)
}

// JavaScriptCore's own error texts (a TypeError where the TS reads a missing token) are not ported: both sides'
// engine errors compare equal, as the conformance standard masks them.
const ENGINE = /^(undefined is not an object|null is not an object|.* is not an Object\.|Object\.entries requires)/
function normalize(value: any): any {
  if (value && typeof value === "object" && !Array.isArray(value) && typeof value.error === "string" && ENGINE.test(value.error))
    return { ...value, error: "<engine message>" }
  return value
}
for (let i = 0; i < expected.length; i++) {
  expected[i] = normalize(expected[i])
  if (actual.has(i)) actual.set(i, normalize(actual.get(i)))
}

// Deep equality, ignoring object key order.
function equal(a: any, b: any): boolean {
  if (a === b) return true
  if (typeof a !== typeof b || a === null || b === null || typeof a !== "object") return false
  if (Array.isArray(a) !== Array.isArray(b)) return false
  if (Array.isArray(a)) return a.length === b.length && a.every((x, i) => equal(x, b[i]))
  const ka = Object.keys(a).sort()
  const kb = Object.keys(b).sort()
  return equal(ka, kb) && ka.every((k) => equal(a[k], b[k]))
}
function firstDifference(a: any, b: any, at = ""): string {
  if (equal(a, b)) return ""
  if (a && b && typeof a === "object" && typeof b === "object" && Array.isArray(a) === Array.isArray(b)) {
    for (const key of new Set([...Object.keys(a), ...Object.keys(b)])) {
      const found = firstDifference(a[key], b[key], `${at}.${key}`)
      if (found) return found
    }
  }
  return `${at}: ts ${JSON.stringify(a)?.slice(0, 300)} rust ${JSON.stringify(b)?.slice(0, 300)}`
}

let differences = 0
const byKind = new Map<string, { total: number; diff: number }>()
for (const [id, entry] of cases.entries()) {
  const kind = `${entry.group}/${(entry.case as any).kind}`
  const stat = byKind.get(kind) ?? { total: 0, diff: 0 }
  stat.total++
  if (!equal(expected[id], actual.get(id))) {
    stat.diff++
    differences++
    if (differences <= show) {
      console.log(`--- case ${id} (${kind}): ${JSON.stringify(entry.case).slice(0, 400)}`)
      console.log("   " + firstDifference(expected[id], actual.get(id)))
    }
  }
  byKind.set(kind, stat)
}
for (const [kind, stat] of [...byKind].sort()) console.log(`${kind}: ${stat.total - stat.diff}/${stat.total}`)
console.log(`${cases.length} cases, ${differences} differences${code ? `, driver exit ${code}` : ""}`)
process.exit(differences || code ? 1 : 0)
