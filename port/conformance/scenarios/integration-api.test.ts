// Integration logins through the server API, as the TUI's connect dialog (dialog-integration.tsx) and `opencode auth`
// drive them: the catalog and unknown integrations, API keys with labels and form answers, external credential
// references (their validation), OAuth attempts (a browser login with Poe's PKCE flow, whose callback the scenario
// calls as the browser would and whose token endpoint the fake network answers; failures, completion and
// cancellation, unknown attempts and methods), and command logins of well-known sources (the fake network answers
// `/.well-known/opencode`; the commands are shell one-liners).
import { type Ctx, scenario } from "../lib/scenario"
import type { ProviderRequest, Responder } from "../lib/openrouter"

const NO_ATTEMPT = "con_00000000000000000000000000"

/** Answers `GET <origin>/.well-known/opencode` with the manifest. */
const manifest =
  (origin: string, body: unknown): Responder =>
  (request) =>
    request.method === "GET" && request.url === `${origin}/.well-known/opencode` ? { status: 200, body } : undefined

/** A request through the implementation that is not recorded (polling, or an answer recorded in another form). */
async function raw(ctx: Ctx, method: string, path: string, body?: unknown) {
  const response = await ctx.handler(
    new Request(`http://opencode.local${path}`, {
      method,
      headers: {
        "x-opencode-directory": encodeURIComponent(ctx.project),
        ...(body === undefined ? {} : { "content-type": "application/json" }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    }),
  )
  return { status: response.status, body: (await response.json().catch(() => null)) as any }
}

/** Waits, unrecorded, until a login attempt's status satisfies `done` (by default: it settled). */
async function waitFor(ctx: Ctx, path: string, done = (data: any) => data?.status !== "pending") {
  const deadline = Date.now() + 20_000
  while (Date.now() < deadline) {
    if (done((await raw(ctx, "GET", path)).body?.data)) return
    await Bun.sleep(50)
  }
  throw new Error(`attempt never got there: ${path}`)
}

scenario("integration catalog entries and unknown integrations", {}, async (ctx) => {
  await ctx.http("GET", "/api/integration/openrouter")
  await ctx.http("GET", "/api/integration/claude-agent")
  await ctx.http("GET", "/api/integration/poe")
  await ctx.http("GET", "/api/integration/no-such-integration")
  await ctx.http("POST", "/api/integration/no-such-integration/connect/key", { key: "x" })
  await ctx.http("POST", "/api/integration/no-such-integration/connect/external", { methodID: "x" })
  await ctx.http("POST", "/api/integration/no-such-integration/connect/command", { methodID: "x" })
  await ctx.http("GET", `/api/integration/no-such-integration/connect/oauth/${NO_ATTEMPT}`)
  await ctx.http("POST", `/api/integration/no-such-integration/connect/oauth/${NO_ATTEMPT}/complete`, {})
  await ctx.http("GET", `/api/integration/no-such-integration/connect/command/${NO_ATTEMPT}`)
})

scenario("external credential references are validated", {}, async (ctx) => {
  await ctx.http("POST", "/api/integration/claude-agent/connect/external", { methodID: "no-such-method" })
  await ctx.http("POST", "/api/integration/claude-agent/connect/external", { methodID: "claude-login", answer: { profile: "x" } })
  await ctx.http("POST", "/api/integration/amazon-bedrock/connect/external", { methodID: "aws-profile" })
  await ctx.http("POST", "/api/integration/amazon-bedrock/connect/external", { methodID: "aws-profile", answer: { profile: " " } })
  await ctx.http("POST", "/api/integration/amazon-bedrock/connect/external", { methodID: "aws-profile", answer: { profile: 3 } })
  await ctx.http("POST", "/api/integration/amazon-bedrock/connect/external", { methodID: "aws-profile", answer: { other: "x" } })
  await ctx.http("POST", "/api/integration/openrouter/connect/external", { methodID: "aws-profile" })
  await ctx.http("GET", "/api/credential")
})

scenario("API keys with labels and form answers", {}, async (ctx) => {
  await ctx.http("POST", "/api/integration/openrouter/connect/key", { key: "sk-or-a", answer: { region: "eu" } })
  await ctx.http("POST", "/api/integration/openrouter/connect/key", { key: "sk-or-a", answer: {} })
  await ctx.http("POST", "/api/integration/openrouter/connect/key", { key: "sk-or-b", label: "Work" })
  await ctx.http("POST", "/api/integration/openrouter/connect/key", { key: "sk-or-c" })
  await ctx.http("POST", "/api/integration/amazon-bedrock/connect/key", { key: "x", answer: { region: "eu" } })
  await ctx.http("GET", "/api/integration/openrouter")
  await ctx.http("GET", "/api/credential")
})

scenario(
  "integrations a policy does not allow to connect",
  { config: { policies: [{ action: "integration.connect", resource: "poe", effect: "deny" }] } },
  async (ctx) => {
    const listed = await raw(ctx, "GET", "/api/integration")
    ctx.note("poe listed", listed.body.data.some((integration: { id: string }) => integration.id === "poe"))
    await ctx.http("GET", "/api/integration/poe")
    await ctx.http("POST", "/api/integration/poe/connect/oauth", { methodID: "browser" })
    await ctx.http("POST", "/api/integration/poe/connect/key", { key: "x" })
    await ctx.http("POST", "/api/integration/poe/connect/command", { methodID: "x" })
  },
)

scenario("OAuth attempts that do not exist", {}, async (ctx) => {
  await ctx.http("GET", `/api/integration/openrouter/connect/oauth/${NO_ATTEMPT}`)
  await ctx.http("POST", `/api/integration/openrouter/connect/oauth/${NO_ATTEMPT}/complete`, {})
  await ctx.http("POST", `/api/integration/openrouter/connect/oauth/${NO_ATTEMPT}/complete`, { code: "abc" })
  await ctx.http("DELETE", `/api/integration/openrouter/connect/oauth/${NO_ATTEMPT}`)
  await ctx.http("DELETE", `/api/integration/no-such-integration/connect/oauth/${NO_ATTEMPT}`)
  await ctx.http("POST", "/api/integration/openrouter/connect/oauth", { methodID: "browser" })
  await ctx.http("POST", "/api/integration/no-such-integration/connect/oauth", { methodID: "browser" })
})

scenario("command logins without a command method", {}, async (ctx) => {
  await ctx.http("POST", "/api/integration/openrouter/connect/command", { methodID: "login" })
  await ctx.http("GET", `/api/integration/openrouter/connect/command/${NO_ATTEMPT}`)
  await ctx.http("DELETE", `/api/integration/openrouter/connect/command/${NO_ATTEMPT}`)
})

// --- Poe's browser login ---

const POE_TOKEN = "https://api.poe.com/token"

/**
 * Poe's token endpoint, answering `answer` for each exchange in turn. An exchange carries the PKCE verifier and the
 * callback address, which are random: the record says whether the verifier matches the challenge of the attempt it
 * completes and the address is that attempt's callback.
 */
function poeToken(attempts: URL[], answers: { status: number; body: unknown }[]): Responder {
  return (request) => {
    if (request.method !== "POST" || request.url !== POE_TOKEN) return undefined
    mask(request, attempts)
    return answers.shift()
  }
}

function mask(request: ProviderRequest, attempts: URL[]) {
  const params = new URLSearchParams(String(request.body))
  const verifier = params.get("code_verifier") ?? ""
  const challenge = new Bun.CryptoHasher("sha256").update(verifier).digest("base64url")
  const opened = attempts.find((url) => url.searchParams.get("code_challenge") === challenge)
  if (opened) params.set("code_verifier", "<verifier of the attempt's challenge>")
  if (opened && params.get("redirect_uri") === opened.searchParams.get("redirect_uri"))
    params.set("redirect_uri", "<the attempt's callback>")
  ;(request as { body: unknown }).body = Object.fromEntries(params)
}

/** The attempt as recorded: its authorization URL's random parts (state, challenge, callback port) described. */
function describe(attempt: { attemptID: string; url: string } & Record<string, unknown>) {
  const url = new URL(attempt.url)
  const params = Object.fromEntries(url.searchParams)
  const callback = new URL(params.redirect_uri)
  return {
    ...attempt,
    url: {
      origin: url.origin,
      pathname: url.pathname,
      params: {
        ...params,
        redirect_uri: `${callback.protocol}//${callback.hostname}:<port>${callback.pathname}`,
        code_challenge: /^[A-Za-z0-9_-]{43}$/.test(params.code_challenge) ? "<43 base64url characters>" : params.code_challenge,
        state: /^[A-Za-z0-9_-]{43}$/.test(params.state) ? "<43 base64url characters>" : params.state,
      },
    },
  }
}

/** Opens the attempt's callback as the browser would after authorizing (`query` overrides the parameters sent). */
async function callback(ctx: Ctx, attempt: URL, query: Record<string, string>) {
  const target = new URL(attempt.searchParams.get("redirect_uri")!)
  for (const [key, value] of Object.entries(query)) target.searchParams.set(key, value)
  const response = await fetch(target)
  const body = await response.text()
  ctx.note("callback", {
    query: Object.fromEntries([...target.searchParams].map(([key, value]) => [key, value === attempt.searchParams.get("state") ? "<state>" : value])),
    status: response.status,
    contentType: response.headers.get("content-type"),
    body,
  })
}

const opened: URL[] = []
scenario(
  "a browser OAuth login",
  {
    responders: [
      poeToken(opened, [
        { status: 400, body: { error: "invalid_grant", error_description: "Authorization code expired" } },
        { status: 200, body: { api_key: "  poe-conformance-key ", api_key_expires_in: null } },
      ]),
    ],
  },
  async (ctx) => {
    opened.length = 0
    const connect = async (body: Record<string, unknown>) => {
      const started = await raw(ctx, "POST", "/api/integration/poe/connect/oauth", body)
      ctx.note("connect", { status: started.status, location: started.body.location, data: describe(started.body.data) })
      const url = new URL(started.body.data.url)
      opened.push(url)
      return { id: started.body.data.attemptID as string, url }
    }
    const status = (id: string) => `/api/integration/poe/connect/oauth/${id}`

    // The callback refuses a wrong state; the attempt fails with it.
    const first = await connect({ methodID: "browser", answer: { ignored: true } })
    await ctx.http("GET", status(first.id))
    await callback(ctx, first.url, { code: "conformance-code", state: "wrong-state" })
    await waitFor(ctx, status(first.id))
    await ctx.http("GET", status(first.id))

    // The token endpoint refuses the code.
    const second = await connect({ methodID: "browser" })
    await callback(ctx, second.url, { code: "conformance-code", state: second.url.searchParams.get("state")! })
    await waitFor(ctx, status(second.id))
    await ctx.http("GET", status(second.id))

    // An automatic attempt completes through its callback only; cancelling releases it.
    const third = await connect({ methodID: "browser", label: "Poe account" })
    await ctx.http("POST", `${status(third.id)}/complete`, {})
    await ctx.http("POST", `${status(third.id)}/complete`, { code: "conformance-code" })
    await ctx.http("DELETE", status(third.id))
    await ctx.http("GET", status(third.id))

    const fourth = await connect({ methodID: "browser", label: "Poe account" })
    await callback(ctx, fourth.url, { code: "conformance-code", state: fourth.url.searchParams.get("state")! })
    await waitFor(ctx, status(fourth.id))
    await ctx.http("GET", status(fourth.id))
    await ctx.http("DELETE", status(fourth.id))
    await ctx.http("GET", "/api/integration/poe")
    await ctx.http("GET", "/api/credential")
  },
)

// --- Well-known sources and their command logins ---

const company = "https://auth.example.test"
const id = encodeURIComponent(company)
const login = (command: string) => ({ auth: { command: ["sh", "-c", command], env: "COMPANY_TOKEN" } })

scenario(
  "a well-known source's command login",
  {
    responders: [
      manifest(company, login("echo 'Opening the company login' >&2; echo 'Signed in as alice' >&2; echo '  company-token  '")),
    ],
  },
  async (ctx) => {
    const from = ctx.events.length
    await ctx.http("POST", "/api/experimental/integration/wellknown", { url: `${company}//` })
    await ctx.until((event) => event?.type === "integration.updated", from)
    // Adding it again changes nothing (no update is announced).
    await ctx.http("POST", "/api/experimental/integration/wellknown", { url: company })
    const listed = await raw(ctx, "GET", "/api/integration")
    ctx.note("listed", listed.body.data.map((integration: { id: string; name: string }) => `${integration.id} (${integration.name})`))
    await ctx.http("GET", `/api/integration/${id}`)
    await ctx.http("POST", `/api/integration/${id}/connect/command`, { methodID: "other" })
    const started = await ctx.http("POST", `/api/integration/${id}/connect/command`, { methodID: "login", label: "Company" })
    await waitFor(ctx, `/api/integration/${id}/connect/command/${started.body.data.attemptID}`)
    await ctx.http("GET", `/api/integration/${id}/connect/command/${started.body.data.attemptID}`)
    await ctx.http("DELETE", `/api/integration/${id}/connect/command/${started.body.data.attemptID}`)
    await ctx.http("GET", `/api/integration/${id}/connect/command/${started.body.data.attemptID}`)
    await ctx.http("GET", `/api/integration/${id}`)
    await ctx.http("GET", "/api/credential")
  },
)

const commands = {
  refused: "echo 'Access denied for alice' >&2; echo 'Ask your administrator' >&2; exit 3",
  silent: "exit 4",
  empty: "echo 'Nothing to print' >&2",
  killed: "echo 'Interrupted' >&2; kill -TERM $$",
  waiting: "echo 'Waiting for approval' >&2; exec sleep 30",
}

scenario(
  "failed and cancelled command logins",
  {
    responders: [
      ...Object.entries(commands).map(([name, command]) => manifest(`https://${name}.example.test`, login(command))),
      manifest("https://absent.example.test", { auth: { command: ["no-such-command-conformance", "--login"], env: "X" } }),
    ],
  },
  async (ctx) => {
    for (const name of [...Object.keys(commands), "absent"]) {
      await ctx.http("POST", "/api/experimental/integration/wellknown", { url: `https://${name}.example.test` })
    }
    const path = (name: string, attempt = "") =>
      `/api/integration/${encodeURIComponent(`https://${name}.example.test`)}/connect/command${attempt && `/${attempt}`}`
    for (const name of ["refused", "silent", "empty", "killed", "absent"]) {
      const started = await ctx.http("POST", path(name), { methodID: "login" })
      await waitFor(ctx, path(name, started.body.data.attemptID))
      await ctx.http("GET", path(name, started.body.data.attemptID))
    }
    const started = await ctx.http("POST", path("waiting"), { methodID: "login" })
    const attempt = path("waiting", started.body.data.attemptID)
    await waitFor(ctx, attempt, (data) => typeof data?.message === "string")
    await ctx.http("GET", attempt)
    await ctx.http("DELETE", attempt)
    await ctx.http("GET", attempt)
    await ctx.http("GET", "/api/credential")
  },
)

scenario(
  "well-known sources that cannot be added",
  {
    responders: [
      manifest("https://manual.example.test", { config: {} }),
      manifest("https://broken.example.test", { auth: { command: "not-a-list" } }),
      (request) => (request.url === "https://html.example.test/.well-known/opencode" ? { raw: "<html></html>", headers: { "content-type": "text/html" } } : undefined),
    ],
  },
  async (ctx) => {
    await ctx.http("POST", "/api/experimental/integration/wellknown", { url: "https://manual.example.test" })
    await ctx.http("POST", "/api/experimental/integration/wellknown", { url: "https://broken.example.test" })
    await ctx.http("POST", "/api/experimental/integration/wellknown", { url: "https://html.example.test" })
    await ctx.http("POST", "/api/experimental/integration/wellknown", { url: "https://missing.example.test/" })
    await ctx.http("POST", "/api/experimental/integration/wellknown", { url: "not a url" })
    await ctx.http("POST", "/api/experimental/integration/wellknown", {})
    const listed = await raw(ctx, "GET", "/api/integration")
    ctx.note("listed", listed.body.data.length)
  },
)
