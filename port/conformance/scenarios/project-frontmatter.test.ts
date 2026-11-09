// Markdown frontmatter that is not JSON: what js-yaml makes (timestamps, binary, aliases that share or contain
// themselves, top-level scalars and sequences, nesting as deep as the JS stack allows) and gray-matter's engines named
// after Object.prototype members, as the agent, command and skill plugins decode them.
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated).
const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await body(ctx)
    await ctx.until((event) => event?.type === "plugin.updated")
  })

const md = (front: string[], body = "Body.") => ["---", ...front, "---", body].join("\n")
const list = async (ctx: Ctx) => {
  await ctx.http("GET", "/api/agent")
  await ctx.http("GET", "/api/command")
  await ctx.http("GET", "/api/skill")
}

scenario(
  "values JSON cannot say",
  {
    files: {
      // An object position takes a Date (with no fields); a string position does not.
      ".opencode/agents/dated.md": md(["description: Dated request.", "request: 2001-12-14"]),
      ".opencode/commands/dated.md": md(["description: 2001-12-14"], "Dated template."),
      ".opencode/skills/dated/SKILL.md": md(["description: 2001-12-14"]),
      // A value containing itself, where nothing decodes it.
      ".opencode/agents/looped.md": md(["description: Looped.", "system: s", "flavor: &a [*a]"]),
      ".opencode/commands/looped.md": md(["description: Looped command.", "flavor: &a {self: *a}"], "Loop."),
      ".opencode/skills/looped/SKILL.md": md(["description: Looped skill.", "metadata: &m {self: *m, opencode/autoinvoke: false}"]),
      // Top-level scalars: a legacy agent spreads a string's characters and a Buffer's bytes into its request body.
      ".opencode/agents/word.md": md(["hello"], "Word body."),
      ".opencode/agents/bytes.md": md(["!!binary aGk="], "Bytes body."),
      ".opencode/agents/listed.md": md(["- a", "- b"], "Listed body."),
      ".opencode/skills/word/SKILL.md": md(["hello"]),
      ".opencode/skills/day/SKILL.md": md(["2001-12-14"]),
      // An own `__proto__` key is only a key.
      ".opencode/skills/proto/SKILL.md": md(["__proto__: {name: inherited}", "description: Own proto key."]),
      ".opencode/agents/proto.md": md(["__proto__: {a: 1}", "flavor: vanilla"], "Proto body."),
      // Aliases share nodes: ten levels of ten references each load without copying.
      ".opencode/agents/shared.md": md([
        "description: Shared nodes.",
        "system: s",
        "a0: &a0 [x]",
        ...Array.from({ length: 10 }, (_, i) => `a${i + 1}: &a${i + 1} [${Array(10).fill(`*a${i}`).join(", ")}]`),
      ]),
    },
  },
  list,
)

scenario(
  "a legacy agent JSON.stringify cannot write fails the agent plugin",
  {
    config: { agents: { configured: { description: "From config." } } },
    files: {
      ".opencode/agents/good.md": md(["description: Good."], "Good body."),
      ".opencode/agents/cyclic.md": md(["flavor: &a [*a]"], "Cyclic body."),
      ".opencode/commands/kept.md": md(["description: Commands load."], "Kept."),
    },
  },
  async (ctx) => {
    await ctx.http("GET", "/api/agent")
    await ctx.http("GET", "/api/agent/configured")
    await ctx.http("GET", "/api/command")
  },
)

scenario(
  "nesting as deep as the JS stack allows",
  {
    files: {
      ".opencode/agents/deep.md": md([
        "description: Deep enough.",
        "system: s",
        "x:",
        `  y: ${"[".repeat(2000)}${"]".repeat(2000)}`,
      ]),
      ".opencode/agents/deeper.md": md([
        "description: Too deep.",
        "system: s",
        "x:",
        `  y: ${"[".repeat(30000)}${"]".repeat(30000)}`,
      ]),
      // At the top level the sanitize fallback makes it a string.
      ".opencode/skills/rescued/SKILL.md": md(["description: Rescued.", `x: ${"[".repeat(30000)}${"]".repeat(30000)}`]),
      ".opencode/skills/sequence/SKILL.md": md(["description: Too deep.", "x:", `  ${"- ".repeat(30000)}z`]),
    },
  },
  list,
)

scenario(
  "engines named after Object.prototype members",
  {
    files: {
      ".opencode/agents/constructor.md": ["---constructor", "ab", "---", "Constructor body."].join("\n"),
      ".opencode/agents/to-string.md": ["---toString", "a: 1", "---", "To string body."].join("\n"),
      ".opencode/agents/value-of.md": ["---valueOf", "a: 1", "---", "Value of body."].join("\n"),
      ".opencode/agents/own.md": ["---hasOwnProperty", "a: 1", "---", "Own body."].join("\n"),
      ".opencode/agents/proto.md": ["---__proto__", "a: 1", "---", "Proto body."].join("\n"),
      ".opencode/commands/constructor.md": ["---constructor", "a: 1", "---", "Constructor template."].join("\n"),
      ".opencode/commands/getter.md": ["---__lookupGetter__", "a: 1", "---", "Getter template."].join("\n"),
      ".opencode/skills/constructor/SKILL.md": ["---constructor", "a: 1", "---", "Constructor skill."].join("\n"),
      ".opencode/skills/to-string/SKILL.md": ["---toString", "a: 1", "---", "To string skill."].join("\n"),
      ".opencode/skills/value-of/SKILL.md": ["---valueOf", "a: 1", "---", "Value of skill."].join("\n"),
    },
  },
  list,
)

scenario(
  "an excerpt separator without a string form skips the file",
  {
    files: {
      ".opencode/commands/plain.md": md(["description: Plain separator.", "excerpt_separator: '<!-- more -->'"], "Kept."),
      ".opencode/commands/object.md": md(["description: Object separator.", "excerpt_separator: {a: 1}"], "Kept too."),
      // On one line the sanitize fallback would make the separator a string; nested, it stays an object.
      ".opencode/commands/rescued.md": md(["description: Rescued separator.", "excerpt_separator: {toString: x}"], "Kept."),
      ".opencode/commands/opaque.md": md(["description: No string form.", "excerpt_separator:", "  toString: x"], "Skipped."),
      ".opencode/skills/opaque/SKILL.md": md(["description: No string form.", "excerpt_separator:", "  - {valueOf: 1, toString: 2}"]),
    },
  },
  list,
)
