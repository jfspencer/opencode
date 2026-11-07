// `opencode acp`: the Agent Client Protocol over stdio (newline-delimited JSON-RPC). The scenario plays the
// client: initialize, open a session in the project, prompt it (one plain reply, one tool call that writes a
// file), then closes stdin. Everything the agent wrote to stdout is recorded when it exits.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say } from "../lib/openrouter"
import { cliScenario, type CliProcess } from "../lib/cli"

const rule = (action: string, resource: string, effect: string) => ({ action, resource, effect })
const quiet = { permissions: [...HIDDEN_TOOLS.map((action) => rule(action, "*", "deny")), rule("*", "*", "deny")] }

/** A JSON-RPC client over the agent's stdio. */
function rpc(agent: CliProcess) {
  let next = 0
  return async (method: string, params: unknown) => {
    const id = ++next
    agent.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n")
    const line = await agent.line((text) => {
      try {
        const message = JSON.parse(text)
        return message.id === id && ("result" in message || "error" in message)
      } catch {
        return false
      }
    }, "stdout", 60_000)
    return JSON.parse(line) as { result?: any; error?: unknown }
  }
}

cliScenario(
  "acp handshake, session and prompts",
  {
    config: { permissions: [...quiet.permissions, rule("edit", "*", "allow")] },
    turns: [say("Hello over ACP."), call("write", { path: "acp.txt", content: "written over ACP\n" }), say("Wrote acp.txt.")],
  },
  async (ctx) => {
    const agent = ctx.start(["acp"])
    const request = rpc(agent)
    await request("initialize", {
      protocolVersion: 1,
      clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false },
      clientInfo: { name: "conformance", version: "1.0.0" },
    })
    const session = await request("session/new", { cwd: ctx.project, mcpServers: [] })
    const sessionId = session.result.sessionId as string
    await request("session/prompt", { sessionId, prompt: [{ type: "text", text: "Say hello" }] })
    await request("session/prompt", { sessionId, prompt: [{ type: "text", text: "Write acp.txt" }] })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario("acp rejects unknown methods and sessions", {}, async (ctx) => {
  const agent = ctx.start(["acp"])
  const request = rpc(agent)
  await request("initialize", { protocolVersion: 1, clientCapabilities: {}, clientInfo: { name: "conformance", version: "1.0.0" } })
  await request("session/frobnicate", {})
  await request("session/prompt", { sessionId: "ses_doesNotExist0000000000001", prompt: [{ type: "text", text: "Hi" }] })
  await request("session/new", { cwd: "relative/dir", mcpServers: [] })
  agent.end()
  await agent.exited(30_000)
})

cliScenario("acp --login without a terminal", {}, async (ctx) => {
  await ctx.cli(["acp", "--login"])
})
