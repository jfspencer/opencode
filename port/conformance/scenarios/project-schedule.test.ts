// Experimental schedules: the experimental.schedules gate, creating schedules for every `when` form with their
// validation errors, conditions (shell and code checks, if/until, evaluated at creation), and the lifecycle routes
// (get, list, pause, resume, delete).
import { baseConfig } from "../lib/model"
import { say } from "../lib/openrouter"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated) so the recorded
// event stream doesn't depend on how fast the scenario body finished.
// Schedule routes don't boot the location by themselves, so boot it first.
const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await ctx.http("GET", "/api/location")
    await ctx.until((event) => event?.type === "plugin.updated")
    await body(ctx)
  })

const enabled = { experimental: { schedules: true } }

scenario("schedules are disabled without the experimental flag", {}, async (ctx) => {
  const sessionID = await ctx.session()
  await ctx.http("POST", "/api/experimental/schedule", { sessionID, when: { in: "1 hour" }, prompt: "Hi" })
  await ctx.http("GET", `/api/experimental/schedule?sessionID=${sessionID}`)
  await ctx.http("GET", "/api/experimental/schedule/sch_doesnotexist0000000000")
})

scenario("create schedules of every kind", { config: { ...enabled, timezone: "Europe/Berlin" } }, async (ctx) => {
  const sessionID = await ctx.session()
  const create = (body: Record<string, unknown>) =>
    ctx.http("POST", "/api/experimental/schedule", { sessionID, prompt: "Check the build", ...body })
  await create({ name: "later", when: { in: "2 hours" } })
  await create({ name: "instant", when: { at: "2031-03-04T05:06:07Z" } })
  await create({ name: "wall clock", when: { at: "2031-03-04T09:00", tz: "America/New_York" } })
  await create({ name: "repeating", when: { every: "2 hours" }, ends: { times: 3 } })
  await create({ name: "weekday mornings", when: { cron: "0 9 * * 1-5" }, ends: { at: "2031-12-31T00:00:00Z" } })
  await create({ name: "zoned cron", when: { cron: "30 6 1 * *", tz: "Asia/Tokyo" }, ends: { after: "400 days" } })
  await create({ when: { every: "1 day" }, ends: { after: "3 days", at: "2031-01-01T00:00:00Z" } })
  await ctx.http("GET", `/api/experimental/schedule?sessionID=${sessionID}`)
})

scenario("schedule validation errors", { config: enabled }, async (ctx) => {
  const sessionID = await ctx.session()
  const create = (body: Record<string, unknown>) =>
    ctx.http("POST", "/api/experimental/schedule", { sessionID, prompt: "Ping", ...body })
  await create({ when: { in: "soon" } })
  await create({ when: { in: "0 seconds" } })
  await create({ when: { at: "2020-01-01T00:00:00Z" } })
  await create({ when: { at: "next tuesday" } })
  await create({ when: { every: "30 seconds" } })
  await create({ when: { cron: "not a cron" } })
  await create({ when: { cron: "*/10 * * * * *" } })
  await create({ when: { cron: "0 0 30 2 *" } })
  await create({ when: { in: "1 hour", tz: "Mars/Olympus" } })
  await create({ when: { every: "1 hour" }, ends: { at: "whenever" } })
  await create({ when: { every: "1 hour" }, ends: { after: "10 minutes" } })
  await create({ when: { every: "1 hour" }, ends: { times: 0 } })
  await create({ prompt: "No when" })
  await create({ when: { in: "1 hour" }, if: { run: "true" }, until: { run: "true" } })
  await create({ when: { in: "1 hour" }, if: { run: "   " } })
  await create({ when: { in: "1 hour" }, if: { run: "echo", is: "a", matches: "b" } })
  await create({ when: { in: "1 hour" }, if: { run: "echo", matches: "(" } })
  await create({ when: { in: "1 hour" }, if: { code: "return (" } })
  await create({ when: { in: "1 hour" }, if: { code: "await tools.opencode.worktree_create({ name: 'x' }); return true" } })
  await create({ when: { in: "1 hour" }, if: { code: "await tools.nope.missing(); return true" } })
  await create({ when: { sometime: true } })
  await create({ when: { in: "1 hour", every: "1 hour" } })
  await create({ when: { in: "1 hour" }, if: { run: "true", code: "return true" } })
  await create({ when: { in: "1 hour" }, prompt: "" })
  await ctx.http("POST", "/api/experimental/schedule", { sessionID: "ses_00000000000000000000000000", when: { in: "1 hour" }, prompt: "x" })
})

scenario("schedule lifecycle routes", { config: enabled, turns: [say("Done.")] }, async (ctx) => {
  const sessionID = await ctx.session()
  const one = await ctx.http("POST", "/api/experimental/schedule", {
    sessionID,
    name: "one-shot",
    when: { in: "1 hour" },
    prompt: "Remind me",
  })
  const every = await ctx.http("POST", "/api/experimental/schedule", {
    sessionID,
    name: "repeating",
    when: { every: "1 hour" },
    prompt: "Again",
  })
  const id = one.body.data.id
  await ctx.http("GET", `/api/experimental/schedule/${id}`)
  await ctx.http("POST", `/api/experimental/schedule/${id}/pause`)
  await ctx.http("POST", `/api/experimental/schedule/${id}/pause`)
  await ctx.http("POST", `/api/experimental/schedule/${id}/resume`)
  await ctx.http("POST", `/api/experimental/schedule/${id}/resume`)
  await ctx.http("POST", `/api/experimental/schedule/${every.body.data.id}/pause`)
  await ctx.http("POST", `/api/experimental/schedule/${every.body.data.id}/resume`)
  await ctx.http("GET", `/api/experimental/schedule?sessionID=${sessionID}`)
  await ctx.http("DELETE", `/api/experimental/schedule/${id}`)
  await ctx.http("GET", `/api/experimental/schedule/${id}`)
  await ctx.http("DELETE", `/api/experimental/schedule/${id}`)
  await ctx.http("POST", `/api/experimental/schedule/${id}/pause`)
  await ctx.http("POST", `/api/experimental/schedule/${id}/resume`)
  await ctx.http("GET", `/api/experimental/schedule?sessionID=${sessionID}`)
  await ctx.http("GET", "/api/experimental/schedule?sessionID=ses_00000000000000000000000000")
  // A one-shot that fell due while paused fires once when resumed (the tool's documented behavior): let it run
  // before deleting the session, which would otherwise race it.
  const due = await ctx.http("POST", "/api/experimental/schedule", { sessionID, name: "due soon", when: { in: "1 second" }, prompt: "Now" })
  await ctx.http("POST", `/api/experimental/schedule/${due.body.data.id}/pause`)
  await new Promise((resolve) => setTimeout(resolve, 1200))
  const from = ctx.events.length
  await ctx.http("POST", `/api/experimental/schedule/${due.body.data.id}/resume`)
  await ctx.until((event) => event?.type === "session.inbox.enqueued" && event?.data?.sessionID === sessionID, from)
  await ctx.settle(sessionID, from)
  // A schedule whose session is gone reads as missing.
  await ctx.http("DELETE", `/api/session/${sessionID}`)
  await ctx.http("GET", `/api/experimental/schedule/${every.body.data.id}`)
})

scenario(
  "shell conditions at creation",
  { config: enabled, files: { "status.txt": "ready\n" } },
  async (ctx) => {
    const sessionID = await ctx.session()
    const create = (body: Record<string, unknown>) =>
      ctx.http("POST", "/api/experimental/schedule", { sessionID, prompt: "Deploy", ...body })
    // `until` checks run once at creation: an immediate match finishes the schedule.
    await create({ name: "already ready", until: { run: "cat status.txt", is: "ready" } })
    await create({ name: "not yet", until: { run: "cat status.txt", matches: "^done" } })
    await create({ name: "watch changes", when: { every: "1 hour" }, if: { run: "cat status.txt", changes: true } })
    await create({ name: "failing check", until: { run: "cat missing.txt", is: "x" } })
    await create({ name: "exit code", when: { in: "1 hour" }, if: { run: "test -f status.txt" } })
    await create({ name: "until exit code", until: { run: "test -f status.txt" } })
    await create({ name: "until missing", until: { run: "test -f missing.txt" } })
    await create({ name: "outside", when: { in: "1 hour" }, if: { run: "cd .. && ls" } })
  },
)

scenario(
  "code conditions at creation",
  { config: enabled },
  async (ctx) => {
    const sessionID = await ctx.session()
    const create = (body: Record<string, unknown>) =>
      ctx.http("POST", "/api/experimental/schedule", { sessionID, prompt: "Report", ...body })
    await create({ name: "true", until: { code: "return true" } })
    await create({ name: "stateful", until: { code: "return { wake: false, state: { runs, seen: state } }" } })
    await create({ name: "message", until: { code: "return { wake: true, message: 'All green' }" } })
    await create({ name: "bad shape", until: { code: "return 42" } })
    await create({ name: "throws", until: { code: "throw new Error('boom')" } })
    await create({ name: "if code", when: { every: "1 hour" }, if: { code: "return now > 0" } })
    await create({
      name: "with a tool",
      when: { every: "1 hour" },
      if: { code: "const found = await tools.opencode.models({ limit: 1 })\nreturn found.total > 0" },
    })
    await create({
      name: "until with a tool",
      until: { code: "const found = await tools.opencode.models({ query: 'deepseek' })\nreturn { wake: found.total > 0, message: `models: ${found.total}` }" },
    })
  },
)

scenario(
  "shell conditions need permission",
  {
    config: { ...enabled, permissions: [...baseConfig.permissions, { action: "shell", resource: "*", effect: "ask" }] },
  },
  async (ctx) => {
    const sessionID = await ctx.session()
    await ctx.http("POST", "/api/experimental/schedule", {
      sessionID,
      when: { in: "1 hour" },
      if: { run: "ls" },
      prompt: "x",
    })
    await ctx.http("GET", "/api/permission/request")
  },
)

scenario(
  "shell conditions outside the project need external directory permission",
  {
    config: {
      ...enabled,
      permissions: [...baseConfig.permissions, { action: "external_directory", resource: "*", effect: "ask" }],
    },
  },
  async (ctx) => {
    const sessionID = await ctx.session()
    await ctx.http("POST", "/api/experimental/schedule", {
      sessionID,
      when: { in: "1 hour" },
      if: { run: "cd .. && ls" },
      prompt: "x",
    })
    await ctx.http("GET", "/api/permission/request")
  },
)

scenario(
  "denied shell conditions",
  {
    config: { ...enabled, permissions: [...baseConfig.permissions, { action: "shell", resource: "*", effect: "deny" }] },
  },
  async (ctx) => {
    const sessionID = await ctx.session()
    await ctx.http("POST", "/api/experimental/schedule", {
      sessionID,
      when: { in: "1 hour" },
      if: { run: "ls" },
      prompt: "x",
    })
  },
)

// Firing (Schedule.run: the tick loop, checks at fire time, wake prompts) is not exercised: the loop is started by
// the Node server process (packages/server/src/process.ts), not by the in-process ServerFetch handler the oracle uses.
