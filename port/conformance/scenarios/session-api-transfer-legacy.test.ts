// Transcripts exported before v2.1, imported: the import payload's messages are plain JSON on the wire, upgraded by
// `SessionLegacy.messages` (packages/schema/src/session-legacy.ts) and only then decoded as current messages
// (protocol/src/groups/session.ts `ImportedMessages`). So the upgrade decides validity: an old name holding a value
// the new name rejects fails the import, and an old field that replaces an invalid new one rescues it. The body is
// read as the server reads any body (a leading BOM dropped).
import { MODEL_ID } from "../lib/model"
import { scenario as baseScenario, type Ctx } from "../lib/scenario"

const scenario: typeof baseScenario = (name, options, body) =>
  baseScenario(name, options, async (ctx) => {
    await ctx.until((event) => event?.type === "server.connected")
    await body(ctx)
    await ctx.handler(
      new Request("http://opencode.local/api/skill", {
        headers: { "x-opencode-directory": encodeURIComponent(ctx.project) },
      }),
    )
    await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === ctx.project)
  })

const MODEL = { providerID: "openrouter", id: MODEL_ID, variant: "max" }
const T0 = Date.UTC(2026, 0, 5, 10, 0, 0)
const sid = (suffix: string) => `ses_00000000000000conformance${suffix}`
const mid = (suffix: string) => `msg_00000000000000conformance${suffix}`
const tokens = { input: 100, output: 20, reasoning: 5, cache: { read: 10, write: 2 } }

const info = (id: string) => ({
  id,
  projectID: "0000000000000000000000000000000000000000",
  title: "legacy",
  cost: 0,
  tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
  time: { created: T0, updated: T0 },
  location: { directory: "/somewhere/else" },
})

const assistant = (id: string, content: unknown[], extra: Record<string, unknown> = {}) => ({
  id,
  type: "assistant",
  agent: "build",
  model: MODEL,
  content,
  finish: "stop",
  cost: 0.25,
  tokens,
  time: { created: T0 + 1000, streamed: T0 + 1900, completed: T0 + 2000 },
  ...extra,
})

const user = (suffix: string) => ({ id: mid(`U${suffix}`), type: "user", text: "Old prompt", time: { created: T0 } })

async function importing(ctx: Ctx, suffix: string, messages: unknown[]) {
  const id = sid(suffix)
  const result = await ctx.http("POST", "/api/experimental/session/import", {
    info: info(id),
    location: { directory: ctx.project },
    messages,
  })
  if (result.status === 200) await ctx.http("GET", `/api/session/${id}/message`)
}

scenario("an old name holding what the new name rejects", {}, async (ctx) => {
  await importing(ctx, "LEGACYA001", [user("A001"), assistant(mid("A1"), [{ type: "text", text: "t", state: "not-an-object" }])])
  await importing(ctx, "LEGACYA002", [user("A002"), assistant(mid("A2"), [{ type: "text", text: "t" }], { providerState: 7 })])
  await importing(ctx, "LEGACYA003", [
    user("A003"),
    assistant(mid("A3"), [
      {
        type: "tool",
        id: "call_1",
        name: "read",
        state: { status: "completed", input: { path: "a.txt" }, content: [{ type: "text", text: "a" }] },
        time: { created: T0 + 1000, ran: T0 + 1100, completed: T0 + 1500 },
        providerState: "not-an-object",
      },
    ]),
  ])
})

scenario("an old field replacing an invalid new one", {}, async (ctx) => {
  await importing(ctx, "LEGACYB001", [
    user("B001"),
    {
      id: mid("B1"),
      type: "compaction",
      status: "completed",
      reason: "auto",
      summary: "Old summary",
      recent: "kept text",
      metadata: "not-an-object",
      time: { created: T0 + 2000 },
    },
  ])
  await importing(ctx, "LEGACYB002", [
    user("B002"),
    {
      id: mid("B2"),
      type: "compaction",
      status: "completed",
      reason: "auto",
      summary: "Old summary",
      native: "not-an-object",
      providerContext: { messages: [{ role: "user", content: [{ type: "text", text: "hi" }] }] },
      time: { created: T0 + 3000 },
    },
  ])
})

scenario("non-object messages and a byte order mark", {}, async (ctx) => {
  await importing(ctx, "LEGACYC001", [user("C001"), 5])
  await importing(ctx, "LEGACYC002", [user("C002"), null])
  const id = sid("LEGACYC003")
  const body = JSON.stringify({
    info: info(id),
    location: { directory: ctx.project },
    messages: [user("C003"), assistant(mid("C3"), [{ type: "text", text: "t", state: { id: "t1" } }], { providerState: { b: 1 } })],
  })
  const response = await ctx.handler(
    new Request("http://opencode.local/api/experimental/session/import", {
      method: "POST",
      headers: { "x-opencode-directory": encodeURIComponent(ctx.project), "content-type": "application/json" },
      body: new Uint8Array([0xef, 0xbb, 0xbf, ...new TextEncoder().encode(body)]),
    }),
  )
  ctx.note("import with a byte order mark", response.status)
  await ctx.http("GET", `/api/experimental/session/${id}/export`)
})
