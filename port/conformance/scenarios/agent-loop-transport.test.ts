// Transport failures below HTTP status codes: the connection is refused or the host does not resolve (the request
// never reaches OpenRouter), the connection closes before the response or drops while it is read, and the response
// stalls past the provider's chunk or whole-request timeout. The fake model has no turn shape for these, so they
// are produced here. In-process, a responder that throws stands for a connection that fails and a response body
// that errors for one that drops, each throwing exactly what Bun's fetch throws for the real failure. Against a
// process, the fake network turns both into the real thing (lib/network.ts: the connection is closed), so the
// implementation's own HTTP client reports them.
//
// A client reaches the fake network through a proxy, so a refused connection or a name that does not resolve
// can't be given to a process: those scenarios run in-process only.
import { say, type ProviderRequest, type Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

async function turn(ctx: Ctx, text: string) {
  const id = await ctx.session({ title: "transport" })
  await ctx.prompt(id, text)
  await ctx.http("GET", `/api/session/${id}`)
  await ctx.http("GET", `/api/session/${id}/message`)
  return id
}

// What Bun's fetch throws for each failure (observed against real sockets).
const failure = (code: string, message: string) => Object.assign(new TypeError(message), { code })
const reset = () =>
  failure(
    "ECONNRESET",
    "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()",
  )
const refused = () => failure("ConnectionRefused", "Unable to connect. Is the computer able to access the url?")
const unresolved = () => failure("ENOTFOUND", "getaddrinfo ENOTFOUND openrouter.ai")
const PROXIED = "a client behind the fake network's proxy never sees a refused connection or a failed name lookup"

/** Fails the next `times` chat requests before any response. */
function failing(error: () => Error, times = 1) {
  let remaining = times
  return (request: ProviderRequest): Turn | undefined => {
    if (request.kind !== "chat" || remaining <= 0) return undefined
    remaining--
    throw error()
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

/**
 * A response whose body delivers `text`, then fails as a reset connection would. With `received`, the failure
 * waits for it: a client may drop bytes it has not read yet when the connection ends, so a scenario about output
 * before the drop waits until the implementation has taken the output in.
 */
function dropped(text: string, received?: Promise<unknown>): Turn {
  let sent = false
  const body = new ReadableStream<Uint8Array>({
    // Error on the read after the text, so the text is delivered first.
    async pull(controller) {
      if (sent) {
        await received
        return controller.error(reset())
      }
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
  { ...RETRY, inProcessOnly: PROXIED, turns: [say("Connected on the second attempt.")], responders: [failing(refused)] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "host does not resolve, then success",
  { ...RETRY, inProcessOnly: PROXIED, turns: [say("Resolved on the second attempt.")], responders: [failing(unresolved)] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "connection closed before the response, then success",
  { ...RETRY, turns: [say("Second attempt.")], responders: [failing(reset)] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "connection dropped before any output, then success",
  { ...RETRY, turns: [dropped(": OPENROUTER PROCESSING\n\n"), say("Second attempt.")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

const firstHalf = gate()
scenario(
  "connection dropped after output, then continued",
  {
    ...RETRY,
    turns: [dropped(chunk({ content: "The first half", role: "assistant" }), firstHalf.promise), say(" and the rest.")],
  },
  (ctx) => {
    // The text has been taken in once its part starts.
    void ctx.until((event) => event?.type === "session.text.started", 0, 30_000).then(firstHalf.open, firstHalf.open)
    return turn(ctx, "Answer please").then(() => undefined)
  },
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
  { ...RETRY, inProcessOnly: PROXIED, turns: [say("Generated after reconnecting.")], responders: [failing(refused)] },
  async (ctx) => {
    await ctx.http("POST", "/api/experimental/generate/text", { prompt: "Write one line." })
  },
)

scenario(
  "one-shot generation and a connection closed before the response",
  { ...RETRY, turns: [say("Generated after reconnecting.")], responders: [failing(reset)] },
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
