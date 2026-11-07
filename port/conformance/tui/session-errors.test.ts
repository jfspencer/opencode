// Session view: provider errors, retries and interrupting a running step, as the user sees them.
import { say, type Responder, type Turn } from "../lib/openrouter"
import { tuiScenario, type TuiCtx, type TuiOptions } from "../lib/tui"

// Title generation runs concurrently with the first step, so its events would interleave with the step's at
// random. Hold the first step until the session is renamed: the recorded order is then fixed. A first turn
// that is an HTTP error cannot be held; hold the title instead (see `heldTitle`).
function scenario(name: string, options: TuiOptions, body: (t: TuiCtx) => Promise<void>) {
  const titled = Promise.withResolvers<void>()
  const [first, ...rest] = options.turns ?? []
  const turns: Turn[] | undefined =
    first && "events" in first ? [{ ...first, events: [{ until: titled.promise }, ...first.events] }, ...rest] : options.turns
  tuiScenario(name, { ...options, turns }, async (t) => {
    const started = Date.now()
    t.until((event) => event?.type === "session.renamed", 0, 60_000).then(titled.resolve, titled.resolve)
    await body(t)
    // The TUI polls /api/experimental/migration/v1 one second after it starts; let that happen in every
    // scenario so the recorded set of endpoints does not depend on how long the scenario took.
    await new Promise((resolve) => setTimeout(resolve, Math.max(0, 2_000 - (Date.now() - started))))
  })
}

/** Answer the title request only once the scenario releases it. */
function heldTitle() {
  const gate = Promise.withResolvers<void>()
  const responder: Responder = (request) =>
    request.kind === "title" ? { events: [{ until: gate.promise }, { text: "Conformance session" }] } : undefined
  return { gate, responder }
}

const error = (status: number, body?: unknown, headers?: Record<string, string>): Turn => ({ status, body, headers })

async function submit(t: TuiCtx, prompt: string) {
  await t.type(prompt)
  await t.enter()
}

/** t.waitFor gives up after a fixed number of rendered frames; keep waiting through quiet periods (retries). */
async function waitLong(t: TuiCtx, match: string | RegExp, ms = 30_000) {
  const deadline = Date.now() + ms
  while (true) {
    try {
      return await t.waitFor(match)
    } catch (error) {
      if (Date.now() > deadline) throw error
      await new Promise((resolve) => setTimeout(resolve, 50))
    }
  }
}

/** The first step fails: wait for the failure to be acknowledged, then let the title in. */
async function failed(t: TuiCtx, title: ReturnType<typeof heldTitle>, message: string) {
  await t.until((event) => event?.type === "session.viewed", 0, 60_000)
  title.gate.resolve()
  await t.until((event) => event?.type === "session.renamed", 0, 60_000)
  await waitLong(t, new RegExp(`Conformance session[\\s\\S]*Session failed[\\s\\S]*Error: ${message}`))
}

{
  const title = heldTitle()
  scenario(
    "provider error that is not retried",
    {
      responders: [title.responder],
      turns: [error(500, { error: { message: "Internal Server Error" } }, { "x-should-retry": "false" })],
    },
    async (t) => {
      await submit(t, "Answer please")
      await failed(t, title, "Internal Server Error")
      await t.frame("failed")
    },
  )
}

{
  const title = heldTitle()
  scenario(
    "unauthorized provider error",
    { responders: [title.responder], turns: [error(401, { error: { message: "No auth credentials found", code: 401 } })] },
    async (t) => {
      await submit(t, "Answer please")
      await failed(t, title, "No auth credentials found")
      await t.frame("failed")
    },
  )
}

scenario(
  "provider error after a tool step",
  {
    files: { "a.txt": "a\n" },
    turns: [
      { events: [{ text: "Reading first." }, { tool: "read", input: { path: "a.txt" } }] },
      error(400, { error: { message: "Invalid value for temperature", code: 400 } }),
    ],
  },
  async (t) => {
    await submit(t, "Read and answer")
    await t.until((event) => event?.type === "session.viewed", 0, 60_000)
    await waitLong(t, /Session failed[\s\S]*Error: Invalid value for temperature/)
    await t.frame("failed")
  },
)

scenario(
  "rate limited, retried, then answered",
  {
    timeout: 60_000,
    turns: [
      { events: [{ text: "Starting." }], truncate: true },
      error(429, { error: { message: "Rate limit exceeded: free-models-per-min", code: 429 } }, { "retry-after": "4" }),
      say("Answered after the rate limit."),
    ],
  },
  async (t) => {
    await submit(t, "Answer please")
    // The countdown ticks once a second; wait for one fixed value of it.
    await waitLong(t, "Retrying in 2s · attempt 3")
    await t.frame("retrying")
    await waitLong(t, "Answered after the rate limit.")
    await t.until((event) => event?.type === "session.viewed", 0, 60_000)
    await t.frame("answered")
  },
)

scenario(
  "stream truncated after text, then continued",
  { timeout: 60_000, turns: [{ events: [{ text: "The first half of" }], truncate: true }, say(" the answer.")] },
  async (t) => {
    await submit(t, "Answer please")
    await waitLong(t, "the answer.")
    await t.until((event) => event?.type === "session.viewed", 0, 60_000)
    await t.frame("continued")
  },
)

const held = () => {
  const gate = Promise.withResolvers<void>()
  return { gate, turn: { events: [{ text: "Working on it" }, { until: gate.promise }, { text: " and done." }] } as Turn }
}

{
  const { gate, turn } = held()
  scenario("interrupt a running step with escape twice", { turns: [turn] }, async (t) => {
    await submit(t, "Do a long task")
    await t.waitFor("Working on it")
    await t.frame("running")
    await t.escape()
    await t.frame("armed")
    await t.escape()
    await t.until((event) => event?.type === "session.viewed", 0, 60_000)
    await t.waitFor("interrupted")
    gate.resolve()
    await t.frame("interrupted")
  })
}

{
  const { gate, turn } = held()
  scenario("a single escape only arms the interrupt", { turns: [turn] }, async (t) => {
    await submit(t, "Do a long task")
    await t.waitFor("Working on it")
    await t.escape()
    await t.frame("armed")
    gate.resolve()
    await t.until((event) => event?.type === "session.viewed", 0, 60_000)
    await t.waitFor("and done.")
    await t.frame("finished")
  })
}
