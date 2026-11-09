// `opencode acp` forms: the question tool's form reaches an ACP client that supports form elicitation as
// `elicitation/create` (a JSON schema of its questions), and the client's answer becomes the form's reply:
// accepted answers (options, custom answers, several questions), a decline, a cancel, an error and malformed
// content (all cancel the form, which ends the turn). A client without form elicitation, or a form that asks for
// something credential-like, never sees the form: it is cancelled with a message the model continues from.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say } from "../lib/openrouter"
import { cliScenario, type CliCtx } from "../lib/cli"
import { acp, initialize, type Answer, type Handler } from "../lib/acp"

const rule = (action: string, resource: string, effect: string) => ({ action, resource, effect })
const config = {
  permissions: [...HIDDEN_TOOLS.map((action) => rule(action, "*", "deny")), rule("*", "*", "deny"), rule("question", "*", "allow")],
}

const color = {
  question: "Which color should the button be?",
  header: "Color",
  options: [
    { label: "Blue", description: "Calm" },
    { label: "Red", description: "Bold" },
  ],
}
const extras = {
  question: "Which extras?",
  header: "Extras",
  multiple: true,
  options: [
    { label: "Shadow", description: "A drop shadow" },
    { label: "Border", description: "A thin border" },
  ],
}
const size = { question: "Which size?", header: "Size", options: [{ label: "Small", description: "Compact" }, { label: "Large", description: "Roomy" }] }

async function open(ctx: CliCtx, clientCapabilities: Record<string, unknown>, handlers: Record<string, Handler>) {
  const agent = ctx.start(["acp"])
  const client = acp(agent, handlers)
  await initialize(client, clientCapabilities)
  const created = await client.request("session/new", { cwd: ctx.project, mcpServers: [] })
  return { agent, client, sessionId: created.result.sessionId as string }
}

const prompt = (text: string) => [{ type: "text", text }]
const forms = { elicitation: { form: {} } }

cliScenario(
  "acp answers questions through form elicitation",
  {
    config,
    turns: [
      call("question", { questions: [color] }),
      say("Blue it is."),
      call("question", { questions: [color] }),
      say("Green, then."),
      call("question", { questions: [extras, size] }),
      say("Shadow, border and glow, large."),
    ],
  },
  async (ctx) => {
    const answers: Answer[] = [
      { result: { action: "accept", content: { q0: "Blue" } } },
      { result: { action: "accept", content: { q0: "Red", q0_custom: "Green" } } },
      { result: { action: "accept", content: { q0: ["Shadow", "Border"], q0_custom: "Glow", q1: "Large" } } },
    ]
    const { agent, client, sessionId } = await open(ctx, forms, { "elicitation/create": () => answers.shift() })
    await client.request("session/prompt", { sessionId, prompt: prompt("Style the button") })
    await client.request("session/prompt", { sessionId, prompt: prompt("Style it again") })
    await client.request("session/prompt", { sessionId, prompt: prompt("Add extras") })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp form declined, cancelled, failed or malformed",
  {
    config,
    // A cancelled form ends the turn: the model is not asked again.
    turns: [1, 2, 3, 4, 5].map(() => call("question", { questions: [color] })),
  },
  async (ctx) => {
    const answers: Answer[] = [
      { result: { action: "decline" } },
      { result: { action: "cancel" } },
      { error: { code: -32603, message: "Internal error" } },
      { result: { action: "accept", content: { q0: { nested: true } } } },
      { result: null },
    ]
    const { agent, client, sessionId } = await open(ctx, forms, { "elicitation/create": () => answers.shift() })
    for (const text of ["Declined", "Cancelled", "Failed", "Malformed", "Empty"])
      await client.request("session/prompt", { sessionId, prompt: prompt(text) })
    agent.end()
    await agent.exited(30_000)
  },
)

cliScenario(
  "acp questions a client cannot show are cancelled with a message",
  {
    config,
    turns: [
      call("question", { questions: [color] }),
      say("I'll pick blue."),
      call("question", { questions: [{ ...color, header: "API key", question: "Which token should I use?" }] }),
      say("I'll use the default."),
    ],
  },
  async (ctx) => {
    // Without form elicitation first, then with it but for a credential-like question.
    const plain = await open(ctx, {}, {})
    await plain.client.request("session/prompt", { sessionId: plain.sessionId, prompt: prompt("Style the button") })
    plain.agent.end()
    await plain.agent.exited(30_000)
    const capable = await open(ctx, forms, { "elicitation/create": () => ({ result: { action: "cancel" } }) })
    await capable.client.request("session/prompt", { sessionId: capable.sessionId, prompt: prompt("Pick a key") })
    capable.agent.end()
    await capable.agent.exited(30_000)
  },
)

cliScenario(
  "acp cancelling a prompt withdraws its elicitation",
  { config, turns: [call("question", { questions: [color] }), say("Back after the cancel.")] },
  async (ctx) => {
    const { agent, client, sessionId } = await open(ctx, forms, { "elicitation/create": () => undefined })
    const pending = client.send("session/prompt", { sessionId, prompt: prompt("Style the button") })
    await client.until((message) => message.method === "elicitation/create")
    client.notify("session/cancel", { sessionId })
    await client.response(pending)
    client.answer(0, { result: { action: "accept", content: { q0: "Blue" } } })
    await client.request("session/prompt", { sessionId, prompt: prompt("Are you there?") })
    agent.end()
    await agent.exited(30_000)
  },
)
