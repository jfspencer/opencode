// File-changing turns and what the session API does with them: per-turn diffs, staged reverts (stage, move,
// clear, commit), and forks. The model edits files through scripted write/edit tool calls in a git project so
// snapshots are captured around each step.
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
const UNKNOWN_MSG = "msg_0000000000000000000000unknown"

const files = {
  "notes.txt": "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\n",
  "src/app.ts": "export const version = 1\n",
}

const messages = (ctx: Ctx, sessionID: string) => ctx.http("GET", `/api/session/${sessionID}/message?order=asc`)

function gate() {
  let open!: () => void
  const promise = new Promise<void>((resolve) => (open = resolve))
  return { promise, open }
}

/** Two turns: the first edits notes.txt, the second rewrites src/app.ts and creates src/new.ts. */
const editTurns: Turn[] = [
  call("read", { path: "notes.txt" }),
  call("edit", { path: "notes.txt", oldString: "delta", newString: "DELTA" }),
  say("Edited the notes."),
  call("write", { path: "src/app.ts", content: "export const version = 2\n" }),
  call("write", { path: "src/new.ts", content: "export const added = true\n" }),
  say("Bumped the version and added a module."),
]

async function twoTurns(ctx: Ctx) {
  const session = await ctx.session({ title: "edits" })
  await ctx.prompt(session, "Uppercase delta in the notes", { id: id("T1") })
  await ctx.prompt(session, "Bump the version and add a module", { id: id("T2") })
  return session
}

scenario("diff the files a turn changed", { git: true, files, turns: editTurns }, async (ctx) => {
  const session = await twoTurns(ctx)
  await ctx.http("GET", `/api/session/${session}/diff`)
  await ctx.http("GET", `/api/session/${session}/diff?from=${id("T1")}`)
  await ctx.http("GET", `/api/session/${session}/diff?from=${id("T1")}&context=0`)
  await ctx.http("GET", `/api/session/${session}/diff?from=${id("T1")}&to=${id("T2")}&context=1`)
  // Ranges must run forward between user messages of this session.
  await ctx.http("GET", `/api/session/${session}/diff?from=${id("T2")}&to=${id("T1")}`)
  const assistant = (await ctx.http("GET", `/api/session/${session}/message?type=assistant&limit=1`)).body.data[0].id
  await ctx.http("GET", `/api/session/${session}/diff?from=${assistant}`)
  await ctx.http("GET", `/api/session/${session}/diff?from=${id("T1")}&to=${assistant}`)
  await ctx.http("GET", `/api/session/${session}/diff?from=${UNKNOWN_MSG}`)
  await ctx.http("GET", `/api/session/${session}/diff?from=${id("T1")}&to=${UNKNOWN_MSG}`)
  await ctx.http("GET", `/api/session/${session}/diff?context=-1`)
  await ctx.http("GET", `/api/session/${session}/diff?from=nope`)
})

scenario(
  "diff a session without changes or without git",
  { files, turns: [say("Nothing to change.")] },
  async (ctx) => {
    const session = await ctx.session({ title: "no git" })
    await ctx.http("GET", `/api/session/${session}/diff`)
    await ctx.prompt(session, "Look around", { id: id("T1") })
    await ctx.http("GET", `/api/session/${session}/diff`)
    await ctx.http("GET", `/api/session/${session}/diff?from=${id("T1")}`)
  },
)

scenario("stage move clear and commit a revert", { git: true, files, turns: editTurns }, async (ctx) => {
  const session = await twoTurns(ctx)
  // Stage the second turn: its file changes are undone.
  await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: id("T2") })
  ctx.note("after staging T2", { app: ctx.read("src/app.ts"), notes: ctx.read("notes.txt") })
  await ctx.http("GET", `/api/session/${session}`)
  await messages(ctx, session)
  await ctx.http("GET", `/api/session/${session}/context`)
  // Move the boundary back to the first turn: its changes are undone too.
  await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: id("T1") })
  ctx.note("after staging T1", { app: ctx.read("src/app.ts"), notes: ctx.read("notes.txt") })
  // Move it forward again: the first turn's changes come back.
  await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: id("T2") })
  ctx.note("after restaging T2", { app: ctx.read("src/app.ts"), notes: ctx.read("notes.txt") })
  // Clear restores every file the staged revert touched, then wakes the session (which finds nothing to do).
  await ctx.http("DELETE", `/api/session/${session}/revert`)
  await ctx.wait(session)
  ctx.note("after clearing", { app: ctx.read("src/app.ts"), notes: ctx.read("notes.txt") })
  await ctx.http("GET", `/api/session/${session}`)
  // Clearing or committing with nothing staged is a no-op.
  await ctx.http("DELETE", `/api/session/${session}/revert`)
  await ctx.wait(session)
  await ctx.http("POST", `/api/session/${session}/revert/commit`)
  // Commit drops the reverted messages for good.
  await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: id("T2") })
  await ctx.http("POST", `/api/session/${session}/revert/commit`)
  await ctx.http("GET", `/api/session/${session}`)
  await messages(ctx, session)
  await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: id("T2") })
  await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: UNKNOWN_MSG })
  await ctx.http("POST", `/api/session/${session}/revert/stage`, {})
})

scenario("stage a revert without touching files", { git: true, files, turns: editTurns }, async (ctx) => {
  const session = await twoTurns(ctx)
  await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: id("T1"), files: false })
  ctx.note("files kept", { app: ctx.read("src/app.ts"), notes: ctx.read("notes.txt") })
  await ctx.http("GET", `/api/session/${session}`)
  // Re-staging with files restores the earlier turn's files.
  await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: id("T2") })
  ctx.note("second turn reverted", { app: ctx.read("src/app.ts"), notes: ctx.read("notes.txt") })
  await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: id("T1"), files: false })
  ctx.note("boundary moved without files", { app: ctx.read("src/app.ts"), notes: ctx.read("notes.txt") })
  await ctx.http("DELETE", `/api/session/${session}/revert`)
  await ctx.wait(session)
  ctx.note("cleared", { app: ctx.read("src/app.ts"), notes: ctx.read("notes.txt") })
})

scenario(
  "new input commits a staged revert",
  { git: true, files, turns: [...editTurns, say("Starting over from the first turn.")] },
  async (ctx) => {
    const session = await twoTurns(ctx)
    await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: id("T2") })
    // Synthetic input is admitted but does not resume while a revert is staged.
    await ctx.http("POST", `/api/session/${session}/synthetic`, { text: "Client note while reverted" })
    await ctx.wait(session)
    await ctx.http("GET", `/api/session/${session}/inbox`)
    await ctx.prompt(session, "Try something else", { id: id("T3") })
    await ctx.http("GET", `/api/session/${session}`)
    await messages(ctx, session)
  },
)

scenario("compaction request commits a staged revert", { git: true, files, turns: editTurns }, async (ctx) => {
  const session = await twoTurns(ctx)
  await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: id("T2") })
  // Compaction makes its own model requests; give it enough scripted replies.
  ctx.provider.push(say("Summary of the conversation so far."), say("More summary."), say("Done summarizing."))
  await ctx.http("POST", `/api/session/${session}/compact`, { id: id("C1") })
  await ctx.wait(session)
  await ctx.http("GET", `/api/session/${session}`)
  await messages(ctx, session)
})

scenario("revert endpoints refuse a busy session", { git: true, files }, async (ctx) => {
  const g = gate()
  ctx.provider.push({ events: [{ text: "Thinking" }, { until: g.promise }, { text: "." }] })
  const session = await ctx.session({ title: "busy" })
  const from = ctx.events.length
  await ctx.http("POST", `/api/session/${session}/prompt`, { id: id("T1"), text: "Hold" })
  await ctx.until((event) => event?.type === "session.text.delta" && event?.data?.sessionID === session, from)
  await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: id("T1") })
  await ctx.http("DELETE", `/api/session/${session}/revert`)
  await ctx.http("POST", `/api/session/${session}/revert/commit`)
  g.open()
  await ctx.settle(session)
})

// Forked copies get message IDs of the form `msg_<fork event>_<seq>`, which the shared ID normalization does not
// recognize. Reads that return them are made outside the HTTP transcript and recorded as notes, with each forked
// ID rewritten to `<forked_N>_<seq>` (N numbered by first appearance), cursors included.
const FORKED = /\bmsg_[0-9A-Za-z]{26}_(\d+)\b/g

function aliasForked(value: unknown, aliases: Map<string, string>): unknown {
  if (typeof value === "string") {
    if (/^[A-Za-z0-9_-]{16,}={0,2}$/.test(value)) {
      try {
        const decoded = JSON.parse(Buffer.from(value, "base64url").toString("utf8"))
        if (decoded && typeof decoded === "object")
          return Buffer.from(JSON.stringify(aliasForked(decoded, aliases))).toString("base64url")
      } catch {}
    }
    return value.replace(FORKED, (raw, seq: string) => {
      const base = raw.slice(0, raw.length - seq.length - 1)
      if (!aliases.has(base)) aliases.set(base, `<forked_${aliases.size + 1}>`)
      return `${aliases.get(base)}_${seq}`
    })
  }
  if (Array.isArray(value)) return value.map((item) => aliasForked(item, aliases))
  if (value && typeof value === "object")
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, aliasForked(item, aliases)]))
  return value
}

function forkObserver(ctx: Ctx) {
  const aliases = new Map<string, string>()
  return async (method: string, path: string, payload?: unknown) => {
    const response = await ctx.handler(
      new Request(new URL(path, "http://opencode.local"), {
        method,
        headers: {
          "x-opencode-directory": encodeURIComponent(ctx.project),
          ...(payload === undefined ? {} : { "content-type": "application/json" }),
        },
        body: payload === undefined ? undefined : JSON.stringify(payload),
      }),
    )
    const text = await response.text()
    const body = text ? JSON.parse(text) : null
    ctx.note(`${method} ${aliasForked(path, aliases)}`, {
      ...(payload === undefined ? {} : { request: aliasForked(payload, aliases) }),
      status: response.status,
      body: aliasForked(body, aliases),
    })
    return { status: response.status, body }
  }
}

scenario("fork a session", { git: true, files, turns: editTurns }, async (ctx) => {
  const observe = forkObserver(ctx)
  const session = await twoTurns(ctx)
  await ctx.http("PUT", `/api/experimental/session/${session}/instructions/entries/team`, { value: { lead: "sam" } })
  const full = await ctx.http("POST", `/api/session/${session}/fork`, {})
  await observe("GET", `/api/session/${full.body.data.id}/message?order=asc`)
  await ctx.http("GET", `/api/experimental/session/${full.body.data.id}/instructions/entries`)
  const partial = await ctx.http("POST", `/api/session/${session}/fork`, { before: id("T2") })
  await observe("GET", `/api/session/${partial.body.data.id}/message?order=asc`)
  await ctx.http("GET", `/api/session?parentID=${session}`)
  await ctx.http("POST", `/api/session/${session}/fork`, { before: UNKNOWN_MSG })
  await ctx.http("POST", `/api/session/${session}/fork`, { before: "nope" })
  const empty = await ctx.session({ title: "empty" })
  await ctx.http("POST", `/api/session/${empty}/fork`, {})
})

// The fork event of a fork names a forked message as its boundary, so this scenario records HTTP only.
scenario("fork a fork", { git: true, files, turns: editTurns, events: false }, async (ctx) => {
  const observe = forkObserver(ctx)
  const session = await twoTurns(ctx)
  const first = await ctx.http("POST", `/api/session/${session}/fork`, {})
  const copied = await observe("GET", `/api/session/${first.body.data.id}/message?order=asc&type=user`)
  // Fork the fork before its copy of the second prompt.
  const nested = await observe("POST", `/api/session/${first.body.data.id}/fork`, { before: copied.body.data[1].id })
  await observe("GET", `/api/session/${nested.body.data.id}`)
  await observe("GET", `/api/session/${nested.body.data.id}/message?order=asc`)
  const again = await observe("POST", `/api/session/${nested.body.data.id}/fork`, {})
  await observe("GET", `/api/session/${again.body.data.id}`)
})

scenario(
  "a forked session continues independently",
  { git: true, files, turns: [...editTurns.slice(0, 3), say("Continuing in the fork.")] },
  async (ctx) => {
    const observe = forkObserver(ctx)
    const session = await ctx.session({ title: "origin" })
    await ctx.prompt(session, "Uppercase delta in the notes", { id: id("T1") })
    const fork = (await ctx.http("POST", `/api/session/${session}/fork`, {})).body.data.id
    await ctx.prompt(fork, "Now continue here")
    await observe("GET", `/api/session/${fork}/message?order=asc`)
    await messages(ctx, session)
  },
)
