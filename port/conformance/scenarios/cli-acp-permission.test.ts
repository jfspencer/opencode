// `opencode acp` permission requests: a tool's permission ask reaches the ACP client as
// `session/request_permission` (with the tool call, its locations and a diff preview of file edits), and the
// client's answer becomes the reply: allow once, always allow, reject, a cancelled outcome, an error or a
// malformed answer (both reject). A cancelled prompt withdraws its open request (`$/cancel_request`) and rejects.
// Completed edits carry their diffs.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say } from "../lib/openrouter"
import { cliScenario, type CliCtx } from "../lib/cli"
import { acp, initialize, type Answer, type Handler } from "../lib/acp"

type Effect = "allow" | "deny" | "ask"
const rule = (action: string, resource: string, effect: Effect) => ({ action, resource, effect })
const only = (...rules: ReturnType<typeof rule>[]) => ({
  permissions: [...HIDDEN_TOOLS.map((action) => rule(action, "*", "deny")), rule("*", "*", "deny"), ...rules],
})

const selected = (optionId: string): Answer => ({ result: { outcome: { outcome: "selected", optionId } } })

/** Starts the agent, initializes it and opens a session in the project. */
async function open(ctx: CliCtx, handlers: Record<string, Handler>) {
  const agent = ctx.start(["acp"])
  const client = acp(agent, handlers)
  await initialize(client)
  const created = await client.request("session/new", { cwd: ctx.project, mcpServers: [] })
  return { agent, client, sessionId: created.result.sessionId as string }
}

const prompt = (text: string) => [{ type: "text", text }]

cliScenario(
  "acp permission allowed once, then always",
  {
    config: only(rule("edit", "*", "ask")),
    turns: [
      call("write", { path: "once.txt", content: "allowed once\n" }),
      say("Wrote once.txt."),
      call("write", { path: "always.txt", content: "always allowed\n" }),
      say("Wrote always.txt."),
      call("write", { path: "again.txt", content: "no question asked\n" }),
      say("Wrote again.txt."),
    ],
  },
  async (ctx) => {
    const answers = ["once", "always"]
    const { agent, client, sessionId } = await open(ctx, {
      "session/request_permission": () => selected(answers.shift() ?? "reject"),
    })
    await client.request("session/prompt", { sessionId, prompt: prompt("Write once.txt") })
    await client.request("session/prompt", { sessionId, prompt: prompt("Write always.txt") })
    await client.request("session/prompt", { sessionId, prompt: prompt("Write again.txt") })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp permission rejected, cancelled, failed or malformed",
  {
    config: only(rule("edit", "*", "ask")),
    // A rejected permission ends the turn: the model is not asked again.
    turns: ["rejected", "cancelled", "failed", "malformed", "unknown"].map((name) => call("write", { path: `${name}.txt`, content: "x\n" })),
  },
  async (ctx) => {
    const answers: Answer[] = [
      selected("reject"),
      { result: { outcome: { outcome: "cancelled" } } },
      { error: { code: -32603, message: "Internal error", data: { reason: "client failure" } } },
      { result: { verdict: "yes" } },
      selected("sometimes"),
    ]
    const { agent, client, sessionId } = await open(ctx, { "session/request_permission": () => answers.shift() })
    for (const name of ["rejected", "cancelled", "failed", "malformed", "unknown"])
      await client.request("session/prompt", { sessionId, prompt: prompt(`Write ${name}.txt`) })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp cancelling a prompt withdraws its permission request",
  {
    config: only(rule("shell", "*", "ask")),
    turns: [call("shell", { command: "echo never" }), say("Back after the cancel.")],
  },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx, { "session/request_permission": () => undefined })
    const pending = client.send("session/prompt", { sessionId, prompt: prompt("Echo something") })
    await client.until((message) => message.method === "session/request_permission")
    client.notify("session/cancel", { sessionId })
    await client.response(pending)
    // The withdrawn request's late answer is ignored.
    client.answer(0, selected("once"))
    client.handle("session/request_permission", () => selected("once"))
    await client.request("session/prompt", { sessionId, prompt: prompt("Are you there?") })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp permission previews and completed diffs of file edits",
  {
    files: { "notes.txt": "alpha\nbeta\ngamma\n", "list.txt": "one\ntwo\n" },
    config: only(rule("edit", "*", "ask"), rule("read", "*", "allow")),
    turns: [
      call("edit", { path: "notes.txt", oldString: "beta", newString: "BETA" }),
      call("write", { path: "fresh.txt", content: "brand new\n" }),
      call("write", { path: "list.txt", content: "one\ntwo\nthree\n" }),
      call("patch", {
        patchText: "*** Begin Patch\n*** Update File: notes.txt\n@@\n alpha\n-BETA\n+beta again\n*** Add File: added.txt\n+added by patch\n*** End Patch",
      }),
      say("Edited everything."),
    ],
  },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx, { "session/request_permission": () => selected("once") })
    await client.request("session/prompt", { sessionId, prompt: prompt("Edit the files") })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp permission requests for shell commands and outside paths",
  {
    files: { "notes.txt": "inside\n" },
    config: only(rule("shell", "*", "ask"), rule("read", "*", "allow"), rule("external_directory", "*", "ask")),
    turns: [
      call("shell", { command: "cat notes.txt" }),
      call("shell", { command: "ls", workdir: "sub" }),
      call("read", { path: "../outside/elsewhere.txt" }),
      say("Done looking."),
    ],
  },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx, { "session/request_permission": () => selected("once") })
    await client.request("session/prompt", { sessionId, prompt: prompt("Look around") })
    agent.end()
    await agent.exited(30_000)
  },
)
