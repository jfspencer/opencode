// The goal loop (core/src/plugin/goal.ts, a built-in plugin): `/goal <objective>` arms a goal and kicks the session
// off with a synthetic notice; whenever the session would go idle, a reviewer (a one-shot generation over the session's
// context) judges the objective and the loop continues the same execution, completes, or pauses the goal. `/goal pause`,
// `resume` and `clear` steer it; failures and interruptions pause it. Clients read a goal through the plugin's RPC
// (`POST /api/rpc/opencode.goal/get`) and follow `rpc.opencode.goal.changed` events (what the TUI's goal label does).
//
// Reviewer requests are ordinary chat requests of the session, so they take the next scripted turn.
import { call, say, type Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const files = { "README.md": "# Demo\n\nA small project.\n" }
const verdict = (value: Record<string, unknown>): Turn => say(JSON.stringify(value))

async function goal(ctx: Ctx, id: string) {
  await ctx.http("POST", "/api/rpc/opencode.goal/get", { input: { sessionID: id } })
}

async function command(ctx: Ctx, id: string, text: string, extra: Record<string, unknown> = {}) {
  return ctx.http("POST", `/api/session/${id}/command`, { name: "goal", text, ...extra })
}

async function run(ctx: Ctx, objective: string) {
  const id = await ctx.session({ title: "goal" })
  await command(ctx, id, objective)
  await ctx.settle(id)
  await goal(ctx, id)
  await ctx.http("GET", `/api/session/${id}/message`)
  return id
}

scenario(
  "a goal met after one round",
  { files, turns: [call("read", { path: "README.md" }), say("Read it."), verdict({ verdict: "done", reason: "The README was read." })] },
  (ctx) => run(ctx, "Read the README").then(() => undefined),
)

scenario(
  "a goal continued until it is met",
  {
    files,
    turns: [
      say("Getting started."),
      verdict({ verdict: "continue", reason: "Nothing was read yet.", next_step: "Read README.md" }),
      call("read", { path: "README.md" }),
      say("Read it."),
      verdict({ verdict: "done", reason: "The README was read." }),
    ],
  },
  (ctx) => run(ctx, "Read the README").then(() => undefined),
)

scenario(
  "a goal paused after rounds without progress",
  {
    turns: [
      say("Thinking."),
      verdict({ verdict: "continue", reason: "No evidence yet." }),
      say("Still thinking."),
      verdict({ verdict: "continue", reason: "Still no evidence." }),
      say("Thinking more."),
      verdict({ verdict: "continue", reason: "No evidence at all." }),
    ],
  },
  (ctx) => run(ctx, "Prove the theorem").then(() => undefined),
)

scenario(
  "a goal paused when the reviewer repeats its reason",
  {
    files,
    turns: [
      call("read", { path: "README.md" }),
      say("Read once."),
      verdict({ verdict: "continue", reason: "Read it again." }),
      call("read", { path: "README.md" }),
      say("Read twice."),
      verdict({ verdict: "continue", reason: "  read it AGAIN. " }),
      call("read", { path: "README.md" }),
      say("Read thrice."),
      verdict({ verdict: "continue", reason: "Read it again." }),
      call("read", { path: "README.md" }),
      say("Read four times."),
      verdict({ verdict: "continue", reason: "Read it again." }),
    ],
  },
  (ctx) => run(ctx, "Read the README until told to stop").then(() => undefined),
)

scenario(
  "a goal paused when its reviewer gives no usable verdict",
  { turns: [say("Tried once."), say("I am not sure."), say('```json\n{"verdict":"maybe","reason":"?"}\n```')] },
  (ctx) => run(ctx, "Ship the release").then(() => undefined),
)

scenario(
  "a reviewer verdict wrapped in prose is read",
  { turns: [say("Done."), say('Here is my verdict: {"verdict":"done","reason":"It is shipped."} Thanks.')] },
  (ctx) => run(ctx, "Ship the release").then(() => undefined),
)

scenario(
  "a reviewer waiting without background work is asked again",
  {
    turns: [
      say("Started."),
      verdict({ verdict: "wait", reason: "The build is running." }),
      verdict({ verdict: "blocked", reason: "Needs the signing key." }),
    ],
  },
  (ctx) => run(ctx, "Ship the release").then(() => undefined),
)

scenario(
  "a reviewer pausing the goal",
  { turns: [say("Stopping."), verdict({ verdict: "paused", reason: "The user asked to stop." })] },
  (ctx) => run(ctx, "Ship the release").then(() => undefined),
)

scenario(
  "goal commands without a goal",
  {},
  async (ctx) => {
    const id = await ctx.session({ title: "goal" })
    for (const text of ["", "   ", "pause", "resume", "clear", "PAUSE"]) await command(ctx, id, text)
    await goal(ctx, id)
    await command(ctx, id, "x".repeat(4_001))
  },
)

scenario(
  "a paused goal resumed, described and cleared",
  {
    turns: [
      say("Started."),
      verdict({ verdict: "blocked", reason: "Needs the signing key." }),
      say("Resumed."),
      verdict({ verdict: "done", reason: "Shipped." }),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "goal" })
    await command(ctx, id, '"Ship the release"')
    await ctx.settle(id)
    await command(ctx, id, "")
    await command(ctx, id, "pause")
    await command(ctx, id, "Resume")
    await ctx.settle(id)
    await goal(ctx, id)
    await command(ctx, id, "resume")
    await command(ctx, id, "")
    await command(ctx, id, "clear")
    await goal(ctx, id)
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

const pauseGate = Promise.withResolvers<void>()
scenario(
  "pausing an active goal",
  { turns: [{ events: [{ until: pauseGate.promise }, { text: "Working." }] }] },
  async (ctx) => {
    const id = await ctx.session({ title: "goal" })
    await command(ctx, id, "Ship the release")
    await ctx.until((event) => event?.type === "session.inbox.delivered")
    await command(ctx, id, "pause")
    await goal(ctx, id)
    pauseGate.resolve()
    await ctx.settle(id)
    await goal(ctx, id)
  },
)

const interruptGate = Promise.withResolvers<void>()
scenario(
  "interrupting the session pauses its goal",
  { turns: [{ events: [{ text: "Working." }, { until: interruptGate.promise }, { text: " More." }] }] },
  async (ctx) => {
    const id = await ctx.session({ title: "goal" })
    await command(ctx, id, "Ship the release")
    await ctx.until((event) => event?.type === "session.text.delta" || event?.type === "session.text.started")
    await ctx.http("POST", `/api/session/${id}/interrupt`)
    interruptGate.resolve()
    await ctx.settle(id)
    await goal(ctx, id)
  },
)

scenario(
  "a failing session pauses its goal",
  { turns: [{ status: 401, body: { error: { message: "No auth credentials found" } } }] },
  async (ctx) => {
    const id = await ctx.session({ title: "goal" })
    await command(ctx, id, "Ship the release")
    await ctx.settle(id)
    await goal(ctx, id)
  },
)

scenario(
  "a failing reviewer pauses the goal",
  { turns: [say("Started."), { status: 401, body: { error: { message: "No auth credentials found" } } }] },
  (ctx) => run(ctx, "Ship the release").then(() => undefined),
)

scenario(
  "a goal names an untitled session",
  { turns: [say("Started."), verdict({ verdict: "done", reason: "Shipped." })] },
  async (ctx) => {
    const id = await ctx.session()
    await command(ctx, id, "Ship   the\nrelease to every customer of the product, including the ones on the legacy plan")
    await ctx.settle(id)
    await ctx.http("GET", `/api/session/${id}`)
    await goal(ctx, id)
  },
)

scenario(
  "a goal with attachments starts as a prompt",
  { files, turns: [say("Read the attachment."), verdict({ verdict: "done", reason: "Read." })] },
  async (ctx) => {
    const id = await ctx.session({ title: "goal" })
    await command(ctx, id, "Summarize the README", { files: [{ uri: `file://${ctx.project}/README.md` }] })
    await ctx.settle(id)
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "a new goal replaces the current one",
  {
    turns: [
      say("First."),
      verdict({ verdict: "blocked", reason: "Needs input." }),
      say("Second."),
      verdict({ verdict: "done", reason: "Done." }),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "goal" })
    await command(ctx, id, "First objective")
    await ctx.settle(id)
    await command(ctx, id, "Second objective")
    await ctx.settle(id)
    await goal(ctx, id)
  },
)

scenario(
  "the goal RPC rejects unknown methods and bad input",
  {},
  async (ctx) => {
    const id = await ctx.session({ title: "goal" })
    await ctx.http("POST", "/api/rpc/opencode.goal/set", { input: { sessionID: id } })
    await ctx.http("POST", "/api/rpc/opencode.goal/get", { input: { session: id } })
    await ctx.http("POST", "/api/rpc/opencode.goal/get", {})
    await ctx.http("POST", "/api/rpc/opencode.missing/get", { input: { sessionID: id } })
    await ctx.http("POST", "/api/rpc/opencode.goal/get", { input: { sessionID: "ses_missing" } })
  },
)

scenario(
  "deleting a session drops its goal",
  { turns: [say("Started."), verdict({ verdict: "blocked", reason: "Needs input." })] },
  async (ctx) => {
    const id = await ctx.session({ title: "goal" })
    await command(ctx, id, "Ship the release")
    await ctx.settle(id)
    await ctx.http("DELETE", `/api/session/${id}`)
    await ctx.quiesce()
    await goal(ctx, id)
  },
)
