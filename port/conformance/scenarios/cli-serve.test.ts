// `opencode serve`: the HTTP API server as a foreground process. Started on port 0 (the OS picks the port,
// recorded as `<port>`), driven over HTTP, and stopped with a signal.
import { HIDDEN_TOOLS } from "../lib/model"
import { say } from "../lib/openrouter"
import { cliScenario } from "../lib/cli"

const quiet = {
  permissions: [...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })), { action: "*", resource: "*", effect: "deny" }],
}

const LISTENING = /^server listening on (http:\/\/\S+)$/
const basic = (password: string) => ({ authorization: `Basic ${Buffer.from(`opencode:${password}`).toString("base64")}` })

cliScenario("serve with a password", { config: quiet, turns: [say("Served.")] }, async (ctx) => {
  const server = ctx.start(["serve", "--port", "0"], { env: { OPENCODE_PASSWORD: "conformance-secret" } })
  const url = LISTENING.exec(await server.line(LISTENING))![1]
  const auth = basic("conformance-secret")
  await ctx.fetch("GET", `${url}/api/session`)
  await ctx.fetch("GET", `${url}/api/session`, undefined, basic("wrong"))
  await ctx.fetch("GET", `${url}/api/session`, undefined, auth)
  await ctx.fetch("GET", `${url}/api/model?directory=${encodeURIComponent(ctx.project)}`, undefined, auth)
  await ctx.fetch("POST", `${url}/api/session`, { location: { directory: ctx.project }, title: "Over HTTP" }, auth)
  await ctx.cli(["session", "list", "--server", url], { env: { OPENCODE_PASSWORD: "conformance-secret" } })
  await ctx.cli(["api", "--server", url, "GET", "/api/session"], { env: { OPENCODE_PASSWORD: "conformance-secret" } })
  await ctx.cli(["run", "--server", url, "--title", "Run against serve", "Hello"], { env: { OPENCODE_PASSWORD: "conformance-secret" } })
  await ctx.cli(["session", "list", "--server", url, "--format", "json"], { env: { OPENCODE_PASSWORD: "conformance-secret" } })
  await server.stop("SIGTERM")
})

cliScenario("serve generates a password when none is given", {}, async (ctx) => {
  const server = ctx.start(["serve", "--port", "0", "--hostname", "127.0.0.1"])
  await server.line(/^server password /)
  const url = LISTENING.exec(await server.line(LISTENING))![1]
  const password = /^server password (\S+)$/.exec(await server.line(/^server password /))![1]
  await ctx.fetch("GET", `${url}/api/session`)
  await ctx.fetch("GET", `${url}/api/session`, undefined, basic(password))
  await server.stop("SIGINT")
})

cliScenario("serve with an allowed CORS origin", {}, async (ctx) => {
  const server = ctx.start(["serve", "--port", "0", "--cors", "https://app.example.test", "--cors", "http://localhost:5173"], {
    env: { OPENCODE_PASSWORD: "conformance-secret" },
  })
  const url = LISTENING.exec(await server.line(LISTENING))![1]
  const preflight = (origin: string) =>
    ctx.fetch("OPTIONS", `${url}/api/session`, undefined, {
      origin,
      "access-control-request-method": "GET",
      "access-control-request-headers": "authorization",
    })
  await preflight("https://app.example.test")
  await preflight("http://localhost:5173")
  await preflight("https://evil.example.test")
  await server.stop()
})

cliScenario("serve over stdio ends when stdin closes", {}, async (ctx) => {
  const server = ctx.start(["serve", "--stdio", "--port", "0"], { env: { OPENCODE_PASSWORD: "conformance-secret" } })
  const ready = JSON.parse(await server.line(/^\{"url"/)) as { url: string }
  await ctx.fetch("GET", `${ready.url}/api/session`, undefined, basic("conformance-secret"))
  server.end()
  await server.exited()
})

cliScenario("serve argument errors", {}, async (ctx) => {
  await ctx.cli(["serve", "--service", "--stdio"])
  const taken = Bun.serve({ hostname: "127.0.0.1", port: await ctx.port(), fetch: () => new Response("taken") })
  try {
    await ctx.cli(["serve", "--port", String(taken.port)], { env: { OPENCODE_PASSWORD: "conformance-secret" } })
  } finally {
    await taken.stop(true)
  }
})
