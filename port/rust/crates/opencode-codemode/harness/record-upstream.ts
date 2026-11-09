// Extracts corpus cases from the upstream codemode test suite (port/.reference/packages/codemode/test): the suite
// runs from a scratch copy whose `lib/codemode.ts` records every execution (code, bindings, limits, and whether it
// had tools, extensions, or hooks). Executions without tools, extensions, or hooks become corpus cases as they are;
// the rest are counted and skipped (they need host functions the corpus cannot describe).
//
// usage: bun harness/record-upstream.ts > corpus/upstream.jsonl
import { cpSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import path from "node:path"

const here = path.dirname(new URL(import.meta.url).pathname)
const pkg = path.resolve(here, "../../../../.reference/packages/codemode")
const scratch = mkdtempSync(path.join(process.env.TMPDIR ?? tmpdir(), "codemode-upstream-"))
const log = path.join(scratch, "recorded.jsonl")

const recorder = `
import { appendFileSync } from "node:fs"
import * as Real from "${pkg}/src/codemode.ts"
import { Effect } from "effect"
export * from "${pkg}/src/codemode.ts"

export const engine = "legacy"

const count = (tools, depth = 0) => {
  if (!tools || typeof tools !== "object" || depth > 8) return 0
  if (tools._tag === "CodeModeTool") return 1
  if (tools._tag === "CodeModeNamespace") return count(tools.tools, depth + 1)
  return Object.values(tools).reduce((n, child) => n + count(child, depth + 1), 0)
}

export const make = (options = {}) => {
  const runtime = Real.make({ ...options, engine: "legacy" })
  const tools = count(options.tools)
  const extensions = (options.extensions ?? []).length
  const hooks = Object.keys(options.hooks ?? {}).length
  return {
    get catalog() {
      return runtime.catalog
    },
    execute: (code, bindings) =>
      Effect.map(runtime.execute(code, bindings), (result) => {
        try {
          appendFileSync(${JSON.stringify(log)}, JSON.stringify({ test: process.env.CODEMODE_TEST_FILE, code, bindings, limits: options.limits, tools, extensions, hooks, kind: result.ok ? "ok" : result.error.kind }) + "\\n")
        } catch {}
        return result
      }),
  }
}

export const execute = (options) => make(options).execute(options.code, options.bindings)
`

let recorded: Array<Record<string, unknown>> = []
try {
  cpSync(path.join(pkg, "test"), path.join(scratch, "test"), { recursive: true })
  symlinkSync(path.join(pkg, "node_modules"), path.join(scratch, "node_modules"))
  writeFileSync(path.join(scratch, "test/lib/codemode.ts"), recorder)
  writeFileSync(path.join(scratch, "test/preload.ts"), "")
  writeFileSync(path.join(scratch, "bunfig.toml"), '[test]\npreload = ["./test/preload.ts"]\n')
  // Direct imports of the package's sources point at the reference.
  for (const dir of ["test", "test/lib"]) {
    for (const file of readdirSync(path.join(scratch, dir)).filter((f) => f.endsWith(".ts"))) {
      const target = path.join(scratch, dir, file)
      const source = readFileSync(target, "utf8")
        .replaceAll('"../../src/', `"${pkg}/src/`)
        .replaceAll('"../src/', `"${pkg}/src/`)
      writeFileSync(target, source)
    }
  }
  const files = readdirSync(path.join(scratch, "test"))
    .filter((f) => f.endsWith(".test.ts") && !["test262.test.ts", "engines.test.ts", "openapi.test.ts"].includes(f) && (!process.env.RECORD_ONLY || f === process.env.RECORD_ONLY))
    .sort()
  for (const file of files) {
    const proc = Bun.spawnSync(["bun", "test", `./test/${file}`], {
      cwd: scratch,
      env: { ...process.env, CODEMODE_TEST_FILE: file, CODEMODE_ENGINE: "legacy" },
      stdout: "ignore",
      stderr: process.env.RECORD_DEBUG ? "inherit" : "ignore",
    })
    void proc
  }
  recorded = readFileSync(log, "utf8")
    .split("\n")
    .filter((line) => line !== "")
    .map((line) => JSON.parse(line))
} finally {
  rmSync(scratch, { recursive: true, force: true })
}

const seen = new Set<string>()
let skipped = 0
let index = 0
for (const entry of recorded) {
  const replayable = entry.tools === 0 && entry.extensions === 0 && entry.hooks === 0 && entry.kind !== "TimeoutExceeded"
  if (!replayable) {
    skipped++
    continue
  }
  const key = JSON.stringify([entry.code, entry.bindings, entry.limits])
  if (seen.has(key)) continue
  seen.add(key)
  const name = `upstream/${entry.test}#${index++}`
  process.stdout.write(
    JSON.stringify({
      name,
      code: entry.code,
      ...(entry.bindings ? { bindings: entry.bindings } : {}),
      ...(entry.limits ? { limits: entry.limits } : {}),
    }) + "\n",
  )
}
console.error(`${recorded.length} executions recorded, ${seen.size} distinct replayable cases, ${skipped} skipped (tools, extensions, hooks, or timeouts)`)
