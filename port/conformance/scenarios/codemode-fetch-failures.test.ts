// Code Mode `fetch` when the request fails below HTTP: the program catches the runtime's own error (Bun's fetch
// rejects with a TypeError carrying `code`, `path` and `errno`), so its words are what the model sees. TLS failures
// come from the fake network's `.cert.test` hosts (lib/network.ts), a reset from a responder that throws, and a
// refused connection from a loopback address the server reaches directly. The fake network is the server
// process's proxy, so these run only as a process.
import { say, type ProviderRequest, type Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const describe = `const describe = (e) => ({
  name: e && e.name,
  message: e && e.message,
  string: String(e),
  own: Object.fromEntries(Object.entries(e || {})),
})
const attempt = async (url) => {
  try {
    const response = await fetch(url)
    return { status: response.status }
  } catch (e) {
    return describe(e)
  }
}
`

// An execute call's progress rows are throttled to one per 100 ms, so which ones appear depends on timing: they are
// left out of the recording, and the final rows are recorded through the session's messages (as codemode-tools does).
async function settled(ctx: Ctx, id: string) {
  await ctx.http("GET", `/api/session/${id}/message`)
  const events = ctx.events as unknown[]
  for (let index = events.length - 1; index >= 0; index--) {
    const event = events[index] as { type?: string; data?: { metadata?: { toolCalls?: unknown } } }
    if (event?.type === "session.tool.progress" && event.data?.metadata?.toolCalls !== undefined) events.splice(index, 1)
  }
}

const program = (urls: string[]): Turn[] =>
  urls.map((url, index) => ({
    events: [{ tool: "execute", input: { code: `${describe}return await attempt(${JSON.stringify(url)})` }, id: `call_${index}` }],
  }))

const reset = (request: ProviderRequest): Turn | undefined => {
  if (request.url.startsWith("https://reset.example.com/") || request.url.startsWith("http://reset.example.com/"))
    throw new Error("conformance: the connection is reset")
  return undefined
}

scenario(
  "codemode fetch: certificates a client refuses",
  { processOnly: "the fake network's TLS servers are reached through the server process's proxy" },
  async (ctx) => {
    const id = await ctx.session()
    ctx.provider.push(
      ...program([
        "https://self-signed.cert.test/",
        "https://expired.cert.test/a?b=1",
        "https://not-yet-valid.cert.test/",
        "https://wrong-name.cert.test/",
        "https://unknown-issuer.cert.test/",
        "https://unknown-chain.cert.test/",
        "https://root-in-chain.cert.test/",
      ]),
      say("Done."),
    )
    await ctx.prompt(id, "Fetch them")
    await settled(ctx, id)
  },
)

scenario(
  "codemode fetch: a connection that is reset",
  { processOnly: "a responder that throws ends the proxied connection", responders: [reset] },
  async (ctx) => {
    const id = await ctx.session()
    ctx.provider.push(...program(["https://reset.example.com/x", "http://reset.example.com/y"]), say("Done."))
    await ctx.prompt(id, "Fetch them")
    await settled(ctx, id)
  },
)

scenario(
  "codemode fetch: a connection that is refused",
  { processOnly: "the server reaches 127.0.0.2 directly (NO_PROXY), where nothing listens on port 9" },
  async (ctx) => {
    await ctx.restart({ NO_PROXY: "127.0.0.2", no_proxy: "127.0.0.2" })
    const id = await ctx.session()
    ctx.provider.push(...program(["http://127.0.0.2:9/x", "https://127.0.0.2:9/y?z"]), say("Done."))
    await ctx.prompt(id, "Fetch them")
    await settled(ctx, id)
  },
)
