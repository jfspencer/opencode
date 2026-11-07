// `opencode api`: raw requests to the server (the background service here) by method and path or by OpenAPI
// operation ID.
import { cliScenario } from "../lib/cli"

cliScenario("api by method and path", {}, async (ctx) => {
  await ctx.cli(["api", "GET", "/api/session"])
  const created = await ctx.cli(["api", "post", "/api/session", "-d", JSON.stringify({ location: { directory: ctx.project }, title: "From api" })])
  const id = JSON.parse(created.stdout).data.id as string
  await ctx.cli(["api", "GET", `/api/session/${id}`])
  await ctx.cli(["api", "PATCH", `/api/session/${id}`, "--data", JSON.stringify({ title: "Renamed" })])
  await ctx.cli(["api", "GET", "/api/session", "-H", "accept:  application/json ", "--header", "x-conformance:yes"])
  await ctx.cli(["api", "DELETE", `/api/session/${id}`])
  await ctx.cli(["api", "GET", `/api/session/${id}`])
  await ctx.cli(["service", "stop"])
})

cliScenario("api by operation ID", {}, async (ctx) => {
  await ctx.cli(["api", "session.list"])
  const created = await ctx.cli(["api", "session.create", "-d", JSON.stringify({ location: { directory: ctx.project }, title: "By operation" })])
  const id = JSON.parse(created.stdout).data.id as string
  await ctx.cli(["api", "session.get", "--param", `sessionID=${id}`])
  await ctx.cli(["api", "session.list", "--param", "limit=1", "--param", "order=asc"])
  await ctx.cli(["api", "session.get"])
  await ctx.cli(["api", "no.such.operation"])
  await ctx.cli(["service", "stop"])
})

cliScenario("api errors", {}, async (ctx) => {
  await ctx.cli(["api", "GET", "/api/no-such-route"])
  await ctx.cli(["api", "POST", "/api/session", "-d", "{ not json"])
  await ctx.cli(["api", "GET", "/api/session", "-H", "no-colon-here"])
  await ctx.cli(["api", "FETCH", "/api/session"])
  await ctx.cli(["api", "GET", "api/session"])
  await ctx.cli(["service", "stop"])
})
