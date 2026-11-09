// Which model writes a session's title (session/context.ts `selectTitle`): the title agent's own model when it names
// one; otherwise the provider's small model (`Model.small`: an active text model of the primary's provider whose family
// is one of the small families) at its most minimal variant, or else the primary model at a variant that lowers its
// reasoning (`none` or `minimal`, or `low` when it ranks below the selected variant). A failed attempt with another
// model falls back to the primary. The base config gives every agent a model, so these scenarios remove the title
// agent's (an `undefined` override is left out of the config). Model IDs stay clear of `anthropic/`: OpenRouter's
// Anthropic models get cache markers, whose content parts the fake does not recognize as a title request.
import { say, type Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const untitledAgent = { agents: { title: { model: undefined } } }

const model = (id: string, family: string | undefined, effort: string[] | undefined, extra: Record<string, unknown> = {}) => ({
  name: id,
  ...(family ? { family } : {}),
  package: "@opencode/ai/providers/openrouter",
  capabilities: {
    tools: true,
    input: ["text"],
    output: ["text"],
    temperature: true,
    reasoning: effort ? { supported: true, effort } : { supported: false },
  },
  limit: { context: 131072, output: 8192 },
  ...extra,
})

const ref = (id: string, variant?: string) => ({ providerID: "openrouter", id, ...(variant ? { variant } : {}) })

async function titled(ctx: Ctx, payload: Record<string, unknown> = {}) {
  const id = await ctx.session(payload)
  await ctx.prompt(id, "Help me refactor the math module")
  await ctx.http("GET", `/api/session/${id}`)
  // The title requests, in order: which model and variant each asked.
  ctx.note(
    "title requests",
    ctx.provider.requests
      .filter((request) => request.kind === "title")
      .map((request) => {
        const body = request.body as Record<string, unknown>
        return { model: body.model, reasoning: body.reasoning ?? null }
      }),
  )
}

scenario("title uses the primary model's lower variant", { config: untitledAgent, turns: [say("Hello there.")] }, (ctx) =>
  titled(ctx),
)

scenario(
  "title uses the provider's small model at its minimal variant",
  {
    config: {
      ...untitledAgent,
      providers: { openrouter: { models: { "google/gemini-3-flash-lite": model("google/gemini-3-flash-lite", "gemini-flash-lite", ["minimal", "low", "high"]) } } },
    },
    turns: [say("Hello there.")],
  },
  (ctx) => titled(ctx),
)

scenario(
  "title prefers the earliest small family",
  {
    config: {
      ...untitledAgent,
      providers: {
        openrouter: {
          models: {
            "acme/haiku-5": model("acme/haiku-5", "claude-haiku", undefined),
            "google/gemini-3-flash": model("google/gemini-3-flash", "gemini-flash", ["low", "high"]),
          },
        },
      },
    },
    turns: [say("Hello there.")],
  },
  (ctx) => titled(ctx),
)

scenario(
  "title skips a small model that is disabled or not a text model",
  {
    config: {
      ...untitledAgent,
      providers: {
        openrouter: {
          models: {
            "google/gemini-3-flash-lite": model("google/gemini-3-flash-lite", "gemini-flash-lite", ["minimal"], { disabled: true }),
            "openai/gpt-luna": { ...model("openai/gpt-luna", "gpt-luna", ["none"]), capabilities: { tools: false, input: ["image"], output: ["image"] } },
          },
        },
      },
    },
    turns: [say("Hello there.")],
  },
  (ctx) => titled(ctx),
)

// The harness releases the step's response once a title request fails, so the fallback's rename would race the step;
// the step's text waits for the rename.
const fallbackRenamed = Promise.withResolvers<void>()
scenario(
  "title falls back to the primary model when the small model fails",
  {
    config: {
      ...untitledAgent,
      providers: { openrouter: { models: { "google/gemini-3-flash": model("google/gemini-3-flash", "gemini-flash", ["low", "high"]) } } },
    },
    responders: [
      (request) =>
        request.kind === "title" && (request.body as Record<string, unknown>).model === "google/gemini-3-flash"
          ? ({ status: 500, body: { error: { message: "Small model down" } } } as Turn)
          : undefined,
    ],
    turns: [{ events: [{ until: fallbackRenamed.promise }, { text: "Hello there." }] }],
  },
  async (ctx) => {
    void ctx.until((event) => event?.type === "session.renamed", 0, 30_000).then(() => fallbackRenamed.resolve())
    await titled(ctx)
  },
)

scenario(
  "title keeps a session at low on the primary model",
  {
    config: { ...untitledAgent, providers: { openrouter: { models: { "acme/thinker": model("acme/thinker", "acme", ["low", "high"]) } } } },
    turns: [say("Low."), say("High.")],
  },
  async (ctx) => {
    await titled(ctx, { model: ref("acme/thinker", "low") })
    const id = await ctx.session({ model: ref("acme/thinker", "high") })
    await ctx.prompt(id, "Now with the high variant")
    await ctx.http("GET", `/api/session/${id}`)
    ctx.note(
      "all title requests",
      ctx.provider.requests.filter((request) => request.kind === "title").map((request) => (request.body as any).reasoning),
    )
  },
)

scenario(
  "title on a primary model without variants",
  {
    config: { ...untitledAgent, providers: { openrouter: { models: { "acme/plain": model("acme/plain", undefined, undefined) } } } },
    turns: [say("Plain.")],
  },
  (ctx) => titled(ctx, { model: ref("acme/plain") }),
)

scenario(
  "title agent's own model at its minimal variant",
  {
    config: {
      agents: { title: { model: "openrouter/acme/titler" } },
      providers: { openrouter: { models: { "acme/titler": model("acme/titler", "acme", ["minimal", "high"]) } } },
    },
    turns: [say("Hello there.")],
  },
  (ctx) => titled(ctx),
)

scenario(
  "regenerated title uses the small model",
  {
    config: {
      ...untitledAgent,
      providers: { openrouter: { models: { "google/gemini-3-flash": model("google/gemini-3-flash", "gemini-flash", ["low"]) } } },
    },
    turns: [say("Hello there.")],
    responders: [
      (request) => (request.kind === "title" ? ({ events: [{ text: "Math module refactor" }] } as Turn) : undefined),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "Named" })
    await ctx.prompt(id, "Help me refactor the math module")
    await ctx.http("PATCH", `/api/session/${id}`, { title: "" })
    await ctx.until((event) => event?.type === "session.renamed")
    await ctx.http("GET", `/api/session/${id}`)
    ctx.note(
      "title requests",
      ctx.provider.requests.filter((request) => request.kind === "title").map((request) => (request.body as any).model),
    )
  },
)
