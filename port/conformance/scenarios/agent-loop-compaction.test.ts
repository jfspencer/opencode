// Compaction: automatic (a reported prompt near the 1,048,576-token window), overflow recovery (the provider rejects
// the request as too long), manual (POST /compact), and background summaries. The summary request is an ordinary
// OpenRouter chat request, so the script answers it like any other turn; a reply must use the summary template's
// headings to count.
import { call, say, type ProviderRequest, type Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

/** A reported prompt past the automatic compaction threshold (85% of the 1,048,576-token window). */
const FULL = { prompt: 900_000, completion: 10 }
/** Within the background-summary lead below the threshold, but not past it. */
const NEAR = { prompt: 850_000, completion: 10 }

const SUMMARY = say("## Objective\n- Keep the demo project tidy\n\n## Work State\n### Completed\n- Read the README\n\n## Next Move\n1. Answer the next question")
const LEGACY_SUMMARY = say("## Objective\n- Keep the demo project tidy\n\n## Additional Context\n- Every file touched so far, in detail")
const contextOverflow: Turn = {
  status: 400,
  body: { error: { message: "This endpoint's maximum context length is 1048576 tokens. However, you requested about 1200000 tokens.", code: 400 } },
}

const files = { "README.md": "# Demo\n\nA small project.\n", "src/math.ts": "export const add = (a: number, b: number) => a + b\n" }

async function messages(ctx: Ctx, id: string) {
  await ctx.http("GET", `/api/session/${id}`)
  await ctx.http("GET", `/api/session/${id}/message`)
}

/** Answers summary requests (whatever their order relative to chat turns) from a dedicated queue. */
function summaries(...replies: Turn[]) {
  return (request: ProviderRequest): Turn | undefined => {
    if (request.kind !== "chat" || !isSummaryRequest(request.body)) return undefined
    return replies.shift() ?? SUMMARY
  }
}

/** Waits until `count` usage updates were published since `from` (one per finished step or summary call). */
async function usageUpdates(ctx: Ctx, from: number, count: number) {
  await ctx.until(() => ctx.events.slice(from).filter((event: any) => event?.type === "session.usage.updated").length >= count)
}

function isSummaryRequest(body: unknown) {
  const messages = (body as { messages?: Array<{ role: string; content: unknown }> })?.messages ?? []
  const last = messages.at(-1)
  const text = typeof last?.content === "string" ? last.content : JSON.stringify(last?.content ?? "")
  return /summarize the conversation above|Update the existing checkpoint|did not fill in the required summary template/.test(text)
}

scenario(
  "automatic compaction between prompts",
  { files, turns: [say("First answer.", FULL), SUMMARY, say("Second answer.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "What is this project?")
    await ctx.prompt(id, "And what does add do?")
    await messages(ctx, id)
  },
)

scenario(
  "automatic compaction within a turn",
  {
    files,
    turns: [
      say("Looking.", { prompt: 1000 }),
      { events: [{ text: "Reading." }, { tool: "read", input: { path: "README.md" } }], usage: FULL },
      SUMMARY,
      call("read", { path: "src/math.ts" }),
      say("Both read."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "Hello")
    await ctx.prompt(id, "Read the README and math")
    await messages(ctx, id)
  },
)

scenario(
  "automatic compaction keeps recent exchanges",
  {
    files,
    turns: [say("One.", { prompt: 1000 }), say("Two.", { prompt: 2000 }), say("Three.", FULL), SUMMARY, say("Four.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First question")
    await ctx.prompt(id, "Second question")
    await ctx.prompt(id, "Third question")
    await ctx.prompt(id, "Fourth question")
    await messages(ctx, id)
  },
)

scenario(
  "compaction summary nudged once",
  { files, turns: [say("First.", FULL), say("Here is a summary without the template."), SUMMARY, say("Second.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "One")
    await ctx.prompt(id, "Two")
    await messages(ctx, id)
  },
)

scenario(
  "compaction summary ignores the template twice",
  { files, turns: [say("First.", FULL), say("No template."), say("Still no template.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "One")
    await ctx.prompt(id, "Two")
    await messages(ctx, id)
  },
)

scenario(
  "compaction summary is empty",
  { files, turns: [say("First.", FULL), { events: [] }, { events: [] }] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "One")
    await ctx.prompt(id, "Two")
    await messages(ctx, id)
  },
)

scenario(
  "compaction summary hits the output limit",
  { files, turns: [say("First.", FULL), { events: [{ text: "## Objective\n- cut" }], finish: "length" }] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "One")
    await ctx.prompt(id, "Two")
    await messages(ctx, id)
  },
)

scenario(
  "compaction summary blocked by the content filter",
  { files, turns: [say("First.", FULL), { events: [{ text: "## Objective" }], finish: "content_filter" }] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "One")
    await ctx.prompt(id, "Two")
    await messages(ctx, id)
  },
)

scenario(
  "compaction summary request fails, then succeeds",
  {
    timeout: 30_000,
    files,
    turns: [say("First.", FULL), { status: 500, body: { error: { message: "Internal Server Error" } } }, SUMMARY, say("Second.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "One")
    await ctx.prompt(id, "Two")
    await messages(ctx, id)
  },
)

scenario(
  "compaction summary finishes with an error, then succeeds",
  { timeout: 30_000, files, turns: [say("First.", FULL), { events: [{ text: "## Objective" }], finish: "error" }, SUMMARY, say("Second.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "One")
    await ctx.prompt(id, "Two")
    await messages(ctx, id)
  },
)

scenario(
  "compaction summary request is not retryable",
  { files, turns: [say("First.", FULL), { status: 401, body: { error: { message: "No auth credentials found" } } }] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "One")
    await ctx.prompt(id, "Two")
    await messages(ctx, id)
  },
)

scenario(
  "compaction summary request rejected as too long",
  {
    files,
    turns: [say("One.", { prompt: 1000 }), call("read", { path: "README.md" }), say("Two.", FULL), contextOverflow, SUMMARY, say("Three.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    await ctx.prompt(id, "Second, read the README")
    await ctx.prompt(id, "Third")
    await messages(ctx, id)
  },
)

scenario(
  "compaction summary request rejected as too long until it gives up",
  {
    files,
    turns: [say("One.", { prompt: 1000 }), say("Two.", FULL), contextOverflow, contextOverflow, contextOverflow, contextOverflow, contextOverflow],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    await ctx.prompt(id, "Second")
    await ctx.prompt(id, "Third")
    await messages(ctx, id)
  },
)

scenario(
  "compaction summary request rejected as payload too large",
  {
    files,
    turns: [
      say("One.", { prompt: 1000 }),
      say("Two.", FULL),
      { status: 413, body: { error: { message: "Request entity too large" } } },
      SUMMARY,
      say("Three."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    await ctx.prompt(id, "Second")
    await ctx.prompt(id, "Third")
    await messages(ctx, id)
  },
)

scenario(
  "context overflow recovered by compaction",
  { files, turns: [say("One.", { prompt: 1000 }), call("read", { path: "README.md" }), contextOverflow, SUMMARY, say("After compaction.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    await ctx.prompt(id, "Read the README")
    await messages(ctx, id)
  },
)

scenario(
  "context overflow again after compaction fails the turn",
  { files, turns: [say("One.", { prompt: 1000 }), contextOverflow, SUMMARY, contextOverflow] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    await ctx.prompt(id, "Second")
    await messages(ctx, id)
  },
)

scenario(
  "context overflow with automatic compaction disabled",
  { config: { compaction: { auto: false } }, turns: [say("One.", FULL), contextOverflow] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    await ctx.prompt(id, "Second")
    await messages(ctx, id)
  },
)

scenario(
  "compaction threshold from a percentage buffer",
  { config: { compaction: { buffer: "50%" } }, turns: [say("One.", { prompt: 600_000 }), SUMMARY, say("Two.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    await ctx.prompt(id, "Second")
    await messages(ctx, id)
  },
)

scenario(
  "compaction threshold from a token buffer and kept tokens",
  {
    config: { compaction: { buffer: 500_000, keep: { tokens: 0 } } },
    turns: [say("One.", { prompt: 1000 }), say("Two.", { prompt: 600_000 }), SUMMARY, say("Three.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    await ctx.prompt(id, "Second")
    await ctx.prompt(id, "Third")
    await messages(ctx, id)
  },
)

scenario(
  "manual compaction",
  { files, turns: [call("read", { path: "README.md" }), say("A demo."), SUMMARY, say("Still a demo.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "What is this?")
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/compact`, {})
    await ctx.settle(id, from)
    await ctx.prompt(id, "Remind me")
    await messages(ctx, id)
  },
)

scenario("manual compaction of an empty session", {}, async (ctx) => {
  const id = await ctx.session({ title: "compaction" })
  const from = ctx.events.length
  await ctx.http("POST", `/api/session/${id}/compact`, {})
  await ctx.settle(id, from)
  await messages(ctx, id)
})

scenario(
  "manual compaction fails",
  { files, turns: [say("A demo."), { status: 400, body: { error: { message: "Invalid request" } } }] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "What is this?")
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/compact`, {})
    await ctx.settle(id, from)
    await messages(ctx, id)
  },
)

scenario(
  "second compaction updates the checkpoint",
  {
    files,
    turns: [say("One.", { prompt: 1000 }), SUMMARY, say("Two.", { prompt: 2000 }), say("Three.", { prompt: 3000 }), SUMMARY, say("Four.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    let from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/compact`, {})
    await ctx.settle(id, from)
    await ctx.prompt(id, "Second")
    await ctx.prompt(id, "Third")
    from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/compact`, {})
    await ctx.settle(id, from)
    await ctx.prompt(id, "Fourth")
    await messages(ctx, id)
  },
)

scenario(
  "legacy checkpoint is rewritten",
  { files, turns: [say("One."), LEGACY_SUMMARY, say("Two."), SUMMARY] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    let from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/compact`, {})
    await ctx.settle(id, from)
    await ctx.prompt(id, "Second")
    from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/compact`, {})
    await ctx.settle(id, from)
    await messages(ctx, id)
  },
)

scenario(
  "compaction after a checkpoint, as text",
  {
    files,
    turns: [say("One."), SUMMARY, say("Two.", { prompt: 2000 }), say("Three.", FULL), contextOverflow, SUMMARY, say("Four.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/compact`, {})
    await ctx.settle(id, from)
    await ctx.prompt(id, "Second")
    await ctx.prompt(id, "Third")
    await ctx.prompt(id, "Fourth")
    await messages(ctx, id)
  },
)

scenario(
  "background summary is swapped in",
  {
    files,
    config: { experimental: { background_compaction: true } },
    turns: [say("One.", { prompt: 1000 }), say("Two.", NEAR), say("Three.", FULL), say("Four.")],
    responders: [summaries()],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    const from = ctx.events.length
    await ctx.prompt(id, "Second")
    // The background summary starts once the session goes idle near the threshold; its usage is billed when it ends.
    await usageUpdates(ctx, from, 2)
    await ctx.prompt(id, "Third")
    await ctx.prompt(id, "Fourth")
    await messages(ctx, id)
  },
)

scenario(
  "background summary that fails is not used",
  {
    files,
    config: { experimental: { background_compaction: true } },
    turns: [say("One.", { prompt: 1000 }), say("Two.", NEAR), say("Three.", FULL), say("Four.")],
    responders: [summaries(say("No template here."), say("Still none."))],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    const from = ctx.events.length
    await ctx.prompt(id, "Second")
    // The summary and its nudge both ignore the template, so the background summary fails.
    await usageUpdates(ctx, from, 3)
    await ctx.prompt(id, "Third")
    await ctx.prompt(id, "Fourth")
    await messages(ctx, id)
  },
)

scenario(
  "background summary replaced after it fails",
  {
    files,
    config: { experimental: { background_compaction: true } },
    turns: [say("One.", { prompt: 1000 }), say("Two.", NEAR), say("Three.", { prompt: 860_000 }), say("Four.", FULL), say("Five.")],
    responders: [summaries(say("No template here."), say("Still none."))],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    let from = ctx.events.length
    await ctx.prompt(id, "Second")
    // The first background summary fails (it ignores the template twice).
    await usageUpdates(ctx, from, 3)
    from = ctx.events.length
    await ctx.prompt(id, "Third")
    // A newer response replaces the failed summary with a fresh one.
    await usageUpdates(ctx, from, 2)
    await ctx.prompt(id, "Fourth")
    await ctx.prompt(id, "Fifth")
    await messages(ctx, id)
  },
)

scenario(
  "background summary is dropped on a context overflow",
  {
    files,
    config: { experimental: { background_compaction: true } },
    turns: [say("One.", { prompt: 1000 }), say("Two.", NEAR), contextOverflow, say("Three.")],
    responders: [summaries()],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    const from = ctx.events.length
    await ctx.prompt(id, "Second")
    await usageUpdates(ctx, from, 2)
    await ctx.prompt(id, "Third")
    await messages(ctx, id)
  },
)

scenario(
  "compaction past the summary room sends the history without the recent tail",
  {
    files,
    turns: [say("One.", { prompt: 1000 }), say("Two.", { prompt: 2000 }), say("Three.", { prompt: 1_040_000 }), SUMMARY, say("Four.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First question")
    await ctx.prompt(id, "Second question")
    await ctx.prompt(id, "Third question")
    await ctx.prompt(id, "Fourth question")
    await messages(ctx, id)
  },
)

scenario(
  "manual compaction rejected as too long cannot shrink further",
  { files, turns: [say("A demo."), contextOverflow] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "What is this?")
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/compact`, {})
    await ctx.settle(id, from)
    await messages(ctx, id)
  },
)

scenario(
  "compaction summary with an unknown finish reason, then succeeds",
  {
    timeout: 30_000,
    files,
    turns: [
      say("First.", FULL),
      {
        raw: [
          ": OPENROUTER PROCESSING\n\n",
          `data: ${JSON.stringify({ id: "gen-raw", object: "chat.completion.chunk", created: 1_767_225_600, model: "deepseek/deepseek-v4.1-flash-1017", choices: [{ index: 0, delta: { content: "## Objective", role: "assistant" }, finish_reason: null }] })}\n\n`,
          `data: ${JSON.stringify({ id: "gen-raw", object: "chat.completion.chunk", created: 1_767_225_600, model: "deepseek/deepseek-v4.1-flash-1017", choices: [{ index: 0, delta: { content: "" }, finish_reason: "mystery" }] })}\n\n`,
          "data: [DONE]\n\n",
        ].join(""),
      },
      SUMMARY,
      say("Second."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "One")
    await ctx.prompt(id, "Two")
    await messages(ctx, id)
  },
)

const long = (label: string) =>
  Array.from({ length: 700 }, (_, index) => `${label} line ${String(index).padStart(3, "0")}: ${"lorem ipsum dolor sit amet ".repeat(2)}`).join("\n") + "\n"

scenario(
  "overflow compaction shortens a long transcript",
  {
    files: { ...files, "docs/one.txt": long("one"), "docs/two.txt": long("two"), "docs/three.txt": long("three") },
    turns: [
      call("read", { path: "docs/one.txt" }),
      call("read", { path: "docs/two.txt" }),
      call("read", { path: "docs/three.txt" }),
      say("Read all three.", { prompt: 30_000 }),
      contextOverflow,
      SUMMARY,
      say("Answered after compaction."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "Read the three docs")
    await ctx.prompt(id, "Now compare them")
    await messages(ctx, id)
  },
)

scenario(
  "manual compaction after switching variants",
  { files, turns: [say("One.", { prompt: 5000 }), SUMMARY] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction" })
    await ctx.prompt(id, "First")
    await ctx.http("POST", `/api/session/${id}/model`, {
      model: { providerID: "openrouter", id: "deepseek/deepseek-v4.1-flash-1017", variant: "low" },
    })
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/compact`, {})
    await ctx.settle(id, from)
    await messages(ctx, id)
  },
)

scenario(
  "manual compaction with an unavailable model",
  { files, turns: [] },
  async (ctx) => {
    const id = await ctx.session({ title: "compaction", model: { providerID: "openrouter", id: "missing/model" } })
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/compact`, {})
    await ctx.settle(id, from)
    await messages(ctx, id)
  },
)
