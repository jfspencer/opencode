// Session control endpoints: agent and model switching (reasoning variants), moving a session between
// directories, the user shell endpoint, text generation from session context, the durable session log, slash
// commands, skills, and API-managed instruction entries.
import { mkdirSync, rmSync } from "node:fs"
import path from "node:path"
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

const MODEL = (variant?: string) => ({ providerID: "openrouter", id: MODEL_ID, ...(variant ? { variant } : {}) })
const messages = (ctx: Ctx, sessionID: string) => ctx.http("GET", `/api/session/${sessionID}/message?order=asc`)

/**
 * Moving boots the destination's location without waiting for its plugins, which then announce themselves in the
 * background. Waiting for the last announcement keeps the recorded global events complete on every run.
 */
const booted = (ctx: Ctx, directory: string) =>
  ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === directory)

scenario(
  "switch the model and its reasoning variant",
  { turns: ["none", "low", "high", "max", "default"].map((variant) => say(`Answered with ${variant}.`)) },
  async (ctx) => {
    const session = await ctx.session({ title: "variants" })
    for (const variant of ["none", "low", "high", "max"]) {
      await ctx.http("POST", `/api/session/${session}/model`, { model: MODEL(variant) })
      await ctx.prompt(session, `Use ${variant}`)
    }
    // Selecting the current model again records nothing; dropping the variant uses the model's default.
    await ctx.http("POST", `/api/session/${session}/model`, { model: MODEL("max") })
    await ctx.http("POST", `/api/session/${session}/model`, { model: MODEL() })
    await ctx.http("POST", `/api/session/${session}/model`, { model: MODEL() })
    await ctx.prompt(session, "Use the default")
    await ctx.http("GET", `/api/session/${session}`)
    await ctx.http("GET", `/api/session/${session}/message?order=asc&type=model-switched`)
    await ctx.http("POST", `/api/session/${session}/model`, { model: { providerID: "openrouter" } })
    await ctx.http("POST", `/api/session/${session}/model`, {})
  },
)

scenario(
  "switch the agent",
  {
    // The plan agent adds a reminder turn of its own when it starts and when it hands back to build.
    turns: [
      say("Planning only."),
      say("Plan reminder noted."),
      say("Building now."),
      say("Build reminder noted."),
      say("Spare."),
    ],
  },
  async (ctx) => {
    const session = await ctx.session({ title: "agents" })
    await ctx.http("POST", `/api/session/${session}/agent`, { agent: "plan" })
    await ctx.prompt(session, "Plan it")
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${session}/agent`, { agent: "build" })
    // Leaving plan mode is announced to the model by a reminder the plan plugin adds on its own.
    await ctx.until(
      (event) =>
        event?.type === "session.inbox.enqueued" &&
        String(event?.data?.item?.payload?.text).includes("NO LONGER in Plan mode"),
      from,
    )
    // Switching to the current agent is still recorded.
    await ctx.http("POST", `/api/session/${session}/agent`, { agent: "build" })
    await ctx.prompt(session, "Build it")
    await ctx.http("GET", `/api/session/${session}`)
    await ctx.http("GET", `/api/session/${session}/message?order=asc&type=agent-switched`)
    await ctx.http("POST", `/api/session/${session}/agent`, {})
  },
)

scenario("a session on an unknown agent fails its turn", { turns: [say("unused")] }, async (ctx) => {
  const session = await ctx.session({ title: "ghost agent", agent: "ghost" })
  await ctx.prompt(session, "Hello")
  await ctx.http("GET", `/api/session/${session}`)
  await messages(ctx, session)
})

scenario("a session on an unknown model fails its turn", { turns: [say("unused")] }, async (ctx) => {
  const session = await ctx.session({ title: "ghost model", model: { providerID: "openrouter", id: "nobody/none" } })
  await ctx.prompt(session, "Hello")
  await ctx.http("GET", `/api/session/${session}`)
  await messages(ctx, session)
  await ctx.http("POST", `/api/session/${session}/generate`, { prompt: "Summarize" })
})

scenario(
  "move a session to another directory",
  {
    files: { "a.txt": "a\n", "sub/b.txt": "b\n", "other/c.txt": "c\n" },
    turns: [say("In the root."), say("Now in sub."), call("read", { path: "c.txt" }), say("Read c.")],
  },
  async (ctx) => {
    const session = await ctx.session({ title: "mover" })
    await ctx.prompt(session, "Where am I?")
    await ctx.http("POST", `/api/session/${session}/move`, { directory: "sub" })
    await ctx.wait(session)
    await ctx.http("GET", `/api/session/${session}`)
    await ctx.prompt(session, "And now?")
    await ctx.http("POST", `/api/session/${session}/move`, { directory: `${ctx.project}/other`, delivery: "queue" })
    await ctx.wait(session)
    await ctx.prompt(session, "Read c.txt")
    await messages(ctx, session)
    await ctx.http("GET", `/api/session/${session}/inbox`)
    await ctx.http("POST", `/api/session/${session}/move`, { directory: "does-not-exist" })
    await ctx.http("POST", `/api/session/${session}/move`, { directory: `${ctx.project}/a.txt` })
    await ctx.http("POST", `/api/session/${session}/move`, {
      directory: `${ctx.project}/../${path.basename(ctx.project)}/sub`,
    })
    await ctx.http("POST", `/api/session/${session}/move`, { directory: "sub", delivery: "later" })
    await ctx.http("POST", `/api/session/${session}/move`, {})
    await ctx.wait(session)
    await ctx.http("GET", `/api/session/${session}`)
  },
)

scenario("move a session to the home directory", { files: { "a.txt": "a\n" } }, async (ctx) => {
  const session = await ctx.session({ title: "home" })
  await ctx.http("POST", `/api/session/${session}/move`, { directory: "~" })
  await ctx.wait(session)
  await booted(ctx, process.env.HOME!)
  await ctx.http("GET", `/api/session/${session}`)
  mkdirSync(path.join(process.env.HOME!, "work"), { recursive: true })
  await ctx.http("POST", `/api/session/${session}/move`, { directory: "~/work" })
  await ctx.wait(session)
  await booted(ctx, path.join(process.env.HOME!, "work"))
  await ctx.http("GET", `/api/session/${session}`)
  await messages(ctx, session)
})

scenario(
  "move a session whose directory is gone",
  { files: { "gone/x.txt": "x\n", "dest/y.txt": "y\n" } },
  async (ctx) => {
    const session = await ctx.session({ title: "orphan", location: { directory: `${ctx.project}/gone` } })
    rmSync(path.join(ctx.project, "gone"), { recursive: true })
    await ctx.http("POST", `/api/session/${session}/move`, { directory: `${ctx.project}/dest` })
    await ctx.wait(session)
    await booted(ctx, `${ctx.project}/dest`)
    await ctx.http("GET", `/api/session/${session}`)
    await messages(ctx, session)
  },
)

scenario(
  "move a busy session at the next step boundary",
  { files: { "a.txt": "a\n", "sub/b.txt": "b\n" } },
  async (ctx) => {
    let open!: () => void
    const gate = new Promise<void>((resolve) => (open = resolve))
    ctx.provider.push(
      { events: [{ tool: "read", input: { path: "a.txt" } }, { until: gate }], finish: "tool_calls" },
      say("Continuing after the move."),
    )
    const session = await ctx.session({ title: "busy mover" })
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Read then continue" })
    await ctx.until((event) => event?.type === "session.tool.input.started" && event?.data?.sessionID === session, from)
    await ctx.http("POST", `/api/session/${session}/move`, { directory: "sub" })
    await ctx.http("GET", `/api/session/${session}/inbox`)
    open()
    await ctx.settle(session)
    await ctx.http("GET", `/api/session/${session}`)
    await messages(ctx, session)
  },
)

scenario("run user shell commands in a session", { files: { "a.txt": "a\n" } }, async (ctx) => {
  // Load the location first (outside the transcript): shell events are global and would otherwise interleave with
  // the plugins announcing themselves in the background.
  await ctx.handler(
    new Request("http://opencode.local/api/skill", {
      headers: { "x-opencode-directory": encodeURIComponent(ctx.project) },
    }),
  )
  await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === ctx.project)
  const session = await ctx.session({ title: "shell" })
  await ctx.http("POST", `/api/session/${session}/shell`, { command: "cat a.txt" })
  await ctx.http("POST", `/api/session/${session}/shell`, {
    id: "msg_00000000000000conformanceSH",
    command: "echo out; echo err >&2; exit 3",
  })
  await ctx.http("POST", `/api/session/${session}/shell`, { command: "definitely-not-a-command-xyz" })
  await ctx.http("POST", `/api/session/${session}/shell`, {})
  await messages(ctx, session)
  await ctx.http("GET", `/api/session/${session}/inbox`)
})

scenario(
  "generate text from session context",
  {
    turns: [
      say("Nothing to go on yet."),
      say("The parser handles JSON."),
      say("A one-line summary."),
      { status: 400, body: { error: { message: "bad request" } } },
    ],
  },
  async (ctx) => {
    const session = await ctx.session({ title: "generate" })
    await ctx.http("POST", `/api/session/${session}/generate`, { prompt: "Say something" })
    await ctx.prompt(session, "Explain the parser")
    await ctx.http("PUT", `/api/experimental/session/${session}/instructions/entries/style`, { value: "terse" })
    await ctx.http("POST", `/api/session/${session}/generate`, { prompt: "Summarize in one line" })
    // Generation does not change the session.
    await messages(ctx, session)
    await ctx.http("POST", `/api/session/${session}/generate`, { prompt: "Fail please" })
    await ctx.http("POST", `/api/session/${session}/generate`, {})
  },
)

/** Read the session log (an SSE stream) outside the transcript and record its parsed frames. */
async function readLog(
  ctx: Ctx,
  sessionID: string,
  query: string,
  until?: (frames: any[]) => boolean,
  act?: () => Promise<void>,
) {
  const response = await ctx.handler(
    new Request(new URL(`/api/experimental/session/${sessionID}/log${query}`, "http://opencode.local"), {
      headers: { "x-opencode-directory": encodeURIComponent(ctx.project), accept: "text/event-stream" },
    }),
  )
  const frames: any[] = []
  if (response.status !== 200) {
    ctx.note(`log ${query}`, { status: response.status, body: await response.json() })
    return frames
  }
  const reader = response.body!.getReader()
  const decoder = new TextDecoder()
  let buffer = ""
  let acted = false
  while (true) {
    const next = await reader.read()
    if (next.done) break
    buffer += decoder.decode(next.value, { stream: true })
    let boundary = buffer.indexOf("\n\n")
    while (boundary !== -1) {
      const frame = buffer.slice(0, boundary)
      buffer = buffer.slice(boundary + 2)
      const data = frame
        .split("\n")
        .filter((line) => line.startsWith("data:"))
        .map((line) => line.slice(5).trimStart())
        .join("\n")
      if (data) frames.push(JSON.parse(data))
      boundary = buffer.indexOf("\n\n")
    }
    if (act && !acted && frames.some((frame) => frame.type === "log.synced")) {
      acted = true
      await act()
    }
    if (until?.(frames)) {
      await reader.cancel()
      break
    }
  }
  ctx.note(`log ${query}`, { status: response.status, frames })
  return frames
}

// The reference server keeps no durable event payloads (events.persist is off), so a log read replays nothing
// and reports only the watermark it reached.
scenario("read the durable session log", { turns: [say("Logged.")] }, async (ctx) => {
  const session = await ctx.session({ title: "log" })
  await ctx.prompt(session, "Write to the log")
  await readLog(ctx, session, "")
  await readLog(ctx, session, "?after=3")
  await readLog(ctx, session, "?after=100000")
  await readLog(ctx, session, "?follow=false")
  await readLog(ctx, session, "?follow=true", (all) => all.some((frame) => frame.type === "log.synced"))
  const empty = await ctx.session({ title: "empty log" })
  await readLog(ctx, empty, "?follow=true", (all) => all.some((frame) => frame.type === "log.synced"))
  await ctx.http("GET", `/api/experimental/session/${session}/log?after=-1`)
  await ctx.http("GET", `/api/experimental/session/${session}/log?follow=maybe`)
})

scenario(
  "run slash commands",
  {
    files: { "README.md": "# Project\n" },
    config: {
      commands: {
        greet: { template: "Say hello to $ARGUMENTS", description: "greets" },
        planner: {
          template: "Plan: $ARGUMENTS",
          agent: "build",
          model: { providerID: "openrouter", model: MODEL_ID, variant: "low" },
        },
      },
    },
    turns: [say("Hello, world."), say("A plan."), say("AGENTS.md drafted.")],
  },
  async (ctx) => {
    const session = await ctx.session({ title: "commands" })
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${session}/command`, { name: "greet", text: "world" })
    await ctx.settle(session, from)
    await ctx.http("POST", `/api/session/${session}/command`, { name: "planner", text: "the release" })
    await ctx.settle(session)
    await ctx.http("POST", `/api/session/${session}/command`, { name: "init", text: "", delivery: "queue" })
    await ctx.settle(session)
    await messages(ctx, session)
    await ctx.http("GET", `/api/session/${session}`)
    await ctx.http("POST", `/api/session/${session}/command`, { name: "no-such-command", text: "" })
    await ctx.http("POST", `/api/session/${session}/command`, { text: "missing name" })
  },
)

const skillFiles = {
  ".opencode/skills/review/SKILL.md":
    "---\nname: review\ndescription: Review code changes carefully\n---\n# Review\n\nCheck every change twice.\n",
  ".opencode/skills/review/checklist.md": "- tests\n- docs\n",
  ".opencode/skills/deploy/SKILL.md": "---\ndescription: Deploy the app\n---\nRun the deploy script.\n",
}

scenario(
  "activate skills and attach them to prompts",
  { files: skillFiles, turns: [say("Reviewing."), say("Using both skills."), say("Deploy noted.")] },
  async (ctx) => {
    const session = await ctx.session({ title: "skills" })
    // Activation without resuming only records the skill.
    await ctx.http("POST", `/api/experimental/session/${session}/skill`, { id: "review", resume: false })
    await ctx.wait(session)
    await ctx.http("POST", `/api/experimental/session/${session}/skill`, { id: "review" })
    await ctx.settle(session)
    await ctx.prompt(session, "Use $review and $deploy", {
      skills: [{ id: "review", mention: { start: 4, end: 11, text: "$review" } }, { id: "deploy" }, { id: "review" }],
    })
    await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Use $missing", skills: [{ id: "missing" }] })
    await ctx.http("POST", `/api/experimental/session/${session}/skill`, { id: "missing" })
    await ctx.http("POST", `/api/experimental/session/${session}/skill`, {})
    await ctx.http("POST", `/api/experimental/session/${session}/skill`, { id: "deploy" })
    await ctx.settle(session)
    await messages(ctx, session)
  },
)

scenario(
  "instruction entries reach the model and announce changes",
  { turns: [say("Noted the team."), say("Noted the changes."), say("Noted the removal.")] },
  async (ctx) => {
    const session = await ctx.session({ title: "entries" })
    const entries = `/api/experimental/session/${session}/instructions/entries`
    await ctx.http("PUT", `${entries}/team`, { value: { lead: "sam", size: 4 } })
    await ctx.http("PUT", `${entries}/style`, { value: "Answer tersely." })
    await ctx.http("GET", entries)
    await ctx.prompt(session, "Who leads?")
    await ctx.http("PUT", `${entries}/team`, { value: { lead: "alex", size: 5 } })
    await ctx.http("PUT", `${entries}/style`, { value: "Answer tersely." })
    await ctx.http("PUT", `${entries}/extra`, { value: ["one", "two"] })
    await ctx.prompt(session, "And now?")
    await ctx.http("DELETE", `${entries}/team`)
    await ctx.http("DELETE", `${entries}/never-set`)
    await ctx.http("GET", entries)
    await ctx.prompt(session, "Anything else?")
    await ctx.http("GET", `/api/session/${session}/context`)
    // A value over the size limit, sent outside the transcript so the recording stays small.
    const huge = await ctx.handler(
      new Request(new URL(`${entries}/huge`, "http://opencode.local"), {
        method: "PUT",
        headers: { "x-opencode-directory": encodeURIComponent(ctx.project), "content-type": "application/json" },
        body: JSON.stringify({ value: "x".repeat(300_000) }),
      }),
    )
    ctx.note("PUT entries/huge with a 300000-character value", { status: huge.status, body: await huge.json() })
    await ctx.http("PUT", `${entries}/bad%20key`, { value: 1 })
    await ctx.http("PUT", `${entries}/novalue`, {})
  },
)

scenario(
  "reading a file loads its directory's instructions once",
  {
    files: {
      "AGENTS.md": "Root rules: be brief.\n",
      "pkg/AGENTS.md": "Package rules: keep exports sorted.\n",
      "pkg/a.ts": "export const a = 1\n",
      "pkg/b.ts": "export const b = 2\n",
    },
    turns: [
      call("read", { path: "pkg/a.ts" }),
      call("read", { path: "pkg/b.ts" }),
      // Sequential reads: results of parallel calls in one step come back in completion order.
      call("read", { path: "pkg" }),
      say("Read the package."),
    ],
  },
  async (ctx) => {
    const session = await ctx.session({ title: "nested instructions" })
    await ctx.prompt(session, "Read the package files")
    await messages(ctx, session)
    await ctx.http("GET", `/api/session/${session}/message?type=synthetic`)
  },
)

scenario("a slash command that fails to run", { files: { "README.md": "# Project\n" } }, async (ctx) => {
  const session = await ctx.session({ title: "failing command" })
  // The command's prompt cannot read its attachment, so the command fails as a whole.
  await ctx.http("POST", `/api/session/${session}/command`, {
    name: "init",
    text: "",
    files: [{ uri: `file://${ctx.project}/missing.md` }],
  })
  await ctx.http("GET", `/api/session/${session}/inbox`)
})

scenario(
  "move to a directory with an invalid configuration",
  { files: { "a.txt": "a\n", "broken/opencode.json": "{ this is not json", "fine/b.txt": "b\n" } },
  async (ctx) => {
    const session = await ctx.session({ title: "mover" })
    await ctx.http("POST", `/api/session/${session}/move`, { directory: "broken" })
    await ctx.wait(session)
    await booted(ctx, `${ctx.project}/broken`)
    await ctx.http("GET", `/api/session/${session}`)
    const second = await ctx.session({ title: "second", location: { directory: `${ctx.project}/broken` } })
    await ctx.http("POST", `/api/session/${second}/move`, { directory: `${ctx.project}/fine` })
    await ctx.wait(second)
    await booted(ctx, `${ctx.project}/fine`)
    await ctx.http("GET", `/api/session/${second}`)
    await messages(ctx, second)
  },
)

// Requests against a session whose directory no longer exists.
scenario("sessions whose directory was deleted", { files: { "gone/x.txt": "x\n" } }, async (ctx) => {
  const session = "ses_00000000000000conformanceGONE01"
  const time = Date.UTC(2026, 0, 5)
  await ctx.http("POST", "/api/experimental/session/import", {
    info: {
      id: session,
      title: "orphaned",
      projectID: "0000000000000000000000000000000000000000",
      cost: 0,
      tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
      time: { created: time, updated: time },
      location: { directory: `${ctx.project}/gone` },
    },
    location: { directory: `${ctx.project}/gone` },
    messages: [{ id: "msg_00000000000000conformanceGON1", type: "user", text: "Old prompt", time: { created: time } }],
  })
  rmSync(path.join(ctx.project, "gone"), { recursive: true })
  await ctx.http("POST", `/api/session/${session}/shell`, { command: "echo hi" })
  await ctx.http("POST", `/api/session/${session}/prompt`, { text: "hello", resume: false })
  await ctx.http("POST", `/api/session/${session}/generate`, { prompt: "summarize" })
  await ctx.http("GET", `/api/session/${session}/diff`)
  await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: "msg_00000000000000conformanceGON1" })
  await ctx.http("GET", `/api/session/${session}/form`)
  await ctx.http("POST", `/api/session/${session}/form`, { title: "t", fields: [{ key: "k", type: "string" }] })
  await ctx.http("POST", `/api/session/${session}/command`, { name: "init", text: "" })
  await ctx.http("POST", `/api/experimental/session/${session}/skill`, { id: "anything" })
  await ctx.http("GET", `/api/session/${session}/message?order=asc`)
  // Forking does not need the directory; the copy keeps the old location.
  const fork = await ctx.http("POST", `/api/session/${session}/fork`, {})
  await ctx.http("GET", `/api/session/${fork.body.data?.id}`)
  await ctx.http("PATCH", `/api/session/${session}`, { title: "" })
  await ctx.http("GET", `/api/session/${session}/inbox`)
})
