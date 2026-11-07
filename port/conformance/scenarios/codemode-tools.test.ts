// Code mode's host boundary: the opencode namespace tools (models, sessions, worktrees), tool references and
// errors, the `search` global, `fetch`, and the progress rows of an `execute` call. Every scenario runs on the
// default interpreter and again on the bytecode VM (`experimental.codemode_vm`, suffix " (vm)").
//
// An execute call's progress updates are throttled to one per 100 ms of wall-clock time, so which intermediate
// `session.tool.progress` events appear depends on how long each tool call takes. Each scenario therefore leaves
// those events out of its recording (`settled`) and records the final rows through the session's messages.
import { mkdirSync, writeFileSync } from "node:fs"
import path from "node:path"
import { baseConfig, MODEL_ID } from "../lib/model"
import { call, say, type ProviderRequest, type Responder, type Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const engines = [
  { suffix: "", config: {} },
  // The bytecode VM is an experimental flag, off in release builds: only part of the standard when asked for.
  ...(process.env.CONFORMANCE_EXPERIMENTAL ? [{ suffix: " (vm)", config: { experimental: { codemode_vm: true } } }] : []),
]

const run = (code: string) => call("execute", { code })
const programs = (...codes: string[]): Turn[] => [...codes.map(run), say("Done.")]

/** Records the session's messages, then drops the execute calls' throttled progress events from the recording. */
async function settled(ctx: Ctx, id: string) {
  await ctx.http("GET", `/api/session/${id}/message`)
  const events = ctx.events as unknown[]
  for (let index = events.length - 1; index >= 0; index--) {
    const event = events[index] as { type?: string; data?: { metadata?: { toolCalls?: unknown } } }
    if (event?.type === "session.tool.progress" && event.data?.metadata?.toolCalls !== undefined) events.splice(index, 1)
  }
}

/**
 * Resolves once the location at `directory` has announced its services (its last boot event). Moving a session
 * boots its destination's location in the background; holding the next model turn until then keeps the boot
 * events of two locations from interleaving.
 */
function booted(ctx: Ctx, directory: string) {
  return ctx.until(
    (event) => event?.type === "plugin.updated" && event?.location?.directory === directory,
    0,
    30_000,
  )
}

/** A model turn that runs `code`, held until `ready` settles. */
const held = (ready: Promise<unknown>, code: string): Turn => ({
  events: [{ until: ready }, { tool: "execute", input: { code } }],
})

function gate() {
  let open!: () => void
  const promise = new Promise<void>((resolve) => (open = resolve))
  return { promise, open }
}

/** Prompt, then answer each permission request with the next decision until the session is idle. */
async function drive(ctx: Ctx, id: string, text: string, decisions: (string | Record<string, string>)[] = []) {
  let from = ctx.events.length
  await ctx.http("POST", `/api/session/${id}/prompt`, { text })
  for (let i = 0; i < 12; i++) {
    const state = await ctx.settle(id, from)
    if (state !== "permission.asked") break
    const pending = await ctx.http("GET", `/api/session/${id}/permission`)
    from = ctx.events.length
    const next = decisions.shift() ?? "once"
    await ctx.http(
      "POST",
      `/api/session/${id}/permission/${pending.body.data[0].id}/reply`,
      typeof next === "string" ? { decision: next } : next,
    )
  }
}

// Extra catalog entries for the models tool: an older model of the same family, another family, and an image model.
const catalog = {
  providers: {
    openrouter: {
      models: {
        [MODEL_ID]: { releaseDate: "2026-10-17" },
        "deepseek/deepseek-v4-flash": {
          name: "DeepSeek V4 Flash",
          family: "deepseek-flash",
          releaseDate: "2026-04-01",
          capabilities: { tools: true, input: ["text"], output: ["text"] },
          limit: { context: 65536, output: 8192 },
        },
        "qwen/qwen-coder": {
          name: "Qwen Coder",
          family: "qwen-coder",
          releaseDate: "2026-02",
          capabilities: { tools: true, input: ["text"], output: ["text"] },
          limit: { context: 65536, output: 8192 },
        },
        "black-forest/flux-pro": {
          name: "Flux Pro",
          family: "flux",
          capabilities: { input: ["text"], output: ["image"] },
          limit: { context: 4096, output: 4096 },
        },
      },
    },
  },
}

for (const engine of engines) {
  scenario(
    `codemode models tool${engine.suffix}`,
    {
      config: { ...engine.config, ...catalog },
      turns: programs(
        `const all = await tools.opencode.models({})
return { all, everything: await tools.opencode.models({ all: true }) }`,
        `return {
  own: await tools.opencode.models({ provider: "openrouter", query: "flash" }),
  byName: await tools.opencode.models({ provider: "OpenRouter", query: "QWEN coder" }),
  otherProvider: await tools.opencode.models({ provider: "anthropic" }),
  none: await tools.opencode.models({ query: "no-such-model" }),
  images: await tools.opencode.models({ type: "image" }),
  page1: await tools.opencode.models({ limit: 1, all: true }),
  page2: await tools.opencode.models({ limit: 1, offset: 1, all: true }),
  past: await tools.opencode.models({ offset: 10 }),
}`,
        `const errors = []
for (const input of [{ limit: 0 }, { limit: 101 }, { offset: -1 }, { type: "video" }, { query: 5 }, { extra: true }, "flash", null]) {
  try { await tools.opencode.models(input) } catch (error) { errors.push(String(error.message).split("\\n").slice(0, 3).join(" | ")) }
}
return errors`,
      ),
    },
    async (ctx) => {
      const id = await ctx.session()
      await ctx.prompt(id, "Which models can I use?")
      await settled(ctx, id)
    },
  )

  scenario(
    `codemode session tools${engine.suffix}`,
    { config: engine.config, files: { "sub/dir/file.txt": "x\n", "README.md": "# r\n" } },
    async (ctx) => {
      const other = await ctx.session({ title: "Other session" })
      const id = await ctx.session()
      ctx.provider.push(
        ...programs(
          `const renamed = await tools.opencode.session_rename({ title: "  Code mode rename  " })
const other = await tools.opencode.session_rename({ sessionID: "${other}", title: "Renamed from code" })
return { renamed, other }`,
          `const results = {}
for (const [label, input] of Object.entries({
  blank: { title: "   " },
  empty: { title: "" },
  missing: { sessionID: "ses_doesnotexist", title: "x" },
  badID: { sessionID: "not-a-session", title: "x" },
})) {
  try { results[label] = await tools.opencode.session_rename(input) } catch (error) { results[label] = error.name + ": " + error.message }
}
return results`,
          `const results = {}
for (const [label, input] of Object.entries({
  other: { sessionID: "${other}", directory: "sub" },
  missingDir: { directory: "does/not/exist" },
  file: { directory: "README.md" },
  missingSession: { sessionID: "ses_doesnotexist", directory: "sub" },
  empty: { directory: "" },
}) ) {
  try { results[label] = await tools.opencode.session_move(input) } catch (error) { results[label] = error.name + ": " + error.message }
}
return results`,
        ).slice(0, -1),
        held(booted(ctx, path.join(ctx.project, "sub")), `return await tools.opencode.session_move({ directory: "sub/dir" })`),
        held(booted(ctx, path.join(ctx.project, "sub/dir")), `return "after move"`),
        say("Done."),
      )
      await ctx.prompt(id, "Organize the sessions")
      await ctx.http("GET", `/api/session/${id}`)
      await ctx.http("GET", `/api/session/${other}`)
      await settled(ctx, id)
    },
  )

  scenario(
    `codemode session move outside the project${engine.suffix}`,
    { config: engine.config, files: { "README.md": "# r\n" } },
    async (ctx) => {
      const id = await ctx.session()
      ctx.provider.push(
        run(`try { return await tools.opencode.session_move({ directory: "../moved-out" }) } catch (error) { return error.message }`),
        held(
          booted(ctx, path.join(path.dirname(ctx.project), "moved-out")),
          `try { return await tools.opencode.session_move({ directory: "~" }) } catch (error) { return error.message }`,
        ),
        { events: [{ until: booted(ctx, process.env.HOME!) }, { text: "Done." }] },
      )
      mkdirSync(path.join(path.dirname(ctx.project), "moved-out"), { recursive: true })
      await drive(ctx, id, "Move this session out", ["reject", { decision: "reject", message: "Stay in the project." }])
      await ctx.http("GET", `/api/session/${id}`)
      await settled(ctx, id)
    },
  )

  scenario(
    `codemode session move outside the project asks when external directories ask${engine.suffix}`,
    {
      config: {
        ...engine.config,
        permissions: [...baseConfig.permissions, { action: "external_directory", resource: "*", effect: "ask" }],
      },
      files: { "README.md": "# r\n" },
    },
    async (ctx) => {
      const id = await ctx.session()
      ctx.provider.push(
        ...programs(
          `try { return await tools.opencode.session_move({ directory: "../moved-out" }) } catch (error) { return error.message }`,
          `try { return await tools.opencode.session_move({ directory: "~" }) } catch (error) { return error.message }`,
        ),
      )
      mkdirSync(path.join(path.dirname(ctx.project), "moved-out"), { recursive: true })
      await drive(ctx, id, "Move this session out", [
        { decision: "reject", message: "Stay in the project." },
        { decision: "reject", message: "Not home either." },
      ])
      await ctx.http("GET", `/api/session/${id}`)
      await settled(ctx, id)
    },
  )

  scenario(
    `codemode worktree tools${engine.suffix}`,
    // Worktrees live under the global data directory, keyed by the project (its root commit): a distinct README per
    // engine keeps the two runs from sharing worktree names.
    { config: engine.config, git: true, files: { "README.md": `# repo${engine.suffix}\n`, "src/a.ts": "export const a = 1\n" } },
    async (ctx) => {
      const id = await ctx.session()
      const dirty = gate()
      ctx.provider.push(
        ...programs(
          `const before = await tools.opencode.worktree_list()
const created = await tools.opencode.worktree_create({ name: "feature-x" })
const after = await tools.opencode.worktree_list()
let duplicate
try { duplicate = await tools.opencode.worktree_create({ name: "feature-x" }) } catch (error) { duplicate = error.message }
let blank
try { blank = await tools.opencode.worktree_create({ name: "   " }) } catch (error) { blank = error.message }
return { before, created, after, duplicate, blank }`,
          `const list = await tools.opencode.worktree_list()
const target = list.worktrees.find((w) => w.directory.includes("feature-x"))
const results = {}
for (const [label, input] of Object.entries({
  project: { directory: "${ctx.project}" },
  missing: { directory: "${ctx.project}-missing" },
  file: { directory: "${ctx.project}/README.md" },
  relative: { directory: "src" },
})) {
  try { results[label] = await tools.opencode.worktree_remove(input) } catch (error) { results[label] = error.message }
}
results.removed = await tools.opencode.worktree_remove({ directory: target.directory })
results.after = await tools.opencode.worktree_list()
return results`,
          `return await tools.opencode.worktree_create({ name: "dirty" })`,
        ).slice(0, -1),
        // Held until the scenario has left an untracked file in the new worktree.
        { events: [{ until: dirty.promise }, { tool: "execute", input: { code: `const created = (await tools.opencode.worktree_list()).worktrees.find((w) => w.directory.includes("dirty"))
const results = {}
try { results.dirty = await tools.opencode.worktree_remove({ directory: created.directory }) } catch (error) { results.dirty = error.message }
results.forced = await tools.opencode.worktree_remove({ directory: created.directory, force: true })
results.after = await tools.opencode.worktree_list()
return results` } }] },
        say("Done."),
      )
      const from = ctx.events.length
      await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Manage worktrees" })
      const text = (event: any) => String(event?.data?.content?.[0]?.text ?? "")
      const created = await ctx.until(
        (event) =>
          event?.type === "session.tool.success" && event?.data?.sessionID === id && /"directory": ".*dirty/.test(text(event)),
        from,
        30_000,
      )
      const directory = text(created).match(/"directory": "(.*dirty[^"]*)"/)?.[1]
      if (directory) writeFileSync(path.join(directory, "untracked.txt"), "work in progress\n")
      ctx.note("dirtied", directory !== undefined)
      dirty.open()
      await ctx.settle(id, from)
      await settled(ctx, id)
    },
  )

  scenario(
    `codemode worktree tools outside a repository${engine.suffix}`,
    { config: engine.config, files: { "README.md": "# not a repo\n" } },
    async (ctx) => {
      const id = await ctx.session()
      ctx.provider.push(
        ...programs(
          `const results = {}
try { results.list = await tools.opencode.worktree_list() } catch (error) { results.list = error.message }
try { results.create = await tools.opencode.worktree_create({ name: "nope" }) } catch (error) { results.create = error.message }
try { results.remove = await tools.opencode.worktree_remove({ directory: "${ctx.project}" }) } catch (error) { results.remove = error.message }
return results`,
        ),
      )
      await ctx.prompt(id, "Try worktrees without git")
      await settled(ctx, id)
    },
  )

  scenario(
    `codemode tool references and failures${engine.suffix}`,
    { config: engine.config },
    async (ctx) => {
      const id = await ctx.session()
      ctx.provider.push(
        ...programs(
          `const out = {}
const t = (label, fn) => { try { out[label] = fn() } catch (error) { out[label] = error.name + ": " + error.message } }
const ns = tools.opencode
t("roots", () => Object.keys(tools))
t("keys", () => Object.keys(ns))
t("forIn", () => { const keys = []; for (const key in ns) keys.push(key); return keys })
t("types", () => [typeof tools, typeof ns, typeof ns.models, typeof tools.opencode.session_rename])
t("string", () => String(ns.models))
t("stringNamespace", () => String(ns))
t("json", () => JSON.stringify({ t: ns.models, n: ns }))
t("template", () => \`\${ns}\`)
t("identity", () => [ns.models === tools.opencode.models, ns === tools.opencode, ns.models == ns.models, ns.models !== ns.session_rename])
t("in", () => "models" in ns)
t("concat", () => ns.models + "")
t("optional", () => typeof tools.opencode?.models)
t("missing", () => typeof tools.opencode.nope)
t("entries", () => Object.entries(ns).length)
t("spread", () => Object.keys({ ...ns }))
out.bracket = (await tools["opencode"]["models"]({ limit: 1 })).total
return out`,
          `const attempts = {
  unknownNamespace: () => tools.nope.thing({}),
  unknownTool: () => tools.opencode.modelz({}),
  typo: () => tools.opencode.session_renam({ title: "x" }),
  nonCodemode: () => tools.read({ path: "README.md" }),
  callNamespace: () => tools.opencode({}),
  callRoot: () => tools({}),
  asCallback: () => [1].map(tools.opencode.models),
  newTool: () => new tools.opencode.models({}),
  assign: () => { tools.opencode.models = 1 },
  deleteTool: () => delete tools.opencode.models,
  spreadArgs: () => tools.opencode.models(...[{ limit: 1 }, { extra: 1 }]),
}
const out = {}
for (const [label, attempt] of Object.entries(attempts)) {
  try {
    const value = attempt()
    out[label] = value && typeof value.then === "function" ? await value : value
  } catch (error) {
    out[label] = error.name + ": " + error.message
  }
}
return out`,
          `const input = { title: "Boundary", skipped: undefined, fn() {}, nested: { when: new Date(0), set: new Set([1, 2]), map: new Map([["a", 1]]) } }
const results = {}
try { results.extraKeys = await tools.opencode.session_rename(input) } catch (error) { results.extraKeys = error.message }
try {
  const cyclic = { title: "cyclic" }
  cyclic.self = cyclic
  results.cyclic = await tools.opencode.session_rename(cyclic)
} catch (error) { results.cyclic = error.name + ": " + error.message }
try { results.bytes = await tools.opencode.session_rename({ title: new Uint8Array([1, 2]) }) } catch (error) { results.bytes = error.name + ": " + error.message }
try { results.promise = await tools.opencode.session_rename({ title: Promise.resolve("p") }) } catch (error) { results.promise = error.name + ": " + error.message }
try { results.undefinedArg = await tools.opencode.models(undefined) } catch (error) { results.undefinedArg = error.name + ": " + error.message }
try { results.noArg = (await tools.opencode.worktree_list()).worktrees } catch (error) { results.noArg = error.name + ": " + error.message }
return results`,
          `const settled = await Promise.allSettled([
  tools.opencode.models({ limit: 1 }),
  tools.opencode.session_rename({ sessionID: "ses_doesnotexist", title: "x" }),
  tools.opencode.models({ limit: 0 }),
])
const raced = await Promise.race([tools.opencode.models({ limit: 1 }), tools.opencode.worktree_list()])
const chained = await tools.opencode.models({ limit: 1 }).then((r) => r.total).finally(() => {})
const caught = await tools.opencode.session_rename({ sessionID: "ses_doesnotexist", title: "x" }).catch((e) => "caught: " + e.message)
tools.opencode.session_rename({ sessionID: "ses_doesnotexist", title: "unawaited" })
tools.opencode.models({})
return { settled: settled.map((s) => s.status + ":" + (s.reason ? s.reason.message : typeof s.value)), raced: typeof raced, chained, caught }`,
          `await tools.opencode.session_rename({ sessionID: "ses_doesnotexist", title: "uncaught" })`,
          `const pending = tools.opencode.session_rename({ sessionID: "ses_doesnotexist", title: "late" })
await tools.opencode.models({ limit: 1 })
return "returned before observing the failure"`,
        ),
      )
      await ctx.prompt(id, "Exercise the tool boundary")
      await ctx.http("GET", `/api/session/${id}/message`)
      await settled(ctx, id)
    },
  )

  scenario(
    `codemode search${engine.suffix}`,
    { config: engine.config },
    async (ctx) => {
      const id = await ctx.session()
      ctx.provider.push(
        ...programs(
          `return {
  worktree: search({ query: "worktree" }),
  plural: search({ query: "sessions" }),
  limited: search({ query: "session", limit: 1 }),
  offset: search({ query: "session", limit: 1, offset: 1 }),
  exact: search({ query: "opencode.models" }),
  exactExpression: search({ query: "tools.opencode.session_move" }),
  namespace: search({ namespace: "opencode", query: "remove" }),
  toolsNamespace: search({ namespace: "tools.opencode", limit: 2 }),
  everything: search({}).items.length,
  nothing: search({ query: "zzzz-not-a-tool" }),
  noArgument: search(),
}`,
          `const awaited = await search({ query: "rename" })
const chained = await search({ query: "rename" }).then((r) => r.items.map((i) => i.path))
const errors = []
for (const input of [{ limit: 0 }, { offset: -1 }, { query: 1 }, 42, "worktree"]) {
  try { search(input) } catch (error) { errors.push(error.name + ": " + error.message.split("\\n")[0]) }
}
const search2 = search
const shadow = (() => { const search = () => "shadowed"; return search("x") })()
const viaTools = (await tools.search({ query: "worktree", limit: 1 })).items.map((i) => i.path)
return { awaited: awaited.items.length, chained, errors, alias: search2({ query: "models" }).items[0].path, shadow, viaTools }`,
        ),
      )
      await ctx.prompt(id, "Search for tools")
      await settled(ctx, id)
    },
  )
}

// Code-mode `fetch` calls the process-wide fetch rather than the server's configured one, so route it to the fake
// for the duration of a scenario (no network).
async function withFetch(ctx: Ctx, body: () => Promise<void>) {
  const original = globalThis.fetch
  globalThis.fetch = ctx.provider.fetch as typeof fetch
  try {
    await body()
  } finally {
    globalThis.fetch = original
  }
}

function site(origin: string, routes: Record<string, (request: ProviderRequest) => Turn>): Responder {
  return (request) => {
    if (request.kind !== "other" || !request.url.startsWith(origin)) return
    const route = routes[new URL(request.url).pathname]
    if (route === undefined) return { raw: "not found", status: 404, headers: { "content-type": "text/plain" } }
    return route(request)
  }
}

const echo = (request: ProviderRequest): Turn => ({
  status: 200,
  body: { method: request.method, url: request.url, headers: request.headers, body: request.body ?? null },
})
const redirect = (location: string, status = 302) => (): Turn => ({ raw: "", status, headers: { location } })

const api = site("https://api.example.com", {
  "/echo": echo,
  "/text": () => ({ raw: "hello text", status: 200, headers: { "content-type": "text/plain", "x-custom": "yes" } }),
  "/bytes": () => ({ raw: "AB", status: 200, headers: { "content-type": "application/octet-stream" } }),
  "/created": () => ({ status: 201, body: { id: 7 } }),
  "/missing": () => ({ status: 404, body: { error: "missing" } }),
  "/bad-json": () => ({ raw: "{not json", status: 200, headers: { "content-type": "application/json" } }),
  "/r301": redirect("/echo", 301),
  "/r302": redirect("https://api.example.com/echo", 302),
  "/r303": redirect("/echo", 303),
  "/r307": redirect("/echo", 307),
  "/r308": redirect("/echo", 308),
  "/cross": redirect("https://other.example.com/echo", 307),
  "/to-ftp": redirect("ftp://api.example.com/file"),
  "/to-creds": redirect("https://user:pw@api.example.com/echo"),
  "/no-location": () => ({ raw: "moved", status: 302, headers: {} }),
  ...Object.fromEntries(Array.from({ length: 22 }, (_, i) => [`/loop/${i}`, redirect(`/loop/${i + 1}`)])),
})
const other = site("https://other.example.com", { "/echo": echo })

for (const engine of engines) {
  scenario(
    `codemode fetch${engine.suffix}`,
    { config: engine.config, responders: [api, other] },
    async (ctx) => {
      const id = await ctx.session()
      ctx.provider.push(
        ...programs(
          `const text = await fetch("https://api.example.com/text")
const json = await fetch("https://api.example.com/echo?q=1#fragment")
const bytes = await fetch("https://api.example.com/bytes")
const created = await fetch(new URL("https://api.example.com/created"))
const missing = await fetch("https://api.example.com/missing")
return {
  text: { url: text.url, status: text.status, statusText: text.statusText, ok: text.ok, redirected: text.redirected, body: await text.text(),
    contentType: text.headers.get("Content-Type"), custom: text.headers.get("x-custom"), has: [text.headers.has("x-custom"), text.headers.has("nope")],
    none: text.headers.get("nope"), entries: text.headers.entries().filter(([k]) => k !== "date") },
  json: await json.json(),
  bytes: Array.from(await bytes.bytes()),
  created: [created.status, await created.json()],
  missing: [missing.ok, missing.status, await missing.text()],
}`,
          `const post = await fetch("https://api.example.com/echo", {
  method: "post",
  headers: { "content-type": "application/json", authorization: "Bearer secret", "x-trace": "1" },
  body: JSON.stringify({ hello: "world" }),
})
const form = await fetch("https://api.example.com/echo", { method: "PUT", headers: [["x-pairs", "yes"]], body: new URLSearchParams({ a: "1", b: "two" }) })
const binary = await fetch("https://api.example.com/echo", { method: "PATCH", body: new TextEncoder().encode("bytes body") })
const head = await fetch("https://api.example.com/text", { method: "HEAD" })
return { post: await post.json(), form: await form.json(), binary: await binary.json(), head: [head.status, await head.text()] }`,
          `const results = {}
for (const path of ["r301", "r302", "r303", "r307", "r308", "cross"]) {
  const response = await fetch("https://api.example.com/" + path, { method: "POST", headers: { authorization: "Bearer x", "content-type": "text/plain" }, body: "payload" })
  const echoed = await response.json()
  results[path] = { redirected: response.redirected, url: response.url, method: echoed.method, body: echoed.body, auth: echoed.headers.authorization ?? null }
}
for (const path of ["to-ftp", "to-creds", "loop/0", "no-location"]) {
  try { const r = await fetch("https://api.example.com/" + path); results[path] = [r.status, r.redirected] } catch (error) { results[path] = error.name + ": " + error.message }
}
const see = await fetch("https://api.example.com/r303", { method: "HEAD" })
results.seeOtherHead = see.status
return results`,
          `const results = {}
for (const [label, url] of Object.entries({ file: "file:///etc/passwd", creds: "https://user:pw@api.example.com/echo", unanswered: "https://unanswered.example.com/" })) {
  try { const r = await fetch(url); results[label] = [r.status, await r.text()] } catch (error) { results[label] = error.name + ": " + error.message }
}
try { await (await fetch("https://api.example.com/bad-json")).json() } catch (error) { results.badJson = error.name }
for (const [label, init] of Object.entries({
  dateBody: { method: "POST", body: new Date(0) },
  mapHeaders: { headers: new Map([["a", "b"]]) },
  headersObj: { headers: new Headers({ "x-h": "1" }) },
  regexBody: { method: "POST", body: /re/g },
  setBody: { method: "POST", body: new Set([1]) },
  urlBody: { method: "POST", body: new URL("https://x.example.com/") },
  arrayBody: { method: "POST", body: [1, 2] },
  fnInit: { method: "POST", body: () => 1 },
  promiseInit: { method: "POST", body: Promise.resolve(1) },
  errorBody: { method: "POST", body: new TypeError("bad", { cause: new Error("root") }) },
  deep: { method: "POST", body: JSON.parse("[".repeat(40) + "]".repeat(40)) },
})) {
  try { const r = await fetch("https://api.example.com/echo", init); results[label] = (await r.json()).body } catch (error) { results[label] = error.name + ": " + error.message }
}
try {
  const looped = { headers: {} }
  looped.headers.self = looped
  await fetch("https://api.example.com/echo", looped)
} catch (error) { results.circular = error.name + ": " + error.message }
return results`,
          `const pending = fetch("https://api.example.com/text")
const all = await Promise.all([fetch("https://api.example.com/text").then((r) => r.text()), fetch("https://api.example.com/created").then((r) => r.status)])
return { all, pendingType: typeof pending.then }`,
        ),
      )
      await withFetch(ctx, () => ctx.prompt(id, "Call the API").then(() => undefined))
      await settled(ctx, id)
    },
  )

  scenario(
    `codemode fetch permissions${engine.suffix}`,
    { config: {
        ...engine.config,
        permissions: [
          { action: "experimental_image_generate", resource: "*", effect: "deny" },
          { action: "websearch", resource: "*", effect: "deny" },
          { action: "opencode_list_mcp_resources", resource: "*", effect: "deny" },
          { action: "opencode_read_mcp_resource", resource: "*", effect: "deny" },
          { action: "webfetch", resource: "https://api.example.com/*", effect: "ask" },
          { action: "webfetch", resource: "https://other.example.com/*", effect: "deny" },
        ],
      }, responders: [api, other] },
    async (ctx) => {
      const id = await ctx.session()
      ctx.provider.push(
        ...programs(
          `const results = {}
for (const path of ["text", "created", "missing", "cross"]) {
  try { const r = await fetch("https://api.example.com/" + path); results[path] = r.status } catch (error) { results[path] = error.name + ": " + error.message }
}
try { await fetch("https://other.example.com/echo") } catch (error) { results.denied = error.name + ": " + error.message }
return results`,
        ),
      )
      await withFetch(ctx, () => drive(ctx, id, "Fetch with approval", ["once", "always"]))
      await ctx.http("GET", `/api/session/${id}/message`)
      await settled(ctx, id)
    },
  )

  scenario(
    `codemode fetch rejected with feedback${engine.suffix}`,
    { config: {
        ...engine.config,
        permissions: [
          { action: "experimental_image_generate", resource: "*", effect: "deny" },
          { action: "websearch", resource: "*", effect: "deny" },
          { action: "opencode_list_mcp_resources", resource: "*", effect: "deny" },
          { action: "opencode_read_mcp_resource", resource: "*", effect: "deny" },
          { action: "webfetch", resource: "https://api.example.com/*", effect: "ask" },
          { action: "webfetch", resource: "https://other.example.com/*", effect: "deny" },
        ],
      }, responders: [api] },
    async (ctx) => {
      const id = await ctx.session()
      ctx.provider.push(
        ...programs(
          `try { await fetch("https://api.example.com/text") } catch (error) { return error.name + ": " + error.message }`,
          `try { await fetch("https://api.example.com/created") } catch (error) { return error.name + ": " + error.message }`,
        ),
      )
      await withFetch(ctx, () =>
        drive(ctx, id, "Fetch with approval", [{ decision: "reject", message: "Not that one." }, "reject"]),
      )
      await ctx.http("GET", `/api/session/${id}/message`)
      await settled(ctx, id)
    },
  )
}

// Large program output is truncated by the session's tool-output limits, and every row of progress shows.
for (const engine of engines) {
  scenario(
    `codemode large output and logs${engine.suffix}`,
    { config: engine.config },
    async (ctx) => {
      const id = await ctx.session()
      ctx.provider.push(
        ...programs(
          `for (let i = 0; i < 3000; i++) console.log("log line " + i)
return "x".repeat(60000)`,
          `const rows = []
for (let i = 0; i < 5; i++) rows.push(await tools.opencode.models({ limit: 1, offset: i }))
return rows.map((r) => r.next)`,
        ),
      )
      await ctx.prompt(id, "Produce a lot of output")
      await ctx.http("GET", `/api/session/${id}/message`)
      await settled(ctx, id)
    },
  )
}
