// Generates markdown files with frontmatter (random YAML documents drawing on what js-yaml makes that JSON cannot
// say: timestamps, binary, shared and self-containing aliases, `__proto__` keys, deep nesting; top-level scalars and
// sequences; the gray-matter engines, `javascript` and `Object.prototype` names included; hand-picked cases) and
// records what the reference makes of each: gray-matter's data (described as a JS value graph), and the markdown
// agent, command and skill the config plugins decode from it, or that they skip it, or that the plugin dies.
//
// usage: bun cases.ts <reference packages dir> <seed> <count> > cases.json
const [packages, seedArg, countArg] = process.argv.slice(2)
const { ConfigMarkdown } = await import(`${packages}/core/src/config/markdown.ts`)
const { SkillFile } = await import(`${packages}/core/src/config/plugin/skill-file.ts`)
const { ConfigAgentV1 } = await import(`${packages}/core/src/v1/config/agent.ts`)
const { ConfigMigrateV1 } = await import(`${packages}/core/src/v1/config/migrate.ts`)
const { ConfigAgent } = await import(`${packages}/schema/src/config/agent.ts`)
const { ConfigCommand } = await import(`${packages}/schema/src/config/command.ts`)
const { Result, Schema, SchemaParser } = await import(Bun.resolveSync("effect", `${packages}/schema`))

// mulberry32.
let state = Number(seedArg ?? 1) >>> 0
const random = () => {
  state = (state + 0x6d2b79f5) >>> 0
  let t = state
  t = Math.imul(t ^ (t >>> 15), t | 1)
  t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296
}
const pick = <T>(items: readonly T[]): T => items[Math.floor(random() * items.length)]
const chance = (p: number) => random() < p
const int = (n: number) => Math.floor(random() * n)

// ---------------------------------------------------------------------------------------------------------------
// The reference's view of a document.

/** A JS value as `jsvalue::describe` writes it (port/rust/crates/opencode/src/inventory/jsvalue.rs). */
function describe(value: unknown, seen = new Map<object, number>()): unknown {
  if (value === undefined) return { t: "undefined" }
  if (value === null) return null
  switch (typeof value) {
    case "boolean":
      return value
    case "number":
      return Number.isFinite(value) && !Object.is(value, -0)
        ? value
        : { t: "number", v: Object.is(value, -0) ? "-0" : String(value) }
    case "string":
      return value.toWellFormed()
    case "bigint":
      return { t: "bigint" }
    case "symbol":
      return { t: "symbol" }
  }
  const object = value as object
  const known = seen.get(object)
  if (known !== undefined) return { ref: known }
  const id = seen.size
  seen.set(object, id)
  const out: Record<string, unknown> = { t: "", id }
  const tag = Object.prototype.toString.call(object)
  if (Array.isArray(object)) {
    out.t = "Array"
    out.length = object.length
  } else if (typeof object === "function") out.t = "Function"
  else if (object instanceof Date) {
    out.t = "Date"
    out.time = describe(object.getTime(), seen)
  } else if (Buffer.isBuffer(object)) out.t = "Buffer"
  else if (object instanceof Uint8Array) out.t = "Uint8Array"
  else if (object instanceof String || object instanceof Number || object instanceof Boolean) {
    out.t = "Boxed"
    out.value = describe(object.valueOf(), seen)
  } else if (object instanceof Error) {
    out.t = "Error"
    out.name = Object.getPrototypeOf(object).name
  } else if (object instanceof URL) {
    out.t = "URL"
    out.href = object.href
  } else if (tag === "[object Object]") {
    out.t = "Plain"
    const proto = Object.getPrototypeOf(object)
    if (proto === null) out.proto = null
    else if (proto !== Object.prototype) out.proto = describe(proto, seen)
  } else out.t = "Other"
  out.props = Object.keys(object).map((key) => [key.toWellFormed(), describe((object as Record<string, unknown>)[key], seen)])
  return out
}

const agentKeys = new Set(["variant", ...Object.keys(ConfigAgent.Info.fields)])
const nativeKeys = ["permissions", "request", "system", "disabled"]
const decodeAgent = SchemaParser.decodeUnknownResult(ConfigAgent.Info)
const decodeLegacyAgent = SchemaParser.decodeUnknownResult(ConfigAgentV1.Info)
const encodeAgent = Schema.encodeSync(ConfigAgent.Info)
const decodeCommand = SchemaParser.decodeUnknownResult(ConfigCommand.Info)
const encodeCommand = Schema.encodeSync(ConfigCommand.Info)

/** config/plugin/agent.ts `decode` for `<dir>/agents/a.md`: the agent, `"skip"`, or `"defect"` when it throws. */
function agent(content: string) {
  try {
    const markdown = ConfigMarkdown.parseOption(content)
    if (!markdown) return "skip"
    const name = "a"
    const body = markdown.content.trim()
    const keys = Object.keys(markdown.data)
    const unsupported = keys.filter((key) => !agentKeys.has(key))
    const legacy = unsupported.length > 0 && !keys.some((key) => nativeKeys.includes(key))
    const data =
      typeof markdown.data.model === "string" &&
      !markdown.data.model.includes("#") &&
      typeof markdown.data.variant === "string" &&
      /^[^#]+$/.test(markdown.data.variant)
        ? { ...markdown.data, model: `${markdown.data.model}#${markdown.data.variant}` }
        : markdown.data
    const decoded = legacy
      ? Result.map(
          decodeLegacyAgent({ name, ...markdown.data, prompt: body }, { errors: "all" }),
          ConfigMigrateV1.migrateAgent,
        )
      : decodeAgent({ ...data, system: body }, { errors: "all" })
    if (Result.isFailure(decoded)) return "skip"
    return encodeAgent(decoded.success)
  } catch {
    return "defect"
  }
}

/** config/plugin/command.ts `decode`. */
function command(content: string) {
  try {
    const markdown = ConfigMarkdown.parseOption(content)
    if (!markdown) return "skip"
    const info = decodeCommand({ ...markdown.data, template: markdown.content.trim() })
    if (Result.isFailure(info)) return "skip"
    return encodeCommand(info.success)
  } catch {
    return "defect"
  }
}

/** SkillFile.parse for `<dir>/skills/a/SKILL.md`. */
function skill(content: string) {
  try {
    const result = SkillFile.parse("/d", "/d/skills/a/SKILL.md", content)
    if (result._tag !== "Parsed") return "skip"
    const { id, name, description, autoinvoke } = result.skill
    return { id, name, ...(description === undefined ? {} : { description }), ...(autoinvoke === undefined ? {} : { autoinvoke }) }
  } catch {
    return "defect"
  }
}

/**
 * Plain JSON with each lone surrogate as U+FFFD, as the port's strings hold them (see jsvalue.rs): what is left to
 * compare is everything else.
 */
const wellFormed = (value: unknown): unknown => {
  if (typeof value === "string") return value.toWellFormed()
  if (Array.isArray(value)) return value.map(wellFormed)
  if (value && typeof value === "object")
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k.toWellFormed(), wellFormed(v)]))
  return value
}

function record(content: string) {
  let matter: unknown
  try {
    const file = ConfigMarkdown.parse(content)
    matter = { data: describe(file.data), content: file.content.toWellFormed() }
  } catch {
    matter = { throws: true }
  }
  return {
    content,
    matter,
    agent: wellFormed(agent(content)),
    command: wellFormed(command(content)),
    skill: wellFormed(skill(content)),
  }
}

// ---------------------------------------------------------------------------------------------------------------
// Documents.

const words = ["a", "b", "x", "hello world", "Uses: colon", "[Tag] text", "#hash", "'q'", '"dq"', "é", "😀", "a#b", "  pad  "]
const keys = [
  ...["model", "variant", "request", "system", "description", "mode", "hidden", "color", "steps", "disabled", "permissions"],
  ...["temperature", "top_p", "prompt", "tools", "disable", "options", "maxSteps", "permission", "name"],
  ...["template", "agent", "subagent", "subtask", "metadata", "disable-model-invocation"],
  ...["flavor", "x", "0", "1", "__proto__", "constructor", "toString", "excerpt_separator", "headers", "body"],
]
const scalars = () =>
  pick([
    () => pick(words),
    () => JSON.stringify(pick(words)),
    () => String(int(5) - 1),
    () => pick(["0.5", "1e3", ".nan", ".inf", "-.inf", "0x1F", "012", "-0", "1:30"]),
    () => pick(["true", "false", "yes", "no", "on", "null", "~", ""]),
    () => pick(["2001-12-14", "2001-12-14t21:59:43.10-05:00", "2001-12-14 21:59:43.10 -5", "1999-01-01T00:00:00Z"]),
    () => pick(["!!binary aGk=", "!!binary ''", "!!str 12", "!!int '7'", "!!float 1", "!!timestamp 2020-02-02"]),
    () => pick(["openrouter/deepseek/x", "anthropic/claude#high", "a/b#c#d", "nomodel", "openrouter/x"]),
    () => pick(["allow", "deny", "ask", "subagent", "primary", "all", "#00AA00", "primary", "#zzzzzz"]),
    () => pick(['"\\uD800"', '"a\\uDE00b"', '"\\uD83D\\uDE00"', '"\\U0001F600"', '"\\x41"']),
  ])()

/** A YAML value in flow style (nested to `depth`). */
function flow(depth: number, anchors: string[]): string {
  if (anchors.length && chance(0.15)) return `*${pick(anchors)}`
  if (depth <= 0 || chance(0.4)) {
    const s = scalars()
    return s.includes(":") || s.includes("#") || s.startsWith("[") || s.startsWith("!") || s === "" ? JSON.stringify(s) : s
  }
  if (chance(0.5)) return `[${Array.from({ length: int(4) }, () => flow(depth - 1, anchors)).join(", ")}]`
  return `{${Array.from({ length: int(4) }, () => `${pick(keys)}: ${flow(depth - 1, anchors)}`).join(", ")}}`
}

/** A block-style value at an indentation. */
function block(indent: string, depth: number, anchors: string[]): string {
  const roll = random()
  const anchor = chance(0.15) ? `a${anchors.length}` : undefined
  const prefix = anchor ? `&${anchor} ` : ""
  if (depth <= 0 || roll < 0.45) {
    const value = chance(0.7) ? scalars() : flow(2, anchors)
    if (anchor) anchors.push(anchor)
    return ` ${prefix}${value}`
  }
  if (roll < 0.7) {
    if (anchor) anchors.push(anchor)
    const items = Array.from({ length: 1 + int(3) }, () => `${indent}-${block(indent + "  ", depth - 1, anchors)}`)
    return ` ${prefix}\n${items.join("\n")}`
  }
  if (anchor) anchors.push(anchor)
  const used = new Set<string>()
  const lines: string[] = []
  for (let i = 0; i < 1 + int(4); i++) {
    const key = chance(0.1) && anchors.length ? "<<" : pick(keys)
    if (used.has(key) && key !== "<<") continue
    used.add(key)
    lines.push(key === "<<" ? `${indent}<<: *${pick(anchors)}` : `${indent}${key}:${block(indent + "  ", depth - 1, anchors)}`)
  }
  return ` ${prefix}\n${lines.join("\n")}`
}

/** A frontmatter document: mostly a mapping of known keys. */
function yamlDocument(): string {
  const anchors: string[] = []
  const roll = random()
  if (roll < 0.06) return scalars()
  if (roll < 0.1) return `- ${scalars()}\n- ${scalars()}`
  if (roll < 0.13) return flow(3, anchors)
  const lines: string[] = []
  const used = new Set<string>()
  for (let i = 0; i < 1 + int(6); i++) {
    const key = pick(keys)
    if (used.has(key)) continue
    used.add(key)
    lines.push(`${key}:${block("  ", 3, anchors)}`)
  }
  // A self-containing alias now and then.
  if (chance(0.08)) lines.push(`${pick(["flavor", "options", "metadata", "request", "x", "tools"])}: &c [*c]`)
  if (chance(0.05)) lines.push(`${pick(["flavor", "options", "metadata", "request"])}: &d {k: *d}`)
  return lines.join("\n")
}

const engines = ["", "yaml", "YAML", "yml", "json", "js", "javascript", "constructor", "toString", "valueOf", "hasOwnProperty",
  "__proto__", "__lookupGetter__", "__defineSetter__", "coffee", "Constructor"]

function document(): string {
  const language = chance(0.9) ? "" : pick(engines)
  const front = language === "json" ? JSON.stringify({ description: pick(words), steps: int(3) }) : yamlDocument()
  return `---${language}\n${front}\n---\n${pick(["Body.", "", "  Body with space  ", "Line\n---\nmore"])}`
}

const md = (front: string[], body = "Body.") => ["---", ...front, "---", body].join("\n")
const deep = (n: number) => `${"[".repeat(n)}${"]".repeat(n)}`
const bomb = (() => {
  let doc = ["a0: &a0 [x]"]
  for (let i = 1; i <= 10; i++) doc.push(`a${i}: &a${i} [${Array(10).fill(`*a${i - 1}`).join(", ")}]`)
  return doc
})()

const handPicked: string[] = [
  md(["description: d", "flavor: &a [*a]"]),
  md(["flavor: &a [*a]"]),
  md(["options:", "  loop: &a {self: *a}"]),
  md(["description: d", "system: s", "request:", "  body:", "    x: &a [*a]"]),
  md(["request: &r", "  headers: {}", "  body: {}", "  more: *r"]),
  md(["permissions: &p [*p]"]),
  md(["name: n", "description: d", "metadata: &m {self: *m, opencode/autoinvoke: true}"]),
  md(["description: 2001-12-14"]),
  md(["request: 2001-12-14", "description: d"]),
  md(["request: !!binary aGk="]),
  md(["model: 2001-12-14"]),
  md(["2001-12-14"]),
  md(["!!binary aGk="]),
  md(["hello"]),
  md(["- a", "- b"]),
  md(["42"]),
  md(["false"]),
  md(["__proto__: {name: inherited}", "description: d"]),
  md(["__proto__: 1", "flavor: x"]),
  md(["options: {__proto__: {a: 1}}", "flavor: x"]),
  md(["metadata: !!binary aGk="]),
  md(["metadata: 2001-12-14", "disable-model-invocation: 1"]),
  md(["disable-model-invocation: ' YES '"]),
  md(["tools: 2001-12-14", "flavor: x"]),
  md(["tools: {a: true, write: false}", "permission: {edit: deny, bash: {'*': ask}}"]),
  md(["permission: allow", "flavor: x"]),
  md(["permission: {question: {a: allow}}", "flavor: x"]),
  md(["temperature: .nan", "flavor: x"]),
  md(["options: {n: .nan, i: .inf, z: -0, d: 2001-12-14, b: !!binary aGk=}"]),
  md(["description: \"\\uD800\""]),
  md(["flavor: \"\\uD83D\""]),
  md(["x:", "  y: " + deep(2000)]),
  md(["x:", "  y: " + deep(20000)]),
  md(["x: " + deep(20000)]),
  md(["description: d", "x:", "  y: " + deep(2000), "system: s"]),
  md(["x:", "  " + "- ".repeat(2000) + "z"]),
  md(["x:", "  " + "- ".repeat(20000) + "z"]),
  md(["description: d", "system: s", ...bomb]),
  md(["options:", ...bomb.map((l) => "  " + l)]),
  md(["model: openrouter/x", "variant: high"]),
  md(["model: openrouter/x", "variant: ''"]),
  md(["model: {providerID: openrouter, model: x}"]),
  ["---constructor", "a: 1", "---", "B"].join("\n"),
  ["---toString", "a: 1", "---", "B"].join("\n"),
  ["---valueOf", "a: 1", "---", "B"].join("\n"),
  ["---hasOwnProperty", "a: 1", "---", "B"].join("\n"),
  ["---__lookupGetter__", "a: 1", "---", "B"].join("\n"),
  ["---__proto__", "a: 1", "---", "B"].join("\n"),
]

const count = Number(countArg ?? 2000)
const contents = [...handPicked, ...Array.from({ length: count }, document)]
process.stdout.write("[\n")
contents.forEach((content, i) => process.stdout.write(`${i ? ",\n" : ""}${JSON.stringify(record(content))}`))
process.stdout.write("\n]\n")
