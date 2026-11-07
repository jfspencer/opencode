// What shapes the request: the agent (build vs plan), the model and its reasoning variant, the maximum step limit,
// prompt attachments, title generation, and one-shot generation outside the loop.
import { call, say, type Turn } from "../lib/openrouter"
import { MODEL_ID } from "../lib/model"
import { scenario, type Ctx } from "../lib/scenario"

const files = { "README.md": "# Demo\n\nA small project.\n", "src/math.ts": "export const add = (a: number, b: number) => a + b\n" }
const model = (variant?: string) => ({ providerID: "openrouter", id: MODEL_ID, ...(variant ? { variant } : {}) })

async function messages(ctx: Ctx, id: string) {
  await ctx.http("GET", `/api/session/${id}`)
  await ctx.http("GET", `/api/session/${id}/message`)
}

scenario(
  "reasoning variants change the request",
  { turns: [say("max"), say("none"), say("low"), say("high"), say("default")] },
  async (ctx) => {
    const id = await ctx.session({ title: "variants" })
    await ctx.prompt(id, "Default variant")
    for (const variant of ["none", "low", "high"]) {
      await ctx.http("POST", `/api/session/${id}/model`, { model: model(variant) })
      await ctx.prompt(id, `Variant ${variant}`)
    }
    await ctx.http("POST", `/api/session/${id}/model`, { model: model() })
    await ctx.prompt(id, "No variant")
    await messages(ctx, id)
  },
)

scenario(
  "reasoning from an earlier variant is replayed",
  {
    turns: [
      { events: [{ reasoning: "Deep thought." }, { text: "Thought hard." }] },
      { events: [{ text: "Quick answer." }] },
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "variants" })
    await ctx.prompt(id, "Think")
    await ctx.http("POST", `/api/session/${id}/model`, { model: model("none") })
    await ctx.prompt(id, "Answer quickly")
    await messages(ctx, id)
  },
)

scenario(
  "session created with a variant",
  { turns: [say("Low effort.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "variants", model: model("low") })
    await ctx.prompt(id, "Hello")
    await messages(ctx, id)
  },
)

scenario(
  "session with an unavailable model",
  { turns: [say("Never sent.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "variants", model: { providerID: "openrouter", id: "missing/model" } })
    await ctx.prompt(id, "Hello")
    await messages(ctx, id)
  },
)

scenario(
  "session with an unknown variant",
  { turns: [say("Never sent.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "variants", model: model("ultra") })
    await ctx.prompt(id, "Hello")
    await messages(ctx, id)
  },
)

scenario(
  "plan agent request",
  // Plan mode adds a reminder to the request and persists it as synthetic input, which takes one more step.
  { files, turns: [say("Here is the plan."), say("Still planning.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "plan", agent: "plan" })
    await ctx.prompt(id, "Plan a refactor")
    await messages(ctx, id)
  },
)

scenario(
  "switching from plan to build",
  {
    files,
    turns: [
      say("Plan: edit math."),
      say("Still planning."),
      call("edit", { path: "src/math.ts", oldString: "a + b", newString: "b + a" }),
      say("Edited."),
      say("Done building."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "plan", agent: "plan" })
    await ctx.prompt(id, "Plan the change")
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/agent`, { agent: "build" })
    // Leaving Plan mode queues a reminder in the background; let it land before the next prompt.
    await ctx.until((event) => event?.type === "session.inbox.enqueued" && event?.data?.item?.type === "synthetic", from)
    await ctx.prompt(id, "Now do it")
    await messages(ctx, id)
  },
)

scenario(
  "plan agent cannot edit",
  { files, turns: [call("edit", { path: "src/math.ts", oldString: "a + b", newString: "b + a" }), say("Cannot edit in plan mode.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "plan", agent: "plan" })
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Edit math" })
    const state = await ctx.settle(id, from)
    if (state === "permission.asked") {
      const pending = await ctx.http("GET", `/api/session/${id}/permission`)
      await ctx.http("POST", `/api/session/${id}/permission/${pending.body.data[0].id}/reply`, { decision: "reject", message: "No edits while planning." })
      await ctx.settle(id)
    }
    await messages(ctx, id)
  },
)

scenario(
  "agent with a custom system prompt",
  { config: { agents: { reviewer: { mode: "primary", system: "You review code. Never edit files.", description: "Reviews code" } } }, turns: [say("Looks fine.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "agents", agent: "reviewer" })
    await ctx.prompt(id, "Review the code")
    await messages(ctx, id)
  },
)

scenario(
  "maximum steps reached",
  {
    files,
    config: { agents: { build: { steps: 2 } } },
    turns: [call("read", { path: "README.md" }), say("I reached the step limit. Read the README so far.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "steps" })
    await ctx.prompt(id, "Read everything")
    await messages(ctx, id)
  },
)

scenario(
  "tool call after the maximum steps",
  {
    files,
    config: { agents: { build: { steps: 2 } } },
    turns: [call("read", { path: "README.md" }), call("read", { path: "src/math.ts" })],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "steps" })
    await ctx.prompt(id, "Read everything")
    await messages(ctx, id)
  },
)

scenario(
  "maximum steps reset on the next prompt",
  {
    files,
    config: { agents: { build: { steps: 1 } } },
    turns: [say("First, without tools."), say("Second, without tools.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "steps" })
    await ctx.prompt(id, "One")
    await ctx.prompt(id, "Two")
    await messages(ctx, id)
  },
)

function gate() {
  let open!: () => void
  const promise = new Promise<void>((resolve) => (open = resolve))
  return { promise, open }
}

/**
 * Title generation runs beside the first step. The step's stream is held until the title has settled, so the title's
 * events always land before the step's.
 */
async function untitled(ctx: Ctx, held: ReturnType<typeof gate>, settled: () => Promise<unknown>) {
  const id = await ctx.session()
  const from = ctx.events.length
  await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Help me refactor the math module" })
  await settled()
  held.open()
  await ctx.settle(id, from)
  await messages(ctx, id)
}

const renamed = (ctx: Ctx) => ctx.until((event) => event?.type === "session.renamed")

const titleGate = gate()
scenario("title generated for an untitled session", { turns: [{ events: [{ until: titleGate.promise }, { text: "Hello there." }] }] }, (ctx) =>
  untitled(ctx, titleGate, () => renamed(ctx)),
)

const failedTitleGate = gate()
scenario(
  "title generation fails",
  {
    turns: [{ events: [{ until: failedTitleGate.promise }, { text: "Hello there." }] }],
    responders: [(request) => (request.kind === "title" ? ({ status: 400, body: { error: { message: "Bad title request" } } } as Turn) : undefined)],
  },
  (ctx) =>
    untitled(ctx, failedTitleGate, async () => {
      await ctx.until(() => ctx.provider.requests.some((request) => request.kind === "title"))
      await new Promise((resolve) => setTimeout(resolve, 300))
    }),
)

const lineTitleGate = gate()
scenario(
  "title is the first non-empty line of the reply",
  {
    turns: [{ events: [{ until: lineTitleGate.promise }, { text: "Hello there." }] }],
    responders: [
      (request) =>
        request.kind === "title"
          ? ({ events: [{ reasoning: "A short title." }, { text: "\n\n  Refactor math module  \nsecond line" }] } as Turn)
          : undefined,
    ],
  },
  (ctx) => untitled(ctx, lineTitleGate, () => renamed(ctx)),
)

scenario(
  "generate text from session context",
  { turns: [say("A demo."), say("A generated one-liner.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "generate" })
    await ctx.prompt(id, "What is this?")
    await ctx.http("POST", `/api/session/${id}/generate`, { prompt: "Summarize the conversation in one line." })
    await messages(ctx, id)
  },
)

scenario(
  "generate text from session context fails",
  { turns: [say("A demo."), { status: 401, body: { error: { message: "No auth credentials found" } } }] },
  async (ctx) => {
    const id = await ctx.session({ title: "generate" })
    await ctx.prompt(id, "What is this?")
    await ctx.http("POST", `/api/session/${id}/generate`, { prompt: "Summarize the conversation in one line." })
  },
)

scenario(
  "generate text without a session",
  { turns: [say("Generated text."), say("With a model.")] },
  async (ctx) => {
    await ctx.http("POST", "/api/experimental/generate/text", { prompt: "Write a haiku about tests." })
    await ctx.http("POST", "/api/experimental/generate/text", { prompt: "Again.", model: model("low") })
  },
)

scenario("generate text with unknown models", {}, async (ctx) => {
  await ctx.http("POST", "/api/experimental/generate/text", { prompt: "Hi.", model: { providerID: "openrouter", id: "missing/model" } })
  await ctx.http("POST", "/api/experimental/generate/text", { prompt: "Hi.", model: model("ultra") })
  await ctx.http("POST", "/api/experimental/generate/decisions", {
    model: model(),
    input: [{ type: "text", text: "Is this spam?" }],
    questions: [{ type: "predicate", name: "spam", instructions: "Is it spam?" }],
  })
})

scenario(
  "generate text fails",
  {
    timeout: 30_000,
    turns: [
      { status: 401, body: { error: { message: "No auth credentials found" } } },
      { status: 400, body: { error: { message: "Invalid request" } } },
      { status: 503, body: { error: { message: "Overloaded" } }, headers: { "retry-after": "1" } },
      say("Recovered."),
      { status: 429, body: { error: { message: "Rate limited" } }, headers: { "x-should-retry": "false" } },
      { status: 400, body: { error: { message: "Blocked", code: "content_policy_violation" } } },
    ],
  },
  async (ctx) => {
    for (let i = 0; i < 5; i++) await ctx.http("POST", "/api/experimental/generate/text", { prompt: `Attempt ${i}` })
  },
)

const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="
const PNG_2 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg=="
const text = (value: string) => Buffer.from(value).toString("base64")

scenario(
  "prompt with a text file attachment",
  { files, turns: [say("It is a README.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "attachments" })
    await ctx.prompt(id, "What is in this file?", {
      files: [{ uri: `file://${ctx.project}/README.md`, description: "The project readme" }],
    })
    await messages(ctx, id)
  },
)

scenario(
  "prompt with a directory attachment",
  { files, turns: [say("It has sources.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "attachments" })
    await ctx.prompt(id, "What is in this directory?", { files: [{ uri: `file://${ctx.project}/src` }] })
    await messages(ctx, id)
  },
)

scenario(
  "prompt with inline attachments",
  { turns: [say("Two inline files.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "attachments" })
    await ctx.prompt(id, "Read these", {
      files: [
        { uri: `data:text/plain;base64,${text("inline notes\nsecond line\n")}`, name: "notes.txt" },
        { uri: `data:text/plain;base64,${text("unnamed")}` },
        { uri: `data:application/zip;base64,${text("PK not really")}`, name: "archive.zip" },
      ],
    })
    await messages(ctx, id)
  },
)

scenario(
  "prompt with image attachments",
  { files, turns: [say("Two pixels.")] },
  async (ctx) => {
    await Bun.write(`${ctx.project}/pixel.png`, Buffer.from(PNG, "base64"))
    const id = await ctx.session({ title: "attachments" })
    await ctx.prompt(id, "Describe @a and @b", {
      files: [
        { uri: `data:image/png;base64,${PNG}`, name: "a.png", mention: { start: 9, end: 11, text: "@a" } },
        { uri: `file://${ctx.project}/pixel.png`, mention: { start: 16, end: 18, text: "@b" } },
      ],
    })
    await messages(ctx, id)
  },
)

scenario(
  "repeated inline images are sent once",
  { turns: [say("Three mentions, two images.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "attachments" })
    await ctx.prompt(id, "Compare @x @x @y", {
      files: [
        { uri: `data:image/png;base64,${PNG}`, name: "x.png", mention: { start: 8, end: 10, text: "@x" } },
        { uri: `data:image/png;base64,${PNG}`, name: "x.png", mention: { start: 11, end: 13, text: "@x" } },
        { uri: `data:image/png;base64,${PNG_2}`, name: "y.png", mention: { start: 14, end: 16, text: "@y" } },
      ],
    })
    await messages(ctx, id)
  },
)

scenario(
  "prompt with a PDF attachment",
  { turns: [say("A PDF.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "attachments" })
    await ctx.prompt(id, "Read the PDF", {
      files: [{ uri: `data:application/pdf;base64,${text("%PDF-1.4\n%%EOF\n")}`, name: "doc.pdf" }],
    })
    await messages(ctx, id)
  },
)

scenario(
  "PDF attachment to a model that reads PDFs",
  {
    config: { providers: { openrouter: { models: { [MODEL_ID]: { capabilities: { input: ["text", "image/png", "pdf"] } } } } } },
    turns: [say("A PDF.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "attachments" })
    await ctx.prompt(id, "Read the PDF", {
      files: [{ uri: `data:application/pdf;base64,${text("%PDF-1.4\n%%EOF\n")}`, name: "doc.pdf" }],
    })
    await messages(ctx, id)
  },
)

scenario(
  "prompt with a missing attachment",
  { turns: [say("Never sent.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "attachments" })
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Read it", files: [{ uri: `file://${ctx.project}/missing.txt` }] })
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Read it", files: [{ uri: "https://example.com/file.txt" }] })
    await messages(ctx, id)
  },
)

scenario(
  "attachments are replayed on later steps",
  { files, turns: [call("read", { path: "src/math.ts" }), say("Compared.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "attachments" })
    await ctx.prompt(id, "Compare this picture with math.ts", {
      files: [{ uri: `data:image/png;base64,${PNG}`, name: "a.png" }],
    })
    await messages(ctx, id)
  },
)

scenario(
  "model settings become OpenRouter request options",
  {
    config: {
      providers: {
        openrouter: {
          models: {
            [MODEL_ID]: {
              settings: { usage: false, user: "conformance-user", provider: { order: ["DeepSeek"], allow_fallbacks: false }, transforms: ["middle-out"] },
              variants: [{ id: "budget", settings: { reasoning: { max_tokens: 500_000 } } }],
            },
          },
        },
      },
    },
    turns: [say("With options."), say("With a thinking budget.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "options" })
    await ctx.prompt(id, "Default")
    await ctx.http("POST", `/api/session/${id}/model`, { model: model("budget") })
    await ctx.prompt(id, "Budget")
    await messages(ctx, id)
  },
)

scenario(
  "switching to another model drops replayed reasoning",
  {
    config: {
      providers: {
        openrouter: {
          models: {
            "deepseek/deepseek-v4.1-pro": {
              name: "DeepSeek V4.1 Pro",
              family: "deepseek-pro",
              package: "@opencode/ai/providers/openrouter",
              capabilities: { tools: true, input: ["text"], output: ["text"], temperature: true, reasoning: { supported: true, effort: ["low", "high"] } },
              limit: { context: 131072, output: 32768 },
            },
          },
        },
      },
    },
    turns: [
      { events: [{ reasoning: "Flash reasoning." }, { text: "Flash answer." }] },
      { events: [{ reasoning: "" }, { text: "Empty reasoning." }] },
      say("Pro answer."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "models" })
    await ctx.prompt(id, "First")
    await ctx.prompt(id, "Second")
    await ctx.http("POST", `/api/session/${id}/model`, { model: { providerID: "openrouter", id: "deepseek/deepseek-v4.1-pro", variant: "high" } })
    await ctx.prompt(id, "Third")
    await messages(ctx, id)
  },
)

scenario(
  "no model available",
  { config: { policies: [{ action: "provider.use", resource: "*", effect: "deny" }] }, turns: [say("Never sent.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "models" })
    await ctx.prompt(id, "Hello")
    await messages(ctx, id)
  },
)

scenario(
  "reading an image file sends it after the tool result",
  { files, turns: [call("read", { path: "pixel.png" }), say("A single pixel.")] },
  async (ctx) => {
    await Bun.write(`${ctx.project}/pixel.png`, Buffer.from(PNG, "base64"))
    const id = await ctx.session({ title: "attachments" })
    await ctx.prompt(id, "What is in pixel.png?")
    await messages(ctx, id)
  },
)

scenario(
  "instruction entries change between prompts",
  { files, turns: [say("First."), say("Second."), say("Generated with the update."), say("Third.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "instructions" })
    await ctx.prompt(id, "One")
    await ctx.http("PUT", `/api/experimental/session/${id}/instructions/entries/style`, { value: "Answer tersely." })
    await ctx.prompt(id, "Two")
    await ctx.http("PUT", `/api/experimental/session/${id}/instructions/entries/style`, { value: "Answer at length." })
    await ctx.http("POST", `/api/session/${id}/generate`, { prompt: "Summarize in one line." })
    await ctx.http("DELETE", `/api/experimental/session/${id}/instructions/entries/style`)
    await ctx.prompt(id, "Three")
    await messages(ctx, id)
  },
)

scenario(
  "binary attachment of an unsupported type is dropped",
  { turns: [say("Nothing readable attached.")] },
  async (ctx) => {
    const zip = Buffer.from([0x50, 0x4b, 0x03, 0x04, 0x14, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x21, 0x00, 0xff, 0xfe]).toString("base64")
    const id = await ctx.session({ title: "attachments" })
    await ctx.prompt(id, "Open the archive", { files: [{ uri: `data:application/zip;base64,${zip}`, name: "archive.zip" }] })
    await messages(ctx, id)
  },
)

scenario(
  "instruction update after an image tool result",
  { files, turns: [call("read", { path: "pixel.png" }), { events: [] }, say("Noted the update.")] },
  async (ctx) => {
    await Bun.write(`${ctx.project}/pixel.png`, Buffer.from(PNG, "base64"))
    const id = await ctx.session({ title: "instructions" })
    await ctx.prompt(id, "Look at pixel.png")
    await ctx.http("PUT", `/api/experimental/session/${id}/instructions/entries/style`, { value: "Answer tersely." })
    await ctx.prompt(id, "Anything else?")
    await messages(ctx, id)
  },
)

scenario(
  "instruction update right after a prompt",
  { turns: [{ events: [] }, { events: [] }, say("Noted both updates.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "instructions" })
    await ctx.prompt(id, "Look at this", { files: [{ uri: `data:image/png;base64,${PNG}`, name: "a.png" }] })
    await ctx.http("PUT", `/api/experimental/session/${id}/instructions/entries/style`, { value: "Answer tersely." })
    await ctx.prompt(id, "Plain text this time")
    await ctx.http("PUT", `/api/experimental/session/${id}/instructions/entries/style`, { value: "Answer at length." })
    await ctx.prompt(id, "Anything else?")
    await messages(ctx, id)
  },
)

scenario(
  "request overlays from provider, model and agent config",
  {
    config: {
      providers: {
        openrouter: {
          headers: { "x-title": "Conformance Provider" },
          body: { provider: { sort: "price" } },
          models: { [MODEL_ID]: { body: { temperature: 0.25, top_p: 0.9 } } },
        },
      },
      agents: { build: { request: { headers: { "http-referer": "https://conformance.invalid" }, body: { seed: 7 } } } },
    },
    turns: [say("With overlays.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "overlays" })
    await ctx.prompt(id, "Hello")
    await messages(ctx, id)
  },
)
