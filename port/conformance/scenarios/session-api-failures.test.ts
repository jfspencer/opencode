// How failures surface on a session: provider HTTP errors mapped to session error types, a provider retry,
// content-filtered steps, tool failures and permission rejections, and title generation edge cases.
import { call, say, type Turn } from "../lib/openrouter"
import { scenario as baseScenario, type Ctx } from "../lib/scenario"

// The live event subscription is only in place once the server says so; waiting for that keeps the first events of
// a scenario (such as its first session.created) from racing the subscription. Several reads boot the project's
// location without waiting for its plugins, which then announce themselves in the background; finishing every
// scenario with the project's location fully loaded (through a read that waits for plugins, made outside the
// transcript) keeps the recorded global events the same on every run.
const scenario: typeof baseScenario = (name, options, body) =>
  baseScenario(name, options, async (ctx) => {
    if (options.events === false) return body(ctx)
    await ctx.until((event) => event?.type === "server.connected")
    await body(ctx)
    await ctx.handler(
      new Request("http://opencode.local/api/skill", {
        headers: { "x-opencode-directory": encodeURIComponent(ctx.project) },
      }),
    )
    await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === ctx.project)
  })

const messages = (ctx: Ctx, sessionID: string) => ctx.http("GET", `/api/session/${sessionID}/message?order=asc`)

const httpError = (status: number, body: unknown, headers: Record<string, string> = {}): Turn => ({
  status,
  body,
  // Retryable statuses would otherwise back off for over a minute before failing.
  headers: { "x-should-retry": "false", ...headers },
})

const failures: Array<[string, Turn]> = [
  ["unauthorized", httpError(401, { error: { message: "No auth credentials found", code: 401 } })],
  [
    "forbidden token sharing",
    httpError(403, { error: { message: "denied", code: "subscription_sharing_invalid_user" } }),
  ],
  ["payment required", httpError(402, { error: { message: "Insufficient credits", code: 402 } })],
  ["free usage limit", httpError(402, { error: { message: "Free tier exhausted", type: "FreeUsageLimitError" } })],
  ["workspace budget", httpError(402, { error: { message: "Budget hit", type: "BudgetLimitError" } })],
  [
    "model budget",
    httpError(402, {
      error: { message: "Model budget hit", type: "BudgetLimitError", metadata: { coverage: "models" } },
    }),
  ],
  [
    "go usage limit",
    httpError(429, { error: { message: "Go limit", type: "GoUsageLimitError", metadata: { limitName: "weekly" } } }),
  ],
  ["go usage limit without a name", httpError(402, { error: { message: "Go limit", type: "GoUsageLimitError" } })],
  [
    "chatgpt usage limit",
    httpError(400, { error: { message: "limit", code: "subscription_sharing_usage_limit_exceeded" } }),
  ],
  ["content policy", httpError(400, { error: { message: "Flagged", code: "content_policy_violation" } })],
  ["invalid request", httpError(400, { error: { message: "Unsupported parameter: foo" } })],
  ["rate limited", httpError(429, { error: { message: "Slow down" } }, { "retry-after": "7" })],
  ["server error", httpError(503, { error: { message: "Upstream overloaded" } })],
  ["malformed stream", { raw: "data: {this is not json\n\n" }],
  [
    "unclassified error event in the stream",
    {
      raw: `data: ${JSON.stringify({ error: { message: "Something odd happened" } })}\n\n`,
      headers: { "x-should-retry": "false" },
    },
  ],
  [
    "error event in the stream",
    { raw: `data: ${JSON.stringify({ error: { message: "Provider melted", code: 400 } })}\n\n` },
  ],
]

// One session takes every failure in turn (each prompt after a failed turn starts a fresh one), which keeps the
// scenario at a single session.
scenario("provider failures become session errors", { turns: failures.map(([, turn]) => turn) }, async (ctx) => {
  const session = await ctx.session({ title: "failures" })
  for (const [name] of failures) {
    await ctx.prompt(session, `Trigger: ${name}`)
    await ctx.http("GET", `/api/session/${session}`)
    await ctx.http("GET", `/api/session/${session}/message?order=desc&limit=2`)
  }
  await messages(ctx, session)
})

scenario(
  "a provider failure is retried once",
  {
    turns: [{ status: 500, body: { error: { message: "Temporary glitch" } } }, say("Recovered after a retry.")],
    timeout: 30_000,
  },
  async (ctx) => {
    const session = await ctx.session({ title: "retry" })
    await ctx.prompt(session, "Try twice")
    await ctx.http("GET", `/api/session/${session}`)
    await messages(ctx, session)
  },
)

scenario(
  "a step stopped by the content filter",
  { turns: [{ events: [{ text: "I started to answer" }], finish: "content_filter" }] },
  async (ctx) => {
    const session = await ctx.session({ title: "filtered" })
    await ctx.prompt(session, "Say something risky")
    await ctx.http("GET", `/api/session/${session}`)
    await messages(ctx, session)
  },
)

scenario(
  "tool failures and permission rejections",
  {
    files: { "a.txt": "a\n" },
    turns: [
      call("read", { path: "missing.txt" }),
      call("edit", { path: "a.txt", oldString: "a", newString: "b" }),
      call("shell", { command: "ls", description: "List files" }),
      call("shell", { command: "pwd", description: "Print directory" }),
      say("Gave up."),
    ],
  },
  async (ctx) => {
    const session = await ctx.session({
      title: "tool errors",
      permissions: [
        { action: "edit", resource: "*", effect: "deny" },
        { action: "shell", resource: "*", effect: "ask" },
      ],
    })
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Try several tools" })
    // The first shell command asks; reject it with feedback. The second asks again; reject it plainly.
    for (const message of ["Use the read tool instead", undefined]) {
      const asked = await ctx.until(
        (event) => event?.type === "permission.asked" && event?.data?.sessionID === session,
        message === undefined ? ctx.events.length : from,
      )
      await ctx.http("POST", `/api/session/${session}/permission/${asked.data.id}/reply`, {
        decision: "reject",
        ...(message === undefined ? {} : { message }),
      })
    }
    await ctx.settle(session)
    await ctx.http("GET", `/api/session/${session}`)
    await messages(ctx, session)
  },
)

scenario(
  "title generation edge cases",
  {
    turns: [say("Answered."), say("Answered."), say("Answered."), say("Answered.")],
    responders: [
      (request) => {
        if (request.kind !== "title") return undefined
        const text = JSON.stringify(request.body)
        if (text.includes("FAIL-TITLE")) return { status: 400, body: { error: { message: "no titles today" } } }
        if (text.includes("LONG-TITLE")) return say(`A ${"very ".repeat(40)}long title that goes on`)
        if (text.includes("BLANK-TITLE")) return say("\n   \n")
        if (text.includes("Name me please")) return say("Named from the first message")
        return say("Named from the conversation")
      },
    ],
  },
  async (ctx) => {
    // Titled sessions are renamed from the conversation; failed and blank generations keep the title. These turns
    // also finish loading the location: title generation does not wait for plugins, so on a cold location it
    // silently does nothing.
    for (const marker of ["FAIL-TITLE", "LONG-TITLE", "BLANK-TITLE", "follow-up"]) {
      const session = await ctx.session({ title: `before ${marker}` })
      await ctx.prompt(session, `${marker} please`)
      if (marker === "follow-up")
        await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Follow-up question", resume: false })
      await ctx.http("PATCH", `/api/session/${session}`, { title: "" })
      await ctx.http("GET", `/api/session/${session}`)
    }
    // An untitled session (imported, so no turn has named it yet) is named from its first message alone.
    const time = Date.UTC(2026, 0, 5)
    const untitled = "ses_00000000000000conformanceUNTITLED"
    await ctx.http("POST", "/api/experimental/session/import", {
      info: {
        id: untitled,
        projectID: "0000000000000000000000000000000000000000",
        cost: 0,
        tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
        time: { created: time, updated: time },
        location: { directory: ctx.project },
      },
      location: { directory: ctx.project },
      messages: [
        { id: "msg_00000000000000conformanceUNT1", type: "user", text: "Name me please", time: { created: time } },
      ],
    })
    await ctx.http("PATCH", `/api/session/${untitled}`, { title: "" })
    await ctx.http("GET", `/api/session/${untitled}`)
    // Generating the same title again changes nothing.
    await ctx.http("PATCH", `/api/session/${untitled}`, { title: "" })
    await ctx.http("GET", `/api/session/${untitled}`)
  },
)

scenario(
  "interrupt a session while it waits to retry",
  { turns: [{ status: 500, body: { error: { message: "Temporary glitch" } } }], timeout: 30_000 },
  async (ctx) => {
    const session = await ctx.session({ title: "retry interrupted" })
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Try once" })
    await ctx.until((event) => event?.type === "session.retry.scheduled" && event?.data?.sessionID === session, from)
    await ctx.http("POST", `/api/session/${session}/interrupt`)
    await ctx.wait(session)
    await ctx.http("GET", `/api/session/${session}`)
    await messages(ctx, session)
  },
)
