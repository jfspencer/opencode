// Prompt attachments (file URIs, line ranges, directories, data URLs, images, agents) and their errors;
// experimental media blobs read back as `media:` refs; session forms; and backgrounding blocking tools.
import { existsSync, rmSync, writeFileSync } from "node:fs"
import path from "node:path"
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

const messages = (ctx: Ctx, sessionID: string, headers?: Record<string, string>) =>
  ctx.http("GET", `/api/session/${sessionID}/message?order=asc`, undefined, headers)

// A 1x1 transparent PNG.
const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="
const files = {
  "notes.txt": "one\ntwo\nthree\nfour\n",
  "docs/guide.md": "# Guide\n",
  "docs/api/ref.md": "# Ref\n",
  "pixel.png": "placeholder",
}
const fileURI = (ctx: Ctx, rel: string) => `file://${path.join(ctx.project, rel)}`

scenario("prompt with file and agent attachments", { files, turns: [say("Read the attachments.")] }, async (ctx) => {
  writeFileSync(path.join(ctx.project, "pixel.png"), Buffer.from(PNG, "base64"))
  const session = await ctx.session({ title: "attachments" })
  await ctx.prompt(session, "Look at @notes.txt, @docs and these, ask @explore", {
    files: [
      {
        uri: fileURI(ctx, "notes.txt"),
        name: "notes",
        description: "the notes file",
        mention: { start: 8, end: 18, text: "@notes.txt" },
      },
      { uri: `${fileURI(ctx, "notes.txt")}?start=2&end=3` },
      { uri: `${fileURI(ctx, "notes.txt")}?start=3` },
      { uri: `${fileURI(ctx, "notes.txt")}?start=zero&end=-1#fragment` },
      { uri: fileURI(ctx, "docs"), mention: { start: 20, end: 25, text: "@docs" } },
      { uri: fileURI(ctx, "pixel.png") },
      { uri: `data:text/plain;base64,${Buffer.from("inline text").toString("base64")}`, name: "inline.txt" },
      { uri: "data:,hello%20world", source: { type: "uri", uri: "clipboard://1" } },
      { uri: `data:image/png;base64,${PNG}` },
    ],
    agents: [{ name: "explore", mention: { start: 42, end: 50, text: "@explore" } }, { name: "general" }],
  })
  await messages(ctx, session)
})

scenario("prompt attachments that cannot be read", { files }, async (ctx) => {
  const session = await ctx.session({ title: "bad attachments" })
  const attempt = (uri: string, extra: Record<string, unknown> = {}) =>
    ctx.http("POST", `/api/session/${session}/prompt`, { text: "Attach", files: [{ uri, ...extra }], resume: false })
  await attempt(fileURI(ctx, "missing.txt"))
  await attempt("file:///dev/null")
  await attempt("file://remote-host/share/file.txt")
  await attempt("not a uri")
  await attempt("https://example.com/file.txt")
  await attempt("data:text/plain;base64")
  await attempt("data:text/plain;base64,YQ")
  await attempt("data:image/png;base64,bm90IGEgcG5n", { name: "broken.png" })
  await attempt("data:image/png;base64,bm90IGEgcG5n")
  await attempt("media:0000000000000000000000000000000000000000000000000000000000000000")
  // Over the 20 MiB limit, checked before reading.
  const big = path.join(ctx.project, "big.bin")
  writeFileSync(big, Buffer.alloc(20 * 1024 * 1024 + 1))
  await attempt(fileURI(ctx, "big.bin"))
  rmSync(big)
  await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Attach", files: [{}] })
  await ctx.http("GET", `/api/session/${session}/inbox`)
})

const MEDIA = { "x-opencode-capabilities": "experimental-media-uri" }

// Forked copies get message IDs `msg_<fork event>_<seq>`, which the shared ID normalization does not recognize; a
// read that returns them is made outside the transcript and recorded as a note with each forked ID rewritten to
// `<forked>_<seq>` (one fork per scenario here), cursors included.
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
  "media blobs are stored once and read back as refs",
  {
    files,
    config: { experimental: { media_blob: true } },
    turns: [say("Saw the image."), say("Saw it again."), say("Fork reply.")],
  },
  async (ctx) => {
    const session = await ctx.session({ title: "media" })
    const image = { uri: `data:image/png;base64,${PNG}`, name: "pixel.png" }
    await ctx.http("POST", `/api/session/${session}/prompt`, {
      text: "Pending with an image",
      files: [image, { uri: `data:text/plain;base64,${Buffer.from("note").toString("base64")}` }],
      resume: false,
    })
    await ctx.http("GET", `/api/session/${session}/inbox`, undefined, MEDIA)
    await ctx.http("GET", `/api/session/${session}/inbox`)
    await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Look", files: [image] })
    await ctx.settle(session)
    const refs = await messages(ctx, session, MEDIA)
    await messages(ctx, session)
    const user = refs.body.data.find((message: any) => message.type === "user" && message.files?.length)
    await ctx.http("GET", `/api/session/${session}/message/${user.id}`, undefined, MEDIA)
    await ctx.http("GET", `/api/session/${session}/message/${user.id}`)
    // A ref read back can be sent again as an attachment.
    const ref = user.files[0].uri
    await ctx.prompt(session, "Same image again", { files: [{ uri: ref, name: "again.png" }] })
    await messages(ctx, session, MEDIA)
    await ctx.http("GET", `/api/experimental/session/${session}/export`)
    // Forks keep the media; cancelled and deleted inputs release it.
    const fork = await ctx.http("POST", `/api/session/${session}/fork`, {})
    await observeForked(ctx, `/api/session/${fork.body.data.id}/message?type=user&limit=1`, MEDIA)
    const pending = await ctx.http("POST", `/api/session/${session}/prompt`, {
      text: "Cancel me",
      files: [{ uri: `data:text/plain;base64,${Buffer.from("cancelled").toString("base64")}` }],
      resume: false,
    })
    await ctx.http("DELETE", `/api/session/${session}/inbox/${pending.body.data.id}`)
    await ctx.http("GET", `/api/session/${session}/inbox`, undefined, MEDIA)
    await ctx.http("DELETE", `/api/session/${fork.body.data.id}`)
    await ctx.http("DELETE", `/api/session/${session}`)
    await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Gone", files: [{ uri: ref }] })
  },
)

scenario(
  "import inline media into blob storage",
  { files, config: { experimental: { media_blob: true } } },
  async (ctx) => {
    const id = "ses_00000000000000conformanceMEDIA1"
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
      messages: [
        {
          id: "msg_00000000000000conformanceMED1",
          type: "user",
          text: "An image",
          files: [{ data: PNG, mime: "image/png", source: { type: "inline" }, name: "pixel.png" }],
          time: { created: time },
        },
        {
          id: "msg_00000000000000conformanceMED2",
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
                input: { path: "pixel.png" },
                content: [
                  { type: "text", text: "Image read" },
                  { type: "file", uri: `data:image/png;base64,${PNG}`, mime: "image/png", name: "pixel.png" },
                ],
              },
              time: { created: time, completed: time + 10 },
            },
          ],
          time: { created: time + 1, completed: time + 20 },
        },
      ],
    })
    await messages(ctx, id, MEDIA)
    await messages(ctx, id)
    await booted(ctx)
  },
)

scenario(
  "the model reads an image into media storage",
  {
    files,
    config: { experimental: { media_blob: true } },
    turns: [call("read", { path: "pixel.png" }), say("It is a pixel.")],
  },
  async (ctx) => {
    writeFileSync(path.join(ctx.project, "pixel.png"), Buffer.from(PNG, "base64"))
    const session = await ctx.session({ title: "tool media" })
    await ctx.prompt(session, "Read the image")
    await messages(ctx, session, MEDIA)
    await messages(ctx, session)
    await ctx.http("GET", `/api/session/${session}/context`)
  },
)

scenario("session forms", {}, async (ctx) => {
  // Load the location first (outside the transcript): form events are global and would otherwise interleave with
  // the plugins announcing themselves in the background.
  await ctx.handler(
    new Request("http://opencode.local/api/skill", {
      headers: { "x-opencode-directory": encodeURIComponent(ctx.project) },
    }),
  )
  await booted(ctx)
  const session = await ctx.session({ title: "forms" })
  const other = await ctx.session({ title: "other" })
  const base = `/api/session/${session}/form`
  await ctx.http("GET", base)
  const created = await ctx.http("POST", base, {
    id: "frm_00000000000000conformance1",
    title: "Deploy settings",
    metadata: { source: "conformance" },
    fields: [
      {
        key: "env",
        type: "string",
        title: "Environment",
        options: [{ value: "prod", label: "Production" }],
        required: true,
      },
      { key: "replicas", type: "integer", minimum: 1, maximum: 5, when: [{ key: "env", op: "eq", value: "prod" }] },
      { key: "confirm", type: "boolean", required: true },
      {
        key: "regions",
        type: "multiselect",
        options: [
          { value: "eu", label: "EU" },
          { value: "us", label: "US" },
        ],
      },
    ],
  })
  await ctx.http("POST", base, { title: "Second", fields: [{ key: "note", type: "string" }] })
  await ctx.http("GET", base)
  const form = `${base}/${created.body.data.id}`
  await ctx.http("GET", form)
  // Invalid answers leave the form pending.
  await ctx.http("POST", `${form}/reply`, { answer: { env: "prod" } })
  await ctx.http("POST", `${form}/reply`, { answer: { env: "prod", confirm: true, unknown: 1 } })
  await ctx.http("POST", `${form}/reply`, { answer: { env: "prod", confirm: true, replicas: 9 } })
  await ctx.http("POST", `${form}/reply`, { answer: { env: "prod", confirm: true, replicas: 3, regions: ["eu"] } })
  await ctx.http("GET", form)
  await ctx.http("POST", `${form}/reply`, { answer: { env: "prod", confirm: true } })
  await ctx.http("DELETE", form)
  // A form is only reachable through the session that owns it.
  await ctx.http("GET", `/api/session/${other}/form/${created.body.data.id}`)
  await ctx.http("POST", `/api/session/${other}/form/${created.body.data.id}/reply`, { answer: {} })
  await ctx.http("DELETE", `/api/session/${other}/form/${created.body.data.id}`)
  // Cancel with a reason, then try to settle it again.
  const second = await ctx.http("GET", base)
  const pending = `${base}/${second.body.data[0].id}`
  await ctx.http("DELETE", `${pending}?message=${encodeURIComponent("Not now")}`)
  await ctx.http("GET", pending)
  await ctx.http("DELETE", pending)
  await ctx.http("POST", `${pending}/reply`, { answer: { note: "late" } })
  await ctx.http("GET", base)
  // Conflicts and invalid definitions.
  await ctx.http("POST", base, {
    id: "frm_00000000000000conformance1",
    title: "Again",
    fields: [{ key: "x", type: "boolean" }],
  })
  await ctx.http("POST", base, {
    title: "Duplicate keys",
    fields: [
      { key: "a", type: "string" },
      { key: "a", type: "number" },
    ],
  })
  await ctx.http("POST", base, {
    title: "Forward reference",
    fields: [
      { key: "a", type: "string", when: [{ key: "b", op: "eq", value: "x" }] },
      { key: "b", type: "string" },
    ],
  })
  await ctx.http("POST", base, { title: "No fields", fields: [] })
  await ctx.http("GET", `${base}/frm_00000000000000000000missing`)
  await ctx.http("POST", `${base}/frm_00000000000000000000missing/reply`, { answer: {} })
  await ctx.http("DELETE", `${base}/frm_00000000000000000000missing`)
  await ctx.http("GET", `${base}/not-a-form`)
  await booted(ctx)
})

scenario("forms of a session whose location is not loaded", { files: { "idle/x.txt": "x\n" } }, async (ctx) => {
  const session = await ctx.session({ title: "cold", location: { directory: `${ctx.project}/idle` } })
  await ctx.http("GET", `/api/session/${session}/form`)
  await ctx.http("GET", "/api/session/global/form")
  await booted(ctx)
})

const allowShell = [{ action: "shell", resource: "*", effect: "allow" }]
// The command signals that it started, then waits for the scenario to let it finish.
const waitingCommand = "touch started.flag; while [ ! -f done.flag ]; do sleep 0.05; done; echo finished"

/** Let the waiting command finish, then wait for its completion notice to be handled by a new turn. */
async function finishCommand(ctx: Ctx, sessionID: string) {
  const from = ctx.events.length
  ctx.write("done.flag", "")
  const notice = await ctx.until(
    (event) =>
      event?.type === "session.inbox.enqueued" &&
      event?.data?.sessionID === sessionID &&
      event?.data?.item?.type === "synthetic",
    from,
  )
  await ctx.until(
    (event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === sessionID,
    ctx.events.indexOf(notice),
  )
  await ctx.wait(sessionID)
}

async function untilFile(ctx: Ctx, rel: string) {
  const deadline = Date.now() + 20_000
  while (!existsSync(path.join(ctx.project, rel))) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${rel}`)
    await new Promise((resolve) => setTimeout(resolve, 20))
  }
}

scenario(
  "background a blocking shell tool",
  {
    files: { "a.txt": "a\n" },
    turns: [
      call("shell", { command: waitingCommand, description: "Wait for the flag" }),
      say("Moved on while it runs."),
      say("Noted the move to the background."),
      say("The command finished."),
    ],
  },
  async (ctx) => {
    const session = await ctx.session({ title: "background", permissions: allowShell })
    // Idle sessions have nothing to background.
    await ctx.http("POST", `/api/session/${session}/background`)
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${session}/prompt`, { text: "Run the waiting command" })
    await ctx.until((event) => event?.type === "session.tool.progress" && event?.data?.sessionID === session, from)
    await untilFile(ctx, "started.flag")
    // Give the tool a moment to register as blocking the session.
    await new Promise((resolve) => setTimeout(resolve, 200))
    await ctx.http("POST", `/api/session/${session}/background`)
    await ctx.wait(session)
    await ctx.http("GET", `/api/experimental/job?sessionID=${session}`)
    await ctx.http("GET", `/api/experimental/job?sessionID=${session}&kind=shell&status=running`)
    await ctx.http("GET", "/api/experimental/job?descendants=true")
    await finishCommand(ctx, session)
    await messages(ctx, session)
    await ctx.http("GET", `/api/experimental/job?sessionID=${session}`)
    rmSync(path.join(ctx.project, "started.flag"))
    rmSync(path.join(ctx.project, "done.flag"))
  },
)

scenario(
  "background jobs of a session and its subagents",
  {
    files: { "a.txt": "a\n" },
    turns: [
      call("shell", { command: waitingCommand, description: "Wait in the background", background: true }),
      say("Started it in the background."),
      say("The background command finished."),
    ],
  },
  async (ctx) => {
    const session = await ctx.session({ title: "jobs", permissions: allowShell })
    const child = (await ctx.http("POST", "/api/session", { parentID: session, title: "child" })).body.data.id
    await ctx.prompt(session, "Start the waiting command in the background")
    await untilFile(ctx, "started.flag")
    await ctx.http("GET", `/api/experimental/job?sessionID=${session}`)
    await ctx.http("GET", `/api/experimental/job?sessionID=${child}&descendants=true`)
    await ctx.http("GET", `/api/experimental/job?sessionID=${session}&descendants=true&kind=subagent`)
    await ctx.http("GET", "/api/experimental/job?sessionID=ses_0000000000000000000000unknown")
    await finishCommand(ctx, session)
    await ctx.http("GET", `/api/experimental/job?sessionID=${session}`)
    await messages(ctx, session)
    rmSync(path.join(ctx.project, "started.flag"))
    rmSync(path.join(ctx.project, "done.flag"))
  },
)

scenario("an inline attachment over the size limit", {}, async (ctx) => {
  const session = await ctx.session({ title: "huge inline" })
  // Sent outside the transcript so the recording stays small: 20 MiB + 1 byte of base64 data.
  const data = Buffer.alloc(20 * 1024 * 1024 + 1).toString("base64")
  const response = await ctx.handler(
    new Request(new URL(`/api/session/${session}/prompt`, "http://opencode.local"), {
      method: "POST",
      headers: { "x-opencode-directory": encodeURIComponent(ctx.project), "content-type": "application/json" },
      body: JSON.stringify({ text: "Attach", files: [{ uri: `data:application/octet-stream;base64,${data}` }] }),
    }),
  )
  ctx.note("POST prompt with a 20 MiB + 1 byte inline attachment", {
    status: response.status,
    body: await response.json(),
  })
  await ctx.http("GET", `/api/session/${session}/inbox`)
})

scenario("attachments the model cannot read", { turns: [say("I can only see the text.")] }, async (ctx) => {
  const session = await ctx.session({ title: "unsupported media" })
  const pdf = Buffer.from("%PDF-1.4\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n").toString("base64")
  // MP4-family containers: the major brand tells audio (M4A) from video. Unrecognized binary data is left out.
  const box = (brand: string) =>
    Buffer.concat([Buffer.from([0, 0, 0, 20]), Buffer.from(`ftyp${brand}`), Buffer.alloc(4), Buffer.from("isom")])
  const unknown = Buffer.concat([Buffer.from("ID3"), Buffer.alloc(16)]).toString("base64")
  await ctx.prompt(session, "Read these", {
    files: [
      { uri: `data:application/pdf;base64,${pdf}`, name: "report.pdf" },
      { uri: `data:audio/mp4;base64,${box("M4A ").toString("base64")}`, name: "voice.m4a" },
      { uri: `data:video/mp4;base64,${box("isom").toString("base64")}` },
      { uri: `data:audio/mpeg;base64,${unknown}` },
    ],
  })
  await messages(ctx, session)
})
