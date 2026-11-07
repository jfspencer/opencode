// Background jobs: a background subagent started by the model shows up in the job list (filtered by session, kind,
// status and descendants) while it runs and after it completes.
import { call, say, type ProviderRequest, type Turn } from "../lib/openrouter"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await body(ctx)
    await ctx.until((event) => event?.type === "plugin.updated")
  })

const CHILD = "CHILD-TASK: summarize the repository"

/** Answers the child session's request (identified by its task text), so parent and child never race for turns. */
function child(turn: Turn) {
  let used = false
  return (request: ProviderRequest) => {
    if (used || request.kind !== "chat") return undefined
    // The child's first user message carries the task; the parent only mentions it inside its tool call.
    const messages = (request.body as { messages?: Array<{ role: string; content: unknown }> }).messages ?? []
    const first = messages.find((message) => message.role === "user")
    if (!JSON.stringify(first?.content ?? "").includes(CHILD)) return undefined
    used = true
    return turn
  }
}

let release: () => void = () => undefined
const gate = new Promise<void>((resolve) => (release = resolve))

scenario(
  "background subagent job",
  {
    turns: [
      call("subagent", { agent: "general", description: "Summarize repo", prompt: CHILD, background: true }),
      say("Started the summary in the background."),
      say("The summary is done."),
    ],
    responders: [child({ events: [{ until: gate }, { text: "The repository is small." }] })],
  },
  async (ctx) => {
    const id = await ctx.session()
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Summarize the repo in the background" })
    // The parent's first turn ends while the child is held at the gate.
    await ctx.until((event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === id, from)
    const created = await ctx.until((event) => event?.type === "session.created" && event?.data?.sessionID !== id, from)
    const childID = created.data.sessionID
    await ctx.http("GET", "/api/experimental/job")
    await ctx.http("GET", `/api/experimental/job?sessionID=${id}`)
    await ctx.http("GET", `/api/experimental/job?sessionID=${id}&kind=subagent&status=running`)
    await ctx.http("GET", `/api/experimental/job?sessionID=${id}&kind=shell`)
    await ctx.http("GET", `/api/experimental/job?sessionID=${childID}&descendants=true`)
    await ctx.http("GET", `/api/experimental/job?sessionID=${id}&descendants=true`)
    const done = ctx.events.length
    release()
    await ctx.until((event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === childID, done)
    await ctx.until((event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === id, done)
    await ctx.wait(id)
    await ctx.http("GET", `/api/experimental/job?sessionID=${id}`)
    await ctx.http("GET", `/api/experimental/job?sessionID=${id}&status=completed`)
    await ctx.http("GET", "/api/experimental/job?status=bogus")
    await ctx.http("GET", "/api/experimental/job?sessionID=ses_00000000000000000000000000")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

function makeGate() {
  let open: () => void = () => undefined
  const promise = new Promise<void>((resolve) => (open = resolve))
  return { promise, open }
}

const moved = makeGate()
scenario(
  "foreground subagent moved to the background",
  {
    turns: [
      call("subagent", { agent: "general", description: "Long research", prompt: CHILD }),
      say("Continuing while the research runs."),
      say("Noted that the research moved to the background."),
      say("Research finished."),
    ],
    responders: [child({ events: [{ until: moved.promise }, { text: "Findings." }] })],
  },
  async (ctx) => {
    const id = await ctx.session()
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Research this" })
    const created = await ctx.until((event) => event?.type === "session.created" && event?.data?.sessionID !== id, from)
    const childID = created.data.sessionID
    // The child is held at the gate (its step has not started streaming) while the parent's subagent call blocks on it.
    await ctx.until((event) => event?.type === "session.inbox.delivered" && event?.data?.sessionID === childID, from)
    await ctx.http("GET", `/api/experimental/job?sessionID=${id}`)
    await ctx.http("POST", `/api/session/${id}/background`)
    await ctx.until((event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === id, from)
    await ctx.http("GET", `/api/experimental/job?sessionID=${id}`)
    const done = ctx.events.length
    moved.open()
    await ctx.until((event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === childID, done)
    await ctx.until((event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === id, done)
    await ctx.wait(id)
    await ctx.http("POST", `/api/session/${id}/background`)
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

const cancelled = makeGate()
scenario(
  "deleting a session cancels its background jobs",
  {
    turns: [
      call("subagent", { agent: "general", description: "Doomed task", prompt: CHILD, background: true }),
      say("Started."),
    ],
    responders: [child({ events: [{ until: cancelled.promise }, { text: "Too late." }] })],
  },
  async (ctx) => {
    const id = await ctx.session()
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Start it" })
    await ctx.until((event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === id, from)
    await ctx.http("GET", "/api/experimental/job")
    await ctx.http("DELETE", `/api/session/${id}`)
    await ctx.http("GET", "/api/experimental/job")
    void cancelled
  },
)
