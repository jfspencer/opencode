// Turns the recorder's raw logs (one per TS TUI process) into data cases: the recorded directory and first status
// configure the case, the rest are its steps.
//
// usage: bun harness/record/cases.ts <raw dir>   (prints JSONL)
import { readdirSync, readFileSync } from "node:fs"
import path from "node:path"

const raw = process.argv[2]
for (const file of readdirSync(raw).filter((f) => f.endsWith(".jsonl")).sort()) {
  const steps = readFileSync(path.join(raw, file), "utf8")
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line) as Record<string, unknown> & { op: string })
  const init = steps.findIndex((step) => step.op === "init")
  if (init < 0) continue
  const body = steps.slice(init + 1)
  // The status the data layer starts from is its first recorded one.
  const first = body.findIndex((step) => step.op === "status")
  const status = first >= 0 ? (body[first].value as string) : "connecting"
  if (first >= 0) body.splice(first, 1)
  // Time before the data layer existed does not matter; keep the gaps after it.
  while (body[0]?.op === "advance") body.shift()
  if (!body.some((step) => step.op === "events")) continue
  process.stdout.write(
    JSON.stringify({ kind: "data", name: `recorded ${file.replace(/\.jsonl$/, "")}`, directory: steps[init].directory, status, steps: body }) + "\n",
  )
}
