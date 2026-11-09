// `opencode acp` session/load: loading a session replays its history to the client as `session/update`s (user
// messages with their attachments, reasoning, text, tool calls in each state with completed diffs, compactions
// in the client's preferred form) before the response; `session/resume` attaches without a replay. A session
// made by one agent process is loaded by the next one, against the same data directory.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say, type Turn } from "../lib/openrouter"
import { cliScenario, type CliCtx } from "../lib/cli"
import { acp, initialize, type AcpClient } from "../lib/acp"

const rule = (action: string, resource: string, effect: string) => ({ action, resource, effect })
const config = {
  permissions: [
    ...HIDDEN_TOOLS.map((action) => rule(action, "*", "deny")),
    rule("*", "*", "deny"),
    rule("read", "*", "allow"),
    rule("edit", "*", "allow"),
  ],
}

const SUMMARY = say("## Objective\n- Keep the notes tidy\n\n## Work State\n### Completed\n- Read and edited notes.txt\n\n## Next Move\n1. Answer the next question")
const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="

async function agent(ctx: CliCtx, clientCapabilities: Record<string, unknown> = {}) {
  const process = ctx.start(["acp"])
  const client = acp(process)
  await initialize(client, clientCapabilities)
  return { process, client }
}

async function done(running: { process: ReturnType<CliCtx["start"]> }) {
  running.process.end()
  await running.process.exited(30_000)
}

const text = (value: string) => ({ type: "text", text: value })

/** One session with a varied history, made by its own agent process; returns its ID. */
async function history(ctx: CliCtx) {
  const first = await agent(ctx)
  const created = await first.client.request("session/new", { cwd: ctx.project, mcpServers: [] })
  const sessionId = created.result.sessionId as string
  await first.client.request("session/prompt", {
    sessionId,
    prompt: [
      text("Read the notes and fix the typo"),
      { type: "resource", resource: { uri: `file://${ctx.project}/notes.txt#L2`, text: "betta\n", mimeType: "text/plain" } },
      { type: "resource_link", uri: `file://${ctx.project}/notes.txt`, name: "notes.txt", mimeType: "text/plain" },
      { type: "image", mimeType: "image/png", data: PNG },
      { ...text("Only for the model"), annotations: { audience: ["assistant"] } },
    ],
  })
  await first.client.request("session/prompt", { sessionId, prompt: [text("/compact")] })
  await first.client.request("session/prompt", { sessionId, prompt: [text("Thanks")] })
  await done(first)
  return sessionId
}

const turns: Turn[] = [
  { events: [{ reasoning: "The notes have a typo." }, { text: "Reading first." }, { tool: "read", input: { path: "notes.txt" } }], finish: "tool_calls" },
  call("read", { path: "missing.txt" }),
  { events: [{ reasoning: "Found it." }, { reasoning: "Fixing it." }, { tool: "edit", input: { path: "notes.txt", oldString: "betta", newString: "beta" } }], finish: "tool_calls" },
  say("Fixed the typo."),
  SUMMARY,
  say("You're welcome."),
]

cliScenario(
  "acp session/load replays the history",
  { files: { "notes.txt": "alpha\nbetta\ngamma\n" }, config, turns: [...turns, say("Still here.")] },
  async (ctx) => {
    const sessionId = await history(ctx)
    // A client that understands compaction updates, then prompts the loaded session.
    const second = await agent(ctx, { session: { compaction: {} } })
    await second.client.request("session/load", { sessionId, cwd: ctx.project, mcpServers: [] })
    await second.client.request("session/prompt", { sessionId, prompt: [text("Are you still there?")] })
    await done(second)
    // One that does not: compactions arrive as session info.
    const third = await agent(ctx)
    await third.client.request("session/load", { sessionId, cwd: ctx.project, mcpServers: [] })
    await done(third)
  },
)

cliScenario(
  "acp session/resume attaches without a replay",
  { files: { "notes.txt": "alpha\nbetta\ngamma\n" }, config, turns: [...turns, say("Resumed.")] },
  async (ctx) => {
    const sessionId = await history(ctx)
    const second = await agent(ctx)
    await second.client.request("session/resume", { sessionId, cwd: ctx.project, mcpServers: [] })
    await second.client.request("session/prompt", { sessionId, prompt: [text("Resume please")] })
    await done(second)
  },
)

cliScenario("acp session/load failures", { config, turns: [say("Hello.")] }, async (ctx) => {
  const running = await agent(ctx)
  const created = await running.client.request("session/new", { cwd: ctx.project, mcpServers: [] })
  const sessionId = created.result.sessionId as string
  await running.client.request("session/prompt", { sessionId, prompt: [text("Hi")] })
  const client: AcpClient = running.client
  await client.request("session/load", { sessionId: "ses_doesNotExist0000000000001", cwd: ctx.project, mcpServers: [] })
  await client.request("session/load", { sessionId: "not-a-session", cwd: ctx.project, mcpServers: [] })
  await client.request("session/load", { sessionId, cwd: `${ctx.project}/elsewhere`, mcpServers: [] })
  await client.request("session/load", { sessionId, cwd: "relative", mcpServers: [] })
  await client.request("session/load", { sessionId, cwd: ctx.project, mcpServers: [], additionalDirectories: ["relative/dir"] })
  // Loading the attached session again replays it again.
  await client.request("session/load", { sessionId, cwd: ctx.project, mcpServers: [] })
  await done(running)
})
