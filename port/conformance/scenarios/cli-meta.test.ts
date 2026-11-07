// CLI surface without a server: version, help of every in-scope command, and argument errors.
import { cliScenario } from "../lib/cli"

cliScenario("version and top-level help", {}, async (ctx) => {
  await ctx.parallel([["--version"], ["-v"], ["--help"], ["-h"], ["run", "--version"]])
})

cliScenario("help of each in-scope command", {}, async (ctx) => {
  await ctx.parallel(
    [
      ["run"],
      ["serve"],
      ["session"],
      ["session", "list"],
      ["session", "delete"],
      ["session", "export"],
      ["session", "import"],
      ["models"],
      ["stats"],
      ["debug"],
      ["debug", "agents"],
      ["debug", "config"],
      ["debug", "paths"],
      ["auth"],
      ["auth", "list"],
      ["auth", "login"],
      ["auth", "logout"],
      ["api"],
      ["service"],
      ["service", "start"],
      ["service", "status"],
      ["service", "get"],
      ["service", "set"],
      ["service", "unset"],
      ["service", "restart"],
      ["service", "stop"],
      ["acp"],
      ["reload"],
    ].map((command) => [...command, "--help"]),
  )
})

cliScenario("command groups without a subcommand", {}, async (ctx) => {
  await ctx.parallel([["session"], ["debug"], ["auth"], ["service"]])
})

cliScenario("unknown commands and subcommands", {}, async (ctx) => {
  // An unknown first word is the optional directory argument of the default (TUI) command.
  await ctx.parallel([
    ["frobnicate"],
    ["export"],
    ["import", "session.json"],
    ["session", "frobnicate"],
    ["service", "reload"],
    ["debug", "frobnicate"],
    ["auth", "frobnicate"],
  ])
})

cliScenario("unknown flags and invalid flag values", {}, async (ctx) => {
  await ctx.parallel([
    ["--frobnicate"],
    ["run", "--frobnicate", "hello"],
    ["run", "--format", "xml", "hello"],
    ["run", "--model"],
    ["session", "list", "--format", "xml"],
    ["session", "list", "--max-count", "0"],
    ["session", "list", "-n", "many"],
    ["stats", "--days", "-1"],
    ["stats", "--year", "1969"],
    ["stats", "--limit", "0"],
    ["debug", "paths", "frobnicate"],
    ["auth", "list", "--format", "yaml"],
    ["serve", "--port", "http"],
    ["serve", "--cors", ""],
  ])
})

cliScenario("missing required arguments", {}, async (ctx) => {
  await ctx.parallel([
    ["session", "delete"],
    ["session", "import"],
    ["api"],
    ["api", "GET", "/api/session", "extra"],
    ["service", "set"],
    ["service", "set", "port"],
    ["service", "unset"],
  ])
})

cliScenario("shell completions", {}, async (ctx) => {
  await ctx.parallel([["--completions", "bash"], ["--completions", "fish"], ["--completions", "tcsh"]])
})
