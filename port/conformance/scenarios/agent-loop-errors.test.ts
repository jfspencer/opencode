// Provider failures on the agent loop: HTTP errors, malformed and truncated streams, and the retry policy that
// follows them. Retries wait for real (2 s backoff with jitter, or longer when the provider asks for it); the
// scheduled time is normalized, the attempt number and the error are not.
import { say, type Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

async function turn(ctx: Ctx, text: string) {
  const id = await ctx.session({ title: "errors" })
  await ctx.prompt(id, text)
  await ctx.http("GET", `/api/session/${id}`)
  await ctx.http("GET", `/api/session/${id}/message`)
  return id
}

const base = {
  id: "gen-raw",
  object: "chat.completion.chunk",
  created: 1_767_225_600,
  model: "deepseek/deepseek-v4.1-flash-1017",
  provider: "DeepSeek",
}
const frame = (value: unknown) => `data: ${JSON.stringify(value)}\n\n`
const delta = (value: Record<string, unknown>, finish: string | null = null) =>
  frame({ ...base, choices: [{ index: 0, delta: value, finish_reason: finish, native_finish_reason: finish }] })
const raw = (...parts: string[]): Turn => ({ raw: [": OPENROUTER PROCESSING\n\n", ...parts].join("") })
const error = (status: number, body?: unknown, headers?: Record<string, string>): Turn => ({ status, body, headers })

const RETRY = { timeout: 30_000 }

scenario(
  "rate limited with retry-after, then success",
  {
    ...RETRY,
    turns: [
      error(429, { error: { message: "Rate limit exceeded: free-models-per-min", code: 429 } }, { "retry-after": "1" }),
      say("Answered after the rate limit."),
    ],
  },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "rate limited with rate limit headers, then success",
  {
    ...RETRY,
    turns: [
      error(
        429,
        { error: { message: "Too many requests" } },
        {
          "retry-after-ms": "500",
          "x-ratelimit-limit-requests": "20",
          "x-ratelimit-remaining-requests": "0",
          "x-ratelimit-reset-requests": "1s",
        },
      ),
      say("Answered."),
    ],
  },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "rate limited without retry-after, then success",
  { ...RETRY, turns: [error(429, { error: { message: "Rate limit exceeded" } }), say("Answered.")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "server error twice, then success",
  {
    timeout: 40_000,
    turns: [
      error(500, { error: { message: "Internal Server Error", code: 500 } }),
      error(502, { error: { message: "Provider returned error", code: 502, metadata: { provider_name: "DeepSeek" } } }),
      say("Third time lucky."),
    ],
  },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "service unavailable with an HTML page, then success",
  { ...RETRY, turns: [error(503, "<html><body>503 Service Unavailable</body></html>"), say("Back.")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "server error marked not retryable",
  { turns: [error(500, { error: { message: "Internal Server Error" } }, { "x-should-retry": "false" })] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "client error marked retryable, then success",
  { ...RETRY, turns: [error(400, { error: { message: "Bad request, try again" } }, { "x-should-retry": "true" }), say("Retried.")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "unauthorized",
  { turns: [error(401, { error: { message: "No auth credentials found", code: 401 } })] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "forbidden",
  { turns: [error(403, { error: { message: "Key limit exceeded", code: 403 } })] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "payment required",
  { turns: [error(402, { error: { message: "Insufficient credits", code: 402 } })] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "invalid request",
  { turns: [error(400, { error: { message: "Invalid value for temperature", code: 400 } })] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "invalid request without a body",
  { turns: [error(404)] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "content policy rejection",
  { turns: [error(400, { error: { message: "Request blocked", code: "content_policy_violation" } })] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "payload too large",
  { turns: [error(413, { error: { message: "Request entity too large" } })] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "plain text error body",
  { ...RETRY, turns: [error(500, "upstream connect error or disconnect/reset before headers"), say("Recovered.")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "malformed SSE chunk",
  { turns: [raw(delta({ content: "Partial", role: "assistant" }), "data: {not json\n\n")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "malformed SSE chunk before any output",
  { turns: [raw("data: {not json\n\n")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "stream truncated before any output, then success",
  { ...RETRY, turns: [{ events: [], truncate: true }, say("Second attempt.")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "stream truncated after text, then continued",
  { ...RETRY, turns: [{ events: [{ text: "The first half of" }], truncate: true }, say(" the answer.")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "stream truncated during a tool call, then continued",
  {
    ...RETRY,
    files: { "README.md": "# Demo\n" },
    turns: [{ events: [{ text: "Reading." }, { tool: "read", rawInput: '{"path": "READ' }], truncate: true }, say("Continued.")],
  },
  (ctx) => turn(ctx, "Read the README").then(() => undefined),
)

scenario(
  "stream ends after finish without DONE",
  { turns: [raw(delta({ content: "Complete answer.", role: "assistant" }), delta({ content: "" }, "stop"))] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "empty response body, then success",
  { ...RETRY, turns: [{ raw: "" }, say("Second attempt.")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "error frame before output, then success",
  {
    ...RETRY,
    turns: [raw(frame({ error: { message: "Upstream provider overloaded", code: 502 } })), say("Recovered.")],
  },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "error frame after output, then continued",
  {
    ...RETRY,
    turns: [
      raw(delta({ content: "Halfway", role: "assistant" }), frame({ ...base, error: { message: "Internal server error", code: "server_error" } })),
      say(" there."),
    ],
  },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "error frame reporting context overflow mid-stream",
  {
    turns: [
      raw(frame({ error: { message: "This endpoint's maximum context length is 1048576 tokens. However, you requested about 1100000 tokens.", code: 400 } })),
      say("## Objective\n- Answer the user\n\n## Next Move\n1. Answer"),
      say("Answer after compaction."),
    ],
  },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "content after the finish reason",
  {
    files: { "README.md": "# Demo\n" },
    turns: [
      raw(
        delta({ content: "Done.", role: "assistant" }, "stop"),
        delta({ tool_calls: [{ index: 0, id: "call_late", type: "function", function: { name: "read", arguments: "{}" } }] }),
        "data: [DONE]\n\n",
      ),
    ],
  },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "tool call without a name",
  {
    turns: [
      raw(
        delta({ tool_calls: [{ index: 0, id: "call_anonymous", type: "function", function: { arguments: "{}" } }] }),
        delta({ content: "" }, "tool_calls"),
        "data: [DONE]\n\n",
      ),
    ],
  },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "non-streaming JSON reply, then success",
  {
    ...RETRY,
    turns: [
      {
        raw: JSON.stringify({
          ...base,
          object: "chat.completion",
          choices: [{ index: 0, message: { role: "assistant", content: "Not streamed." }, finish_reason: "stop" }],
        }),
        headers: { "content-type": "application/json" },
      },
      say("Streamed."),
    ],
  },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "comments and blank frames are ignored",
  {
    turns: [
      raw(
        ": keep-alive\n\n",
        delta({ content: "Hello", role: "assistant" }),
        ": OPENROUTER PROCESSING\n\n",
        delta({ content: " world.", role: "assistant" }),
        delta({ content: "" }, "stop"),
        frame({ ...base, choices: [], usage: { prompt_tokens: 12, completion_tokens: 3, total_tokens: 15 } }),
        "data: [DONE]\n\n",
      ),
    ],
  },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "unrecognized error frame, then success",
  { ...RETRY, turns: [raw(frame({ error: { message: "Something odd happened upstream" } })), say("Recovered.")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "rate limited with a past retry-after date and per-provider headers",
  {
    ...RETRY,
    turns: [
      error(
        429,
        { error: { message: "Rate limit exceeded" } },
        {
          "retry-after": "Thu, 01 Jan 2026 00:00:00 GMT",
          "anthropic-ratelimit-requests-limit": "50",
          "anthropic-ratelimit-requests-remaining": "0",
          "anthropic-ratelimit-requests-reset": "2026-01-01T00:00:00Z",
        },
      ),
      error(429, { error: { message: "Rate limit exceeded" } }, { "retry-after": "soon" }),
      say("Answered."),
    ],
    timeout: 40_000,
  },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "rate limited by a quota",
  { turns: [error(429, { error: { message: "You exceeded your current quota, insufficient_quota" } })] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)

scenario(
  "server error with a retry-after header, then success",
  { ...RETRY, turns: [error(503, { error: { message: "Overloaded" } }, { "retry-after": "1" }), say("Back.")] },
  (ctx) => turn(ctx, "Answer please").then(() => undefined),
)
