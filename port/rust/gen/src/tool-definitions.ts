// Records the provider definitions (`{description, parameters}`, as packages/ai/src/tool.ts's toJsonSchema builds
// them) of the built-in tools no conformance scenario shows a model: `patch` (GPT models only) and `todowrite` (off by
// flag). The other definitions under crates/opencode/src/tool/definitions/ were recorded from the scenarios' requests.
//
// usage: bun src/tool-definitions.ts            write the definitions
//        bun src/tool-definitions.ts --check    fail if the checked-in definitions differ
import { readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { Schema } from "effect"

const reference = process.env.REFERENCE ?? join(import.meta.dir, "../../../.reference")
const core = join(reference, "packages/core/src")
const { PatchTool } = await import(join(core, "tool/plugin/patch.ts"))
const { TodoTool } = await import(join(core, "tool/plugin/todo.ts"))
const toJsonSchema = (schema: Schema.Top) => {
  const document = Schema.toJsonSchemaDocument(schema, { onExcessProperty: "error" })
  if (Object.keys(document.definitions).length === 0) return document.schema
  return { ...document.schema, $defs: document.definitions }
}
const tools = [
  ["patch", PatchTool.Input, "tool/patch.txt"],
  ["todowrite", TodoTool.Input, "tool/todowrite.txt"],
] as const
const dir = join(import.meta.dir, "../../crates/opencode/src/tool/definitions")
let stale = false
for (const [name, input, description] of tools) {
  const text =
    JSON.stringify({ description: readFileSync(join(core, description), "utf8"), parameters: toJsonSchema(input) }) + "\n"
  const file = join(dir, `${name}.json`)
  if (!process.argv.includes("--check")) writeFileSync(file, text)
  else if (readFileSync(file, "utf8") !== text) stale = true
}
if (stale) {
  console.error("tool definitions are stale: run port/scripts/gen-types.sh")
  process.exit(1)
}
console.log(`tool definitions ${process.argv.includes("--check") ? "are up to date" : "recorded"}: patch, todowrite`)
