// Schedules that fall due together: one tick of the schedule runner fires every due schedule once (up to four at a
// time, in due order), each waking its own session with its prompt. Five sessions share one due time here, so one
// tick fires more schedules than it runs at once.
import { say } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

const enabled = { experimental: { schedules: true } }

scenario(
  "schedules due together fire in one tick",
  { config: enabled, turns: Array.from({ length: 5 }, () => say("Reported.")), timeout: 90_000 },
  async (ctx) => {
    // Schedule routes don't boot the location by themselves.
    await ctx.http("GET", "/api/location")
    await ctx.until((event) => event?.type === "plugin.updated")
    // Titled sessions: no title generation runs beside the scheduled prompts.
    const sessions: string[] = []
    for (let i = 0; i < 5; i++) sessions.push(await ctx.session({ title: `Report ${i}` }))
    const at = new Date(Date.now() + 4000).toISOString().replace(/\.\d+Z$/, "Z")
    for (const [i, sessionID] of sessions.entries())
      await ctx.http("POST", "/api/experimental/schedule", { sessionID, name: `report ${i}`, when: { at }, prompt: `Report ${i}` })
    for (const sessionID of sessions) {
      await ctx.until(
        (event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === sessionID,
        0,
        60_000,
      )
      await ctx.settle(sessionID)
    }
    for (const sessionID of sessions) await ctx.http("GET", `/api/experimental/schedule?sessionID=${sessionID}`)
  },
)
