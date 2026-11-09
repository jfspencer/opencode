// Idle locations: once a minute the server unloads every location whose last durable session activity is older than
// its idle timeout (`experimental.location_idle_timeout`, an hour by default; `false` never unloads), unless a session
// placed there is still executing. The next request boots an unloaded location again.
//
// The sweep runs a minute after the server starts and every minute after that; nothing makes it run sooner, so this
// scenario waits for the first one. A timeout of zero makes every idle location due at that first sweep.
import path from "node:path"
import { call, say } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

const question = {
  question: "Which color should the button be?",
  header: "Button color",
  options: [
    { label: "Blue", description: "Matches the brand palette" },
    { label: "Green", description: "Signals success" },
  ],
}

scenario(
  "idle locations are unloaded at the sweep",
  {
    files: {
      "opencode.json": JSON.stringify({ experimental: { location_idle_timeout: "0 seconds" } }),
      "keep/opencode.json": JSON.stringify({ experimental: { location_idle_timeout: false } }),
      "busy/task.txt": "task\n",
    },
    turns: [call("question", { questions: [question] }), say("Blue it is.")],
    timeout: 180_000,
  },
  async (ctx) => {
    const at = (directory: string) => ({ "x-opencode-directory": encodeURIComponent(path.join(ctx.project, directory)) })
    // Locations announce their plugins after they boot: wait, so the boots' announcements do not interleave.
    const booted = (directory: string) =>
      ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === path.join(ctx.project, directory))
    // `keep` never unloads.
    await ctx.http("GET", "/api/location", undefined, at("keep"))
    await booted("keep")
    // `busy` has a session waiting on a question when the sweep comes: it stays loaded.
    await ctx.http("GET", "/api/location", undefined, at("busy"))
    await booted("busy")
    const busy = await ctx.session({ location: { directory: path.join(ctx.project, "busy") } })
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${busy}/prompt`, { text: "Style the button" })
    await ctx.until((event) => event?.type === "form.created" && event?.data?.form?.sessionID === busy, from)
    await ctx.http("GET", "/api/debug/location")
    // The project location itself is idle: the first sweep unloads it.
    const shutdown = await ctx.until((event) => event?.type === "location.shutdown", from, 120_000)
    ctx.note("unloaded", shutdown.location)
    await ctx.http("GET", "/api/debug/location")
    const forms = await ctx.http("GET", `/api/session/${busy}/form`)
    await ctx.http("POST", `/api/session/${busy}/form/${forms.body.data[0].id}/reply`, { answer: { q0: "Blue" } })
    await ctx.settle(busy)
    // The next request boots the project location again.
    const reboot = ctx.events.length
    await ctx.http("GET", "/api/agent")
    await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === ctx.project, reboot)
    await ctx.http("GET", "/api/debug/location")
  },
)
