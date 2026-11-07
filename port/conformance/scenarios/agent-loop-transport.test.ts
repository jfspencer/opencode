// Transport failures below HTTP status codes: the connection is refused or the host does not resolve (the request
// never reaches OpenRouter), the connection drops while the response is read, and the response stalls past the
// provider's chunk or whole-request timeout. The fake model has no turn shape for these, so they are produced here:
// a responder that throws stands for a failed connection, and a response body that errors stands for a dropped one.
import { say, type ProviderRequest, type Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

async function turn(ctx: Ctx, text: string) {
  const id = await ctx.session({ title: "transport" })
  await ctx.prompt(id, text)
  await ctx.http("GET", `/api/session/${id}`)
  await ctx.http("GET", `/api/session/${id}/message`)
  return id
}

/** Fails the next `times` chat requests before any response, as a socket error with `code` would. */
function connectionFailure(code: string, message: string, times = 1) {
  let remaining = times
  return (request: ProviderRequest): Turn | undefined => {
    if (request.kind !== "chat" || remaining <= 0) return undefined
    remaining--
    throw Object.assign(new Error(message), { code })
  }
}

const encoder = new TextEncoder()
const chunk = (delta: Record<string, unknown>, finish: string | null = null) =>
  `data: ${JSON.stringify({
    id: "gen-raw",
    object: "chat.completion.chunk",
    created: 1_767_225_600,
    model: "deepseek/deepseek-v4.1-flash-1017",
    provider: "DeepSeek",
    choices: [{ index: 0, delta, finish_reason: finish, native_finish_reason: finish }],
  })}\n\n`

/** A response whose body delivers `text`, then fails as a reset connection would. */
function dropped(text: string): Turn {
  let sent = false
  const body = new ReadableStream<Uint8Array>({
    // Error on the read after the text, so the text is delivered first.
    pull(controller) {
      if (sent) return controller.error(Object.assign(new Error("socket hang up"), { code: "ECONNRESET" }))
      sent = true
      controller.enqueue(encoder.encode(text))
    },
  })
  // The fake passes `raw` straight to the Response body, so a stream works where a string is declared.
  return { raw: body as unknown as string }
}

function gate() {
  let open!: () => void
  const promise = new Promise<void>((resolve) => (open = resolve))
  return { promise, open }
}

const RETRY = { timeout: 30_000 }

scenario(
  "connection refused, then success",
  { ...RETRY, turns: [say("Connected on the second attempt.")], responders: [connectionFailure("ECONNREFUSED", "connect ECONNREFUSED 127.0.0.1:443")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "host does not resolve, then success",
  { ...RETRY, turns: [say("Resolved on the second attempt.")], responders: [connectionFailure("ENOTFOUND", "getaddrinfo ENOTFOUND openrouter.ai")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "connection failure without a code, then success",
  { ...RETRY, turns: [say("Second attempt.")], responders: [connectionFailure(undefined as unknown as string, "fetch failed")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "connection dropped before any output, then success",
  { ...RETRY, turns: [dropped(": OPENROUTER PROCESSING\n\n"), say("Second attempt.")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "connection dropped after output, then continued",
  { ...RETRY, turns: [dropped(chunk({ content: "The first half", role: "assistant" })), say(" and the rest.")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

const stalled = gate()
scenario(
  "response stalls past the chunk timeout, then success",
  {
    ...RETRY,
    config: { providers: { openrouter: { settings: { chunkTimeout: 300 } } } },
    turns: [{ events: [{ until: stalled.promise }, { text: "Too late." }] }, say("Second attempt.")],
  },
  async (ctx) => {
    await turn(ctx, "Answer please")
    stalled.open()
  },
)

const stalledAfter = gate()
scenario(
  "response stalls after output past the chunk timeout, then continued",
  {
    ...RETRY,
    config: { providers: { openrouter: { settings: { chunkTimeout: 300 } } } },
    turns: [{ events: [{ text: "Started" }, { until: stalledAfter.promise }, { text: " never." }] }, say(" and finished.")],
  },
  async (ctx) => {
    await turn(ctx, "Answer please")
    stalledAfter.open()
  },
)

const slow = gate()
scenario(
  "response exceeds the whole-request timeout, then success",
  {
    ...RETRY,
    config: { providers: { openrouter: { settings: { timeout: 400 } } } },
    turns: [{ events: [{ until: slow.promise }, { text: "Too late." }] }, say("Second attempt.")],
  },
  async (ctx) => {
    await turn(ctx, "Answer please")
    slow.open()
  },
)

scenario(
  "one-shot generation retries a refused connection",
  { ...RETRY, turns: [say("Generated after reconnecting.")], responders: [connectionFailure("ECONNREFUSED", "connect ECONNREFUSED 127.0.0.1:443")] },
  async (ctx) => {
    await ctx.http("POST", "/api/experimental/generate/text", { prompt: "Write one line." })
  },
)

scenario(
  "one-shot generation does not retry a dropped connection",
  { turns: [dropped(chunk({ content: "Partial", role: "assistant" }))] },
  async (ctx) => {
    await ctx.http("POST", "/api/experimental/generate/text", { prompt: "Write one line." })
  },
)
