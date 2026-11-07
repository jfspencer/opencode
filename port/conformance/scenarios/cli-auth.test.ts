// `opencode auth ...` for OpenRouter API keys, through the paths that need no terminal: listing, the
// non-interactive errors of login and logout, and stored keys created with `auth import` (then listed, exported,
// used for model requests, and logged out). The sandbox exports OPENROUTER_API_KEY like the user's shell does;
// scenarios about stored keys unset it.
import { say } from "../lib/openrouter"
import { HIDDEN_TOOLS } from "../lib/model"
import { cliScenario } from "../lib/cli"

const quiet = {
  permissions: [...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })), { action: "*", resource: "*", effect: "deny" }],
}
const noEnvKey = { OPENROUTER_API_KEY: undefined }

const exported = (entries: { id: string; label: string; key: string; active: boolean }[]) =>
  JSON.stringify(
    entries.map((entry) => ({
      id: entry.id,
      integrationID: "openrouter",
      label: entry.label,
      active: entry.active,
      value: { type: "key", key: entry.key },
    })),
    null,
    2,
  ) + "\n"

cliScenario("auth list with the API key from the environment", {}, async (ctx) => {
  await ctx.cli(["auth", "list"])
  await ctx.cli(["auth", "list", "--format", "json"])
})

cliScenario("auth list without credentials", { env: noEnvKey }, async (ctx) => {
  await ctx.cli(["auth", "list"])
  await ctx.cli(["auth", "list", "--format", "json"])
})

cliScenario("auth login without a terminal", {}, async (ctx) => {
  await ctx.cli(["auth", "login"])
  await ctx.cli(["auth", "login", "openrouter"])
  await ctx.cli(["auth", "login", "OpenRouter", "--method", "key"])
  await ctx.cli(["auth", "login", "openrouter", "--method", "oauth"])
  await ctx.cli(["auth", "login", "openrouter", "--answer", "region=eu"])
  await ctx.cli(["auth", "login", "openrouter", "--answer", "novalue"])
  await ctx.cli(["auth", "login", "no-such-integration"])
  await ctx.cli(["auth", "list"])
})

cliScenario("auth logout without stored credentials", {}, async (ctx) => {
  await ctx.cli(["auth", "logout"])
  await ctx.cli(["auth", "logout", "openrouter"])
  await ctx.cli(["auth", "logout", "openrouter", "default"])
  await ctx.cli(["auth", "logout", "no-such-integration", "default"])
})

cliScenario(
  "stored OpenRouter keys: import, list, export, use and log out",
  { env: noEnvKey, config: quiet, turns: [say("Using the stored key."), say("Using the second key.")] },
  async (ctx) => {
    await ctx.cli(["auth", "import"], {
      stdin: exported([
        { id: "cred_conformanceWorkKey0001", label: "Work key", key: "sk-or-work", active: true },
        { id: "cred_conformanceHomeKey0002", label: "Home key", key: "sk-or-home", active: false },
      ]),
    })
    await ctx.cli(["auth", "list"])
    await ctx.cli(["auth", "list", "--format", "json"])
    await ctx.cli(["auth", "export"])
    await ctx.cli(["auth", "export", "OpenRouter"])
    await ctx.cli(["run", "Which key?"])
    // Importing the same credentials again skips them.
    await ctx.cli(["auth", "import"], {
      stdin: exported([{ id: "cred_conformanceWorkKey0001", label: "Work key", key: "sk-or-work", active: true }]),
    })
    await ctx.cli(["auth", "logout", "openrouter", "work key"])
    await ctx.cli(["auth", "list"])
    await ctx.cli(["run", "Which key now?"])
    await ctx.cli(["auth", "logout", "openrouter", "cred_conformanceHomeKey0002"])
    await ctx.cli(["auth", "logout", "openrouter", "Home key"])
    await ctx.cli(["auth", "list"])
  },
)

cliScenario("auth import from a file and its errors", { env: noEnvKey }, async (ctx) => {
  ctx.write("keys.json", exported([{ id: "cred_conformanceFileKey0001", label: "From file", key: "sk-or-file", active: true }]))
  ctx.write("broken.json", "{ not json")
  ctx.write("wrong.json", JSON.stringify([{ id: "x", label: "No value" }]))
  await ctx.cli(["auth", "import", "keys.json"])
  await ctx.cli(["auth", "import", "missing.json"])
  await ctx.cli(["auth", "import", "broken.json"])
  await ctx.cli(["auth", "import", "wrong.json"])
  await ctx.cli(["auth", "import"], { stdin: "" })
  await ctx.cli(["auth", "list", "--format", "json"])
})

cliScenario("stored key and environment key together", { config: quiet, turns: [say("Which one?")] }, async (ctx) => {
  await ctx.cli(["auth", "import"], {
    stdin: exported([{ id: "cred_conformanceWorkKey0001", label: "Work key", key: "sk-or-work", active: true }]),
  })
  await ctx.cli(["auth", "list"])
  await ctx.cli(["run", "Which key?"])
})

cliScenario(
  "auth login with an API key typed in a terminal",
  { env: noEnvKey, config: quiet, turns: [say("Typed key works."), say("Second key works.")] },
  async (ctx) => {
    // An empty answer is refused, then the key is typed (the prompt masks it).
    await ctx.terminal(["auth", "login", "openrouter"], [
      { waitFor: "API Key", type: "\r" },
      { waitFor: "Required", type: "sk-or-typed\r" },
    ])
    await ctx.cli(["auth", "list"])
    await ctx.cli(["auth", "export", "openrouter"])
    await ctx.cli(["run", "Which key?"])
    // Logging in again adds a second account, which becomes the active one.
    await ctx.terminal(["auth", "login", "OpenRouter", "--method", "key"], [{ waitFor: "API Key", type: "sk-or-second\r" }])
    await ctx.cli(["auth", "list", "--format", "json"])
    await ctx.cli(["run", "Which key now?"])
  },
)
