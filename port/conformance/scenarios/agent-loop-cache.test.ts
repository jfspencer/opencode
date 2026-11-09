// Prompt-cache markers on OpenRouter (ai/src/cache-policy.ts, protocols/utils/cache.ts, openai-chat.ts): Anthropic
// models (`anthropic/…`, and `~anthropic/…` aliases) get the automatic policy, breakpoints on the last tool, the first
// and last system parts and the conversation's tail; Qwen models only on the system parts and the tail; other upstreams
// cache without breakpoints. Breakpoints are `cache_control` on content parts, so a marked message's content becomes a
// list of parts. A model's `cache.ttl` setting goes on the markers, or, for models without markers, as a request option
// (`prompt_cache_retention` for "24h", `prompt_cache_options.ttl` otherwise). Sessions are named up front: the fake
// tells title requests apart by a plain-text system prompt, which marked requests do not have.
import { call, say } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const model = (id: string, settings?: Record<string, unknown>) => ({
  name: id,
  package: "@opencode/ai/providers/openrouter",
  capabilities: { tools: true, input: ["text", "image/png"], output: ["text"], temperature: true, reasoning: { supported: false } },
  limit: { context: 200000, output: 16000 },
  ...(settings ? { settings } : {}),
})

const models = (entries: Record<string, ReturnType<typeof model>>) => ({ providers: { openrouter: { models: entries } } })
const ref = (id: string) => ({ providerID: "openrouter", id })
const files = { "src/math.ts": "export const add = (a: number, b: number) => a + b\n" }
const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="

async function run(ctx: Ctx, id: string, prompts: string[]) {
  const session = await ctx.session({ title: "cache", model: ref(id) })
  for (const text of prompts) await ctx.prompt(session, text)
  await ctx.http("GET", `/api/session/${session}/message`)
}

scenario(
  "Anthropic models on OpenRouter get cache breakpoints",
  { files, config: models({ "anthropic/claude-sonnet-5": model("anthropic/claude-sonnet-5") }), turns: [call("read", { path: "src/math.ts" }), say("Read."), say("Again.")] },
  (ctx) => run(ctx, "anthropic/claude-sonnet-5", ["Read the math module", "Thanks"]),
)

scenario(
  "an Anthropic alias gets cache breakpoints",
  { config: models({ "~anthropic/claude-sonnet-latest": model("~anthropic/claude-sonnet-latest") }), turns: [say("Alias.")] },
  (ctx) => run(ctx, "~anthropic/claude-sonnet-latest", ["Hello"]),
)

scenario(
  "a cache lifetime goes on the breakpoints",
  {
    files,
    config: models({ "anthropic/claude-sonnet-5": model("anthropic/claude-sonnet-5", { cache: { ttl: "1h" } }) }),
    turns: [call("read", { path: "src/math.ts" }), say("Read.")],
  },
  (ctx) => run(ctx, "anthropic/claude-sonnet-5", ["Read the math module"]),
)

scenario(
  "Qwen models get breakpoints on the system prompt and the tail",
  { files, config: models({ "qwen/qwen3-coder": model("qwen/qwen3-coder") }), turns: [call("read", { path: "src/math.ts" }), say("Read.")] },
  (ctx) => run(ctx, "qwen/qwen3-coder", ["Read the math module"]),
)

scenario(
  "other models get the cache lifetime as a request option",
  {
    config: models({
      "acme/thirty": model("acme/thirty", { cache: { ttl: "30m" } }),
      "acme/day": model("acme/day", { cache: { ttl: "24h" } }),
    }),
    turns: [say("Thirty minutes."), say("A day.")],
  },
  async (ctx) => {
    await run(ctx, "acme/thirty", ["Cache for thirty minutes"])
    await run(ctx, "acme/day", ["Cache for a day"])
  },
)

scenario(
  "the tail breakpoint lands on an attachment prompt's text",
  { config: models({ "anthropic/claude-sonnet-5": model("anthropic/claude-sonnet-5") }), turns: [say("A pixel.")] },
  async (ctx) => {
    const session = await ctx.session({ title: "cache", model: ref("anthropic/claude-sonnet-5") })
    await ctx.prompt(session, "Describe this", { files: [{ uri: `data:image/png;base64,${PNG}`, name: "a.png" }] })
    await ctx.http("GET", `/api/session/${session}/message`)
  },
)

scenario(
  "an instruction update before the prompt stays plain text",
  {
    // The file watcher is off: the scenario reloads after the write, and the watcher's reload would race it.
    server: { filewatcher: false },
    files: { "AGENTS.md": "Be brief.\n" },
    config: models({ "anthropic/claude-sonnet-5": model("anthropic/claude-sonnet-5") }),
    turns: [say("First."), say("Second.")],
  },
  async (ctx) => {
    const session = await ctx.session({ title: "cache", model: ref("anthropic/claude-sonnet-5") })
    await ctx.prompt(session, "First")
    ctx.write("AGENTS.md", "Be very brief.\n")
    const from = ctx.events.length
    await ctx.http("POST", "/api/location/reload")
    await ctx.until((event) => event?.type === "plugin.updated", from)
    await ctx.prompt(session, "Second")
    await ctx.http("GET", `/api/session/${session}/message`)
  },
)
