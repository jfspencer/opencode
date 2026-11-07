// Interrupting a step mid-stream, and input that arrives while a step runs: steering prompts (taken at the next
// step boundary), queued prompts (taken after the turn), and controls such as a manual compaction. The fake model
// holds its stream open on a gate the scenario releases, so what the loop has seen at each point is fixed.
import { call, say } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const files = { "README.md": "# Demo\n\nA small project.\n", "src/math.ts": "export const add = (a: number, b: number) => a + b\n" }

function gate() {
  let open!: () => void
  const promise = new Promise<void>((resolve) => (open = resolve))
  return { promise, open }
}

const event = (type: string, sessionID: string) => (e: any) => e?.type === type && e?.data?.sessionID === sessionID

/** Sends a prompt without waiting for the turn. */
async function send(ctx: Ctx, id: string, text: string, payload: Record<string, unknown> = {}) {
  return ctx.http("POST", `/api/session/${id}/prompt`, { text, ...payload })
}

async function messages(ctx: Ctx, id: string) {
  await ctx.http("GET", `/api/session/${id}`)
  await ctx.http("GET", `/api/session/${id}/message`)
}

const held = gate()
scenario(
  "interrupt while text is streaming",
  { turns: [{ events: [{ text: "The answer is" }, { until: held.promise }, { text: " never finished." }] }, say("Fresh answer.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "interrupt" })
    const from = ctx.events.length
    await send(ctx, id, "Answer slowly")
    await ctx.until(event("session.text.delta", id), from)
    await ctx.http("POST", `/api/session/${id}/interrupt`)
    await ctx.settle(id)
    held.open()
    await ctx.prompt(id, "Try again")
    await messages(ctx, id)
  },
)

const heldReasoning = gate()
scenario(
  "interrupt while reasoning",
  {
    turns: [
      { events: [{ reasoning: "Let me think about" }, { until: heldReasoning.promise }, { reasoning: " this." }] },
      say("Answer after the interrupted reasoning."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "interrupt" })
    const from = ctx.events.length
    await send(ctx, id, "Think hard")
    await ctx.until(event("session.reasoning.delta", id), from)
    await ctx.http("POST", `/api/session/${id}/interrupt`)
    await ctx.settle(id)
    heldReasoning.open()
    await ctx.prompt(id, "Just answer")
    await messages(ctx, id)
  },
)

const heldTool = gate()
scenario(
  "interrupt while a tool call is streaming",
  {
    files,
    turns: [{ events: [{ text: "Reading." }, { tool: "read", rawInput: '{"path": "READ' }, { until: heldTool.promise }] }, say("Second.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "interrupt" })
    const from = ctx.events.length
    await send(ctx, id, "Read the README")
    await ctx.until(event("session.tool.input.started", id), from)
    await ctx.http("POST", `/api/session/${id}/interrupt`)
    await ctx.settle(id)
    heldTool.open()
    await ctx.prompt(id, "Again")
    await messages(ctx, id)
  },
)

const heldStart = gate()
scenario(
  "interrupt before any output",
  { turns: [{ events: [{ until: heldStart.promise }, { text: "Too late." }] }, say("Second.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "interrupt" })
    await send(ctx, id, "Answer")
    await ctx.until(() => ctx.provider.requests.some((request) => request.kind === "chat"))
    // The step's start snapshot is captured while the request is in flight; let it finish (it fails fast outside
    // a git repository) so the interrupt always lands after it.
    await new Promise((resolve) => setTimeout(resolve, 500))
    await ctx.http("POST", `/api/session/${id}/interrupt`)
    await ctx.settle(id)
    heldStart.open()
    await ctx.prompt(id, "Again")
    await messages(ctx, id)
  },
)

scenario(
  "interrupt while a shell command runs",
  { files, timeout: 30_000, turns: [call("shell", { command: "echo started; sleep 20" }), say("Stopped.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "interrupt" })
    const from = ctx.events.length
    await send(ctx, id, "Sleep for a while")
    await ctx.until(event("session.tool.progress", id), from)
    await ctx.http("POST", `/api/session/${id}/interrupt`)
    await ctx.settle(id)
    await ctx.prompt(id, "Stop and summarize")
    await messages(ctx, id)
  },
)

const secrets = { ...files, ".env": "TOKEN=secret\n" }

scenario(
  "interrupt while waiting for a permission",
  { files: secrets, turns: [call("read", { path: ".env" }), say("Fine.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "interrupt" })
    const from = ctx.events.length
    await send(ctx, id, "Read the env file")
    await ctx.settle(id, from)
    await ctx.http("POST", `/api/session/${id}/interrupt`)
    await ctx.settle(id)
    await ctx.http("GET", `/api/session/${id}/permission`)
    await ctx.prompt(id, "Never mind")
    await messages(ctx, id)
  },
)

scenario(
  "decline a permission",
  { files: secrets, turns: [call("read", { path: ".env" }), say("Understood.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "interrupt" })
    const from = ctx.events.length
    await send(ctx, id, "Read the env file")
    await ctx.settle(id, from)
    const pending = await ctx.http("GET", `/api/session/${id}/permission`)
    await ctx.http("POST", `/api/session/${id}/permission/${pending.body.data[0].id}/reply`, { decision: "reject" })
    await ctx.settle(id)
    await messages(ctx, id)
  },
)

scenario(
  "decline a permission with feedback",
  { files: secrets, turns: [call("read", { path: ".env" }), say("I will not read it.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "interrupt" })
    const from = ctx.events.length
    await send(ctx, id, "Read the env file")
    await ctx.settle(id, from)
    const pending = await ctx.http("GET", `/api/session/${id}/permission`)
    await ctx.http("POST", `/api/session/${id}/permission/${pending.body.data[0].id}/reply`, {
      decision: "reject",
      message: "Do not read secrets.",
    })
    await ctx.settle(id)
    await messages(ctx, id)
  },
)

scenario("interrupt an idle session", {}, async (ctx) => {
  const id = await ctx.session({ title: "interrupt" })
  await ctx.http("POST", `/api/session/${id}/interrupt`)
  await messages(ctx, id)
})

const steerGate = gate()
scenario(
  "steering prompt joins at the next step",
  {
    files,
    turns: [
      { events: [{ tool: "read", input: { path: "README.md" } }, { until: steerGate.promise }] },
      say("Read the README, and noted the math file."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "steer" })
    const from = ctx.events.length
    await send(ctx, id, "Read the README")
    await ctx.until(event("session.tool.input.started", id), from)
    await send(ctx, id, "Also mention src/math.ts")
    steerGate.open()
    await ctx.settle(id)
    await messages(ctx, id)
  },
)

const queueGate = gate()
scenario(
  "queued prompt runs after the turn",
  {
    turns: [{ events: [{ text: "First answer" }, { until: queueGate.promise }, { text: " done." }] }, say("Second answer.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "queue" })
    const from = ctx.events.length
    await send(ctx, id, "First question")
    await ctx.until(event("session.text.delta", id), from)
    await send(ctx, id, "Second question", { delivery: "queue" })
    await ctx.http("GET", `/api/session/${id}/inbox`)
    queueGate.open()
    await ctx.until(() => ctx.provider.requests.filter((request) => request.kind === "chat").length >= 2)
    await ctx.settle(id)
    await messages(ctx, id)
  },
)

const interruptQueued = gate()
scenario(
  "interrupt keeps queued prompts parked",
  { turns: [{ events: [{ text: "Working" }, { until: interruptQueued.promise }] }, say("Steered answer.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "queue" })
    const from = ctx.events.length
    await send(ctx, id, "Start")
    await ctx.until(event("session.text.delta", id), from)
    await send(ctx, id, "Later", { delivery: "queue" })
    await ctx.http("POST", `/api/session/${id}/interrupt?resume=true`)
    await ctx.settle(id)
    interruptQueued.open()
    await ctx.http("GET", `/api/session/${id}/inbox`)
    await messages(ctx, id)
  },
)

const interruptSteer = gate()
scenario(
  "interrupt and resume with a steering prompt",
  { turns: [{ events: [{ text: "Working" }, { until: interruptSteer.promise }] }, say("Steered answer.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "steer" })
    const from = ctx.events.length
    await send(ctx, id, "Start")
    await ctx.until(event("session.text.delta", id), from)
    await send(ctx, id, "Change of plan")
    await ctx.http("POST", `/api/session/${id}/interrupt?resume=true`)
    await ctx.until(() => ctx.provider.requests.filter((request) => request.kind === "chat").length >= 2)
    await ctx.settle(id)
    interruptSteer.open()
    await messages(ctx, id)
  },
)

const compactGate = gate()
scenario(
  "manual compaction requested during a step",
  {
    files,
    turns: [
      { events: [{ tool: "read", input: { path: "README.md" } }, { until: compactGate.promise }] },
      say("## Objective\n- Describe the project\n\n## Next Move\n1. Answer"),
      say("It is a demo."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compact" })
    const from = ctx.events.length
    await send(ctx, id, "Read the README")
    await ctx.until(event("session.tool.input.started", id), from)
    await ctx.http("POST", `/api/session/${id}/compact`, {})
    compactGate.open()
    await ctx.settle(id)
    await messages(ctx, id)
  },
)

const cancelGate = gate()
scenario(
  "interrupt during manual compaction",
  {
    files,
    turns: [say("A demo."), { events: [{ text: "## Objective" }, { until: cancelGate.promise }] }, say("Still here.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compact" })
    await ctx.prompt(id, "What is this?")
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/compact`, {})
    await ctx.until(event("session.compaction.delta", id), from)
    await ctx.http("POST", `/api/session/${id}/interrupt`)
    await ctx.settle(id)
    cancelGate.open()
    await ctx.prompt(id, "Continue")
    await messages(ctx, id)
  },
)

const autoGate = gate()
scenario(
  "interrupt during automatic compaction",
  {
    files,
    turns: [say("A demo.", { prompt: 900_000 }), { events: [{ text: "## Objective" }, { until: autoGate.promise }] }, say("Still here.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "compact" })
    await ctx.prompt(id, "What is this?")
    const from = ctx.events.length
    await send(ctx, id, "And now?")
    await ctx.until(event("session.compaction.delta", id), from)
    await ctx.http("POST", `/api/session/${id}/interrupt`)
    await ctx.settle(id)
    autoGate.open()
    await messages(ctx, id)
  },
)

scenario(
  "prompt admitted without resuming",
  { turns: [say("Answering both.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "resume" })
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "First, parked", resume: false })
    await ctx.http("GET", `/api/session/${id}/inbox`)
    await ctx.prompt(id, "Second, runs both")
    await messages(ctx, id)
  },
)

scenario(
  "synthetic message runs the loop",
  { turns: [say("Handled the synthetic input.")] },
  async (ctx) => {
    const id = await ctx.session({ title: "synthetic" })
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/synthetic`, { text: "A background job finished.", description: "job" })
    await ctx.settle(id, from)
    await messages(ctx, id)
  },
)

const cancelQueued = gate()
scenario(
  "cancel a queued prompt",
  { turns: [{ events: [{ text: "Busy" }, { until: cancelQueued.promise }] }] },
  async (ctx) => {
    const id = await ctx.session({ title: "queue" })
    const from = ctx.events.length
    await send(ctx, id, "Start")
    await ctx.until(event("session.text.delta", id), from)
    const queued = await send(ctx, id, "Later", { delivery: "queue" })
    await ctx.http("DELETE", `/api/session/${id}/inbox/${queued.body.data.id}`)
    cancelQueued.open()
    await ctx.settle(id)
    await messages(ctx, id)
  },
)
