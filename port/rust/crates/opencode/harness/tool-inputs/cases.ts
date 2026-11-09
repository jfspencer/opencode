// Random tool inputs through the reference's input path (plugin/tool-input-repair.ts `repair`, then tool/runtime.ts
// `execute`'s `decodeInput`: Effect's decode under `errors: "all"` and `formatInputIssues`), recorded with their
// outcome, for tool/file_input.rs. Each schema's JSON is the definition runtime.ts gives the model (allOf flattened,
// acyclic `$ref`s inlined). The schemas are the real inputs of the tools decoded there and synthetic ones for what
// those do not use (recursive `$ref`s, array lengths, nested objects). `repair-only` schemas (unions, tuples,
// records) record the repaired value alone: the checks reconstruct Effect's wording from a JSON schema only for the
// features the tools have. Run by run.sh: bun cases.ts <seed> <count> <core> <repair module>.
const [seedText, countText, core, repairModule] = process.argv.slice(2)
const { Effect, Exit, Schema } = await import(Bun.resolveSync("effect", core!))
const { repair } = await import(repairModule!)
const runtime = await import(`${core}/src/tool/runtime.ts`)
const fake = (name: string, input: unknown) => ({ name, description: "", input, execute: () => Effect.succeed({ content: "ok" }) })
const decode = (name: string, schema: unknown, value: unknown) => runtime.execute(fake(name, schema), value, {})

let seed = Number(seedText ?? 1)
const random = () => {
  seed = (seed * 1103515245 + 12345) % 2147483648
  return seed / 2147483648
}
const pick = <T,>(items: readonly T[]) => items[Math.floor(random() * items.length)]!

// The definition's JSON schema, as the model receives it.
const toJsonSchema = (schema: unknown) => runtime.definition(fake("tool", schema)).inputSchema

const tool = async (file: string) => (await import(`${core}/src/tool/plugin/${file}.ts`)).Input

// No identifiers on inlined schemas: the definition drops them, as it drops a brand's, so a field that has one is
// worded by its tool (`Wording::Expected`). `Schema.Number` would be `anyOf [number, "Infinity" | ...]` (its JSON
// codec), so these use `Schema.Finite`.
const Item = Schema.Struct({ count: Schema.Finite.check(Schema.isGreaterThanOrEqualTo(0)), label: Schema.String })
// A recursive schema keeps its `$ref` in the definition (runtime.ts inlines only acyclic ones).
interface Node {
  readonly name: string
  readonly children?: ReadonlyArray<Node>
}
const Node: any = Schema.Struct({
  name: Schema.String.check(Schema.isMaxLength(5)),
  children: Schema.optionalKey(Schema.Array(Schema.suspend((): any => Node)).check(Schema.isMaxLength(2))),
}).annotate({ identifier: "Node" })
const Inner = Schema.Struct({ id: Schema.String.check(Schema.isStartingWith("it_")), size: Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: 9 })) })

// [name, Effect schema, wording the Rust caller passes, repair only]
const schemas: Array<[string, unknown, Array<[string, { message?: string; expected?: string }]>, boolean]> = [
  ["read", await tool("read"), [], false],
  ["write", await tool("write"), [], false],
  ["edit", await tool("edit"), [], false],
  ["glob", await tool("glob"), [["path", { expected: "RelativePath" }]], false],
  ["grep", await tool("grep"), [["pattern", { message: "Pattern must not be empty" }], ["path", { expected: "RelativePath" }]], false],
  ["patch", await tool("patch"), [], false],
  ["todowrite", await tool("todo"), [], false],
  ["subagent", await tool("subagent"), [["sessionID", { expected: "Session.ID" }]], false],
  [
    "lengths",
    Schema.Struct({
      items: Schema.Array(Item).check(Schema.isMinLength(1), Schema.isMaxLength(3)),
      ref: Item,
      refs: Schema.optionalKey(Schema.Array(Item)),
      tags: Schema.Array(Schema.String).check(Schema.isMaxLength(2)),
      short: Schema.String.check(Schema.isMaxLength(4)),
      prefixed: Schema.optionalKey(Schema.String.check(Schema.isStartingWith("ab"))),
      inner: Schema.optionalKey(Inner),
      inners: Schema.optionalKey(Schema.Array(Inner)),
      tree: Schema.optionalKey(Node),
    }),
    [],
    false,
  ],
  [
    "compositions",
    Schema.Struct({
      maybe: Schema.NullOr(Schema.Number),
      either: Schema.optionalKey(Schema.Union([Schema.Number, Schema.Boolean])),
      pair: Schema.optionalKey(Schema.Tuple([Schema.Number, Schema.Boolean])),
      dict: Schema.optionalKey(Schema.Record(Schema.String, Schema.Number)),
      list: Schema.optionalKey(Schema.Array(Schema.Int)),
      flags: Schema.optionalKey(Schema.Array(Schema.Boolean)),
      words: Schema.optionalKey(Schema.Array(Schema.String)),
      items: Schema.optionalKey(Schema.Array(Item)),
      nested: Schema.optionalKey(Schema.Struct({ ref: Item, n: Schema.optionalKey(Schema.NullOr(Schema.Int)) })),
    }),
    [],
    true,
  ],
]

/** A value for a JSON schema: usually valid, sometimes not, from the schema's own shape. */
function sample(schema: any, root: any, depth: number): unknown {
  if (schema?.$ref) return sample(root.$defs?.[schema.$ref.split("/").pop()], root, depth + 1)
  if (Array.isArray(schema?.anyOf)) return sample(pick(schema.anyOf), root, depth + 1)
  if (Array.isArray(schema?.enum)) return pick(schema.enum)
  const type = Array.isArray(schema?.type) ? pick(schema.type) : schema?.type
  switch (type) {
    case "string": {
      const prefix = typeof schema.pattern === "string" && schema.pattern.startsWith("^") ? schema.pattern.slice(1).replace(/\\/g, "") : ""
      return prefix + pick(["", "a", "src/x.ts", "hello world", "é", "*.ts", "12"]).slice(0, schema.maxLength ?? 99)
    }
    case "integer":
    case "number":
      return pick([0, 1, 2, 5, 9, 100, -1, 1.5])
    case "boolean":
      return random() < 0.5
    case "null":
      return null
    case "array": {
      if (Array.isArray(schema.prefixItems)) return schema.prefixItems.map((item: unknown) => sample(item, root, depth + 1))
      const length = depth > 3 ? 0 : Math.floor(random() * 4)
      return Array.from({ length }, () => sample(schema.items, root, depth + 1))
    }
    case "object": {
      const out: Record<string, unknown> = {}
      for (const [key, property] of Object.entries<any>(schema.properties ?? {}))
        if ((schema.required ?? []).includes(key) || random() < 0.5) out[key] = sample(property, root, depth + 1)
      if (schema.additionalProperties && typeof schema.additionalProperties === "object" && random() < 0.5)
        out[pick(["k", "other"])] = sample(schema.additionalProperties, root, depth + 1)
      return out
    }
    default:
      return pick(["x", 1, true, null])
  }
}

/** The mutations models make: wrong types, strings for numbers and booleans, stringified values, extra keys. */
function mutate(value: unknown, depth = 0): unknown {
  const roll = random()
  if (roll < 0.12) return pick([null, "x", 7, true, false, {}, [], "", "true", "false", "3", " 4 ", "0x10", "1e3", "abc", "-2"])
  if (roll < 0.2) return JSON.stringify(value)
  if (roll < 0.24 && typeof value === "number") return String(value)
  if (roll < 0.28 && typeof value === "boolean") return String(value)
  if (roll < 0.31 && value !== undefined) return [value]
  if (Array.isArray(value) && depth < 4) {
    const items = value.map((item) => (random() < 0.4 ? mutate(item, depth + 1) : item))
    if (random() < 0.15) items.push(items[0] ?? "x")
    return items
  }
  if (value && typeof value === "object" && depth < 4) {
    const out: Record<string, unknown> = {}
    for (const [key, item] of Object.entries(value)) {
      if (random() < 0.1) continue
      out[key] = random() < 0.35 ? mutate(item, depth + 1) : item
    }
    if (random() < 0.15) out[pick(["extra", "filePath", "content"])] = pick(["x", 1, null])
    return out
  }
  return value
}

const count = Number(countText ?? 2000)
const lines: string[] = []
for (const [name, schema, wording, repairOnly] of schemas) {
  const parameters = toJsonSchema(schema)
  for (let i = 0; i < count; i++) {
    const valid = sample(parameters, parameters, 0)
    const input = random() < 0.15 ? valid : mutate(valid)
    const repaired = repair(input, parameters, parameters, 0)
    const result: Record<string, unknown> = { tool: name, parameters, wording, input, repaired }
    if (!repairOnly) {
      const exit = Effect.runSyncExit(decode(name, schema, repaired))
      result.outcome = Exit.isSuccess(exit) ? { ok: true } : { error: errorText(exit) }
    }
    lines.push(JSON.stringify(result))
  }
}
console.log(`[${lines.join(",\n")}]`)

function errorText(exit: any): string {
  const failure = exit.cause?.reasons?.find((reason: any) => reason._tag === "Fail")
  return failure?.error?.message ?? "<defect>"
}
