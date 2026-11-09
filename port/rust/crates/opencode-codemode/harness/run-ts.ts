// The TS side of the differential check: runs every case of a corpus (JSON lines, see harness/README.md) through
// the reference interpreter (packages/codemode, default engine) and prints one JSON line per case:
// `{"name", "result", "output"}`, where `result` is the CodeMode.Result exactly as the tool layer receives it and
// `output` is the text core's `execute` tool hands to the model (core/src/codemode/tool.ts `formatResult`).
//
// usage: bun harness/run-ts.ts corpus.jsonl [--scan] > ts.jsonl
//
// With --scan, each case's code goes through `CodeMode.scanTools` instead, printing `{"name", "scan"}` where `scan` is
// `{"ok": true, "paths"}` or `{"ok": false, "error"}`.
import { CodeMode, Extension, Tool } from "../../../../.reference/packages/codemode/src/index.ts"
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

// Host globals described as data (harness/README.md): each builds a host function or object.
type GlobalSpec =
  | { kind: "echo"; async?: boolean }
  | { kind: "args"; async?: boolean }
  | { kind: "value"; value: unknown; async?: boolean }
  | { kind: "throw"; name?: string; message: string; fields?: Record<string, unknown>; async?: boolean }
  | { kind: "response"; body: string }
  | { kind: "object"; members: Record<string, GlobalSpec> }

const hostError = (spec: { name?: string; message: string; fields?: Record<string, unknown> }) => {
  const types: Record<string, ErrorConstructor> = { TypeError, RangeError, SyntaxError, Error }
  const error = new (types[spec.name ?? "Error"] ?? Error)(spec.message)
  if (spec.name && !types[spec.name]) error.name = spec.name
  Object.assign(error, spec.fields ?? {})
  return error
}

const buildGlobal = (spec: GlobalSpec): unknown => {
  const mark = (fn: (...args: Array<unknown>) => unknown, async?: boolean) => (async ? Extension.async(fn) : fn)
  switch (spec.kind) {
    case "echo":
      return mark((...args) => (spec.async ? Promise.resolve(args[0]) : args[0]), spec.async)
    case "args":
      return mark((...args) => (spec.async ? Promise.resolve(args) : args), spec.async)
    case "value":
      return mark(() => (spec.async ? Promise.resolve(spec.value) : spec.value), spec.async)
    case "throw":
      return mark(() => (spec.async ? Promise.reject(hostError(spec)) : (() => { throw hostError(spec) })()), spec.async)
    case "response":
      return Extension.async(async (url: unknown) => {
        const bytes = new TextEncoder().encode(spec.body)
        return {
          url: String(url),
          status: 200,
          ok: true,
          headers: { get: (name: string) => (name === "content-type" ? "application/json" : null) },
          text: async () => new TextDecoder().decode(bytes),
          json: async () => JSON.parse(new TextDecoder().decode(bytes)),
          bytes: async () => bytes,
          sync: () => "sync",
        }
      })
    case "object":
      return Object.fromEntries(Object.entries(spec.members).map(([name, member]) => [name, buildGlobal(member)]))
  }
}

type Case = {
  name: string
  code: string
  tools?: Array<ToolSpec>
  bindings?: Record<string, unknown>
  limits?: { timeoutMs?: number; maxToolCalls?: number; maxOutputBytes?: number }
  extensions?: Array<{ name: string; globals: Record<string, GlobalSpec> }>
  /** Record the hooks' events: each `before` as it starts, each `after` with the index of its `before`. */
  hooks?: boolean
  scanOnly?: boolean
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
if (!path) throw new Error("usage: bun harness/run-ts.ts corpus.jsonl [--scan]")
const lines = (await Bun.file(path).text()).split("\n").filter((line) => line.trim() !== "")
const scanning = process.argv.includes("--scan")
for (const line of lines) {
  const item = JSON.parse(line) as Case
  if (scanning) {
    // A tree deeper than the JS stack holds makes scanTools throw (Rust: an ExecutionFailure diagnostic).
    let scan: unknown
    try {
      const scanned = CodeMode.scanTools(item.code)
      scan = scanned._tag === "Success" ? { ok: true, paths: scanned.success } : { ok: false, error: scanned.failure }
    } catch (error) {
      scan = { ok: false, thrown: error instanceof Error ? error.name : typeof error }
    }
    process.stdout.write(JSON.stringify({ name: item.name, scan }) + "\n")
    continue
  }
  // Cases written for the scan alone (some never finish when run).
  if (item.scanOnly) {
    process.stdout.write(JSON.stringify({ name: item.name, skipped: true }) + "\n")
    continue
  }
  const events: Array<unknown> = []
  const started = new Map<object, number>()
  const begin = (call: object, event: Array<unknown>) =>
    Effect.sync(() => {
      started.set(call, events.length)
      events.push(event)
    })
  const finish = (kind: string, call: object, result: { status: string }) =>
    Effect.sync(() => {
      events.push([kind, started.get(call) ?? -1, result.status])
    })
  const hooks = {
    "tool.before": (call: { name: string; input: unknown }) =>
      begin(call, ["tool.before", call.name, call.input === undefined ? null : call.input]),
    "tool.after": (call: object, result: { status: string }) => finish("tool.after", call, result),
    "extension.before": (call: { extension: string; name: string; args: ReadonlyArray<unknown> }) =>
      begin(call, ["extension.before", call.extension, call.name, call.args.length]),
    "extension.after": (call: object, result: { status: string }) => finish("extension.after", call, result),
  }
  const result = await Effect.runPromise(
    CodeMode.execute({
      code: item.code,
      tools: buildTools(item.tools ?? []) as never,
      ...(item.bindings ? { bindings: item.bindings as never } : {}),
      ...(item.limits ? { limits: item.limits } : {}),
      ...(item.hooks ? { hooks } : {}),
      ...(item.extensions
        ? {
            extensions: item.extensions.map((extension) =>
              Extension.make({
                name: extension.name,
                globals: Object.fromEntries(Object.entries(extension.globals).map(([name, spec]) => [name, buildGlobal(spec)])),
              }),
            ),
          }
        : {}),
    } as never),
  )
  const record = { name: item.name, result, output: formatResult(result), ...(item.hooks ? { events } : {}) }
  process.stdout.write(JSON.stringify(record) + "\n")
}
