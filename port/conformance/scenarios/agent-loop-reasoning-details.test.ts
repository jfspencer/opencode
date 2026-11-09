// Structured reasoning details on OpenRouter's chat stream (ai/src/protocols/openai-chat.ts): OpenRouter's `reasoning.*`
// entries are kept as they arrived (consecutive text and summary deltas of one identity merged, encrypted ones never)
// and replayed with `reasoning` beside them; Kimi's bare `summary` / `encrypted` entries merge by their stream-only
// `index`, show the summary only when no reasoning text streamed, and are replayed without `index`. Other entries are
// dropped. Raw streams stand in for the upstreams.
import type { Turn } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

const sse = (deltas: Record<string, unknown>[], finish = "stop"): Turn => {
  const chunk = (delta: Record<string, unknown>, extra: Record<string, unknown> = {}) =>
    `data: ${JSON.stringify({
      id: "gen-details",
      object: "chat.completion.chunk",
      created: 1_767_225_600,
      model: "moonshotai/kimi-k3",
      choices: [{ index: 0, delta, finish_reason: null, ...extra }],
    })}\n\n`
  const usage = `data: ${JSON.stringify({
    id: "gen-details",
    object: "chat.completion.chunk",
    created: 1_767_225_600,
    model: "moonshotai/kimi-k3",
    choices: [{ index: 0, delta: { content: "" }, finish_reason: finish }],
    usage: { prompt_tokens: 50, completion_tokens: 20, total_tokens: 70, completion_tokens_details: { reasoning_tokens: 8 } },
  })}\n\n`
  return {
    raw: [...deltas.map((delta) => chunk({ role: "assistant", ...delta })), usage, "data: [DONE]\n\n"].join(""),
    headers: { "content-type": "text/event-stream" },
  }
}

const say = (text: string) => sse([{ content: text }])

scenario(
  "Kimi reasoning details merge by index and replay without it",
  {
    turns: [
      sse([
        { reasoning_content: "Let me think", reasoning_details: [{ type: "summary", summary: "Think", index: 0 }] },
        { reasoning_content: " more.", reasoning_details: [{ type: "summary", summary: "ing.", index: 0 }] },
        { reasoning_details: [{ type: "encrypted", encrypted: "opaque-1", index: 1 }] },
        { content: "Answer." },
      ]),
      say("Second."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "details" })
    await ctx.prompt(id, "First")
    await ctx.prompt(id, "Second")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "a Kimi summary shows when no reasoning text streams",
  {
    turns: [
      sse([
        { reasoning_details: [{ type: "summary", summary: "Planned ", index: 0 }] },
        { reasoning_details: [{ type: "summary", summary: "the steps.", index: 0 }, { type: "summary", summary: "Second block.", index: 1 }] },
        { content: "Done." },
      ]),
      say("Again."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "details" })
    await ctx.prompt(id, "Plan")
    await ctx.prompt(id, "Again")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "OpenRouter reasoning details keep their identity",
  {
    turns: [
      sse([
        { reasoning: "Step one.", reasoning_details: [{ type: "reasoning.text", text: "Step one.", format: "anthropic-claude-v1", index: 0 }] },
        { reasoning: " Step two.", reasoning_details: [{ type: "reasoning.text", text: " Step two.", index: 0, signature: "sig-1" }] },
        { reasoning_details: [{ type: "reasoning.encrypted", data: "blob-1", id: "enc-1", format: "anthropic-claude-v1", index: 1 }] },
        { reasoning_details: [{ type: "reasoning.summary", summary: "A summary.", id: "sum-1", index: 2 }, { type: "unknown.kind", text: "dropped" }, { type: "reasoning.text", text: 5 }] },
        { content: "Answer." },
      ]),
      say("Second."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "details" })
    await ctx.prompt(id, "First")
    await ctx.prompt(id, "Second")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "reasoning details with conflicting identities stay apart",
  {
    turns: [
      sse([
        { reasoning_details: [{ type: "reasoning.text", text: "A", id: "one", index: 0 }] },
        { reasoning_details: [{ type: "reasoning.text", text: "B", id: "two", index: 0 }] },
        { reasoning_details: [{ type: "reasoning.text", text: "C", index: 0, signature: "s1" }] },
        { reasoning_details: [{ type: "reasoning.text", text: "D", index: 0, signature: "s2" }] },
        { reasoning_details: [{ type: "reasoning.encrypted", data: "e1", index: 0 }, { type: "reasoning.encrypted", data: "e2", index: 0 }] },
        { content: "Answer." },
      ]),
      say("Second."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "details" })
    await ctx.prompt(id, "First")
    await ctx.prompt(id, "Second")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "reasoning details without content open the reasoning block",
  {
    turns: [
      sse([{ reasoning_details: [{ type: "reasoning.encrypted", data: "only-encrypted", index: 0 }] }, { content: "Answer." }]),
      sse([{ reasoning_details: [] }, { content: "Empty details." }]),
      say("Third."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "details" })
    await ctx.prompt(id, "First")
    await ctx.prompt(id, "Second")
    await ctx.prompt(id, "Third")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)
