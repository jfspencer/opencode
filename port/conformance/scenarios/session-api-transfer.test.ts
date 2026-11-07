// Session export and import (including sanitized exports and transcripts from earlier releases), and session
// statistics computed over imported history with fixed timestamps.
import { MODEL_ID } from "../lib/model"
import { call, say } from "../lib/openrouter"
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

/**
 * Some reads boot the session's location without waiting for its plugins, which then announce themselves in the
 * background. Waiting for the last announcement keeps the recorded global events complete on every run.
 */
const booted = (ctx: Ctx, directory = ctx.project) =>
  ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === directory)

const MODEL = { providerID: "openrouter", id: MODEL_ID, variant: "max" }
const UNKNOWN = "ses_0000000000000000000000unknown"
// Fixed history timestamps (January 2026) so stats and transcripts do not depend on the wall clock.
const DAY = 86_400_000
const T0 = Date.UTC(2026, 0, 5, 10, 0, 0)
const sid = (suffix: string) => `ses_00000000000000conformance${suffix}`
const mid = (suffix: string) => `msg_00000000000000conformance${suffix}`
const tokens = (input: number, output: number, reasoning = 0, read = 0, write = 0) => ({
  input,
  output,
  reasoning,
  cache: { read, write },
})

function info(id: string, overrides: Record<string, unknown> = {}) {
  return {
    id,
    projectID: "0000000000000000000000000000000000000000",
    cost: 0,
    tokens: tokens(0, 0),
    time: { created: T0, updated: T0 },
    location: { directory: "/somewhere/else" },
    ...overrides,
  }
}

const user = (id: string, time: number, text: string) => ({ id, type: "user", text, time: { created: time } })

function assistant(id: string, time: number, content: unknown[], extra: Record<string, unknown> = {}) {
  return {
    id,
    type: "assistant",
    agent: "build",
    model: MODEL,
    content,
    finish: "stop",
    cost: 0.25,
    tokens: tokens(100, 20, 5, 10, 2),
    time: { created: time, streamed: time + 900, completed: time + 1000 },
    ...extra,
  }
}

const toolPart = (id: string, name: string, time: number, state: Record<string, unknown>, ran = 100, took = 400) => ({
  type: "tool",
  id,
  name,
  state,
  time: { created: time, ran: time + ran, completed: time + ran + took },
})

const completed = (input: Record<string, unknown>, text: string) => ({
  status: "completed",
  input,
  content: [{ type: "text", text }],
})

/** Every message type, in every state, with every optional field a sanitized export rewrites. */
function richTranscript(directory: string) {
  const t = (n: number) => T0 + n * 1000
  return [
    { id: mid("R01"), type: "agent-switched", agent: "plan", previous: "build", time: { created: t(1) } },
    {
      id: mid("R02"),
      type: "model-switched",
      model: { ...MODEL, variant: "low" },
      previous: MODEL,
      time: { created: t(2) },
    },
    {
      id: mid("R03"),
      type: "location-switched",
      location: { directory },
      subpath: "",
      previous: { location: { directory: "/old/place" }, subpath: "" },
      time: { created: t(3) },
    },
    {
      id: mid("R04"),
      type: "user",
      text: "Look at @notes.txt with @explore and $review",
      metadata: { client: "conformance" },
      files: [
        {
          data: Buffer.from("alpha\nbeta\n").toString("base64"),
          mime: "text/plain",
          source: { type: "uri", uri: "file:///project/notes.txt" },
          name: "notes.txt",
          description: "the notes",
          mention: { start: 8, end: 18, text: "@notes.txt" },
        },
        { data: Buffer.from("plain").toString("base64"), mime: "text/plain", source: { type: "inline" } },
      ],
      agents: [{ name: "explore", mention: { start: 24, end: 32, text: "@explore" } }, { name: "general" }],
      skills: [
        { id: "review", name: "review", text: "Review carefully.", mention: { start: 37, end: 44, text: "$review" } },
        { id: "lint", name: "lint" },
      ],
      time: { created: t(4) },
    },
    { id: mid("R05"), type: "user", text: "   ", time: { created: t(5) } },
    {
      id: mid("R06"),
      type: "synthetic",
      text: "Injected context",
      description: "client note",
      metadata: { source: "client" },
      time: { created: t(6) },
    },
    { id: mid("R07"), type: "synthetic", text: "No description", metadata: {}, time: { created: t(7) } },
    { id: mid("R08"), type: "system", text: "Instructions changed", description: "update", time: { created: t(8) } },
    { id: mid("R09"), type: "skill", skill: "review", name: "review", text: "Skill body", time: { created: t(9) } },
    {
      id: mid("R10"),
      type: "shell",
      shellID: "sh_00000000000000conformance1",
      command: "ls -la",
      status: "exited",
      exit: 0,
      output: { output: "total 0\n", cursor: 8, size: 8, truncated: false },
      time: { created: t(10), completed: t(11) },
    },
    {
      id: mid("R11"),
      type: "shell",
      shellID: "sh_00000000000000conformance2",
      command: "true",
      status: "killed",
      time: { created: t(11), completed: t(12) },
    },
    {
      id: mid("R12"),
      type: "shell",
      shellID: "sh_00000000000000conformance3",
      command: "sleep 100",
      status: "running",
      time: { created: t(12) },
    },
    assistant(
      mid("R13"),
      t(13),
      [
        { type: "reasoning", text: "Thinking it over", native: { signature: "abc" }, time: { created: t(13) } },
        { type: "reasoning", text: "" },
        { type: "text", text: "Here is what I found", native: { id: "text-1" } },
        { type: "text", text: "No native" },
        toolPart(
          "call_1",
          "read",
          t(13),
          {
            status: "completed",
            input: { path: "notes.txt" },
            content: [
              { type: "text", text: "alpha" },
              { type: "file", uri: "file:///project/notes.txt", mime: "text/plain", name: "notes.txt" },
              { type: "file", uri: "data:text/plain;base64,YQ==", mime: "text/plain" },
            ],
            metadata: { truncated: false },
          },
          100,
          300,
        ),
        {
          ...toolPart("call_2", "grep", t(13), completed({ pattern: "x" }, `${"match line\n".repeat(130)}end`)),
          native: { item: "n1" },
        },
        toolPart(
          "call_3",
          "edit",
          t(13),
          {
            status: "error",
            input: { path: "notes.txt" },
            error: { type: "tool", message: "oldString not found" },
            content: [{ type: "text", text: "failed" }],
            metadata: { attempts: 1 },
          },
          50,
          150,
        ),
        toolPart("call_4", "glob", t(13), {
          status: "error",
          input: { pattern: "*" },
          error: { type: "tool", message: "denied" },
        }),
        {
          type: "tool",
          id: "call_5",
          name: "shell",
          state: { status: "running", input: { command: "ls" }, metadata: {} },
          time: { created: t(13) },
        },
        {
          type: "tool",
          id: "call_6",
          name: "write",
          state: { status: "streaming", input: '{"path":' },
          time: { created: t(13) },
        },
        {
          type: "tool",
          id: "call_7",
          name: "read",
          state: { status: "streaming", input: "" },
          time: { created: t(13) },
        },
      ],
      { metadata: { step: 1 }, native: { block: "none" }, snapshot: { start: "a", end: "b", files: ["notes.txt"] } },
    ),
    { ...assistant(mid("R14"), t(14), [{ type: "text", text: "unfinished" }]), time: { created: t(14) } },
    {
      id: mid("R15"),
      type: "compaction",
      status: "completed",
      reason: "manual",
      summary: "Summary of the work",
      model: MODEL,
      native: { messages: [] },
      cost: 0.01,
      tokens: tokens(50, 10),
      metadata: { keepID: mid("R13") },
      time: { created: t(15) },
    },
    {
      id: mid("R16"),
      type: "compaction",
      status: "completed",
      reason: "auto",
      summary: "Another summary",
      native: {},
      // The latest summary keeps everything from the location switch on; context rebuilds that text.
      metadata: { keepID: mid("R03") },
      time: { created: t(16) },
    },
    {
      id: mid("R17"),
      type: "compaction",
      status: "failed",
      reason: "auto",
      error: { type: "provider", message: "context too long", status: 400 },
      metadata: { attempt: 2 },
      time: { created: t(17) },
    },
    { id: mid("R18"), type: "compaction", status: "running", reason: "manual", summary: "", time: { created: t(18) } },
    {
      id: mid("R19"),
      type: "idle",
      outcome: "failed",
      error: { type: "provider", message: "boom" },
      time: { created: t(19) },
    },
  ]
}

const messages = (ctx: Ctx, sessionID: string) => ctx.http("GET", `/api/session/${sessionID}/message?order=asc`)

scenario(
  "export a live session and import it elsewhere",
  {
    files: { "notes.txt": "one\ntwo\n", "pkg/readme.md": "# pkg\n" },
    turns: [
      call("read", { path: "notes.txt" }),
      { events: [{ reasoning: "Need to update" }, { tool: "write", input: { path: "notes.txt", content: "one\n" } }] },
      say("Done editing."),
    ],
  },
  async (ctx) => {
    const session = await ctx.session({
      title: "exported",
      metadata: { team: "core" },
      permissions: [{ action: "edit", resource: "*", effect: "allow" }],
    })
    await ctx.http("POST", `/api/session/${session}/synthetic`, {
      text: "Context",
      description: "ctx",
      resume: false,
    })
    await ctx.prompt(session, "Trim the notes")
    const exported = await ctx.http("GET", `/api/experimental/session/${session}/export`)
    await ctx.http("GET", `/api/experimental/session/${session}/export?sanitize=false`)
    await ctx.http("GET", `/api/experimental/session/${session}/export?sanitize=true`)
    // Importing under the same ID conflicts. Under a new session ID the copied message IDs still collide with the
    // original's, which fails as an unexpected server error.
    await ctx.http("POST", "/api/experimental/session/import", { ...exported.body.data })
    await ctx.http("POST", "/api/experimental/session/import", {
      info: { ...exported.body.data.info, id: sid("IMPORTED0") },
      messages: exported.body.data.messages,
    })
    // With fresh message IDs the copy lands at the requested location.
    const copy = sid("IMPORTED1")
    await ctx.http("POST", "/api/experimental/session/import", {
      info: { ...exported.body.data.info, id: copy },
      messages: exported.body.data.messages.map((message: { id: string }) => ({ ...message, id: `${message.id}c` })),
      location: { directory: `${ctx.project}/pkg` },
    })
    await ctx.http("GET", `/api/session/${copy}`)
    await messages(ctx, copy)
    // The imported copy continues like any other session.
    ctx.provider.push(say("Continuing the imported session."))
    await ctx.prompt(copy, "Continue")
    await messages(ctx, copy)
    await ctx.http("GET", `/api/experimental/session/${UNKNOWN}/export`)
    await ctx.http("GET", `/api/experimental/session/${session}/export?sanitize=yes`)
  },
)

scenario("import a transcript with every message type", { files: { "notes.txt": "x\n" } }, async (ctx) => {
  const id = sid("RICH000001")
  await ctx.http("POST", "/api/experimental/session/import", {
    info: info(id, {
      title: "rich",
      agent: "explore",
      model: MODEL,
      metadata: { owner: "conformance" },
      permissions: [{ action: "edit", resource: "*", effect: "ask" }],
      cost: 1.5,
      tokens: tokens(1000, 200, 50, 30, 4),
      outcome: "failed",
      revert: {
        messageID: mid("R13"),
        snapshot: "deadbeef",
        files: [{ file: "notes.txt", patch: "@@ -1 +1 @@", additions: 1, deletions: 1, status: "modified" }],
      },
      time: {
        created: T0,
        updated: T0 + 60_000,
        busy: T0 + 30_000,
        idle: T0 + 20_000,
        viewed: T0 + 40_000,
        archived: T0 + 50_000,
      },
    }),
    messages: richTranscript(ctx.project),
    location: { directory: ctx.project },
  })
  await ctx.http("GET", `/api/session/${id}`)
  await messages(ctx, id)
  await ctx.http("GET", `/api/session/${id}/context`)
  await ctx.http("GET", `/api/experimental/session/${id}/export?sanitize=true`)
  await ctx.http("GET", `/api/session/${id}/message/${mid("R13")}`)
  await booted(ctx)
})

scenario("import without a location and as a child", {}, async (ctx) => {
  const parent = sid("PARENT0001")
  await ctx.http("POST", "/api/experimental/session/import", {
    info: info(parent, { title: "parent", time: { created: T0, updated: T0, idle: T0, busy: T0 - 5 } }),
    messages: [user(mid("P1"), T0, "hello")],
  })
  await ctx.http("POST", "/api/experimental/session/import", {
    info: info(sid("CHILD00001"), { parentID: parent, title: "child" }),
    messages: [],
    location: { directory: ctx.project },
  })
  await ctx.http("GET", `/api/session?parentID=${parent}`)
  await ctx.http("POST", "/api/experimental/session/import", {
    info: info(sid("ORPHAN0001"), { parentID: UNKNOWN }),
    messages: [],
    location: { directory: ctx.project },
  })
  await ctx.http("POST", "/api/experimental/session/import", {
    info: info(sid("BAD0000001")),
    messages: [{ id: mid("X1"), type: "mystery", time: { created: T0 } }],
  })
  await ctx.http("POST", "/api/experimental/session/import", { info: { id: "nope" }, messages: [] })
  await ctx.http("POST", "/api/experimental/session/import", {
    info: info(sid("BAD0000002")),
    messages: "not a list",
  })
})

scenario("import a transcript exported by an earlier release", { files: { "a.txt": "a\n" } }, async (ctx) => {
  const id = sid("LEGACY0001")
  await ctx.http("POST", "/api/experimental/session/import", {
    info: info(id, { title: "legacy" }),
    location: { directory: ctx.project },
    messages: [
      user(mid("L1"), T0, "Old prompt"),
      {
        ...assistant(mid("L2"), T0 + 1000, [
          { type: "text", text: "old text", state: { id: "t1" } },
          { type: "text", text: "both names", state: { old: true }, native: { kept: true } },
          { type: "reasoning", text: "old reasoning", state: { sig: "s" } },
          {
            ...toolPart("call_1", "read", T0 + 1000, completed({ path: "a.txt" }, "a")),
            providerState: { call: "c1" },
            providerResultState: { result: "r1" },
          },
          {
            ...toolPart("call_2", "read", T0 + 1000, completed({ path: "a.txt" }, "a")),
            providerResultState: "not-an-object",
          },
        ]),
        providerState: { block: "old" },
      },
      {
        id: mid("L3"),
        type: "compaction",
        status: "completed",
        reason: "auto",
        summary: "Old summary",
        recent: "kept text",
        providerState: { p: 1 },
        providerContext: {
          messages: [
            {
              role: "user",
              content: [
                { type: "media", mediaType: "image/png", data: "iVBORw0KGgo=" },
                { type: "text", text: "hi" },
              ],
            },
            "not-a-message",
          ],
        },
        time: { created: T0 + 2000 },
      },
      {
        id: mid("L4"),
        type: "compaction",
        status: "completed",
        reason: "auto",
        summary: "Empty recent",
        recent: "",
        providerContext: "not-an-object",
        time: { created: T0 + 3000 },
      },
    ],
  })
  await messages(ctx, id)
  await ctx.http("GET", `/api/experimental/session/${id}/export`)
  // The next turn rebuilds the model context from the upgraded history, including the old compaction's kept text.
  ctx.provider.push(say("Continuing the old conversation."))
  await ctx.prompt(id, "Carry on")
  await ctx.http("GET", `/api/session/${id}/context`)
})

scenario("export leaves out unsettled messages", { files: { "a.txt": "a\n" } }, async (ctx) => {
  let open!: () => void
  const gate = new Promise<void>((resolve) => (open = resolve))
  ctx.provider.push({ events: [{ text: "Still going" }, { until: gate }, { text: "." }] })
  const session = await ctx.session({ title: "unsettled" })
  const from = ctx.events.length
  await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Hold" })
  await ctx.until((event) => event?.type === "session.text.delta" && event?.data?.sessionID === session, from)
  await messages(ctx, session)
  await ctx.http("GET", `/api/experimental/session/${session}/export`)
  open()
  await ctx.settle(session)
  await ctx.http("GET", `/api/experimental/session/${session}/export`)
})

// Stats aggregate the scenario's whole database, so they only see the imported history below: fixed timestamps
// make the activity days, durations and ranges stable.
scenario("session statistics over imported history", { files: { "pkg/x.txt": "x\n" } }, async (ctx) => {
  const root = sid("STATSROOT1")
  const day = (n: number, hour = 0) => T0 + n * DAY + hour * 3_600_000
  const tool = (id: string, at: number, status: "completed" | "error", took: number) =>
    toolPart(
      id,
      status === "completed" ? "read" : "edit",
      at,
      status === "completed"
        ? completed({ path: "x" }, "x")
        : { status: "error", input: { path: "x" }, error: { type: "tool", message: "bad" } },
      100,
      took,
    )
  await ctx.http("POST", "/api/experimental/session/import", {
    info: info(root, { title: "stats root" }),
    location: { directory: ctx.project },
    messages: [
      user(mid("S1"), day(0), "first"),
      assistant(mid("S2"), day(0, 1), [tool("c1", day(0, 1), "completed", 400), tool("c2", day(0, 1), "error", 200)]),
      assistant(mid("S3"), day(1, 23), [tool("c3", day(1, 23), "completed", 600)], {
        model: { providerID: "openrouter", id: MODEL_ID, variant: "low" },
        tokens: tokens(10, 1),
      }),
      user(mid("S4"), day(3), "later"),
      assistant(mid("S5"), day(3, 2), [
        { type: "text", text: "done" },
        {
          type: "tool",
          id: "c4",
          name: "glob",
          state: { status: "running", input: {}, metadata: {} },
          time: { created: day(3, 2) },
        },
      ]),
      { ...assistant(mid("S6"), day(4), []), model: { providerID: "openrouter", id: MODEL_ID } },
    ],
  })
  await ctx.http("POST", "/api/experimental/session/import", {
    info: info(sid("STATSCHILD"), { parentID: root, title: "subagent" }),
    location: { directory: ctx.project },
    messages: [user(mid("C1"), day(1), "sub task"), assistant(mid("C2"), day(1, 2), [{ type: "text", text: "ok" }])],
  })
  const other = await ctx.http("POST", "/api/experimental/session/import", {
    info: info(sid("STATSOTHER"), { title: "other project" }),
    location: { directory: `${ctx.project}/pkg` },
    messages: [user(mid("O1"), day(2), "elsewhere"), assistant(mid("O2"), day(2, 5), [])],
  })
  // A fork copies history from before it existed; stats count it once, under the original.
  await ctx.http("POST", `/api/session/${root}/fork`, {})

  const range = `from=${day(-1)}&to=${day(10)}`
  await ctx.http("GET", `/api/experimental/session/stats?${range}`)
  await ctx.http("GET", `/api/experimental/session/stats?${range}&tools=detail`)
  await ctx.http("GET", `/api/experimental/session/stats?${range}&tools=none`)
  await ctx.http("GET", `/api/experimental/session/stats?${range}&timezone=Pacific/Kiritimati`)
  await ctx.http("GET", `/api/experimental/session/stats?${range}&timezone=America/Los_Angeles&tools=detail`)
  await ctx.http("GET", `/api/experimental/session/stats?${range}&project=${other.body.data.projectID}`)
  await ctx.http("GET", `/api/experimental/session/stats?from=${day(1)}&to=${day(2)}`)
  // Without `from`, the range starts at the oldest prompt or step before `to`.
  await ctx.http("GET", `/api/experimental/session/stats?to=${day(10)}`)
  await ctx.http("GET", `/api/experimental/session/stats?to=${day(-30)}`)
  // A range spanning several 31-day windows.
  await ctx.http("GET", `/api/experimental/session/stats?from=${day(-100)}&to=${day(100)}&tools=detail`)
  await ctx.http("GET", "/api/experimental/session/stats")
  await ctx.http("GET", "/api/experimental/session/stats?timezone=Not/AZone")
  await ctx.http("GET", `/api/experimental/session/stats?from=${day(5)}&to=${day(5)}`)
  await ctx.http("GET", "/api/experimental/session/stats?tools=all")
  await ctx.http("GET", "/api/experimental/session/stats?from=yesterday")
  await booted(ctx)
})

const SUMMARY =
  "## Objective\n- Keep stats honest\n\n## Work State\n### Completed\n- Imported the history\n\n## Next Move\n1. Wait"

// A compaction of imported history happens now, while the imported steps keep their fixed times. Compaction usage
// is read from stored event payloads, which the reference server does not keep (events.persist is off), so the
// totals stay those of the imported steps.
scenario(
  "statistics after compacting imported history",
  { turns: [say(SUMMARY, { prompt: 300, completion: 40 })] },
  async (ctx) => {
    const id = sid("STATSCOMP1")
    await ctx.http("POST", "/api/experimental/session/import", {
      info: info(id, { title: "compacted" }),
      location: { directory: ctx.project },
      messages: [
        user(mid("K1"), T0, "an old question"),
        assistant(mid("K2"), T0 + 1000, [{ type: "text", text: "an old answer" }]),
      ],
    })
    await ctx.http("POST", `/api/session/${id}/compact`, {})
    await ctx.wait(id)
    await ctx.http("GET", `/api/session/${id}/message?order=asc&type=compaction`)
    await ctx.http("GET", `/api/experimental/session/stats?from=${T0 - DAY}&tools=none`)
    await ctx.http(
      "GET",
      `/api/experimental/session/stats?from=${T0 - DAY}&project=${(await ctx.http("GET", `/api/session/${id}`)).body.data.projectID}`,
    )
  },
)
