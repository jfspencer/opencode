// `opencode service ...` and how other commands find the background service, all inside the scenario's sandbox:
// the service registers under the sandbox's XDG_STATE_HOME and listens on the sandbox's own port
// (`<service-port>`, see lib/cli.ts), so it never meets the user's real service. `reload`, `--standalone` and
// `--server` are here too, since they choose or address the server a command talks to.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say } from "../lib/openrouter"
import { cliScenario } from "../lib/cli"

// File names follow the build's channel (the oracle's is "conformance", see lib/server.ts).
const CONFIG = "config/opencode/service-conformance.json"
const REGISTRATION = "state/opencode/service-conformance.json"
const rule = (action: string, resource: string, effect: string) => ({ action, resource, effect })
const quiet = { permissions: [...HIDDEN_TOOLS.map((action) => rule(action, "*", "deny")), rule("*", "*", "deny")] }

cliScenario("service lifecycle", {}, async (ctx) => {
  await ctx.cli(["service", "status"])
  await ctx.cli(["service", "stop"])
  ctx.file(REGISTRATION)
  await ctx.cli(["service", "start"])
  await ctx.cli(["service", "status"])
  ctx.file(CONFIG)
  await ctx.cli(["service", "start"])
  await ctx.cli(["service", "restart"])
  await ctx.cli(["service", "status"])
  await ctx.cli(["service", "stop"])
  await ctx.cli(["service", "status"])
  ctx.file(REGISTRATION)
  await ctx.cli(["service", "restart"])
  await ctx.cli(["service", "stop"])
})

cliScenario("service configuration get, set and unset", {}, async (ctx) => {
  await ctx.cli(["service", "get"])
  await ctx.parallel([
    ["service", "get", "disabled"],
    ["service", "get", "remote"],
    ["service", "get", "hostname"],
    ["service", "get", "port"],
    ["service", "get", "cors"],
    ["service", "get", "env"],
    ["service", "get", "env", "MISSING"],
  ])
  await ctx.cli(["service", "set", "hostname", "localhost"])
  await ctx.cli(["service", "set", "cors", "http://app.example.test, https://other.example.test:8443"])
  await ctx.cli(["service", "set", "env", "CONFORMANCE_FLAG", "on"])
  await ctx.cli(["service", "set", "env", "SECOND", "two words"])
  await ctx.cli(["service", "set", "password", "conformance-password"])
  await ctx.cli(["service", "set", "remote", "false"])
  await ctx.cli(["service", "get"])
  ctx.file(CONFIG)
  await ctx.parallel([
    ["service", "get", "hostname"],
    ["service", "get", "cors"],
    ["service", "get", "env"],
    ["service", "get", "env", "CONFORMANCE_FLAG"],
    ["service", "get", "password"],
    ["service", "get", "remote"],
  ])
  await ctx.cli(["service", "unset", "env", "SECOND"])
  await ctx.cli(["service", "unset", "env", "CONFORMANCE_FLAG"])
  await ctx.cli(["service", "unset", "cors"])
  await ctx.cli(["service", "unset", "hostname"])
  await ctx.cli(["service", "unset", "password"])
  await ctx.cli(["service", "unset", "remote"])
  await ctx.cli(["service", "unset", "disabled"])
  ctx.file(CONFIG)
  // Without a stored password, reading it creates one.
  await ctx.cli(["service", "get", "password"])
  ctx.file(CONFIG)
})

cliScenario("service configuration errors", {}, async (ctx) => {
  await ctx.parallel([
    ["service", "get", "frobnicate"],
    ["service", "get", "port", "extra"],
    ["service", "set", "frobnicate", "1"],
    ["service", "set", "port", "0"],
    ["service", "set", "port", "65536"],
    ["service", "set", "port", "eighty"],
    ["service", "set", "disabled", "maybe"],
    ["service", "set", "remote", "sometimes"],
    ["service", "set", "hostname", "localhost", "extra"],
    ["service", "set", "env", "ONLY_NAME"],
    ["service", "set", "cors", "app.example.test"],
    ["service", "set", "cors", "https://app.example.test/path"],
    ["service", "unset", "frobnicate"],
    ["service", "unset", "port", "extra"],
    ["service", "unset", "env"],
  ])
  ctx.file(CONFIG)
})

cliScenario(
  "commands start the service on demand and run reuses it",
  { config: quiet, turns: [say("Via the service.")] },
  async (ctx) => {
    // A one-off run never leaves a service behind: with none running it uses a private server.
    await ctx.cli(["service", "status"])
    await ctx.cli(["session", "list"])
    await ctx.cli(["service", "status"])
    await ctx.cli(["run", "--title", "Through the service", "Hello"])
    await ctx.cli(["session", "list", "--format", "json"])
    await ctx.cli(["service", "stop"])
    await ctx.cli(["service", "status"])
  },
)

cliScenario(
  "tools in a run see the environment of the run, not the service's",
  {
    config: { permissions: [...quiet.permissions, rule("shell", "*", "allow")] },
    turns: [
      call("shell", { command: "echo flag=$CONFORMANCE_FLAG" }),
      say("Printed the flag."),
      call("shell", { command: "echo flag=$CONFORMANCE_FLAG" }),
      say("Printed it again."),
    ],
  },
  async (ctx) => {
    await ctx.cli(["service", "set", "env", "CONFORMANCE_FLAG", "from-service"])
    await ctx.cli(["service", "start"])
    await ctx.cli(["run", "Print the flag"])
    await ctx.cli(["run", "Print the flag"], { env: { CONFORMANCE_FLAG: "from-the-shell" } })
    await ctx.cli(["service", "stop"])
  },
)

cliScenario("changing service settings stops the running service", {}, async (ctx) => {
  await ctx.cli(["service", "start"])
  await ctx.cli(["service", "set", "env", "CONFORMANCE_FLAG", "on"])
  await ctx.cli(["service", "status"])
  await ctx.cli(["service", "start"])
  await ctx.cli(["service", "unset", "env", "CONFORMANCE_FLAG"])
  await ctx.cli(["service", "status"])
})

cliScenario("disabled service makes commands use a private server", {}, async (ctx) => {
  await ctx.cli(["service", "set", "disabled", "true"])
  await ctx.cli(["service", "get", "disabled"])
  await ctx.cli(["models"])
  await ctx.cli(["service", "status"])
  await ctx.cli(["service", "set", "disabled", "false"])
  await ctx.cli(["models"])
  await ctx.cli(["service", "status"])
  await ctx.cli(["service", "stop"])
})

cliScenario("standalone flag keeps the service stopped", {}, async (ctx) => {
  await ctx.cli(["session", "list", "--standalone"])
  await ctx.cli(["models", "--standalone"])
  await ctx.cli(["--standalone", "debug", "agents"]).then(() => undefined)
  await ctx.cli(["service", "status"])
})

cliScenario("reload configuration", {}, async (ctx) => {
  await ctx.cli(["reload"])
  await ctx.cli(["reload", "--standalone"])
  await ctx.cli(["service", "stop"])
})

cliScenario("server flag addresses a running server", {}, async (ctx) => {
  await ctx.cli(["service", "set", "password", "conformance-password"])
  const url = (await ctx.cli(["service", "start"])).stdout.trim()
  const password = { OPENCODE_PASSWORD: "conformance-password" }
  await ctx.cli(["models", "--server", url], { env: password })
  await ctx.cli(["session", "list", "--server", url], { env: { OPENCODE_SERVER_PASSWORD: "conformance-password" } })
  await ctx.cli(["models", "--server", url])
  await ctx.cli(["models", "--server", url], { env: { OPENCODE_PASSWORD: "wrong" } })
  await ctx.cli(["models", "--server", url, "--standalone"], { env: password })
  const closed = await ctx.port()
  await ctx.cli(["models", "--server", `http://127.0.0.1:${closed}`])
  await ctx.cli(["service", "stop"])
})
