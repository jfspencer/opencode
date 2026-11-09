// `opencode acp` child sessions (subagents): a client that asks for child-session updates
// (`_meta["opencode/child-session-updates"]`) gets each child's lifecycle and updates as
// `opencode/session/child_update` notifications; any other client gets the child's updates as its own session's,
// marked with the child (`_meta["opencode/child-session"]`) and with prefixed tool call IDs and titles. A child's
// permission requests and forms reach the client too, and a background child is followed after the turn ends.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say, type ProviderRequest, type Responder, type Turn } from "../lib/openrouter"
import { cliScenario, type CliCtx } from "../lib/cli"
import { acp, initialize, type Handler } from "../lib/acp"

const rule = (action: string, resource: string, effect: string) => ({ action, resource, effect })
const only = (...rules: ReturnType<typeof rule>[]) => ({
  permissions: [...HIDDEN_TOOLS.map((action) => rule(action, "*", "deny")), rule("*", "*", "deny"), rule("subagent", "*", "allow"), ...rules],
})

/**
 * Answers chat requests from one script per session, by order of each session's first request. Child sessions
 * share their root's `prompt_cache_key`, so a session is told apart by its first user message.
 */
function bySession(...scripts: Turn[][]): Responder {
  const order: string[] = []
  const cursors: number[] = []
  return (request: ProviderRequest) => {
    if (request.kind !== "chat") return
    const messages = (request.body as { messages?: { role: string; content: unknown }[] })?.messages ?? []
    const key = JSON.stringify(messages.find((message) => message.role === "user")?.content)
    if (!order.includes(key)) order.push(key)
    const index = order.indexOf(key)
    const cursor = cursors[index] ?? 0
    cursors[index] = cursor + 1
    return scripts[index]?.[cursor] ?? { status: 500, body: { error: { message: "conformance: script exhausted" } } }
  }
}

const updates = { _meta: { "opencode/child-session-updates": true } }
const files = { "src/a.ts": "export const a = 1\n", "src/b.ts": "export const b = 2\n" }
const prompt = (text: string) => [{ type: "text", text }]

async function open(ctx: CliCtx, clientCapabilities: Record<string, unknown>, handlers: Record<string, Handler> = {}) {
  const agent = ctx.start(["acp"])
  const client = acp(agent, handlers)
  await initialize(client, clientCapabilities)
  const created = await client.request("session/new", { cwd: ctx.project, mcpServers: [] })
  return { agent, client, sessionId: created.result.sessionId as string }
}

const counting = () =>
  bySession(
    [call("subagent", { agent: "general", description: "Count TS files", prompt: "Count the TypeScript files in src." }), say("The subagent counted two.")],
    [
      { events: [{ reasoning: "Reading both." }, { text: "Reading." }, { tool: "read", input: { path: "src/a.ts" } }], finish: "tool_calls" },
      call("read", { path: "src/missing.ts" }),
      say("There are two TypeScript files."),
    ],
  )

cliScenario(
  "acp child session updates as notifications",
  { files, config: only(rule("read", "*", "allow")), responders: [counting()] },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx, updates)
    await client.request("session/prompt", { sessionId, prompt: prompt("How many TS files are there?") })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp child session updates folded into the session",
  { files, config: only(rule("read", "*", "allow")), responders: [counting()] },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx, {})
    await client.request("session/prompt", { sessionId, prompt: prompt("How many TS files are there?") })
    agent.end()
    await agent.exited(30_000)
  },
)

const asking = () =>
  bySession(
    [call("subagent", { agent: "general", description: "Check notes", prompt: "Show notes.txt and ask about it." }), say("The subagent is done.")],
    [
      call("shell", { command: "cat notes.txt" }),
      call("question", { questions: [{ question: "Keep the notes?", header: "Notes", options: [{ label: "Keep", description: "Leave them" }, { label: "Drop", description: "Remove them" }] }] }),
      say("Kept the notes."),
    ],
  )

for (const [name, capabilities] of [
  ["acp child permission requests and forms with child updates", { ...updates, elicitation: { form: {} } }],
  ["acp child permission requests and forms without child updates", { elicitation: { form: {} } }],
] as const)
  cliScenario(
    name,
    { files: { "notes.txt": "keep me\n" }, config: only(rule("shell", "*", "ask"), rule("question", "*", "allow")), responders: [asking()] },
    async (ctx) => {
      const { agent, client, sessionId } = await open(ctx, capabilities, {
        "session/request_permission": () => ({ result: { outcome: { outcome: "selected", optionId: "once" } } }),
        "elicitation/create": () => ({ result: { action: "accept", content: { q0: "Keep" } } }),
      })
      await client.request("session/prompt", { sessionId, prompt: prompt("Check the notes with a subagent") })
      agent.end()
      await agent.exited(30_000)
    },
  )

{
  // The child is held at the fake model until the turn has ended, so the follower carries its updates and ask.
  let release!: () => void
  const turnEnded = new Promise<void>((resolve) => (release = resolve))
  cliScenario(
    "acp background child followed after the turn",
    {
      files,
      config: only(rule("read", "*", "allow"), rule("shell", "*", "ask")),
      responders: [
        bySession(
          [
            call("subagent", { agent: "general", description: "Read in background", prompt: "Read src/a.ts in the background.", background: true }),
            say("Started the subagent."),
            say("The background subagent finished."),
          ],
          [{ events: [{ until: turnEnded }, { tool: "shell", input: { command: "cat src/a.ts" } }], finish: "tool_calls" }, say("It exports a.")],
        ),
      ],
    },
    async (ctx) => {
      const { agent, client, sessionId } = await open(ctx, updates, {
        "session/request_permission": () => ({ result: { outcome: { outcome: "selected", optionId: "once" } } }),
      })
      await client.request("session/prompt", { sessionId, prompt: prompt("Read src/a.ts in the background") })
      release()
      await client.until((message) => message.method === "opencode/session/child_update" && message.params?.status === "completed")
      // The parent hears back from its child in an execution of its own, which the client is not shown.
      const deadline = Date.now() + 30_000
      while (ctx.provider.requests.filter((request) => request.kind === "chat").length < 5) {
        if (Date.now() > deadline) throw new Error("the parent never heard back from its child")
        await Bun.sleep(20)
      }
      agent.end()
      await agent.exited(30_000)
    },
  )
}
