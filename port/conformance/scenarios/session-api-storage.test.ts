// The experimental part-row storage (`experimental.part_rows`): assistant parts stored as their own rows must read
// back, export, fork and revert exactly like inline ones, alone and together with media blobs.
import { writeFileSync } from "node:fs"
import path from "node:path"
import { call, say, type Turn } from "../lib/openrouter"
import { scenario as baseScenario, type Ctx } from "../lib/scenario"

// The live event subscription is only in place once the server says so; waiting for that keeps the first events of
// a scenario (such as its first session.created) from racing the subscription. Several reads boot the project's
// location without waiting for its plugins, which then announce themselves in the background; finishing every
// scenario with the project's location fully loaded (through a read that waits for plugins, made outside the
// transcript) keeps the recorded global events the same on every run.
const scenario: typeof baseScenario = (name, options, body) =>
  baseScenario(name, options, async (ctx) => {
    if (options.events === false) return body(ctx)
    await ctx.until((event) => event?.type === "server.connected")
    await body(ctx)
    await ctx.handler(
      new Request("http://opencode.local/api/skill", {
        headers: { "x-opencode-directory": encodeURIComponent(ctx.project) },
      }),
    )
    await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === ctx.project)
  })

const id = (suffix: string) => `msg_00000000000000conformance${suffix}`
const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="
const MEDIA = { "x-opencode-capabilities": "experimental-media-uri" }
const messages = (ctx: Ctx, sessionID: string, headers?: Record<string, string>) =>
  ctx.http("GET", `/api/session/${sessionID}/message?order=asc`, undefined, headers)

const files = { "notes.txt": "one\ntwo\n", "src/app.ts": "export const v = 1\n" }

const turns: Turn[] = [
  {
    events: [
      { reasoning: "Read first, then edit." },
      { text: "Reading the notes." },
      { tool: "read", input: { path: "notes.txt" } },
    ],
  },
  // One tool per step: results of parallel calls in one step come back in completion order.
  call("read", { path: "missing.txt" }),
  call("edit", { path: "notes.txt", oldString: "two", newString: "TWO" }),
  call("write", { path: "src/app.ts", content: "export const v = 2\n" }),
  say("Edited both files."),
  say("Second turn reply."),
]

// Forked copies get message IDs `msg_<fork event>_<seq>` that the shared normalization does not recognize; reads of a
// fork go outside the transcript and are recorded as notes with those IDs rewritten to `<forked>_<seq>`.
function aliasForked(value: unknown): unknown {
  if (typeof value === "string") {
    if (/^[A-Za-z0-9_-]{16,}={0,2}$/.test(value)) {
      try {
        const decoded = JSON.parse(Buffer.from(value, "base64url").toString("utf8"))
        if (decoded && typeof decoded === "object")
          return Buffer.from(JSON.stringify(aliasForked(decoded))).toString("base64url")
      } catch {}
    }
    return value.replace(/\bmsg_[0-9A-Za-z]{26}_(\d+)\b/g, (_raw, seq: string) => `<forked>_${seq}`)
  }
  if (Array.isArray(value)) return value.map(aliasForked)
  if (value && typeof value === "object")
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, aliasForked(item)]))
  return value
}

async function observeForked(ctx: Ctx, url: string, headers: Record<string, string> = {}) {
  const response = await ctx.handler(
    new Request(new URL(url, "http://opencode.local"), {
      headers: { "x-opencode-directory": encodeURIComponent(ctx.project), ...headers },
    }),
  )
  ctx.note(`GET ${url}`, { status: response.status, body: aliasForked(await response.json()) })
}

scenario(
  "assistant parts stored as rows read back like inline content",
  { git: true, files, config: { experimental: { part_rows: true } }, turns },
  async (ctx) => {
    const session = await ctx.session({ title: "part rows" })
    await ctx.prompt(session, "Edit the notes and bump the version", { id: id("P1") })
    await ctx.prompt(session, "Anything else?", { id: id("P2") })
    await messages(ctx, session)
    const assistant = (await ctx.http("GET", `/api/session/${session}/message?type=assistant&order=asc&limit=1`)).body
      .data[0].id
    await ctx.http("GET", `/api/session/${session}/message/${assistant}`)
    await ctx.http("GET", `/api/session/${session}/context`)
    await ctx.http("GET", `/api/experimental/session/${session}/export?sanitize=true`)
    await ctx.http("GET", `/api/session/${session}/diff?from=${id("P1")}`)
    const fork = await ctx.http("POST", `/api/session/${session}/fork`, { before: id("P2") })
    await observeForked(ctx, `/api/session/${fork.body.data.id}/message?order=asc`)
    // Reverting the first turn restores both files and drops its part rows on commit.
    await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: id("P1") })
    ctx.note("files after staging", { notes: ctx.read("notes.txt"), app: ctx.read("src/app.ts") })
    await ctx.http("POST", `/api/session/${session}/revert/commit`)
    await messages(ctx, session)
  },
)

scenario(
  "part rows with media blobs",
  {
    files,
    config: { experimental: { part_rows: true, media_blob: true } },
    turns: [call("read", { path: "pixel.png" }), say("A pixel."), say("Fork reply.")],
  },
  async (ctx) => {
    writeFileSync(path.join(ctx.project, "pixel.png"), Buffer.from(PNG, "base64"))
    const session = await ctx.session({ title: "part media" })
    await ctx.prompt(session, "Read the image", { id: id("M1") })
    await messages(ctx, session, MEDIA)
    await messages(ctx, session)
    const fork = await ctx.http("POST", `/api/session/${session}/fork`, {})
    await observeForked(ctx, `/api/session/${fork.body.data.id}/message?order=asc&type=assistant`, MEDIA)
    await ctx.prompt(fork.body.data.id, "Continue in the fork")
    await ctx.http("DELETE", `/api/session/${session}`)
    await observeForked(ctx, `/api/session/${fork.body.data.id}/message?order=asc&type=assistant`, MEDIA)
  },
)

scenario(
  "a tool file that refers to missing media reads as unavailable",
  { files, config: { experimental: { media_blob: true } } },
  async (ctx) => {
    const session = "ses_00000000000000conformanceMISSING"
    const time = Date.UTC(2026, 0, 5)
    await ctx.http("POST", "/api/experimental/session/import", {
      info: {
        id: session,
        projectID: "0000000000000000000000000000000000000000",
        cost: 0,
        tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
        time: { created: time, updated: time },
        location: { directory: ctx.project },
      },
      location: { directory: ctx.project },
      messages: [
        { id: id("U1"), type: "user", text: "Show me", time: { created: time } },
        {
          id: id("A1"),
          type: "assistant",
          agent: "build",
          model: { providerID: "openrouter", id: "deepseek/deepseek-v4.1-flash-1017" },
          content: [
            {
              type: "tool",
              id: "call_1",
              name: "read",
              state: {
                status: "completed",
                input: { path: "gone.png" },
                content: [
                  { type: "file", uri: `media:${"0".repeat(64)}`, mime: "image/png", name: "gone.png" },
                  { type: "file", uri: `media:${"1".repeat(64)}`, mime: "image/png" },
                ],
              },
              time: { created: time, completed: time + 1 },
            },
          ],
          time: { created: time + 1, completed: time + 2 },
        },
      ],
    })
    await messages(ctx, session)
    await messages(ctx, session, MEDIA)
    await ctx.http("GET", `/api/session/${session}/message/${id("A1")}`, undefined, MEDIA)
  },
)
