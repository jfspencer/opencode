// Generates crates/opencode-protocol/src/generated/*.rs from the TS reference's schemas.
//
// usage: bun src/generate.ts            write the generated files
//        bun src/generate.ts --check    fail if the checked-in files differ from a fresh generation
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { emit } from "./emit.ts"
import { build } from "./model.ts"

const dir = join(import.meta.dir, "../../crates/opencode-protocol/src/generated")
const model = build()
const out = emit(model)

const notes = [
  ...model.notes.map(
    (n) =>
      `${n.kind === "custom" ? "not reproduced" : n.kind === "hook" ? "hand-written (wire/hook.rs)" : "not enforced"}: ${n.what} (${n.where})`,
  ),
  ...out.unenforced.map((u) => `not enforced: ${u}`),
]
const summary =
  `// ${model.defs.length} definitions, ${model.endpoints.length} endpoints, ${model.events.length} event definitions, ` +
  `${new Set(model.roots.map((r) => r.name)).size} root types.\n` +
  (notes.length ? `//\n// Known gaps (see port/README.md):\n${notes.map((n) => `// - ${n}\n`).join("")}` : "")

const files: Record<string, string> = {
  "types.rs": out.types.replace("\n#![allow", `${summary}\n#![allow`),
  "routes.rs": out.routes,
  "events.rs": out.events,
  "roots.rs": out.roots,
}

if (process.argv.includes("--check")) {
  const stale = Object.entries(files).filter(([name, text]) => {
    try {
      return readFileSync(join(dir, name), "utf8") !== text
    } catch {
      return true
    }
  })
  if (stale.length) {
    console.error(`generated code is stale: ${stale.map(([n]) => n).join(", ")}; run port/scripts/gen-types.sh`)
    process.exit(1)
  }
  console.log(`generated code is up to date (${summary.split("\n")[0].slice(3)})`)
} else {
  mkdirSync(dir, { recursive: true })
  for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, name), text)
  console.log(summary.trimEnd())
}
