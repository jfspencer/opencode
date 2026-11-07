// Permission system through its public API: rule evaluation (config, agent defaults, session rules, saved
// approvals; ordered, last match wins), pending requests and their replies, saved-approval management, and
// the permission flow of tool calls.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

type Effect = "allow" | "deny" | "ask"
const rule = (action: string, resource: string, effect: Effect) => ({ action, resource, effect })
/** Base permissions with out-of-scope tools hidden, as in lib/model.ts. */
const hidden = HIDDEN_TOOLS.map((action) => rule(action, "*", "deny"))

/** Create a permission request through the API; returns the response body's data. */
async function ask(ctx: Ctx, sessionID: string, payload: Record<string, unknown>) {
  const result = await ctx.http("POST", `/api/session/${sessionID}/permission`, payload)
  return result.body?.data as { id: string; effect: Effect } | undefined
}

/**
 * Wait for the location's plugins (agents, config rules) to finish activating. The permission request endpoint
 * does not wait for them itself, so requests made earlier would race agent loading.
 */
const ready = (ctx: Ctx, agents = false) => ctx.http("GET", agents ? "/api/agent" : "/api/provider")

const reply = (ctx: Ctx, sessionID: string, requestID: string, payload: Record<string, unknown>) =>
  ctx.http("POST", `/api/session/${sessionID}/permission/${requestID}/reply`, payload)

scenario(
  "evaluate permission requests against agent, config and session rules",
  {
    config: {
      permissions: [
        ...hidden,
        rule("deploy", "*", "ask"),
        rule("deploy", "staging/*", "allow"),
        rule("deploy", "staging/db/*", "deny"),
        rule("publish", "*", "deny"),
      ],
    },
  },
  async (ctx) => {
    await ready(ctx)
    const id = await ctx.session({
      title: "evaluate",
      permissions: [rule("publish", "docs/*", "allow"), rule("archive", "*", "deny")],
    })
    // The build agent allows everything not otherwise configured.
    await ask(ctx, id, { action: "lint", resources: ["src/a.ts"] })
    // Config rules: last match wins.
    await ask(ctx, id, { action: "deploy", resources: ["staging/web"] })
    await ask(ctx, id, { action: "deploy", resources: ["staging/db/main"] })
    await ask(ctx, id, { action: "deploy", resources: ["staging", "staging/web"] })
    // Any denied resource denies the whole request, even when another one asks.
    await ask(ctx, id, { action: "deploy", resources: ["production", "staging/db/x"] })
    // Session rules come after config rules.
    await ask(ctx, id, { action: "publish", resources: ["docs/index.md"] })
    await ask(ctx, id, { action: "publish", resources: ["docs/index.md", "src/x"] })
    await ask(ctx, id, { action: "archive", resources: ["anything"] })
    // An empty resource list neither asks nor denies.
    await ask(ctx, id, { action: "deploy", resources: [] })
    // The build agent's own defaults: reading env files asks, except examples.
    await ask(ctx, id, { action: "read", resources: [".env"] })
    await ask(ctx, id, { action: "read", resources: ["config/.env.local"] })
    await ask(ctx, id, { action: "read", resources: [".env.example"] })
    // Nothing is pending except the asks above.
    await ctx.http("GET", `/api/session/${id}/permission`)
  },
)

scenario(
  "rule patterns match actions and resources",
  {
    config: {
      permissions: [
        ...hidden,
        rule("check", "*", "ask"),
        rule("check", "git status *", "allow"),
        rule("check", "?.txt", "allow"),
        rule("check", "docs/*", "allow"),
        rule("check", "*.lock", "deny"),
        rule("dep*", "*", "deny"),
        rule("deploy", "preview *", "allow"),
      ],
    },
  },
  async (ctx) => {
    await ready(ctx)
    const id = await ctx.session({ title: "patterns" })
    // A trailing " *" also matches the bare prefix; "*" spans "/" and spaces; "?" is one character.
    for (const resource of ["git status", "git status -s", "git statusx", "a.txt", "ab.txt", "docs/a/b.md", "docs", "yarn.lock", "x.lock.bak"])
      await ask(ctx, id, { action: "check", resources: [resource] })
    // Backslashes match as forward slashes.
    await ask(ctx, id, { action: "check", resources: ["docs\\guide.md"] })
    // Action patterns: later rules for a narrower action still win.
    await ask(ctx, id, { action: "depend", resources: ["x"] })
    await ask(ctx, id, { action: "deploy", resources: ["preview web"] })
    await ask(ctx, id, { action: "deploy", resources: ["production"] })
    await ctx.http("GET", "/api/permission/request")
  },
)

scenario(
  "evaluate permission requests for an explicit agent",
  { config: { permissions: [...hidden, rule("shell", "git *", "ask")] } },
  async (ctx) => {
    await ready(ctx)
  const id = await ctx.session({ title: "agents" })
    // The plan agent denies edits outside its plan directory; explore denies everything but read-only tools.
    await ask(ctx, id, { action: "edit", resources: ["src/a.ts"], agent: "plan" })
    await ask(ctx, id, { action: "edit", resources: ["src/a.ts"], agent: "build" })
    await ask(ctx, id, { action: "edit", resources: ["src/a.ts"], agent: "explore" })
    await ask(ctx, id, { action: "grep", resources: ["*"], agent: "explore" })
    await ask(ctx, id, { action: "read", resources: [".env"], agent: "explore" })
    // Config rules are appended to every agent's defaults, so they win over explore's shell allow.
    await ask(ctx, id, { action: "shell", resources: ["git status"], agent: "explore" })
    await ask(ctx, id, { action: "shell", resources: ["ls"], agent: "explore" })
    // Hidden agents deny everything; an unknown agent has no rules and is denied.
    await ask(ctx, id, { action: "read", resources: ["a.txt"], agent: "title" })
    await ask(ctx, id, { action: "read", resources: ["a.txt"], agent: "no-such-agent" })
    // A session created for the plan agent evaluates with it when the request names none.
    const plan = await ctx.session({ title: "plan", agent: "plan" })
    await ask(ctx, plan, { action: "edit", resources: ["src/a.ts"] })
    await ctx.http("GET", "/api/permission/request")
  },
)

scenario("pending permission request lifecycle", { config: { permissions: [...hidden, rule("deploy", "*", "ask")] } }, async (ctx) => {
  await ready(ctx)
  const id = await ctx.session({ title: "lifecycle" })
  const other = await ctx.session({ title: "other" })
  const first = await ask(ctx, id, {
    action: "deploy",
    resources: ["web"],
    save: ["web*"],
    metadata: { target: "web", dryRun: false, nested: { count: 2 } },
  })
  const second = await ask(ctx, id, { id: "per_conformance_explicit_id", action: "deploy", resources: ["api"] })
  await ask(ctx, other, { action: "deploy", resources: ["docs"] })
  // Listing: per session, and every pending request of the location.
  await ctx.http("GET", `/api/session/${id}/permission`)
  await ctx.http("GET", `/api/session/${other}/permission`)
  await ctx.http("GET", "/api/permission/request")
  await ctx.http("GET", `/api/session/${id}/permission/${first!.id}`)
  // A request is only visible through the session that owns it.
  await ctx.http("GET", `/api/session/${other}/permission/${first!.id}`)
  await reply(ctx, other, first!.id, { decision: "once" })
  await ctx.http("GET", `/api/session/${id}/permission/per_conformance_missing_request`)
  // Answering once resolves only that request and saves nothing.
  await reply(ctx, id, first!.id, { decision: "once" })
  await ctx.http("GET", `/api/session/${id}/permission/${first!.id}`)
  await reply(ctx, id, first!.id, { decision: "once" })
  await ctx.http("GET", `/api/session/${id}/permission`)
  await reply(ctx, id, second!.id, { decision: "once", message: "ignored for approvals" })
  await ctx.http("GET", "/api/permission/saved")
  // Invalid payloads and identifiers.
  await reply(ctx, other, "per_conformance_missing_request", { decision: "maybe" })
  await reply(ctx, other, "per_conformance_missing_request", {})
  await ctx.http("GET", `/api/session/${other}/permission/not-a-permission-id`)
  await ask(ctx, other, { action: "deploy" })
  await ask(ctx, "ses_conformance_missing_session", { action: "deploy", resources: ["x"] })
  await ctx.http("GET", "/api/session/ses_conformance_missing_session/permission")
})

scenario(
  "rejecting a request rejects the other pending requests of its session",
  { config: { permissions: [...hidden, rule("deploy", "*", "ask")] } },
  async (ctx) => {
    await ready(ctx)
  const id = await ctx.session({ title: "reject" })
    const other = await ctx.session({ title: "other" })
    const a = await ask(ctx, id, { action: "deploy", resources: ["a"] })
    await ask(ctx, id, { action: "deploy", resources: ["b"] })
    await ask(ctx, other, { action: "deploy", resources: ["c"] })
    await reply(ctx, id, a!.id, { decision: "reject" })
    await ctx.http("GET", `/api/session/${id}/permission`)
    await ctx.http("GET", "/api/permission/request")
    // Feedback with a rejection applies to the whole batch as well.
    const d = await ask(ctx, id, { action: "deploy", resources: ["d"] })
    await ask(ctx, id, { action: "deploy", resources: ["e"] })
    await reply(ctx, id, d!.id, { decision: "reject", message: "Use the staging target instead." })
    await ctx.http("GET", "/api/permission/request")
    await ctx.http("GET", "/api/permission/saved")
  },
)

scenario(
  "always saves approvals and settles other pending requests they now allow",
  {
    config: {
      permissions: [...hidden, rule("deploy", "*", "ask"), rule("deploy", "prod/*", "deny"), rule("publish", "*", "ask")],
    },
  },
  async (ctx) => {
    await ready(ctx)
    const id = await ctx.session({ title: "always" })
    const other = await ctx.session({ title: "other" })
    const first = await ask(ctx, id, { action: "deploy", resources: ["web/1"], save: ["web/*", "prod/*"] })
    // Pending in this session and in another one; only those the saved approvals now allow settle.
    await ask(ctx, id, { action: "deploy", resources: ["web/2"] })
    await ask(ctx, other, { action: "deploy", resources: ["web/3", "api/1"] })
    await ask(ctx, other, { action: "deploy", resources: ["web/4"] })
    await ask(ctx, id, { action: "publish", resources: ["web/5"] })
    await reply(ctx, id, first!.id, { decision: "always" })
    await ctx.http("GET", "/api/permission/request")
    await ctx.http("GET", "/api/permission/saved")
    // Saved approvals allow later requests, but configured denies still win.
    await ask(ctx, id, { action: "deploy", resources: ["web/6"] })
    await ask(ctx, id, { action: "deploy", resources: ["prod/1"] })
    await ask(ctx, other, { action: "deploy", resources: ["web/7", "api/2"] })
    // Always without save patterns resolves the request but saves nothing.
    const bare = await ask(ctx, id, { action: "publish", resources: ["x"] })
    await reply(ctx, id, bare!.id, { decision: "always" })
    const empty = await ask(ctx, id, { action: "publish", resources: ["y"], save: [] })
    await reply(ctx, id, empty!.id, { decision: "always" })
    // Saving an approval that already exists keeps one copy.
    const again = await ask(ctx, other, { action: "deploy", resources: ["api/3"], save: ["web/*", "api/*"] })
    await reply(ctx, other, again!.id, { decision: "always" })
    await ctx.http("GET", "/api/permission/saved")
    await ctx.http("GET", "/api/permission/request")
  },
)

scenario(
  "edit and remove saved approvals",
  { config: { permissions: [...hidden, rule("deploy", "*", "ask")] } },
  async (ctx) => {
    await ready(ctx)
  const id = await ctx.session({ title: "saved" })
    for (const resource of ["src/*", "docs/*"]) {
      const asked = await ask(ctx, id, { action: "deploy", resources: [resource], save: [resource] })
      await reply(ctx, id, asked!.id, { decision: "always" })
    }
    const listed = await ctx.http("GET", "/api/permission/saved")
    const [docs, src] = [...listed.body.data].sort((a: any, b: any) => a.resource.localeCompare(b.resource))
    // Narrow one approval: requests outside it ask again.
    await ctx.http("PATCH", `/api/permission/saved/${src.id}`, { resource: "src/allowed.ts" })
    await ask(ctx, id, { action: "deploy", resources: ["src/allowed.ts"] })
    const pending = await ask(ctx, id, { action: "deploy", resources: ["src/other.ts"] })
    await reply(ctx, id, pending!.id, { decision: "reject" })
    // Change the action: the approval now applies to another action.
    await ctx.http("PATCH", `/api/permission/saved/${docs.id}`, { action: "publish" })
    await ask(ctx, id, { action: "publish", resources: ["docs/a.md"] })
    const asked = await ask(ctx, id, { action: "deploy", resources: ["docs/a.md"] })
    await reply(ctx, id, asked!.id, { decision: "reject" })
    // An empty patch keeps the approval; a duplicate conflicts; unknown IDs are not found.
    await ctx.http("PATCH", `/api/permission/saved/${docs.id}`, {})
    await ctx.http("PATCH", `/api/permission/saved/${src.id}`, { action: "publish", resource: "docs/*" })
    await ctx.http("PATCH", "/api/permission/saved/psv_conformance_missing", { resource: "x" })
    await ctx.http("GET", "/api/permission/saved")
    // Saved approvals belong to a project.
    await ctx.http("GET", `/api/permission/saved?projectID=${listed.body.data[0].projectID}`)
    await ctx.http("GET", "/api/permission/saved?projectID=prj_conformance_other_project")
    // Removing is idempotent.
    await ctx.http("DELETE", `/api/permission/saved/${src.id}`)
    await ctx.http("DELETE", `/api/permission/saved/${src.id}`)
    await ctx.http("GET", "/api/permission/saved")
    const after = await ask(ctx, id, { action: "deploy", resources: ["src/allowed.ts"] })
    await reply(ctx, id, after!.id, { decision: "reject" })
  },
)

// Tool calls: the shell tool asks for its scanned commands; the model is told about rejections. Every other tool
// is denied (and so hidden) to keep the recorded provider requests small.
const shellAsks = [...hidden, rule("*", "*", "deny"), rule("shell", "*", "ask")]

async function prompt(ctx: Ctx, id: string, text: string, decide: (request: any) => Record<string, unknown>) {
  let from = ctx.events.length
  await ctx.http("POST", `/api/session/${id}/prompt`, { text })
  let state = await ctx.settle(id, from)
  while (state === "permission.asked") {
    const asked = ctx.events.filter((event: any) => event?.type === "permission.asked" && event.data.sessionID === id).at(-1) as any
    from = ctx.events.length
    await reply(ctx, id, asked.data.id, decide(asked.data))
    state = await ctx.settle(id, from)
  }
}

scenario(
  "approve a shell command once",
  {
    files: { "notes.txt": "alpha\nbeta\n" },
    config: { permissions: shellAsks },
    turns: [call("shell", { command: "cat notes.txt" }), say("Read it.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "once" })
    await prompt(ctx, id, "Show notes.txt", () => ({ decision: "once" }))
    await ctx.http("GET", "/api/permission/saved")
  },
)

scenario(
  "decline a shell command without feedback",
  {
    files: { "notes.txt": "alpha\n" },
    config: { permissions: shellAsks },
    turns: [call("shell", { command: "rm notes.txt" }), say("Not reached.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "decline" })
    await prompt(ctx, id, "Delete notes.txt", () => ({ decision: "reject" }))
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "decline a shell command with feedback",
  {
    files: { "notes.txt": "alpha\n" },
    config: { permissions: shellAsks },
    turns: [call("shell", { command: "rm notes.txt" }), call("shell", { command: "mv notes.txt old-notes.txt" }), say("Moved instead.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "feedback" })
    await prompt(ctx, id, "Delete notes.txt", (request) =>
      request.resources[0].startsWith("rm") ? { decision: "reject", message: "Move it instead of deleting it." } : { decision: "once" },
    )
  },
)

scenario(
  "always approve a shell command prefix",
  {
    files: { "a.txt": "a\n", "b.txt": "b\n" },
    config: { permissions: shellAsks },
    turns: [
      call("shell", { command: "cat a.txt" }),
      call("shell", { command: "cat b.txt && wc -l a.txt" }),
      call("shell", { command: "wc -c b.txt" }),
      say("Done."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "always" })
    await prompt(ctx, id, "Read the files", () => ({ decision: "always" }))
    await ctx.http("GET", "/api/permission/saved")
  },
)

scenario(
  "always on a wrapped command approves only the wrapped command",
  {
    files: { "a.txt": "a\n" },
    config: { permissions: shellAsks },
    turns: [
      call("shell", { command: "env echo one" }),
      call("shell", { command: "env cat a.txt" }),
      call("shell", { command: "env echo two" }),
      say("Done."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "wrapped" })
    await prompt(ctx, id, "Echo through env", (request) =>
      request.resources[0].startsWith("env echo") ? { decision: "always" } : { decision: "reject", message: "Not that one." },
    )
    await ctx.http("GET", "/api/permission/saved")
  },
)

scenario(
  "always on a command with an environment assignment approves it under that assignment only",
  {
    config: { permissions: shellAsks },
    turns: [
      call("shell", { command: "LANG=C echo one" }),
      call("shell", { command: "LANG=C echo one" }),
      call("shell", { command: "LC_ALL=C echo one" }),
      say("Done."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "assignment" })
    await prompt(ctx, id, "Echo with a locale", (request) =>
      request.resources[0].startsWith("LANG=") ? { decision: "always" } : { decision: "reject", message: "Not that one." },
    )
    await ctx.http("GET", "/api/permission/saved")
  },
)

scenario(
  "always on a command with wildcard characters approves only that text",
  {
    files: { "a.txt": "a\n", "b.txt": "b\n" },
    config: { permissions: shellAsks },
    turns: [
      call("shell", { command: "sh -c 'echo *.txt'" }),
      call("shell", { command: "sh -c 'echo ; cat a.txt'" }),
      call("shell", { command: "sh -c 'echo *.txt'" }),
      say("Done."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "wildcards" })
    await prompt(ctx, id, "List the text files", (request) =>
      request.resources[0] === "sh -c 'echo *.txt'" ? { decision: "always" } : { decision: "reject", message: "Not that one." },
    )
    await ctx.http("GET", "/api/permission/saved")
  },
)

scenario(
  "configured denies win over saved shell approvals",
  {
    files: { "a.txt": "a\n" },
    config: { permissions: [...shellAsks, rule("shell", "cat secret*", "deny")] },
    turns: [call("shell", { command: "cat a.txt" }), call("shell", { command: "cat secret.txt" }), say("Done.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "deny wins" })
    await prompt(ctx, id, "Read the files", () => ({ decision: "always" }))
  },
)

scenario(
  "session rules allow and deny shell commands",
  {
    files: { "a.txt": "a\n" },
    config: { permissions: shellAsks },
    turns: [call("shell", { command: "cat a.txt" }), call("shell", { command: "touch b.txt" }), say("Done.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "session rules", permissions: [rule("shell", "cat *", "allow"), rule("shell", "touch *", "deny")] })
    await prompt(ctx, id, "Go", () => ({ decision: "reject" }))
    // Replacing the session rules changes later decisions.
    await ctx.http("PATCH", `/api/session/${id}`, { permissions: [rule("shell", "touch *", "allow")] })
    await ctx.http("GET", `/api/session/${id}`)
    ctx.provider.push(call("shell", { command: "touch b.txt" }), say("Touched."))
    await prompt(ctx, id, "Touch it now", () => ({ decision: "reject" }))
  },
)

scenario(
  "web fetch redirects are checked against deny rules",
  {
    config: {
      permissions: [...hidden, rule("webfetch", "*", "allow"), rule("webfetch", "https://blocked.example/*", "deny")],
    },
    responders: [
      (request) =>
        request.url.startsWith("https://docs.example/")
          ? { status: 302, headers: { location: "https://blocked.example/secret" } }
          : undefined,
    ],
    turns: [call("webfetch", { url: "https://docs.example/start", format: "text" }), say("Blocked.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "redirect" })
    await ctx.prompt(id, "Fetch the docs")
  },
)
