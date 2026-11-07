// Session CRUD through the public API: create, get, list (filters, paging, cursors), update, view,
// environment, remove, active, project adoption, and their error responses.
import { spawnSync } from "node:child_process"
import { MODEL_ID } from "../lib/model"
import { say } from "../lib/openrouter"
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

// Session lists sort by update time, ties broken by ID. Sessions touched within the same millisecond would
// tie-break on their IDs, so scenarios that order sessions leave a gap between mutations.
const tick = () => new Promise((resolve) => setTimeout(resolve, 5))

const UNKNOWN = "ses_0000000000000000000000unknown"

scenario("create get update and remove a session", {}, async (ctx) => {
  const id = await ctx.session({ title: "first" })
  await ctx.http("GET", `/api/session/${id}`)
  await ctx.http("PATCH", `/api/session/${id}`, { title: "renamed" })
  await ctx.http("GET", `/api/session/${id}`)
  await ctx.http("DELETE", `/api/session/${id}`)
  await ctx.http("GET", `/api/session/${id}`)
})

scenario("create a session with every optional field", {}, async (ctx) => {
  const id = "ses_fffffffffff0conformance01"
  const payload = {
    id,
    title: "configured",
    agent: "plan",
    model: { providerID: "openrouter", id: MODEL_ID, variant: "low" },
    location: { directory: ctx.project },
    metadata: { team: "core", priority: 2, tags: ["a", "b"], nested: { ok: true } },
    permissions: [{ action: "edit", resource: "*", effect: "deny" }],
  }
  await ctx.http("POST", "/api/session", payload)
  // Creating again with the same ID returns the recorded session unchanged.
  await ctx.http("POST", "/api/session", { ...payload, title: "ignored" })
  await ctx.http("GET", `/api/session/${id}`)
})

scenario("create a session without a location uses the server directory", {}, async (ctx) => {
  const created = await ctx.http("POST", "/api/session", {})
  await ctx.http("GET", `/api/session/${created.body.data.id}`)
})

scenario("create a child session inherits from its parent", {}, async (ctx) => {
  const parent = await ctx.session({
    title: "parent",
    metadata: { owner: "parent" },
    permissions: [{ action: "shell", resource: "*", effect: "deny" }],
  })
  const child = await ctx.http("POST", "/api/session", { parentID: parent, title: "child" })
  // A child's own metadata and permissions replace the inherited ones.
  await ctx.http("POST", "/api/session", {
    parentID: parent,
    metadata: { owner: "override" },
    permissions: [],
  })
  await ctx.http("GET", `/api/session/${child.body.data.id}`)
  await ctx.http("POST", "/api/session", { parentID: UNKNOWN, title: "orphan" })
})

scenario("create a session with invalid payloads", {}, async (ctx) => {
  await ctx.http("POST", "/api/session", { id: "not-a-session-id" })
  await ctx.http("POST", "/api/session", { title: 42 })
  await ctx.http("POST", "/api/session", { model: { providerID: "openrouter" } })
  await ctx.http("POST", "/api/session", { location: { directory: "relative/path" } })
  await ctx.http("POST", "/api/session", { permissions: [{ action: "edit", resource: "*", effect: "maybe" }] })
  await ctx.http("POST", "/api/session", { parentID: "msg_notasession000000000" })
  await ctx.http("POST", "/api/session", { agent: "missing-agent", location: { directory: ctx.project } })
})

scenario("list sessions with filters", { files: { "pkg/a.txt": "a\n", "other/b.txt": "b\n" } }, async (ctx) => {
  const alpha = await ctx.session({ title: "alpha one" })
  await tick()
  await ctx.session({ title: "beta two", location: { directory: `${ctx.project}/pkg` } })
  await tick()
  const gamma = await ctx.session({ title: "gamma three" })
  await tick()
  await ctx.http("POST", "/api/session", { parentID: alpha, title: "alpha child" })
  await tick()
  await ctx.http("PATCH", `/api/session/${gamma}`, { archived: true })
  const project = (await ctx.http("GET", `/api/session/${alpha}`)).body.data.projectID

  await ctx.http("GET", "/api/session")
  await ctx.http("GET", "/api/session?order=asc")
  await ctx.http("GET", "/api/session?search=alpha")
  await ctx.http("GET", "/api/session?search=nothing-matches")
  await ctx.http("GET", "/api/session?parentID=null")
  await ctx.http("GET", `/api/session?parentID=${alpha}`)
  await ctx.http("GET", "/api/session?archived=true")
  await ctx.http("GET", "/api/session?archived=false")
  await ctx.http("GET", `/api/session?directory=${encodeURIComponent(`${ctx.project}/pkg`)}`)
  await ctx.http("GET", `/api/session?directory=${encodeURIComponent(ctx.project)}&parentID=null&archived=false`)
  await ctx.http("GET", `/api/session?project=${project}`)
  await ctx.http("GET", `/api/session?project=${project}&subpath=pkg`)
  await ctx.http("GET", `/api/session?project=${project}&subpath=`)
})

// Cursors embed session IDs and update times, so a URL carrying one is not stable across runs. Pages reached by
// following a returned cursor are fetched outside the HTTP transcript and recorded as notes, with the cursor
// named by where it came from.
async function follow(ctx: Ctx, label: string, path: string) {
  const response = await ctx.handler(
    new Request(new URL(path, "http://opencode.local"), {
      headers: { "x-opencode-directory": encodeURIComponent(ctx.project) },
    }),
  )
  const text = await response.text()
  const body = text ? JSON.parse(text) : null
  ctx.note(`GET ${label}`, { status: response.status, body })
  return { status: response.status, body }
}

scenario("list sessions page by page with cursors", {}, async (ctx) => {
  for (const title of ["one", "two", "three", "four", "five"]) {
    await ctx.session({ title })
    await tick()
  }
  const first = await ctx.http("GET", "/api/session?limit=2")
  const second = await follow(
    ctx,
    "limit=2 cursor=<page 1 next>",
    `/api/session?limit=2&cursor=${first.body.cursor.next}`,
  )
  const third = await follow(
    ctx,
    "limit=2 cursor=<page 2 next>",
    `/api/session?limit=2&cursor=${second.body.cursor.next}`,
  )
  await follow(ctx, "limit=2 cursor=<page 3 next>", `/api/session?limit=2&cursor=${third.body.cursor.next}`)
  // Walking back from the third page returns the second page again, then the first.
  const back = await follow(
    ctx,
    "limit=2 cursor=<page 3 previous>",
    `/api/session?limit=2&cursor=${third.body.cursor.previous}`,
  )
  await follow(ctx, "limit=2 cursor=<page 2 previous>", `/api/session?limit=2&cursor=${back.body.cursor.previous}`)
  await follow(ctx, "limit=2 cursor=<page 1 previous>", `/api/session?limit=2&cursor=${first.body.cursor.previous}`)
  // A cursor carries its filters and order: the search and order passed with it are ignored.
  const asc = await ctx.http("GET", "/api/session?limit=2&order=asc&search=o")
  await follow(
    ctx,
    "limit=2 search=five cursor=<asc search=o next>",
    `/api/session?limit=2&search=five&cursor=${asc.body.cursor.next}`,
  )
  await follow(ctx, "limit=2 cursor=<asc search=o previous>", `/api/session?limit=2&cursor=${asc.body.cursor.previous}`)
  // Cursors built by the client: anchors past either end of the list.
  const encode = (value: unknown) => Buffer.from(JSON.stringify(value)).toString("base64url")
  const future = { id: "ses_zzzzzzzzzzzzzzzzzzzzzzzzzz", time: 4_102_444_800_000 }
  const past = { id: "ses_00000000000000000000000000", time: 0 }
  await ctx.http("GET", `/api/session?limit=3&cursor=${encode({ anchor: { ...future, direction: "next" } })}`)
  await ctx.http("GET", `/api/session?limit=3&cursor=${encode({ anchor: { ...future, direction: "previous" } })}`)
  await ctx.http("GET", `/api/session?limit=3&cursor=${encode({ anchor: { ...past, direction: "previous" } })}`)
  await ctx.http(
    "GET",
    `/api/session?cursor=${encode({ order: "asc", search: "t", anchor: { ...past, direction: "next" } })}`,
  )
  await ctx.http(
    "GET",
    `/api/session?cursor=${encode({ parentID: "null", archived: "false", anchor: { ...future, direction: "next" } })}`,
  )
})

scenario("list sessions with invalid queries", {}, async (ctx) => {
  await ctx.session({ title: "only" })
  await ctx.http("GET", "/api/session?cursor=not-a-cursor")
  await ctx.http(
    "GET",
    `/api/session?cursor=${Buffer.from(JSON.stringify({ anchor: { id: "x" } })).toString("base64url")}`,
  )
  await ctx.http("GET", "/api/session?limit=0")
  await ctx.http("GET", "/api/session?limit=abc")
  await ctx.http("GET", "/api/session?order=sideways")
  await ctx.http("GET", "/api/session?archived=maybe")
  await ctx.http("GET", "/api/session?directory=relative")
  await ctx.http("GET", "/api/session?parentID=nope")
})

scenario("update session fields", {}, async (ctx) => {
  const id = await ctx.session({ title: "before" })
  await ctx.http("PATCH", `/api/session/${id}`, { title: "after" })
  await ctx.http("PATCH", `/api/session/${id}`, { metadata: { stage: "review", count: 3 } })
  await ctx.http("PATCH", `/api/session/${id}`, {
    permissions: [{ action: "read", resource: "*.env", effect: "deny" }],
  })
  await ctx.http("PATCH", `/api/session/${id}`, { archived: true })
  await ctx.http("GET", `/api/session/${id}`)
  // Repeating the current archive state is a no-op.
  await ctx.http("PATCH", `/api/session/${id}`, { archived: true })
  await ctx.http("PATCH", `/api/session/${id}`, { archived: false })
  await ctx.http("PATCH", `/api/session/${id}`, { archived: false })
  await ctx.http("PATCH", `/api/session/${id}`, {})
  await ctx.http("PATCH", `/api/session/${id}`, { title: "all at once", metadata: {}, permissions: [], archived: true })
  await ctx.http("GET", `/api/session/${id}`)
  await ctx.http("PATCH", `/api/session/${id}`, { archived: "yes" })
  await ctx.http("PATCH", `/api/session/${UNKNOWN}`, { title: "missing" })
})

scenario("an empty title on a session without messages generates nothing", {}, async (ctx) => {
  const id = await ctx.session({ title: "kept" })
  await ctx.http("PATCH", `/api/session/${id}`, { title: "" })
  await ctx.http("GET", `/api/session/${id}`)
  await booted(ctx)
})

scenario(
  "an empty title regenerates the title from the conversation",
  {
    turns: [say("Sure, here is the plan.")],
    responders: [
      (request) => (request.kind === "title" ? say("Planning the refactor\nsecond line ignored") : undefined),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "manual title" })
    await ctx.prompt(id, "Plan a refactor of the parser")
    await ctx.http("PATCH", `/api/session/${id}`, { title: "" })
    await ctx.http("GET", `/api/session/${id}`)
  },
)

// Importing a session doesn't boot its location, so the title request is the location's first: generating the title
// waits for its plugins (they register the title agent).
scenario(
  "an empty title on a location's first request",
  {
    boot: false,
    responders: [(request) => (request.kind === "title" ? say("Named on a cold location") : undefined)],
  },
  async (ctx) => {
    const id = "ses_00000000000000conformanceCOLDNAME"
    const time = Date.UTC(2026, 0, 5)
    await ctx.http("POST", "/api/experimental/session/import", {
      info: {
        id,
        projectID: "0000000000000000000000000000000000000000",
        cost: 0,
        tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
        time: { created: time, updated: time },
        location: { directory: ctx.project },
      },
      location: { directory: ctx.project },
      messages: [{ id: "msg_00000000000000conformanceCOLD1", type: "user", text: "Name me", time: { created: time } }],
    })
    await ctx.http("PATCH", `/api/session/${id}`, { title: "" })
    await ctx.http("GET", `/api/session/${id}`)
  },
)

scenario("remove a session with its descendants", {}, async (ctx) => {
  const root = await ctx.session({ title: "root" })
  const child = (await ctx.http("POST", "/api/session", { parentID: root, title: "child" })).body.data.id
  const grandchild = (await ctx.http("POST", "/api/session", { parentID: child, title: "grandchild" })).body.data.id
  const sibling = await ctx.session({ title: "sibling" })
  await ctx.http("DELETE", `/api/session/${root}`)
  await ctx.http("GET", `/api/session/${child}`)
  await ctx.http("GET", `/api/session/${grandchild}`)
  await ctx.http("GET", "/api/session")
  await ctx.http("DELETE", `/api/session/${root}`)
  await ctx.http("GET", `/api/session/${sibling}`)
})

scenario("view marks an observed idle transition", { turns: [say("Done.")] }, async (ctx) => {
  const id = await ctx.session({ title: "viewed" })
  // Without any idle transition, viewing is a no-op.
  await ctx.http("POST", `/api/session/${id}/view`, { idle: 1_767_225_600_000 })
  await ctx.prompt(id, "Do it")
  const info = await ctx.http("GET", `/api/session/${id}`)
  const raw = info.body.data.time.idle
  const idle: number = typeof raw === "number" ? raw : Date.parse(raw)
  // An idle time newer than the session's latest transition is ignored; an older one is recorded.
  await ctx.http("POST", `/api/session/${id}/view`, { idle: idle + 1 })
  await ctx.http("POST", `/api/session/${id}/view`, { idle: idle - 1 })
  await ctx.http("POST", `/api/session/${id}/view`, { idle })
  await ctx.http("GET", `/api/session/${id}`)
  // Viewing the same transition again is a no-op.
  await ctx.http("POST", `/api/session/${id}/view`, { idle })
  await ctx.http("POST", `/api/session/${id}/view`, { idle: "yesterday" })
  await ctx.http("POST", `/api/session/${UNKNOWN}/view`, { idle: 1_767_225_600_000 })
})

scenario("environment variables reach session shell commands", {}, async (ctx) => {
  // Load the location first (outside the transcript): shell events are global and would otherwise interleave with
  // the plugins announcing themselves in the background.
  await ctx.handler(
    new Request("http://opencode.local/api/skill", {
      headers: { "x-opencode-directory": encodeURIComponent(ctx.project) },
    }),
  )
  await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === ctx.project)
  const id = await ctx.session({ title: "env" })
  await ctx.http("PUT", `/api/session/${id}/environment`, { variables: { CONFORMANCE_GREETING: "hello-env" } })
  await ctx.http("POST", `/api/session/${id}/shell`, { command: "echo $CONFORMANCE_GREETING" })
  await ctx.http("PUT", `/api/session/${id}/environment`, { variables: {} })
  await ctx.http("POST", `/api/session/${id}/shell`, { command: 'echo "[$CONFORMANCE_GREETING]"' })
  await ctx.http("GET", `/api/session/${id}/message?order=asc`)
  await ctx.http("PUT", `/api/session/${id}/environment`, { variables: { BAD: 1 } })
  await ctx.http("PUT", `/api/session/${UNKNOWN}/environment`, { variables: {} })
})

scenario("active sessions while idle", {}, async (ctx) => {
  await ctx.session({ title: "idle" })
  await ctx.http("GET", "/api/session/active")
})

scenario("session routes reject unknown and malformed session IDs", {}, async (ctx) => {
  const routes: Array<[string, string, unknown?]> = [
    ["GET", `/api/session/${UNKNOWN}`],
    ["DELETE", `/api/session/${UNKNOWN}`],
    ["POST", `/api/session/${UNKNOWN}/fork`, {}],
    ["POST", `/api/session/${UNKNOWN}/agent`, { agent: "plan" }],
    ["POST", `/api/session/${UNKNOWN}/model`, { model: { providerID: "openrouter", id: MODEL_ID } }],
    ["POST", `/api/session/${UNKNOWN}/move`, { directory: ctx.project }],
    ["POST", `/api/session/${UNKNOWN}/prompt`, { text: "hi" }],
    ["POST", `/api/session/${UNKNOWN}/synthetic`, { text: "hi" }],
    ["POST", `/api/session/${UNKNOWN}/shell`, { command: "true" }],
    ["POST", `/api/session/${UNKNOWN}/compact`, {}],
    ["POST", `/api/experimental/session/${UNKNOWN}/wait`],
    ["POST", `/api/session/${UNKNOWN}/revert/stage`, { messageID: "msg_0000000000000000000000unknown" }],
    ["DELETE", `/api/session/${UNKNOWN}/revert`],
    ["POST", `/api/session/${UNKNOWN}/revert/commit`],
    ["GET", `/api/session/${UNKNOWN}/context`],
    ["GET", `/api/session/${UNKNOWN}/diff`],
    ["GET", `/api/session/${UNKNOWN}/inbox`],
    ["DELETE", `/api/session/${UNKNOWN}/inbox/msg_0000000000000000000000unknown`],
    ["PATCH", `/api/session/${UNKNOWN}/inbox/msg_0000000000000000000000unknown`, { delivery: "steer" }],
    ["PATCH", `/api/session/${UNKNOWN}/inbox/msg_0000000000000000000000unknown`, { delivery: "queue" }],
    ["POST", `/api/session/${UNKNOWN}/generate`, { prompt: "hi" }],
    ["GET", `/api/experimental/session/${UNKNOWN}/log`],
    ["POST", `/api/session/${UNKNOWN}/interrupt`],
    ["POST", `/api/session/${UNKNOWN}/background`],
    ["GET", `/api/session/${UNKNOWN}/message`],
    ["GET", `/api/session/${UNKNOWN}/message/msg_0000000000000000000000unknown`],
    ["GET", `/api/session/${UNKNOWN}/form`],
    ["POST", `/api/session/${UNKNOWN}/form`, { title: "t", fields: [{ key: "k", type: "string" }] }],
    ["GET", `/api/session/${UNKNOWN}/form/frm_0000000000000000000unknown`],
    ["POST", `/api/session/${UNKNOWN}/form/frm_0000000000000000000unknown/reply`, { answer: {} }],
    ["DELETE", `/api/session/${UNKNOWN}/form/frm_0000000000000000000unknown`],
    ["PUT", `/api/session/${UNKNOWN}/environment`, { variables: {} }],
    ["POST", `/api/session/${UNKNOWN}/view`, { idle: 1_767_225_600_000 }],
    ["GET", `/api/experimental/session/${UNKNOWN}/export`],
    ["POST", `/api/session/${UNKNOWN}/command`, { name: "init", text: "" }],
    ["POST", `/api/experimental/session/${UNKNOWN}/skill`, { id: "anything" }],
    ["GET", `/api/experimental/session/${UNKNOWN}/instructions/entries`],
    ["PUT", `/api/experimental/session/${UNKNOWN}/instructions/entries/key`, { value: 1 }],
    ["DELETE", `/api/experimental/session/${UNKNOWN}/instructions/entries/key`],
    // Malformed IDs fail validation before any lookup.
    ["GET", "/api/session/not-a-session"],
    ["POST", "/api/session/not-a-session/prompt", { text: "hi" }],
    ["GET", "/api/session/not-a-session/form"],
    ["POST", "/api/session/not-a-session/form", { title: "t", fields: [{ key: "k", type: "string" }] }],
  ]
  for (const [method, url, body] of routes) await ctx.http(method, url, body)
})

// A directory that becomes a git repository gets a new project identity; its existing sessions follow it.
// The location must load for the first time after `git init`, so the harness must not boot it beforehand.
scenario("sessions follow their directory into a new git project", { files: { "a.txt": "a\n" }, boot: false }, async (ctx) => {
  const session = await ctx.session({ title: "before git" })
  const child = (await ctx.http("POST", "/api/session", { parentID: session, title: "child" })).body.data.id
  await ctx.http("GET", `/api/session/${session}`)
  for (const args of [
    ["init", "-q", "-b", "main"],
    ["add", "-A"],
    ["commit", "-q", "-m", "initial"],
  ]) {
    const result = spawnSync("git", args, { cwd: ctx.project, encoding: "utf8" })
    if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${result.stderr}`)
  }
  // Loading the location resolves the directory again.
  // (A revert of an unknown message loads the location, then fails without recording anything.)
  await ctx.http("POST", `/api/session/${session}/revert/stage`, { messageID: "msg_0000000000000000000000unknown" })
  await ctx.until((event) => event?.type === "worktree.resolved")
  await ctx.http("GET", `/api/session/${session}`)
  await ctx.http("GET", `/api/session/${child}`)
  await ctx.http(
    "GET",
    `/api/session?project=${(await ctx.http("GET", `/api/session/${session}`)).body.data.projectID}`,
  )
})
