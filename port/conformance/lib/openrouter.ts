// Scripted stand-in for OpenRouter's chat-completions API, emitting the real wire format (modeled on
// packages/ai/test/fixtures/recordings/openrouter-*.json). It is a plain (Request) => Response handler:
// the TS reference gets it as its `fetch` option in-process, and the Rust implementation will get the same
// handler served over HTTP. Every outbound request is recorded, including ones the script doesn't expect.

import { STATUS_CODES } from "node:http"

export type StreamEvent =
  | { readonly reasoning: string }
  | { readonly text: string }
  | { readonly tool: string; readonly input: unknown; readonly id?: string }
  /** Raw tool-call argument text, e.g. malformed JSON. */
  | { readonly tool: string; readonly rawInput: string; readonly id?: string }
  | { readonly comment: string }
  /** Pause the stream until the promise settles (to observe in-flight state or interrupt). */
  | { readonly until: Promise<unknown> }

export interface Usage {
  readonly prompt?: number
  readonly completion?: number
  readonly reasoning?: number
  readonly cached?: number
}

export type Turn =
  | {
      readonly events: readonly StreamEvent[]
      readonly finish?: "stop" | "tool_calls" | "length" | "content_filter" | "error"
      readonly usage?: Usage | false
      /** Cut the stream without a finish chunk or [DONE]. */
      readonly truncate?: boolean
    }
  | { readonly status: number; readonly body?: unknown; readonly headers?: Record<string, string> }
  | {
      readonly raw: string
      readonly status?: number
      readonly headers?: Record<string, string>
      /** `raw` is base64: the body is the bytes it decodes to (binary files, such as images). */
      readonly base64?: boolean
    }

export interface ProviderRequest {
  readonly index: number
  readonly method: string
  readonly url: string
  readonly headers: Record<string, string>
  readonly body: unknown
  readonly kind: "chat" | "title" | "other"
}

export type Responder = (request: ProviderRequest) => Turn | undefined

const CHAT_PATH = "/api/v1/chat/completions"
const KEPT_HEADERS = ["authorization", "content-type", "http-referer", "x-title", "accept"]

/** Text turn helper. */
export const say = (text: string, usage?: Usage): Turn => ({ events: [{ text }], usage })
/** Tool-call turn helper. */
export const call = (tool: string, input: unknown, text?: string): Turn => ({
  events: [...(text ? [{ text }] : []), { tool, input }],
  finish: "tool_calls",
})

/** The default answer to title requests. */
const TITLE = say("Conformance session")
const EXHAUSTED: Turn = { status: 500, body: { error: { message: "conformance: script exhausted" } } }

export class FakeOpenRouter {
  readonly requests: ProviderRequest[] = []
  /** Title requests answered with a title, by session (prompt_cache_key): the scenario waits for their renames. */
  readonly titles: { readonly session: unknown; readonly answered: boolean }[] = []
  /**
   * Title generation runs beside a session's steps, so its events and its usage would land among the step's at
   * random. Set by the scenario (lib/scenario.ts), this orders it first: a session's first model response is
   * held until the session's generated title has been applied (or its title request failed). `expects` asks
   * the server whether the session is untitled, so the order holds for any implementation, whichever request
   * it sends first. Unset (CLI scenarios), nothing is held.
   */
  titleOrder?: { expects(session: string): Promise<boolean>; applied(session: string): Promise<void> }
  private readonly queue: Turn[] = []
  private counter = 0
  private readonly firstChat = new Set<string>()
  private readonly titleFailed = new Map<string, PromiseWithResolvers<void>>()

  constructor(
    turns: readonly Turn[] = [],
    private readonly responders: readonly Responder[] = [],
  ) {
    this.queue.push(...turns)
  }

  /** Queue more scripted turns for chat requests. */
  push(...turns: Turn[]) {
    this.queue.push(...turns)
  }

  readonly fetch = async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
    const request = input instanceof Request ? input : new Request(input, init)
    const url = new URL(request.url)
    const text = request.method === "GET" || request.method === "HEAD" ? "" : await request.text()
    const body = parseJson(text)
    const recorded: ProviderRequest = {
      index: this.requests.length,
      method: request.method,
      url: `${url.origin}${url.pathname}${url.search}`,
      headers: Object.fromEntries(
        [...request.headers]
          .filter(([key, value]) => KEPT_HEADERS.includes(key.toLowerCase()) && !isDefaultAccept(key, value))
          .map(([k, v]) => [k.toLowerCase(), v]),
      ),
      body,
      kind: url.pathname !== CHAT_PATH ? "other" : isTitleRequest(body) ? "title" : "chat",
    }
    this.requests.push(recorded)
    const session = isRecord(body) && typeof body.prompt_cache_key === "string" ? body.prompt_cache_key : undefined
    const scripted = this.script(recorded, session)
    if (!scripted) return new Response(JSON.stringify({ error: "conformance: network access denied" }), { status: 599 })
    if (recorded.kind === "title") {
      const answered = "events" in scripted && !scripted.truncate
      this.titles.push({ session, answered })
      if (!answered) this.titleFailure(session)?.resolve()
    }
    if (recorded.kind === "chat" && session) await this.titleFirst(session)
    return this.respond(scripted, body, request.method)
  }

  /** The turn for a request: a responder's, the default title, or the next scripted turn; none for other hosts. */
  private script(recorded: ProviderRequest, session: string | undefined): Turn | undefined {
    try {
      for (const responder of this.responders) {
        const turn = responder(recorded)
        if (turn) return turn
      }
    } catch (error) {
      // A responder that throws stands for a failed connection.
      if (recorded.kind === "title") this.titleFailure(session)?.resolve()
      throw error
    }
    if (recorded.kind === "title") return TITLE
    if (recorded.kind === "other") return undefined
    return this.queue.shift() ?? EXHAUSTED
  }

  private async titleFirst(session: string) {
    if (!this.titleOrder || this.firstChat.has(session)) return
    this.firstChat.add(session)
    if (!(await this.titleOrder.expects(session))) return
    // Bounded: a configuration may keep a title from being generated at all.
    await Promise.race([this.titleOrder.applied(session), this.titleFailure(session)!.promise, Bun.sleep(15_000)])
  }

  private titleFailure(session: string | undefined) {
    if (!session) return undefined
    const existing = this.titleFailed.get(session)
    if (existing) return existing
    const created = Promise.withResolvers<void>()
    this.titleFailed.set(session, created)
    return created
  }

  private respond(turn: Turn, body: unknown, method = "POST"): Response {
    // What a real server sends: the standard reason phrase, and no body for HEAD.
    const init = (status: number, headers: Record<string, string>) => ({ status, statusText: STATUS_CODES[status] ?? "", headers })
    if ("status" in turn && !("events" in turn) && !("raw" in turn))
      return new Response(method === "HEAD" || turn.body === undefined ? null : JSON.stringify(turn.body), {
        ...init(turn.status, { "content-type": "application/json", ...turn.headers }),
      })
    if ("raw" in turn)
      return new Response(method === "HEAD" ? null : turn.base64 ? Buffer.from(turn.raw, "base64") : turn.raw, {
        ...init(turn.status ?? 200, { "content-type": "text/event-stream", ...turn.headers }),
      })
    const id = `gen-conformance-${++this.counter}`
    const model = isRecord(body) && typeof body.model === "string" ? body.model : "unknown"
    const chunk = (delta: Record<string, unknown>, extra: Record<string, unknown> = {}) =>
      `data: ${JSON.stringify({
        id,
        object: "chat.completion.chunk",
        created: 1_767_225_600,
        model,
        provider: "DeepSeek",
        choices: [{ index: 0, delta, finish_reason: null, native_finish_reason: null, ...extra }],
      })}\n\n`
    const encoder = new TextEncoder()
    const events = turn.events
    const toolCount = events.filter((event) => "tool" in event).length
    const finish = turn.finish ?? (toolCount > 0 ? "tool_calls" : "stop")
    const usage = turn.usage === false ? undefined : (turn.usage ?? {})
    const stream = new ReadableStream<Uint8Array>({
      async start(controller) {
        const send = (text: string) => controller.enqueue(encoder.encode(text))
        send(": OPENROUTER PROCESSING\n\n")
        let toolIndex = 0
        for (const event of events) {
          if ("until" in event) {
            await event.until.catch(() => undefined)
            continue
          }
          if ("comment" in event) send(`: ${event.comment}\n\n`)
          if ("reasoning" in event)
            send(
              chunk({
                content: "",
                role: "assistant",
                reasoning: event.reasoning,
                reasoning_details: [{ type: "reasoning.text", text: event.reasoning, format: "unknown", index: 0 }],
              }),
            )
          if ("text" in event) send(chunk({ content: event.text, role: "assistant" }))
          if ("tool" in event) {
            const index = toolIndex++
            const args = "rawInput" in event ? event.rawInput : JSON.stringify(event.input)
            send(
              chunk({
                role: "assistant",
                tool_calls: [
                  { index, id: event.id ?? `call_conformance_${index}`, type: "function", function: { name: event.tool, arguments: "" } },
                ],
              }),
            )
            send(chunk({ role: "assistant", tool_calls: [{ index, function: { arguments: args } }] }))
          }
        }
        if (turn.truncate) return controller.close()
        send(chunk({ content: "", role: "assistant" }, { finish_reason: finish, native_finish_reason: finish }))
        if (usage) {
          const prompt = usage.prompt ?? 100
          const completion = usage.completion ?? 10
          send(
            `data: ${JSON.stringify({
              id,
              object: "chat.completion.chunk",
              created: 1_767_225_600,
              model,
              provider: "DeepSeek",
              choices: [{ index: 0, delta: { content: "", role: "assistant" }, finish_reason: finish, native_finish_reason: finish }],
              usage: {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: prompt + completion,
                cost: 0,
                is_byok: false,
                prompt_tokens_details: { cached_tokens: usage.cached ?? 0, cache_write_tokens: 0, audio_tokens: 0 },
                completion_tokens_details: { reasoning_tokens: usage.reasoning ?? 0, image_tokens: 0, audio_tokens: 0 },
              },
            })}\n\n`,
          )
        }
        send("data: [DONE]\n\n")
        controller.close()
      },
    })
    return new Response(stream, { status: 200, statusText: "OK", headers: { "content-type": "text/event-stream" } })
  }
}

function isTitleRequest(body: unknown) {
  if (!isRecord(body) || !Array.isArray(body.messages)) return false
  const system = body.messages.find((message) => isRecord(message) && message.role === "system")
  return isRecord(system) && typeof system.content === "string" && /\btitle\b/i.test(system.content.slice(0, 400))
}

// HTTP clients send `Accept: */*` when the caller sets none (Bun's fetch on the wire, reqwest); a request
// handed over in-process has no header at all. Both mean the same (RFC 9110 §12.5.1).
const isDefaultAccept = (key: string, value: string) => key.toLowerCase() === "accept" && value === "*/*"

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null

function parseJson(text: string): unknown {
  if (!text) return undefined
  try {
    return JSON.parse(text)
  } catch {
    return text
  }
}
