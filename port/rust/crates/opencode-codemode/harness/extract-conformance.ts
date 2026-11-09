// Extracts every `execute` program from port/conformance/scenarios/codemode-*.test.ts as corpus cases (JSON lines on
// stdout). Each scenario file is loaded from a scratch copy whose `../lib/*` modules are stubs that record the
// scripted turns, so the programs come out exactly as the scenarios build them. The `tools` tree is a stand-in for
// the conformance inventory (the opencode namespace tools) with fixed echo behavior: what matters here is that both
// interpreters see the same tools.
//
// usage: bun harness/extract-conformance.ts > corpus/conformance.jsonl
import { cpSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import path from "node:path"

const here = path.dirname(new URL(import.meta.url).pathname)
const scenarios = path.resolve(here, "../../../../conformance/scenarios")
const scratch = mkdtempSync(path.join(process.env.TMPDIR ?? tmpdir(), "codemode-extract-"))

const recorded: Array<{ scenario: string; codes: Array<string> }> = []
;(globalThis as Record<string, unknown>).__codemodeRecorded = recorded

const stubs: Record<string, string> = {
  "openrouter.ts": `
export const say = (text, usage) => ({ events: [{ text }], usage })
export const call = (tool, input, text) => ({ events: [...(text ? [{ text }] : []), { tool, input }], finish: "tool_calls" })
`,
  "scenario.ts": `
const codes = (value, out) => {
  if (Array.isArray(value)) { for (const item of value) codes(item, out); return out }
  if (value && typeof value === "object") {
    if (value.tool === "execute" && value.input && typeof value.input.code === "string") out.push(value.input.code)
    for (const item of Object.values(value)) codes(item, out)
  }
  return out
}
export const scenario = (name, options) => {
  globalThis.__codemodeRecorded.push({ scenario: name, codes: codes(options?.turns ?? [], []) })
}
`,
  "model.ts": `
export const MODEL_ID = "deepseek/deepseek-v4.1-flash-1017"
export const baseConfig = new Proxy({}, { get: () => [] })
`,
}

try {
  mkdirSync(path.join(scratch, "lib"))
  mkdirSync(path.join(scratch, "scenarios"))
  for (const [name, source] of Object.entries(stubs)) writeFileSync(path.join(scratch, "lib", name), source)
  const files = readdirSync(scenarios)
    .filter((file) => file.startsWith("codemode-") && file.endsWith(".test.ts"))
    .sort()
  for (const file of files) {
    cpSync(path.join(scenarios, file), path.join(scratch, "scenarios", file))
    const before = recorded.length
    await import(path.join(scratch, "scenarios", file))
    for (const entry of recorded.slice(before)) (entry as { file?: string }).file = file
  }
} finally {
  rmSync(scratch, { recursive: true, force: true })
}

const object = (properties: Record<string, unknown> = {}) => ({ type: "object", properties })
const tools = [
  { path: "opencode.models", description: "Search the models available to use.", input: object({ query: { type: "string" }, provider: { type: "string" }, limit: { type: "integer" } }), output: object({ providers: { type: "array" } }) },
  { path: "opencode.session_move", description: "Move a session to another directory.", input: object({ sessionID: { type: "string" }, directory: { type: "string" } }), output: object() },
  { path: "opencode.session_rename", description: "Rename a session.", input: object({ sessionID: { type: "string" }, title: { type: "string" } }), output: object() },
  { path: "opencode.worktree_create", description: "Create a named worktree.", input: object({ name: { type: "string" } }), output: object() },
  { path: "opencode.worktree_list", description: "List a repository's worktrees.", input: object(), output: object({ worktrees: { type: "array" } }) },
  { path: "opencode.worktree_remove", description: "Remove a worktree.", input: object({ directory: { type: "string" } }), output: object() },
]

for (const entry of recorded) {
  entry.codes.forEach((code, index) => {
    const name = `conformance/${(entry as { file?: string }).file}/${entry.scenario}#${index}`
    process.stdout.write(JSON.stringify({ name, code, tools }) + "\n")
  })
}
