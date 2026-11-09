// The subagent tool runs an agent in a child session. Child sessions issue their own model requests, so the
// model is scripted per session (the parent is the first session that talks to the model, children follow in
// order of their first request); gates hold a session's stream to make cross-session timing deterministic.
import { HIDDEN_TOOLS, MODEL_REF } from "../lib/model"
import { call, say, type ProviderRequest, type Responder, type Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

type Step = Turn | ((request: ProviderRequest) => Turn)

/**
 * Answers chat requests from one script per session, by order of each session's first request. Child sessions
 * share their root's `prompt_cache_key`, so a session is told apart by its first user message (each subagent
 * prompt in a scenario is distinct).
 */
function bySession(...scripts: Step[][]): Responder {
  const order: string[] = []
  const cursors: number[] = []
  return (request) => {
    if (request.kind !== "chat") return
    const messages = (request.body as { messages?: { role: string; content: unknown }[] })?.messages ?? []
    const key = JSON.stringify(messages.find((message) => message.role === "user")?.content)
    if (!order.includes(key)) order.push(key)
    const index = order.indexOf(key)
    const cursor = cursors[index] ?? 0
    cursors[index] = cursor + 1
    const step = scripts[index]?.[cursor]
    if (step === undefined) return { status: 500, body: { error: { message: "conformance: script exhausted" } } }
    return typeof step === "function" ? step(request) : step
  }
}

function gate() {
  let open!: () => void
  const promise = new Promise<void>((resolve) => (open = resolve))
  return { promise, open }
}

/** The child session ID reported by the most recent subagent tool result in a request. */
function childID(request: ProviderRequest) {
  const messages = (request.body as { messages: { role: string; content: unknown }[] }).messages
  const text = messages
    .filter((message) => message.role === "tool" || message.role === "user")
    .map((message) => (typeof message.content === "string" ? message.content : JSON.stringify(message.content)))
    .join("\n")
  const found = [...text.matchAll(/sessionID[=:]\s*\\?"?(ses_[0-9A-Za-z]+)/g)].at(-1)
  if (!found) throw new Error("no child session in request")
  return found[1]
}

const files = { "src/a.ts": "export const a = 1\n", "src/b.ts": "export const b = 2\n" }

async function run(ctx: Ctx, text: string) {
  const id = await ctx.session()
  await ctx.prompt(id, text)
  return id
}

async function children(ctx: Ctx, parent: string) {
  const list = await ctx.http("GET", `/api/session?parentID=${parent}`)
  return list
}

/** Waits until the session has finished `count` executions (counting from the start of the scenario). */
async function executions(ctx: Ctx, sessionID: string, count: number) {
  const done = (event: any) =>
    (event?.type === "session.execution.succeeded" || event?.type === "session.execution.failed") &&
    event?.data?.sessionID === sessionID
  await ctx.until(() => ctx.events.filter(done).length >= count, 0, 30_000)
  ctx.note("executions", { sessionID, count })
}

scenario(
  "subagent general in the foreground",
  {
    files,
    responders: [
      bySession(
        [
          call("subagent", { agent: "general", description: "Count TS files", prompt: "Count the TypeScript files in src." }),
          say("The subagent counted them."),
        ],
        [call("glob", { pattern: "src/a*.ts" }), say("There is 1 matching TypeScript file.")],
      ),
    ],
  },
  async (ctx) => {
    const id = await run(ctx, "How many TS files are there? Use a subagent.")
    await ctx.http("GET", `/api/session/${id}/message`)
    const list = await children(ctx, id)
    const child = list.body?.data?.find?.((session: any) => session.parentID === id)?.id
    if (child) {
      await ctx.http("GET", `/api/session/${child}`)
      await ctx.http("GET", `/api/session/${child}/message`)
    }
  },
)

scenario(
  "subagent explore in the foreground",
  {
    files,
    responders: [
      bySession(
        [
          call("subagent", { agent: "explore", description: "Find exports", prompt: "quick: find exported constants" }),
          say("Explored."),
        ],
        [call("grep", { pattern: "export const b" }), say("Found `b`.")],
      ),
    ],
  },
  async (ctx) => {
    await run(ctx, "Find exported constants with an explorer.")
  },
)

scenario(
  "subagent that finishes without text",
  {
    files,
    responders: [
      bySession(
        [call("subagent", { agent: "general", description: "Silent work", prompt: "Read a.ts" }), say("Nothing said.")],
        [call("read", { path: "src/a.ts" }), { events: [], finish: "stop" }],
      ),
    ],
  },
  async (ctx) => {
    await run(ctx, "Delegate silently.")
  },
)

scenario(
  "subagent with unknown and primary agents",
  {
    responders: [
      bySession([
        call("subagent", { agent: "nope", description: "Nothing", prompt: "x" }),
        call("subagent", { agent: "build", description: "Build agent", prompt: "x" }),
        call("subagent", { agent: "general", description: "Bad session", prompt: "x", sessionID: "ses_doesnotexist" }),
        say("All failed."),
      ]),
    ],
  },
  async (ctx) => {
    await run(ctx, "Try agents.")
  },
)

scenario(
  "subagent with model overrides",
  {
    responders: [
      bySession(
        [
          call("subagent", { agent: "general", description: "Bad ref", prompt: "x", model: "not-a-model-ref" }),
          call("subagent", { agent: "general", description: "Missing", prompt: "x", model: "openrouter/missing-model" }),
          call("subagent", { agent: "general", description: "Bad variant", prompt: "x", model: `${MODEL_REF}#ultra` }),
          call("subagent", { agent: "general", description: "Low effort", prompt: "Say hi.", model: `${MODEL_REF}#low` }),
          say("Done."),
        ],
        [say("hi")],
      ),
    ],
  },
  async (ctx) => {
    const id = await run(ctx, "Use specific models.")
    await ctx.http("GET", `/api/session?parentID=${id}`)
  },
)

scenario(
  "subagent with empty optional arguments",
  {
    responders: [
      bySession(
        [
          call("subagent", { agent: "general", description: "Empty optionals", prompt: "Say ok.", model: "", sessionID: "" }),
          say("Done."),
        ],
        [say("ok")],
      ),
    ],
  },
  async (ctx) => {
    await run(ctx, "Delegate.")
  },
)

scenario(
  "subagent continued by sessionID",
  {
    responders: [
      bySession(
        [
          call("subagent", { agent: "general", description: "First task", prompt: "Remember the number 7." }),
          (request) =>
            call("subagent", {
              agent: "general",
              description: "Follow-up",
              prompt: "What number did I give you?",
              sessionID: childID(request),
            }),
          (request) =>
            call("subagent", {
              agent: "explore",
              description: "Switch agent",
              prompt: "Now explore.",
              sessionID: childID(request),
            }),
          say("The subagent remembered."),
        ],
        [say("Remembered 7."), say("You gave me 7."), say("Explored as explore.")],
      ),
    ],
  },
  async (ctx) => {
    const id = await run(ctx, "Delegate twice.")
    const list = await children(ctx, id)
    const child = list.body?.data?.find?.((session: any) => session.parentID === id)?.id
    if (child) await ctx.http("GET", `/api/session/${child}/message`)
  },
)

scenario(
  "subagent continuation of a session that is not a child",
  { turns: [] },
  async (ctx) => {
    const other = await ctx.session()
    ctx.provider.push(
      call("subagent", { agent: "general", description: "Foreign", prompt: "x", sessionID: other }),
      say("Refused."),
    )
    const id = await ctx.session()
    await ctx.prompt(id, "Continue someone else's session.")
  },
)

scenario(
  "subagent failure in the child",
  {
    responders: [
      bySession(
        [call("subagent", { agent: "general", description: "Will fail", prompt: "Fail." }), say("It failed.")],
        [{ status: 400, body: { error: { message: "Bad request from provider", code: 400 } } }],
      ),
    ],
  },
  async (ctx) => {
    const id = await run(ctx, "Delegate something that fails.")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

// Built-in subagents may not delegate; a configured one may, up to the depth limit.
const delegator = {
  agents: {
    delegator: { mode: "subagent", description: "Delegates work to other subagents.", model: `${MODEL_REF}#max` },
  },
}

scenario(
  "subagent depth limit",
  {
    config: delegator,
    responders: [
      bySession(
        [call("subagent", { agent: "delegator", description: "Outer", prompt: "Delegate again." }), say("Done.")],
        [call("subagent", { agent: "general", description: "Inner", prompt: "Nested." }), say("Could not nest.")],
      ),
    ],
  },
  async (ctx) => {
    await run(ctx, "Nest subagents.")
  },
)

scenario(
  "subagent nested within a raised depth limit",
  {
    config: { ...delegator, experimental: { subagent_depth: 2 } },
    responders: [
      bySession(
        [call("subagent", { agent: "delegator", description: "Outer", prompt: "Delegate again." }), say("Done.")],
        [call("subagent", { agent: "explore", description: "Inner", prompt: "Nested." }), say("Nested ok.")],
        [say("Inner result.")],
      ),
    ],
  },
  async (ctx) => {
    await run(ctx, "Nest subagents.")
  },
)

scenario(
  "subagent denied by permission rules",
  {
    config: {
      permissions: [
        ...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })),
        { action: "subagent", resource: "general", effect: "deny" },
      ],
    },
    responders: [
      bySession([call("subagent", { agent: "general", description: "Denied", prompt: "x" }), say("Denied.")]),
    ],
  },
  async (ctx) => {
    await run(ctx, "Delegate to general.")
  },
)

{
  const childGate = gate()
  scenario(
    "subagent in the background",
    {
      files,
      responders: [
        bySession(
          [
            call("subagent", { agent: "general", description: "Background count", prompt: "Count files.", background: true }),
            say("Launched it."),
            say("The background subagent reported back."),
          ],
          [{ events: [{ until: childGate.promise }, { text: "Two files." }] }],
        ),
      ],
    },
    async (ctx) => {
      const id = await run(ctx, "Count files in the background.")
      // The child is held at the fake model: once its request has arrived there, its job is as the listing records it.
      await ctx.until(() =>
        ctx.provider.requests.some((request) => {
          if (request.kind !== "chat") return false
          const messages = (request.body as { messages?: { role: string; content: unknown }[] })?.messages ?? []
          return JSON.stringify(messages.find((message) => message.role === "user")?.content).includes('Count files."')
        }),
      )
      await ctx.http("GET", `/api/experimental/job?sessionID=${id}`)
      childGate.open()
      await executions(ctx, id, 2)
      await ctx.wait(id)
      await ctx.http("GET", `/api/session/${id}/message`)
    },
  )
}

{
  const childGate = gate()
  scenario(
    "subagent moved to the background while running",
    {
      responders: [
        bySession(
          [
            call("subagent", { agent: "general", description: "Long task", prompt: "Work for a while." }),
            say("It continues in the background."),
            say("Noted that the work moved to the background."),
            say("Background work finished."),
          ],
          [{ events: [{ until: childGate.promise }, { text: "Long task done." }] }],
        ),
      ],
    },
    async (ctx) => {
      const id = await ctx.session()
      await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Start a long task." })
      // The child is waiting on its first model reply.
      await ctx.until((event) => event?.type === "session.tool.called" && event?.data?.sessionID === id)
      await ctx.until(() => ctx.provider.requests.filter((request) => request.kind === "chat").length >= 2)
      await ctx.http("POST", `/api/session/${id}/background`)
      await executions(ctx, id, 1)
      childGate.open()
      await executions(ctx, id, 2)
      await ctx.wait(id)
      await ctx.http("GET", `/api/session/${id}/message`)
    },
  )
}

{
  const childGate = gate()
  scenario(
    "subagent child interrupted while the parent waits",
    {
      responders: [
        bySession(
          [call("subagent", { agent: "general", description: "Interrupted", prompt: "Take forever." }), say("Cancelled.")],
          [{ events: [{ reasoning: "Working on it." }, { until: childGate.promise }, { text: "never" }] }],
        ),
      ],
    },
    async (ctx) => {
      const id = await ctx.session()
      await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Start." })
      // A step starts with the first response chunk: interrupt once the child's step is under way.
      await ctx.until((event) => event?.type === "session.step.started" && event?.data?.sessionID !== id)
      const list = await children(ctx, id)
      const child = list.body?.data?.find?.((session: any) => session.parentID === id)?.id
      await ctx.http("POST", `/api/session/${child}/interrupt`)
      await executions(ctx, id, 1)
      await ctx.wait(id)
      childGate.open()
      await ctx.http("GET", `/api/session/${id}/message`)
    },
  )
}

{
  const childGate = gate()
  scenario(
    "subagent parent interrupted while the child runs",
    {
      responders: [
        bySession(
          [call("subagent", { agent: "general", description: "Orphaned", prompt: "Take forever." })],
          [{ events: [{ reasoning: "Working on it." }, { until: childGate.promise }, { text: "never" }] }],
        ),
      ],
    },
    async (ctx) => {
      const id = await ctx.session()
      await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Start." })
      // A step starts with the first response chunk: interrupt once the child's step is under way.
      await ctx.until((event) => event?.type === "session.step.started" && event?.data?.sessionID !== id)
      await ctx.http("POST", `/api/session/${id}/interrupt`)
      await ctx.wait(id)
      const list = await children(ctx, id)
      const child = list.body?.data?.find?.((session: any) => session.parentID === id)?.id
      if (child) {
        await ctx.wait(child)
        await ctx.http("GET", `/api/session/${child}/message`)
      }
      childGate.open()
      await ctx.http("GET", `/api/session/${id}/message`)
    },
  )
}
