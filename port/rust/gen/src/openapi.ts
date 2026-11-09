// Records the server's OpenAPI document (`GET /openapi.json`: HttpApiBuilder's `openapiPath`, which answers
// `OpenApi.fromApi(Api)` as compact JSON) into crates/opencode/src/generated/openapi.json, so the Rust server serves
// the same bytes. The document is built from the server's own `Api` (its location middlewares declare errors that
// every located endpoint documents), loaded from the TS reference.
//
// usage: bun src/openapi.ts            write the document
//        bun src/openapi.ts --check    fail if the checked-in document differs from a fresh one
import { readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { OpenApi } from "effect/http-api"

const reference = process.env.REFERENCE ?? join(import.meta.dir, "../../../.reference")
const { Api } = await import(join(reference, "packages/server/src/api.ts"))
const text = JSON.stringify(OpenApi.fromApi(Api))
const file = join(import.meta.dir, "../../crates/opencode/src/generated/openapi.json")

if (process.argv.includes("--check")) {
  let current = ""
  try {
    current = readFileSync(file, "utf8")
  } catch {}
  if (current !== text) {
    console.error("the OpenAPI document is stale: run port/scripts/gen-types.sh")
    process.exit(1)
  }
  console.log(`the OpenAPI document is up to date (${text.length} bytes)`)
} else {
  writeFileSync(file, text)
  console.log(`OpenAPI document: ${text.length} bytes`)
}
