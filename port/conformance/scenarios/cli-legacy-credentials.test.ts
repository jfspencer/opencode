// Credentials of the legacy store (`<data>/opencode/auth.json`) are imported once, when the database is created:
// API keys and OAuth tokens become credentials of their integration (the entry's key without trailing slashes),
// entries that do not decode are skipped, and an integration that already has a credential keeps it. A database that
// exists already is never imported into again. Observed through `auth export` (the stored credentials), `auth list`,
// and the imported OpenRouter key in model requests. (Tokens of catalog providers, such as OpenAI's or Copilot's, are
// left out: their provider plugins, out of scope, would use them.)
import { mkdirSync, writeFileSync } from "node:fs"
import path from "node:path"
import { cliScenario } from "../lib/cli"
import { HIDDEN_TOOLS } from "../lib/model"
import { say } from "../lib/openrouter"

const quiet = {
  permissions: [...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })), { action: "*", resource: "*", effect: "deny" }],
}
const noEnvKey = { OPENROUTER_API_KEY: undefined }

const legacy = (sandbox: string, entries: Record<string, unknown> | string) => {
  const directory = path.join(sandbox, "data", "opencode")
  mkdirSync(directory, { recursive: true })
  writeFileSync(path.join(directory, "auth.json"), typeof entries === "string" ? entries : JSON.stringify(entries, null, 2))
}

cliScenario(
  "legacy credentials are imported when the database is created",
  { env: noEnvKey, config: quiet, turns: [say("Using the legacy key."), say("Still the legacy key.")] },
  async (ctx) => {
    legacy(ctx.sandbox, {
      "openrouter/": { type: "api", key: "sk-or-legacy" },
      anthropic: { type: "oauth", refresh: "refresh-a", access: "access-a", expires: 1767225600000, accountId: "acct-1", extra: true },
      acme: { type: "oauth", refresh: "refresh-o", access: "access-o", expires: 0, enterpriseUrl: "" },
      "acme-enterprise": { type: "oauth", refresh: "refresh-e", access: "access-e", expires: 5, enterpriseUrl: "https://acme.example.com" },
      xai: { type: "oauth", refresh: "refresh-x", access: "access-x", expires: 7 },
      groq: { type: "api", key: "gsk-legacy", metadata: { region: "us", tier: "free" } },
      "bad-metadata": { type: "api", key: "x", metadata: { n: 1 } },
      "bad-expiry": { type: "oauth", refresh: "r", access: "a", expires: 1.5 },
      "negative-expiry": { type: "oauth", refresh: "r", access: "a", expires: -1 },
      "missing-key": { type: "api" },
      unknown: { type: "password", value: "x" },
      "/": { type: "api", key: "no-integration" },
      "42": { type: "api", key: "numeric-id" },
      "groq/": { type: "api", key: "second-groq" },
    })
    await ctx.cli(["auth", "export"])
    await ctx.cli(["auth", "list"])
    await ctx.cli(["run", "Which key?"])
    // The database exists now: a changed legacy file is not imported again, even by a new service.
    legacy(ctx.sandbox, { openrouter: { type: "api", key: "sk-or-changed" }, mistral: { type: "api", key: "mistral-legacy" } })
    await ctx.cli(["service", "stop"])
    await ctx.cli(["auth", "export"])
    await ctx.cli(["run", "Which key now?"])
  },
)

cliScenario("a legacy file that is not a JSON object is skipped", { env: noEnvKey, config: quiet }, async (ctx) => {
  legacy(ctx.sandbox, '[{ "type": "api", "key": "sk-or-legacy" }]')
  await ctx.cli(["auth", "export"])
})

cliScenario("a legacy file that is not JSON is skipped", { env: noEnvKey, config: quiet }, async (ctx) => {
  legacy(ctx.sandbox, "openrouter=sk-or-legacy\n")
  await ctx.cli(["auth", "export"])
})
