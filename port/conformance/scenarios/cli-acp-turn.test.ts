// `opencode acp` turns beyond the basics: config options (model with an effort suffix, effort, mode, and their
// errors), slash commands, compaction updates in both forms, retries as session info, stop reasons and errors,
// cancellation (`session/cancel` and `$/cancel_request`), prompt content (links, unreadable files, blobs,
// audiences), tool call updates (search locations, todos, unknown tools), the session lifecycle (list, fork, close, delete,
// additional directories) and commands added while a session is attached.
import { mkdirSync, writeFileSync } from "node:fs"
import path from "node:path"
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say, type Turn } from "../lib/openrouter"
import { cliScenario, type CliCtx } from "../lib/cli"
import { acp, initialize, type Handler } from "../lib/acp"

const rule = (action: string, resource: string, effect: string) => ({ action, resource, effect })
const only = (...rules: ReturnType<typeof rule>[]) => ({
  permissions: [...HIDDEN_TOOLS.map((action) => rule(action, "*", "deny")), rule("*", "*", "deny"), ...rules],
})
const MODEL = "openrouter/deepseek/deepseek-v4.1-flash-1017"
const SUMMARY = say("## Objective\n- Answer questions\n\n## Work State\n### Completed\n- Said hello\n\n## Next Move\n1. Answer the next question")
const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="

async function open(ctx: CliCtx, clientCapabilities: Record<string, unknown> = {}, handlers: Record<string, Handler> = {}) {
  const agent = ctx.start(["acp"])
  const client = acp(agent, handlers)
  await initialize(client, clientCapabilities)
  const created = await client.request("session/new", { cwd: ctx.project, mcpServers: [] })
  return { agent, client, sessionId: created.result.sessionId as string }
}

const text = (value: string) => ({ type: "text", text: value })
const prompt = (value: string) => [text(value)]

/** Waits until the fake model has received `count` requests (work the client is not shown). */
async function requests(ctx: CliCtx, count: number) {
  const deadline = Date.now() + 30_000
  while (ctx.provider.requests.length < count) {
    if (Date.now() > deadline) throw new Error(`expected ${count} provider requests, got ${ctx.provider.requests.length}`)
    await Bun.sleep(20)
  }
}

cliScenario(
  "acp config options select model, effort and mode",
  { config: only(), turns: [say("High."), say("Low."), say("Planned."), say("Building."), say("Built.")] },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx)
    const set = (configId: string, value: unknown) => client.request("session/set_config_option", { sessionId, configId, value })
    // A switch is echoed back by the server as a selection event, which the agent folds into the session's
    // selection when it arrives. A prompt after each switch lets the echo arrive before the next switch: one
    // that arrived after a newer switch would revert it.
    await set("model", `${MODEL}/high`)
    await client.request("session/prompt", { sessionId, prompt: prompt("Think hard") })
    await set("effort", "low")
    await client.request("session/prompt", { sessionId, prompt: prompt("Think less") })
    await set("effort", "turbo")
    await set("model", MODEL)
    await set("model", "openrouter/nonexistent")
    await set("model", `${MODEL}/turbo`)
    await set("mode", "plan")
    await set("mode", "nonexistent")
    await set("verbosity", "high")
    await client.request("session/set_config_option", { sessionId, configId: "mode", type: "boolean", value: true })
    await client.request("session/set_config_option", { sessionId: "ses_doesNotExist0000000000001", configId: "mode", value: "plan" })
    await client.request("session/prompt", { sessionId, prompt: prompt("Plan something") })
    await client.request("session/set_mode", { sessionId, modeId: "build" })
    await client.request("session/prompt", { sessionId, prompt: prompt("Start building") })
    await client.request("session/set_mode", { sessionId, modeId: "nonexistent" })
    await set("effort", "default")
    await client.request("session/prompt", { sessionId, prompt: prompt("Build something") })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp slash commands",
  {
    files: { ".opencode/commands/greet.md": "---\ndescription: Greet someone\n---\nSay hello to $ARGUMENTS.\n" },
    config: only(),
    turns: [say("Hello, Ada."), say("Not a command, but hello.")],
  },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx)
    // A command is submitted and the prompt returns at once; its reply is not part of the turn.
    await client.request("session/prompt", { sessionId, prompt: prompt("  /greet   Ada  ") })
    await requests(ctx, 2)
    agent.end()
    await agent.exited(30_000)
    const second = await open(ctx)
    await second.client.request("session/prompt", { sessionId: second.sessionId, prompt: prompt("/frobnicate the widget") })
    second.agent.end()
    await second.agent.exited(30_000)
  },
)

cliScenario(
  "acp compaction updates",
  {
    config: only(),
    turns: [
      say("Hello."),
      SUMMARY,
      say("Hello again."),
      { status: 401, body: { error: { message: "No auth credentials found" } } },
    ],
  },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx, { session: { compaction: {} } })
    await client.request("session/prompt", { sessionId, prompt: prompt("Hello") })
    await client.request("session/prompt", { sessionId, prompt: prompt("/compact") })
    await client.request("session/prompt", { sessionId, prompt: prompt("Hello again") })
    await client.request("session/prompt", { sessionId, prompt: prompt("/compact") })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp retries are reported as session info",
  {
    config: only(),
    turns: [{ status: 429, body: { error: { message: "Rate limit exceeded" } }, headers: { "retry-after-ms": "300" } }, say("Answered after the retry.")],
  },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx)
    await client.request("session/prompt", { sessionId, prompt: prompt("Answer please") })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp stop reasons, usage and failures",
  {
    config: only(),
    turns: [
      say("Counted.", { prompt: 120, completion: 30, reasoning: 12, cached: 40 }),
      { events: [{ text: "Filtered" }], finish: "content_filter" },
      { status: 401, body: { error: { message: "No auth credentials found", code: 401 } } },
      { status: 400, body: { error: { message: "Bad request: context too long", code: 400 } } },
      // A step cut at the output limit continues in another step.
      { events: [{ text: "This answer is cut" }], finish: "length" },
      say(" and ends here."),
    ],
  },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx)
    for (const value of ["Count", "Filter", "Auth", "Bad", "Cut"]) await client.request("session/prompt", { sessionId, prompt: prompt(value) })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp cancels a running tool",
  {
    config: only(rule("shell", "*", "allow")),
    turns: [call("shell", { command: "sleep 30" }), call("shell", { command: "sleep 31" }), say("Still here.")],
  },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx)
    const update = (command: string) => (message: { method?: string; params?: any }) =>
      message.method === "session/update" && message.params?.update?.status === "in_progress" && message.params?.update?.rawInput?.command === command
    // The call starts (one update), then the shell reports its process (a second, throttled): cancel once both came.
    const running = async (command: string) => {
      await client.until(update(command))
      await client.until(update(command))
    }
    const first = client.send("session/prompt", { sessionId, prompt: prompt("Sleep") })
    await running("sleep 30")
    client.notify("session/cancel", { sessionId })
    await client.response(first)
    // A cancelled request cancels its turn like `session/cancel`, and still answers.
    const second = client.send("session/prompt", { sessionId, prompt: prompt("Sleep again") })
    await running("sleep 31")
    client.notify("$/cancel_request", { requestId: second })
    await client.response(second)
    // Cancelling an idle session does nothing.
    client.notify("session/cancel", { sessionId })
    await client.request("session/prompt", { sessionId, prompt: prompt("Are you there?") })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp prompt content",
  { files: { "notes.txt": "some notes\n" }, config: only(), turns: [say("Seen."), say("Seen again."), say("Seen once more.")] },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx)
    await client.request("session/prompt", {
      sessionId,
      prompt: [
        text("Look at these"),
        { type: "resource_link", uri: "https://example.com/docs/page", name: "Docs" },
        { type: "resource_link", uri: "https://example.com/plain", name: "" },
        { type: "resource_link", uri: `file://${ctx.project}/missing.txt`, name: "missing.txt" },
        { type: "resource_link", uri: `zed:///agent/file?path=${encodeURIComponent(path.join(ctx.project, "notes.txt"))}`, name: "" },
        { type: "resource_link", uri: `file://${ctx.project}/notes.txt`, name: "" },
      ],
    })
    await client.request("session/prompt", {
      sessionId,
      prompt: [
        { type: "image", mimeType: "image/png", data: "", uri: "https://example.com/pixel.png" },
        { type: "image", mimeType: "image/png", data: PNG, uri: `file://${ctx.project}/pixel.png` },
        { type: "resource", resource: { uri: "file:///tmp/blob.bin", mimeType: "application/octet-stream", blob: "AAEC" } },
        { type: "resource", resource: { uri: "untitled:Scratch", text: "scratch text" } },
        { type: "resource", resource: { uri: "file:///nothing", blob: "AAEC" } },
        { ...text("Shown to the user only"), annotations: { audience: ["user"] } },
        { ...text("Context for the model"), annotations: { audience: ["assistant"] } },
        text("What do you see?"),
      ],
    })
    await client.request("session/prompt", { sessionId, prompt: [{ type: "image", mimeType: "image/png", data: "" }] })
    await client.request("session/prompt", { sessionId, prompt: [{ type: "audio", mimeType: "audio/wav", data: "AAEC" }, text("And this?")] })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp tool call updates",
  {
    files: { "src/a.ts": "export const a = 1\n" },
    config: only(rule("read", "*", "allow"), rule("glob", "*", "allow"), rule("grep", "*", "allow"), rule("todowrite", "*", "allow")),
    turns: [
      call("glob", { pattern: "**/*.ts", path: "src" }),
      call("grep", { pattern: "const", path: "src" }),
      call("read", { path: "src/a.ts", offset: 1, limit: 1 }),
      call("todowrite", { todos: [{ content: "Check a.ts", status: "completed", priority: "high" }] }),
      call("unknown_tool", { anything: true }),
      say("Looked around."),
    ],
  },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx)
    await client.request("session/prompt", { sessionId, prompt: prompt("Look around") })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp session lifecycle",
  { config: only(), turns: [say("Hello."), say("Hello from the fork.")] },
  async (ctx) => {
    const outside = path.join(path.dirname(ctx.project), "shared-docs")
    const { agent, client, sessionId } = await open(ctx)
    await client.request("session/prompt", { sessionId, prompt: prompt("Hello") })
    const extra = await client.request("session/new", { cwd: ctx.project, mcpServers: [], additionalDirectories: [outside, ctx.project, outside] })
    const unused = await client.request("session/new", { cwd: ctx.project, mcpServers: [] })
    await client.request("session/list", { cwd: ctx.project })
    await client.request("session/list", {})
    const forked = await client.request("session/fork", { sessionId, cwd: ctx.project, mcpServers: [], additionalDirectories: [outside] })
    await client.request("session/prompt", { sessionId: forked.result.sessionId, prompt: prompt("Hello fork") })
    await client.request("session/resume", { sessionId: extra.result.sessionId, cwd: ctx.project, mcpServers: [] })
    await client.request("session/load", { sessionId, cwd: ctx.project, mcpServers: [], additionalDirectories: [outside] })
    // Closing an unprompted session removes it; closing a prompted one keeps it.
    await client.request("session/close", { sessionId: unused.result.sessionId })
    await client.request("session/close", { sessionId })
    await client.request("session/prompt", { sessionId, prompt: prompt("Closed?") })
    await client.request("session/delete", { sessionId: forked.result.sessionId })
    await client.request("session/delete", { sessionId: "ses_doesNotExist0000000000001" })
    await client.request("session/list", { cwd: ctx.project })
    await client.request("session/list", { cwd: "relative" })
    await client.request("session/new", { cwd: ctx.project, mcpServers: [{ type: "sse", name: "events", url: "https://example.com/sse", headers: [] }] })
    await client.request("authenticate", { methodId: "opencode-login" })
    await client.request("authenticate", { methodId: "password" })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp commands added while a session is attached",
  { config: only() },
  async (ctx) => {
    const { agent, client } = await open(ctx)
    mkdirSync(path.join(ctx.project, ".opencode", "commands"), { recursive: true })
    writeFileSync(path.join(ctx.project, ".opencode", "commands", "later.md"), "---\ndescription: Added later\n---\nDo it later.\n")
    await client.until((message) => message.method === "session/update" && JSON.stringify(message.params).includes('"later"'))
    agent.end()
    await agent.exited(30_000)
  },
)
