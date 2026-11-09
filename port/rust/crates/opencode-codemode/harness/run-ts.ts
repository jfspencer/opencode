// The TS side of the differential check: runs every case of a corpus (JSON lines, see harness/README.md) through
// the reference interpreter (packages/codemode, default engine) and prints one JSON line per case:
// `{"name", "result", "output"}`, where `result` is the CodeMode.Result exactly as the tool layer receives it and
// `output` is the text core's `execute` tool hands to the model (core/src/codemode/tool.ts `formatResult`).
//
// usage: bun harness/run-ts.ts corpus.jsonl > ts.jsonl
import { CodeMode, Tool } from "../../../../.reference/packages/codemode/src/index.ts"
import { Effect, Schema } from "../../../../.reference/packages/codemode/node_modules/effect/dist/index.js"

type Behavior =
  | { kind: "echo" }
  | { kind: "value"; value: unknown }
  | { kind: "fail"; message: string }
  | { kind: "die"; message: string }
  | { kind: "sequence"; values: Array<{ ok: unknown } | { fail: string }> }

type ToolSpec = {
  path: string
  description?: string
  input?: unknown
  /** A JSON Schema, "nullOrString" for core's default `Schema.NullOr(Schema.String)`, or absent for `void`. */
  output?: unknown
  behavior?: Behavior
  /** "sync" completes inside the call; "yield" after one scheduler turn. */
  timing?: "sync" | "yield"
}

type Case = {
  name: string
  code: string
  tools?: Array<ToolSpec>
  bindings?: Record<string, unknown>
  limits?: { timeoutMs?: number; maxToolCalls?: number; maxOutputBytes?: number }
}

const behave = (spec: ToolSpec) => {
  let calls = 0
  const behavior = spec.behavior ?? { kind: "echo" }
  const outcome = (input: unknown): Effect.Effect<unknown, unknown> => {
    switch (behavior.kind) {
      case "echo":
        return Effect.succeed(input)
      case "value":
        return Effect.succeed(behavior.value)
      case "fail":
        return Effect.fail(new Error(behavior.message))
      case "die":
        return Effect.die(new Error(behavior.message))
      case "sequence": {
        const step = behavior.values[Math.min(calls++, behavior.values.length - 1)]
        if (step === undefined) return Effect.succeed(null)
        return "fail" in step ? Effect.fail(new Error(step.fail)) : Effect.succeed(step.ok)
      }
    }
  }
  return (input: unknown) =>
    spec.timing === "yield" ? Effect.andThen(Effect.yieldNow, Effect.suspend(() => outcome(input))) : outcome(input)
}

const buildTools = (specs: Array<ToolSpec>) => {
  const tools: Record<string, unknown> = {}
  for (const spec of specs) {
    tools[spec.path] = Tool.make({
      description: spec.description ?? "",
      input: (spec.input ?? { type: "object", properties: {} }) as never,
      output: (spec.output === "nullOrString" ? Schema.NullOr(Schema.String) : spec.output) as never,
      execute: behave(spec) as never,
    })
  }
  return tools
}

// core/src/codemode/tool.ts
function formatResult(result: CodeMode.Result) {
  const output = result.ok
    ? formatValue(result.value)
    : [result.error.message, ...(result.error.suggestions ?? []).filter((hint) => !result.error.message.includes(hint))]
        .join("\n")
        .trim()
  const warnings =
    result.ok && result.warnings && result.warnings.length > 0
      ? `Warnings:\n${result.warnings.map((item) => `- [${item.kind}] ${item.message}`).join("\n")}`
      : undefined
  const logs = result.logs && result.logs.length > 0 ? `Logs:\n${result.logs.join("\n")}` : undefined
  return [output, warnings, logs].filter((part) => part !== undefined && part !== "").join("\n\n")
}

function formatValue(value: CodeMode.DataValue) {
  if (typeof value === "string") return value
  return JSON.stringify(value, null, 2) ?? String(value)
}

const path = process.argv[2]
if (!path) throw new Error("usage: bun harness/run-ts.ts corpus.jsonl")
const lines = (await Bun.file(path).text()).split("\n").filter((line) => line.trim() !== "")
for (const line of lines) {
  const item = JSON.parse(line) as Case
  const result = await Effect.runPromise(
    CodeMode.execute({
      code: item.code,
      tools: buildTools(item.tools ?? []) as never,
      ...(item.bindings ? { bindings: item.bindings as never } : {}),
      ...(item.limits ? { limits: item.limits } : {}),
    }),
  )
  process.stdout.write(JSON.stringify({ name: item.name, result, output: formatResult(result) }) + "\n")
}
