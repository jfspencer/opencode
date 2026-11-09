// `opencode auth` flows beyond API keys: the integration, method and account pickers on a terminal, form fields
// (on the terminal and as `--answer`), external credential references, command logins of well-known sources (the
// fake network answers the manifest; the command is a shell one-liner), and an OAuth login in the browser (Poe's
// PKCE flow: a stand-in browser on the PATH calls the local callback, the fake network answers the token exchange).
import { readFileSync } from "node:fs"
import { cliScenario, type CliCtx } from "../lib/cli"
import type { ProviderRequest, Responder } from "../lib/openrouter"

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

const manifest =
  (origin: string, body: unknown): Responder =>
  (request) =>
    request.method === "GET" && request.url === `${origin}/.well-known/opencode` ? { status: 200, body } : undefined

const DOWN = "\x1b[B"
const CTRL_C = "\x03"
const BACKSPACE = "\x7f"
const FOOTER = "│  ↑/↓ to select • Enter: confirm • Type: to search\n└"

cliScenario("auth login picks an integration from the list", { env: noEnvKey }, async (ctx) => {
  await ctx.terminal(["auth", "login"], [
    { waitFor: FOOTER, type: "openrouter" },
    { waitFor: `│  Search: openrouter█\n│\n│  Services\n│  ● OpenRouter\n${FOOTER}`, type: "\r" },
    { waitFor: "Manually enter API Key", type: "sk-or-picked\r" },
  ])
  await ctx.cli(["auth", "list"])
})

cliScenario("the integration picker's search and cancel", { env: noEnvKey }, async (ctx) => {
  await ctx.terminal(["auth", "login"], [
    { waitFor: FOOTER, type: "zzz" },
    { waitFor: `│  Search: zzz█\n│  No integrations found\n${FOOTER}`, type: CTRL_C },
  ])
  // "open" matches two popular integrations before OpenRouter.
  await ctx.terminal(["auth", "login"], [
    { waitFor: FOOTER, type: "open" },
    { waitFor: "│  Search: open█", type: DOWN + DOWN },
    {
      waitFor: `│  Search: open\n│\n│  Popular\n│  ○ OpenCode Console\n│  ○ openai\n│\n│  Services\n│  ● OpenRouter\n${FOOTER}`,
      type: "\r",
    },
    { waitFor: "Manually enter API Key", type: "sk-or-found\r" },
  ])
  await ctx.cli(["auth", "list"])
})

cliScenario("login method picker and an external login with a form", {}, async (ctx) => {
  // The AWS profile has no listed profiles here: the field offers only a typed answer.
  // A blank profile is refused, then corrected.
  await ctx.terminal(["auth", "login", "amazon-bedrock"], [
    { waitFor: "◆  Select login method\n│  ● AWS profile (SSO or named profile)\n│  ○ Bedrock API key\n└", type: "\r" },
    { waitFor: "◆  AWS profile\n│  ● Type your own answer\n└", type: "\r" },
    { waitFor: "◆  AWS profile\n│  Profile name\n└", type: " \r" },
    { waitFor: "▲  AWS profile\n│   █\n└  Invalid format", type: BACKSPACE + "dev\r" },
  ])
  await ctx.terminal(["auth", "login", "amazon-bedrock"], [
    { waitFor: "Select login method", type: DOWN },
    { waitFor: "● Bedrock API key", type: "\r" },
    { waitFor: "│  _", type: CTRL_C },
  ])
  await ctx.cli(["auth", "list"])
})

cliScenario("form answers on the command line", {}, async (ctx) => {
  await ctx.cli(["auth", "login", "amazon-bedrock", "--method", "aws-profile"])
  await ctx.cli(["auth", "login", "amazon-bedrock", "--method", "aws-profile", "--answer", "profile="])
  await ctx.cli(["auth", "login", "amazon-bedrock", "--method", "aws-profile", "--answer", "region=eu"])
  await ctx.cli(["auth", "login", "amazon-bedrock", "--method", "aws-profile", "--answer", "profile=dev"])
  await ctx.cli(["auth", "login", "claude-agent", "--answer", "profile=dev"])
  await ctx.cli(["auth", "list", "--format", "json"])
})

cliScenario("form fields that depend on earlier answers", {}, async (ctx) => {
  await ctx.cli(["auth", "login", "github-copilot", "--answer", "deploymentType=other"])
  await ctx.cli(["auth", "login", "github-copilot", "--answer", "deploymentType=github.com", "--answer", "enterpriseUrl=x"])
  // The enterprise URL is asked only for an enterprise deployment.
  await ctx.terminal(["auth", "login", "github-copilot"], [
    { waitFor: "◆  Select GitHub deployment type\n│  ● GitHub.com (Public)\n│  ○ GitHub Enterprise\n└", type: DOWN },
    { waitFor: "│  ● GitHub Enterprise (Data residency or self-hosted)", type: "\r" },
    { waitFor: "◆  Enter your GitHub Enterprise URL or domain\n│  company.ghe.com or https://company.ghe.com", type: "company.ghe.com" },
    { waitFor: "│  company.ghe.com█", type: CTRL_C },
  ])
  // A typed project, then the location's choices (the default first).
  await ctx.terminal(["auth", "login", "google-vertex"], [
    { waitFor: "◆  Google Cloud project\n│  ● Type your own answer\n└", type: "\r" },
    { waitFor: "◆  Google Cloud project\n│  Project ID", type: "my-project\r" },
    { waitFor: "◆  Location\n│  ● global\n│  ○ us\n│  ○ eu\n│  ○ Type your own answer\n└", type: DOWN + DOWN + DOWN },
    { waitFor: "│  ● Type your own answer", type: "\r" },
    { waitFor: "◆  Location\n│  global", type: CTRL_C },
  ])
})

const company = "https://auth.example.test"
const refused = "https://refused.example.test"

cliScenario(
  "a well-known source's command login",
  {
    responders: [
      manifest(company, {
        auth: {
          // The login reports progress on stderr, waits, then prints the credential.
          command: ["sh", "-c", "echo 'Opening the company login' >&2; echo 'Signed in as alice' >&2; sleep 2; echo company-token"],
          env: "COMPANY_TOKEN",
        },
      }),
      // It waits before failing too, so the CLI's polling sees its message while it runs.
      manifest(refused, {
        auth: { command: ["sh", "-c", "echo 'Access denied for alice' >&2; sleep 2; exit 3"], env: "COMPANY_TOKEN" },
      }),
      manifest("https://manual.example.test", { config: {} }),
    ],
  },
  async (ctx) => {
    await ctx.cli(["auth", "login", `${company}/`])
    await ctx.cli(["auth", "login", refused])
    await ctx.cli(["auth", "login", "https://manual.example.test"])
    await ctx.cli(["auth", "login", "https://missing.example.test"])
    await ctx.cli(["auth", "list"])
    await ctx.cli(["auth", "export", company])
  },
)

// A browser: completes the authorization it is sent to by calling the callback with the request's state and a code,
// and keeps the URL it opened and the callback page's answer.
const browser = (sandbox: string) => `#!/bin/sh
url="$1"
query="\${url#*\\?}"
param() { printf '%s' "$query" | tr '&' '\\n' | sed -n "s/^$1=//p"; }
redirect=$(param redirect_uri | sed 's/%3A/:/g; s/%2F/\\//g')
state=$(param state)
printf '%s\\n' "$url" > ${sandbox}/browser-url.txt
curl -s --noproxy '*' -o ${sandbox}/callback.html -w '%{http_code}\\n' "$redirect?code=conformance-code&state=$state" > ${sandbox}/callback.tmp
mv ${sandbox}/callback.tmp ${sandbox}/callback-status.txt
`

/**
 * Poe's token endpoint. The exchange carries the PKCE verifier and the callback address, which are random: the record
 * says whether the verifier matches the challenge the browser was sent and the address is the callback's.
 */
const poeToken =
  (sandbox: () => string): Responder =>
  (request) => {
    if (request.method !== "POST" || request.url !== "https://api.poe.com/token") return undefined
    mask(request, sandbox())
    return { status: 200, body: { api_key: "poe-conformance-key", api_key_expires_in: null } }
  }

function mask(request: ProviderRequest, sandbox: string) {
  const opened = new URL(readFileSync(`${sandbox}/browser-url.txt`, "utf8").trim())
  const params = new URLSearchParams(String(request.body))
  const verifier = params.get("code_verifier") ?? ""
  const challenge = new Bun.CryptoHasher("sha256").update(verifier).digest("base64url")
  params.set("code_verifier", challenge === opened.searchParams.get("code_challenge") ? "<verifier of the challenge>" : verifier)
  if (params.get("redirect_uri") === opened.searchParams.get("redirect_uri")) params.set("redirect_uri", "<the callback>")
  ;(request as { body: unknown }).body = Object.fromEntries(params)
}

let sandboxOf: CliCtx | undefined
cliScenario(
  "OAuth login in the browser",
  {
    env: noEnvKey,
    bin: { "www-browser": browser, "xdg-open": browser },
    responders: [poeToken(() => sandboxOf!.sandbox)],
  },
  async (ctx) => {
    sandboxOf = ctx
    await ctx.terminal(["auth", "login", "poe"], [])
    ctx.file("browser-url.txt")
    ctx.file("callback-status.txt")
    ctx.file("callback.html")
    await ctx.cli(["auth", "list", "--format", "json"])
  },
)

cliScenario("account pickers for switching and logging out", { env: noEnvKey }, async (ctx) => {
  await ctx.terminal(["auth", "logout"], [])
  await ctx.cli(["auth", "import"], {
    stdin: exported([
      { id: "cred_conformanceWorkKey0001", label: "Work key", key: "sk-or-work", active: true },
      { id: "cred_conformanceHomeKey0002", label: "Home key", key: "sk-or-home", active: false },
    ]),
  })
  await ctx.terminal(["auth", "switch"], [
    { waitFor: "Select integration", type: "\r" },
    { waitFor: "account to switch to", type: DOWN },
    { waitFor: "● Home key", type: "\r" },
  ])
  await ctx.cli(["auth", "list", "--format", "json"])
  await ctx.terminal(["auth", "logout", "openrouter"], [{ waitFor: "account to log out", type: "work\r" }])
  await ctx.terminal(["auth", "logout"], [
    { waitFor: "Select integration", type: "\r" },
    { waitFor: "account to log out", type: CTRL_C },
  ])
  await ctx.cli(["auth", "list"])
})
