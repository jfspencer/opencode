// Structural differences between results written by CONFORMANCE_ACTUAL=<dir> and the goldens.
// usage: bun diff.ts <dir> [--limit N]   (prints one line per differing JSON path)
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs"
import path from "node:path"
import { expand } from "./lib/golden"

const dir = process.argv[2]
const limit = Number(process.argv[process.argv.indexOf("--limit") + 1]) || 12
if (!dir) throw new Error("usage: bun diff.ts <dir> [--limit N]")
const golden = path.join(import.meta.dir, "goldens")

const short = (value: unknown) => {
  const text = value === undefined ? "(absent)" : JSON.stringify(value)
  return text.length > 160 ? text.slice(0, 157) + "..." : text
}

function diff(a: unknown, b: unknown, at: string, out: string[]) {
  if (Bun.deepEquals(a, b)) return
  if (a && b && typeof a === "object" && typeof b === "object" && Array.isArray(a) === Array.isArray(b)) {
    const keys = new Set([...Object.keys(a as object), ...Object.keys(b as object)])
    for (const key of keys) diff((a as any)[key], (b as any)[key], `${at}.${key}`, out)
    return
  }
  out.push(`  ${at}: expected ${short(a)} got ${short(b)}`)
}

const walk = (current: string): string[] =>
  readdirSync(current).flatMap((entry) => {
    const full = path.join(current, entry)
    return statSync(full).isDirectory() ? walk(full) : [full]
  })

if (!existsSync(dir)) process.exit(0)
for (const file of walk(dir).sort()) {
  const rel = path.relative(dir, file)
  const expected = expand(JSON.parse(readFileSync(path.join(golden, rel), "utf8")))
  const actual = JSON.parse(readFileSync(file, "utf8"))
  const out: string[] = []
  diff(expected, actual, "", out)
  console.log(`${rel} (${out.length})`)
  for (const line of out.slice(0, limit)) console.log(line)
}
