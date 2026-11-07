// The agent loop: multi-step tool loops, reasoning round-trips, usage accounting and finish reasons. Each scenario
// scripts the model's replies and records what the loop sent back (the next OpenRouter request), the session's
// messages and the event stream.
import { call, say, type Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const files = {
  "README.md": "# Demo\n\nA small project.\n",
  "src/math.ts": "export function add(a: number, b: number) {\n  return a + b\n}\n",
  "src/strings.ts": "export const greet = (name: string) => `hello ${name}`\n",
}

async function turn(ctx: Ctx, text: string) {
  const id = await ctx.session({ title: "agent loop" })
  await ctx.prompt(id, text)
  await ctx.http("GET", `/api/session/${id}`)
  await ctx.http("GET", `/api/session/${id}/message`)
  return id
}

/** A raw OpenRouter SSE body built from explicit chunks, for wire shapes the turn helpers don't produce. */
function sse(
  deltas: ReadonlyArray<Record<string, unknown> | { readonly frame: unknown } | { readonly line: string }>,
  end: { finish?: string | null; native?: string; usage?: Record<string, unknown>; done?: boolean } = {},
): Turn {
  const base = { id: "gen-raw", object: "chat.completion.chunk", created: 1_767_225_600, model: "deepseek/deepseek-v4.1-flash-1017", provider: "DeepSeek" }
  const out = [": OPENROUTER PROCESSING\n\n"]
  for (const delta of deltas) {
    if ("line" in delta) out.push(`${delta.line}\n\n`)
    else if ("frame" in delta) out.push(`data: ${JSON.stringify(delta.frame)}\n\n`)
    else out.push(`data: ${JSON.stringify({ ...base, choices: [{ index: 0, delta, finish_reason: null, native_finish_reason: null }] })}\n\n`)
  }
  if (end.finish !== null) {
    const finish = end.finish ?? "stop"
    out.push(
      `data: ${JSON.stringify({ ...base, choices: [{ index: 0, delta: { content: "", role: "assistant" }, finish_reason: finish, native_finish_reason: end.native ?? finish }] })}\n\n`,
    )
  }
  if (end.usage) out.push(`data: ${JSON.stringify({ ...base, choices: [], usage: end.usage })}\n\n`)
  if (end.done !== false) out.push("data: [DONE]\n\n")
  return { raw: out.join("") }
}

scenario(
  "multi-step tool loop",
  {
    files,
    turns: [
      // One match only: glob lists matches in ripgrep's walk order, which is not stable across runs.
      call("glob", { pattern: "src/m*.ts" }, "Let me look around."),
      call("read", { path: "src/math.ts" }),
      call("edit", { path: "src/math.ts", oldString: "return a + b", newString: "return b + a" }),
      call("read", { path: "src/math.ts" }),
      say("Swapped the operands in add."),
    ],
  },
  (ctx) => turn(ctx, "Swap the operands of add").then(() => undefined),
)

scenario(
  "reasoning is echoed back on the next request",
  {
    files,
    turns: [
      { events: [{ reasoning: "The user wants the README. " }, { reasoning: "I will read it." }, { tool: "read", input: { path: "README.md" } }] },
      { events: [{ reasoning: "It is a demo project." }, { text: "It is a small demo project." }], usage: { prompt: 300, completion: 40, reasoning: 25 } },
    ],
  },
  (ctx) => turn(ctx, "What is this project?").then(() => undefined),
)

scenario(
  "reasoning-only reply",
  { turns: [{ events: [{ reasoning: "Thinking about nothing in particular." }] }, say("Second answer.")] },
  async (ctx) => {
    const id = await turn(ctx, "Think quietly")
    await ctx.prompt(id, "Now answer")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "reasoning details with signature and encrypted parts",
  {
    files,
    turns: [
      sse(
        [
          { content: "", role: "assistant", reasoning: "First half", reasoning_details: [{ type: "reasoning.text", text: "First half", format: "anthropic-claude-v1", index: 0 }] },
          { content: "", role: "assistant", reasoning: " second half.", reasoning_details: [{ type: "reasoning.text", text: " second half.", format: "anthropic-claude-v1", index: 0 }] },
          { content: "", role: "assistant", reasoning_details: [{ type: "reasoning.text", signature: "c2lnbmF0dXJl", format: "anthropic-claude-v1", index: 0 }] },
          { content: "", role: "assistant", reasoning_details: [{ type: "reasoning.encrypted", data: "ZW5jcnlwdGVk", format: "anthropic-claude-v1", index: 1 }] },
          { content: "Reading it.", role: "assistant" },
          { role: "assistant", tool_calls: [{ index: 0, id: "toolu_signed", type: "function", function: { name: "read", arguments: "" } }] },
          { role: "assistant", tool_calls: [{ index: 0, function: { arguments: '{"path": "READ' } }] },
          { role: "assistant", tool_calls: [{ index: 0, function: { arguments: 'ME.md"}' } }] },
        ],
        { finish: "tool_calls", native: "tool_use", usage: { prompt_tokens: 50, completion_tokens: 20, total_tokens: 70 } },
      ),
      sse(
        [
          { content: "", role: "assistant", reasoning_details: [{ type: "reasoning.summary", summary: "Summary part one. ", format: "openai-responses-v1", index: 0 }] },
          { content: "", role: "assistant", reasoning_details: [{ type: "reasoning.summary", summary: "Part two.", format: "openai-responses-v1", index: 0 }] },
          { content: "A demo.", role: "assistant" },
        ],
        { finish: "stop" },
      ),
      say("Third."),
    ],
  },
  async (ctx) => {
    const id = await turn(ctx, "Describe the README")
    await ctx.prompt(id, "Again")
  },
)

scenario(
  "usage accounting across turns",
  {
    turns: [
      say("First.", { prompt: 1200, completion: 300, reasoning: 120, cached: 800 }),
      say("Second.", { prompt: 1600, completion: 50, cached: 1500 }),
    ],
  },
  async (ctx) => {
    const id = await turn(ctx, "One")
    await ctx.prompt(id, "Two")
    await ctx.http("GET", `/api/session/${id}`)
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "usage with cache writes and reported cost",
  {
    turns: [
      sse([{ content: "Cached.", role: "assistant" }], {
        usage: {
          prompt_tokens: 900,
          completion_tokens: 30,
          total_tokens: 930,
          cost: 0.0125,
          prompt_tokens_details: { cached_tokens: 200, cache_write_tokens: 600 },
          completion_tokens_details: { reasoning_tokens: 5 },
        },
      }),
    ],
  },
  (ctx) => turn(ctx, "Cache this").then(() => undefined),
)

scenario("reply without usage", { turns: [{ events: [{ text: "No usage reported." }], usage: false }] }, (ctx) =>
  turn(ctx, "Answer").then(() => undefined),
)

scenario(
  "text and tool call in one step",
  { files, turns: [call("read", { path: "README.md" }, "I'll read the README first."), say("Done reading.")] },
  (ctx) => turn(ctx, "Read the README").then(() => undefined),
)

scenario(
  "parallel tool calls in one step",
  {
    // The three tools run concurrently, so their completion events interleave in any order.
    events: false,
    files,
    turns: [
      {
        events: [
          { tool: "read", input: { path: "src/math.ts" }, id: "call_a" },
          { tool: "read", input: { path: "src/strings.ts" }, id: "call_b" },
          { tool: "read", input: { path: "README.md" }, id: "call_c" },
        ],
      },
      say("Read both files."),
    ],
  },
  (ctx) => turn(ctx, "Read both sources").then(() => undefined),
)

scenario(
  "finish reason length continues the response",
  {
    turns: [{ events: [{ text: "The answer begins here and" }], finish: "length", usage: { prompt: 100, completion: 393216 } }, say(" ends here.")],
  },
  (ctx) => turn(ctx, "Write a long answer").then(() => undefined),
)

scenario(
  "finish reason length three times fails the turn",
  {
    turns: [
      { events: [{ text: "Part one" }], finish: "length" },
      { events: [{ text: "Part two" }], finish: "length" },
      { events: [{ text: "Part three" }], finish: "length" },
    ],
  },
  (ctx) => turn(ctx, "Write forever").then(() => undefined),
)

scenario(
  "finish reason length cuts a tool call",
  {
    files,
    turns: [{ events: [{ tool: "write", rawInput: '{"path": "big.txt", "content": "aaaa' }], finish: "length" }, say("I will write it in pieces.")],
  },
  (ctx) => turn(ctx, "Write a big file").then(() => undefined),
)

scenario(
  "finish reason length after a complete tool call",
  { files, turns: [{ events: [{ text: "Reading." }, { tool: "read", input: { path: "README.md" } }], finish: "length" }, say("Read it.")] },
  (ctx) => turn(ctx, "Read the README").then(() => undefined),
)

scenario(
  "finish reason content_filter fails the step",
  { turns: [{ events: [{ text: "Here is the part that" }], finish: "content_filter" }] },
  (ctx) => turn(ctx, "Say something blocked").then(() => undefined),
)

scenario(
  "finish reason content_filter during a tool call",
  { files, turns: [{ events: [{ tool: "read", input: { path: "README.md" } }], finish: "content_filter" }] },
  (ctx) => turn(ctx, "Read something blocked").then(() => undefined),
)

scenario(
  "finish reason error after output continues",
  { timeout: 30_000, turns: [{ events: [{ text: "Starting the answer" }], finish: "error" }, say("Recovered answer.")] },
  (ctx) => turn(ctx, "Answer despite errors").then(() => undefined),
)

scenario(
  "unknown finish reason",
  { timeout: 30_000, turns: [sse([{ content: "Odd ending.", role: "assistant" }], { finish: "mystery" }), say("Clean ending.")] },
  (ctx) => turn(ctx, "End strangely").then(() => undefined),
)

scenario(
  "empty reply",
  { turns: [{ events: [] }, say("Second.")] },
  async (ctx) => {
    const id = await turn(ctx, "Say nothing")
    await ctx.prompt(id, "Now say something")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "refusal text",
  { turns: [sse([{ role: "assistant", refusal: "I can't help with that." }])] },
  (ctx) => turn(ctx, "Do something bad").then(() => undefined),
)

scenario(
  "tool call streamed without indexes",
  {
    files,
    turns: [
      sse(
        [
          { role: "assistant", tool_calls: [{ id: "call_noindex", type: "function", function: { name: "read", arguments: "" } }] },
          { role: "assistant", tool_calls: [{ function: { arguments: '{"path":"README.md"}' } }] },
          { role: "assistant", tool_calls: [{ id: "call_second", type: "function", function: { name: "glob", arguments: '{"pattern":"*.md"}' } }] },
        ],
        { finish: "tool_calls" },
      ),
      say("Both done."),
    ],
  },
  (ctx) => turn(ctx, "Read and glob").then(() => undefined),
)

scenario(
  "tool call whose name arrives late",
  {
    files,
    turns: [
      sse(
        [
          { role: "assistant", tool_calls: [{ index: 0, id: "call_late", type: "function", function: { arguments: '{"path":' } }] },
          { role: "assistant", tool_calls: [{ index: 0, function: { name: "read", arguments: '"README.md"}' } }] },
        ],
        { finish: "tool_calls" },
      ),
      say("Read."),
    ],
  },
  (ctx) => turn(ctx, "Read late").then(() => undefined),
)

scenario(
  "conversation continues across prompts",
  { files, turns: [call("read", { path: "README.md" }), say("A demo."), call("glob", { pattern: "**/*.md" }), say("One markdown file.")] },
  async (ctx) => {
    const id = await turn(ctx, "What is this?")
    await ctx.prompt(id, "Which markdown files exist?")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "encrypted reasoning without readable text",
  {
    turns: [
      sse([{ content: "", role: "assistant", reasoning_details: [{ type: "reasoning.encrypted", data: "b3BhcXVl", format: "openai-responses-v1", index: 0 }] }], {
        finish: "stop",
      }),
      say("Answered after hidden reasoning."),
    ],
  },
  async (ctx) => {
    const id = await turn(ctx, "Think privately")
    await ctx.prompt(id, "Now answer")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "summary reasoning details beside reasoning text",
  {
    turns: [
      sse([
        { content: "", role: "assistant", reasoning_content: "Full thinking. ", reasoning_details: [{ type: "summary", summary: "Short ", index: 0 }] },
        { content: "", role: "assistant", reasoning_content: "More thinking.", reasoning_details: [{ type: "summary", summary: "summary.", index: 0 }] },
        { content: "Done.", role: "assistant" },
      ]),
      sse([
        { content: "", role: "assistant", reasoning_details: [{ type: "summary", summary: "Only a summary ", index: 0 }] },
        { content: "", role: "assistant", reasoning_details: [{ type: "summary", summary: "this time.", index: 0 }] },
        { content: "Done again.", role: "assistant" },
      ]),
      say("Third."),
    ],
  },
  async (ctx) => {
    const id = await turn(ctx, "Think in summaries")
    await ctx.prompt(id, "Again")
    await ctx.prompt(id, "Once more")
  },
)

scenario(
  "reasoning in the reasoning_content field",
  {
    files,
    turns: [
      sse(
        [
          { content: "", role: "assistant", reasoning_content: "I should read the README." },
          { role: "assistant", tool_calls: [{ index: 0, id: "call_rc", type: "function", function: { name: "read", arguments: '{"path":"README.md"}' } }] },
        ],
        { finish: "tool_calls" },
      ),
      say("A demo."),
    ],
  },
  (ctx) => turn(ctx, "What is this?").then(() => undefined),
)

scenario(
  "usage frame without token counts",
  { turns: [sse([{ content: "Counted nothing.", role: "assistant" }], { usage: { cost: 0 } })] },
  (ctx) => turn(ctx, "Answer").then(() => undefined),
)

scenario(
  "finish reason network_error, then success",
  { timeout: 30_000, turns: [sse([], { finish: "network_error" }), say("Recovered.")] },
  (ctx) => turn(ctx, "Answer").then(() => undefined),
)

scenario(
  "steps in a git project record the files they changed",
  {
    git: true,
    files,
    turns: [
      call("write", { path: "src/new.ts", content: "export const created = true\n" }),
      call("edit", { path: "README.md", oldString: "A small project.", newString: "A small, edited project." }),
      say("Created one file and edited another."),
    ],
  },
  async (ctx) => {
    const id = await turn(ctx, "Create src/new.ts and edit the README")
    await ctx.http("GET", `/api/session/${id}/diff`)
  },
)
