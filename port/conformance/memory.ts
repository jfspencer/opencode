// Summarize the server memory log written by process runs (CONFORMANCE_MEMORY_LOG, see lib/server.ts).
// usage: bun memory.ts <log.jsonl> [<other.jsonl>...]
//   One block per implementation: peak resident memory of the server process per scenario (median, p90,
//   max) and the heaviest scenarios. With two implementations, also the per-scenario ratio. Server and TUI
//   processes (lib/tui.ts) are separate blocks.
import { readFileSync } from "node:fs"

type Row = { impl: string; role?: string; file: string; scenario: string; peakKiB: number; rssKiB: number }

const rows: Row[] = process.argv
  .slice(2)
  .flatMap((file) => readFileSync(file, "utf8").split("\n").filter(Boolean))
  .map((line) => JSON.parse(line))
  // One block per implementation and process role (server, tui).
  .map((row: Row) => ({ ...row, impl: `${row.impl} ${row.role ?? "server"}` }))
if (rows.length === 0) throw new Error("usage: bun memory.ts <log.jsonl>...")

const mib = (kib: number) => `${(kib / 1024).toFixed(1)} MiB`
const quantile = (values: number[], q: number) => {
  const sorted = [...values].sort((a, b) => a - b)
  return sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))]
}

const byImpl = Map.groupBy(rows, (row) => row.impl)
for (const [impl, list] of byImpl) {
  const peaks = list.map((row) => row.peakKiB)
  console.log(
    `${impl}: ${list.length} scenarios, peak median ${mib(quantile(peaks, 0.5))}, p90 ${mib(quantile(peaks, 0.9))}, max ${mib(Math.max(...peaks))}`,
  )
  for (const row of [...list].sort((a, b) => b.peakKiB - a.peakKiB).slice(0, 5))
    console.log(`  ${mib(row.peakKiB).padStart(10)}  ${row.file} › ${row.scenario}`)
}

// Two implementations of the same role (e.g. "ts server" and "opencode server"): the per-scenario ratio.
const roles = Map.groupBy([...byImpl.keys()], (impl) => impl.split(" ").at(-1)!)
for (const [, impls] of roles) {
  if (impls.length !== 2) continue
  const [a, b] = impls
  const key = (row: Row) => `${row.file}\u0000${row.scenario}`
  const other = new Map(byImpl.get(b)!.map((row) => [key(row), row.peakKiB]))
  const ratios = byImpl
    .get(a)!
    .filter((row) => other.has(key(row)))
    .map((row) => other.get(key(row))! / row.peakKiB)
  if (ratios.length)
    console.log(
      `${b}/${a} peak ratio over ${ratios.length} shared scenarios: median ${quantile(ratios, 0.5).toFixed(2)}`,
    )
}
