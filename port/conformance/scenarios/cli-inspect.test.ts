// Read-only inspection commands: `models`, `debug paths/agents/config`, `stats`.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say } from "../lib/openrouter"
import { cliScenario } from "../lib/cli"

const rule = (action: string, resource: string, effect: string) => ({ action, resource, effect })
const quiet = { permissions: [...HIDDEN_TOOLS.map((action) => rule(action, "*", "deny")), rule("*", "*", "deny")] }

cliScenario("models lists only the configured model", {}, async (ctx) => {
  await ctx.cli(["models"])
  await ctx.cli(["models", "--standalone"])
})

cliScenario(
  "models with a second configured model",
  {
    config: {
      providers: {
        openrouter: {
          models: {
            "acme/tiny-1": {
              name: "Acme Tiny",
              capabilities: { tools: true, input: ["text"], output: ["text"], temperature: true },
              limit: { context: 8192, output: 1024 },
            },
          },
        },
      },
    },
  },
  async (ctx) => {
    await ctx.cli(["models"])
  },
)

cliScenario("debug paths", {}, async (ctx) => {
  await ctx.cli(["debug", "paths"])
  await ctx.parallel(["db", "home", "data", "config", "cache", "state", "tmp", "bin", "log", "repos"].map((name) => ["debug", "paths", name]))
})

cliScenario("debug agents and config", { files: { "opencode.json": JSON.stringify({ instructions: ["RULES.md"] }) + "\n", "RULES.md": "Be brief.\n" } }, async (ctx) => {
  await ctx.cli(["debug", "agents"])
  await ctx.cli(["debug", "config"])
})

cliScenario("stats with no activity", {}, async (ctx) => {
  await ctx.cli(["stats"])
  await ctx.cli(["stats", "--json"])
  await ctx.cli(["stats", "--days", "0"])
  await ctx.cli(["stats", "--all", "--project", "."])
  await ctx.cli(["stats", "--days", "7", "--all"])
})

cliScenario(
  "stats after some runs",
  {
    files: { "notes.txt": "alpha\n" },
    config: { permissions: [...quiet.permissions, rule("read", "*", "allow")] },
    turns: [
      call("read", { path: "notes.txt" }),
      say("Read it.", { prompt: 1200, completion: 80, reasoning: 20, cached: 200 }),
      call("read", { path: "missing.txt" }),
      say("Missing.", { prompt: 900, completion: 40 }),
    ],
  },
  async (ctx) => {
    await ctx.cli(["run", "Read notes.txt"])
    await ctx.cli(["run", "Read missing.txt"])
    await ctx.cli(["stats"])
    await ctx.cli(["stats", "--full"])
    await ctx.cli(["stats", "--models", "--tools", "--cost", "--limit", "1"])
    await ctx.cli(["stats", "--days", "7", "--project", "."])
    await ctx.cli(["stats", "--all", "--project", "prj_doesNotExist"])
    await ctx.cli(["stats", "--json", "--all"])
  },
)
