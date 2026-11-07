// Golden blob maintenance.
//   bun lib/blobs.ts compact   rewrite every golden with large values moved to goldens/_blobs (idempotent)
//   bun lib/blobs.ts gc        delete blobs no golden references
import { readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs"
import path from "node:path"
import { BLOBS, blobPath, compact, expand } from "./golden"

const ROOT = path.dirname(BLOBS)

function goldens(dir = ROOT): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = path.join(dir, entry)
    if (full === BLOBS) return []
    if (statSync(full).isDirectory()) return goldens(full)
    return entry.endsWith(".json") ? [full] : []
  })
}

function references(value: unknown, out = new Set<string>()): Set<string> {
  if (Array.isArray(value)) value.forEach((item) => references(item, out))
  else if (value && typeof value === "object") {
    const record = value as Record<string, unknown>
    if (typeof record["<blob>"] === "string" && Object.keys(record).length === 1) {
      out.add(record["<blob>"] as string)
      references(JSON.parse(readFileSync(blobPath(record["<blob>"] as string), "utf8")), out)
    } else Object.values(record).forEach((item) => references(item, out))
  }
  return out
}

const command = process.argv[2]
if (command === "compact") {
  const files = goldens()
  for (const file of files) writeFileSync(file, JSON.stringify(compact(expand(JSON.parse(readFileSync(file, "utf8")))), null, 2) + "\n")
  console.log(`compacted ${files.length} goldens`)
} else if (command === "gc") {
  const used = new Set<string>()
  for (const file of goldens()) references(JSON.parse(readFileSync(file, "utf8")), used)
  let removed = 0
  for (const shard of readdirSync(BLOBS))
    for (const blob of readdirSync(path.join(BLOBS, shard))) {
      if (used.has(blob.replace(/\.json$/, ""))) continue
      rmSync(path.join(BLOBS, shard, blob))
      removed++
    }
  console.log(`kept ${used.size} blobs, removed ${removed}`)
} else {
  console.error("usage: bun lib/blobs.ts compact|gc")
  process.exit(1)
}
